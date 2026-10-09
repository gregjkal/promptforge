//! Unit tests for the per-platform Tauri configs, the macOS Gateway
//! bundle, and the macOS installer sequences.

use serde_json::Value;

use super::installer_tests::{
    bundle, bundled, collect_signature, expected_commands, gateway, node_found, output, request,
    signer_command, version_printed,
};
use super::*;
use crate::installer::Platform;
use crate::installer::bundle::{self, BundleSources};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn tauri_config(name: &str) -> Value {
    let path = repository_root()
        .join("crates")
        .join("workshop")
        .join("desktop")
        .join(name);
    let text = std::fs::read_to_string(&path).expect("Tauri config");
    serde_json::from_str(&text).expect("Tauri config JSON")
}

#[test]
fn each_platform_config_builds_the_installer_bundle_and_only_windows_carries_the_gateway() {
    let base = tauri_config("tauri.conf.json");
    assert_eq!(base["bundle"].get("targets"), None);
    assert_eq!(base["bundle"].get("externalBin"), None);
    for (file, target) in [
        ("tauri.windows.conf.json", "x86_64-pc-windows-msvc"),
        ("tauri.macos.conf.json", "aarch64-apple-darwin"),
        ("tauri.linux.conf.json", "x86_64-unknown-linux-gnu"),
    ] {
        let config = tauri_config(file);
        let platform = Platform::from_triple(target).expect("platform");
        assert_eq!(
            config["bundle"]["targets"],
            serde_json::json!([platform.bundle()]),
            "{file}"
        );
        let external = config["bundle"].get("externalBin");
        if sidecar::bundles_sidecar(target) {
            assert_eq!(
                external,
                Some(&serde_json::json!(["binaries/promptforge-gateway"])),
                "{file}"
            );
        } else {
            assert_eq!(external, None, "{file}");
        }
    }
}

#[test]
fn the_gateway_bundle_holds_the_versioned_plist_the_executable_and_the_icon() {
    let temp = tempfile::tempdir().expect("temporary root");
    let gateway = temp.path().join("promptforge-gateway");
    write_file(&gateway, b"gateway");
    let app = temp.path().join("PromptForge Gateway.app");
    let sources = BundleSources::in_workspace(&repository_root());
    // The guard in `assemble` only checks that the placeholder appears, so
    // anywhere but the two version keys would defeat it.
    let template = std::fs::read_to_string(&sources.info_plist).expect("template");
    assert_eq!(template.matches("@VERSION@").count(), 2, "{template}");

    bundle::assemble(&sources, &gateway, &app).expect("assemble");

    let mut files = Vec::new();
    collect_files(&app, &app, &mut files);
    files.sort();
    assert_eq!(
        files,
        [
            "Contents/Info.plist",
            "Contents/MacOS/promptforge-gateway",
            "Contents/Resources/icon.icns",
        ]
    );
    let plist =
        std::fs::read_to_string(app.join("Contents").join("Info.plist")).expect("Info.plist");
    assert!(
        plist.contains(&format!(
            "<key>CFBundleShortVersionString</key>\n    <string>{VERSION}</string>"
        )),
        "{plist}"
    );
    assert!(
        plist.contains(&format!(
            "<key>CFBundleVersion</key>\n    <string>{VERSION}</string>"
        )),
        "{plist}"
    );
    assert!(!plist.contains("@VERSION@"), "{plist}");
    assert!(
        plist.contains("<key>CFBundleExecutable</key>\n    <string>promptforge-gateway</string>"),
        "{plist}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let executable = app
            .join("Contents")
            .join("MacOS")
            .join("promptforge-gateway");
        let mode = std::fs::metadata(executable)
            .expect("executable")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
    }
}

#[test]
fn a_template_without_the_placeholder_or_a_missing_icon_fails_naming_the_file() {
    let temp = tempfile::tempdir().expect("temporary root");
    let gateway = temp.path().join("promptforge-gateway");
    write_file(&gateway, b"gateway");
    let real = BundleSources::in_workspace(&repository_root());
    let fixed = temp.path().join("Info.plist");
    write_file(&fixed, b"<plist><string>1.0.0</string></plist>");

    let error = bundle::assemble(
        &BundleSources {
            info_plist: fixed.clone(),
            icon: real.icon.clone(),
        },
        &gateway,
        &temp.path().join("a.app"),
    )
    .expect_err("no placeholder");
    assert!(error.contains(&fixed.display().to_string()), "{error}");
    assert!(error.contains("@VERSION@"), "{error}");

    let missing = temp.path().join("icon.icns");
    let error = bundle::assemble(
        &BundleSources {
            info_plist: real.info_plist,
            icon: missing.clone(),
        },
        &gateway,
        &temp.path().join("b.app"),
    )
    .expect_err("no icon");
    assert!(error.contains(&missing.display().to_string()), "{error}");
}

fn collect_files(root: &Path, directory: &Path, files: &mut Vec<String>) {
    for entry in std::fs::read_dir(directory).expect("read directory") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            collect_files(root, &path, files);
        } else {
            let relative = path.strip_prefix(root).expect("under root");
            files.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
}

#[test]
fn macos_signed_collects_the_payload_and_signs_the_gateway_archive() {
    let mut test_environment = environment();
    set_signing_key(&mut test_environment.environment);
    let environment = &test_environment.environment;
    let target = "aarch64-apple-darwin";
    let macos = bundle(environment, target, "macos");
    let updater = macos.join("PromptForge.app.tar.gz");
    let output_root = output(environment, target);
    let archive = output_root.join("publish").join(format!(
        "promptforge-gateway_{VERSION}_darwin-aarch64.tar.gz"
    ));
    let signed_archive = archive.clone();
    let mut runner = FakeRunner::with_responses(vec![
        node_found(),
        gateway_built(gateway(environment, target)),
        version_printed(),
        bundled(
            test_environment.sidecar(target),
            target,
            vec![
                macos
                    .join("PromptForge.app")
                    .join("Contents")
                    .join("MacOS")
                    .join("promptforge-workshop"),
                collect_signature(&updater),
                updater,
            ],
            success(""),
        ),
        act(
            move || {
                assert!(signed_archive.is_file(), "archive missing when signed");
                write_file(&collect_signature(&signed_archive), b"signature");
            },
            success(""),
        ),
    ]);

    build_installer(&request(target, true), environment, &mut runner).expect("installer");

    let mut expected = expected_commands(environment, target, "app", true);
    expected.push(signer_command(environment, &archive));
    assert_eq!(runner.commands, expected);
    let payload = output_root.join("payload");
    assert!(
        payload
            .join("PromptForge.app")
            .join("Contents")
            .join("MacOS")
            .join("promptforge-workshop")
            .is_file()
    );
    assert_eq!(
        std::fs::read(
            payload
                .join("PromptForge Gateway.app")
                .join("Contents")
                .join("MacOS")
                .join("promptforge-gateway")
        )
        .expect("gateway payload"),
        b"gateway"
    );
    assert!(!payload.join("promptforge-gateway").exists());
    let publish = output_root.join("publish");
    let app_updater = publish.join(format!("PromptForge_{VERSION}_aarch64.app.tar.gz"));
    assert!(app_updater.is_file());
    assert!(collect_signature(&app_updater).is_file());
    assert!(collect_signature(&archive).is_file());
}

#[test]
fn macos_unsigned_collects_the_payload_only() {
    let test_environment = environment();
    let environment = &test_environment.environment;
    let target = "x86_64-apple-darwin";
    let app = bundle(environment, target, "macos").join("PromptForge.app");
    let mut runner = FakeRunner::with_responses(vec![
        node_found(),
        gateway_built(gateway(environment, target)),
        version_printed(),
        bundled(
            test_environment.sidecar(target),
            target,
            vec![
                app.join("Contents")
                    .join("MacOS")
                    .join("promptforge-workshop"),
            ],
            success(""),
        ),
    ]);

    build_installer(&request(target, false), environment, &mut runner).expect("installer");

    assert_eq!(
        runner.commands,
        expected_commands(environment, target, "app", false)
    );
    let payload = output(environment, target).join("payload");
    assert!(
        payload
            .join("PromptForge.app")
            .join("Contents")
            .join("MacOS")
            .join("promptforge-workshop")
            .is_file()
    );
    assert!(
        payload
            .join("PromptForge Gateway.app")
            .join("Contents")
            .join("Info.plist")
            .is_file()
    );
    assert!(!payload.join("promptforge-gateway").exists());
    assert_eq!(
        std::fs::read_dir(output(environment, target).join("publish"))
            .expect("publish")
            .count(),
        0
    );
}
