//! Unit tests for the installer command sequence, its preflight, and its
//! failure paths.

use super::*;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn release(environment: &BuildEnvironment, target: &str) -> PathBuf {
    environment.target_root.join(target).join("release")
}

fn gateway(environment: &BuildEnvironment, target: &str) -> PathBuf {
    release(environment, target).join(sidecar::gateway_binary_name(target))
}

fn bundle(environment: &BuildEnvironment, target: &str, directory: &str) -> PathBuf {
    release(environment, target).join("bundle").join(directory)
}

fn output(environment: &BuildEnvironment, target: &str) -> PathBuf {
    environment.target_root.join("installer").join(target)
}

fn version_printed() -> FakeResponse {
    success(&format!("promptforge-gateway {VERSION}\n"))
}

/// A Tauri build that writes `files` while the sidecar is staged.
fn bundled(sidecar: PathBuf, files: Vec<PathBuf>, response: FakeResponse) -> FakeResponse {
    act(
        move || {
            assert!(sidecar.is_file(), "sidecar not staged during the bundle");
            for file in files {
                write_file(&file, b"bundle");
            }
        },
        response,
    )
}

fn expected_commands(
    environment: &BuildEnvironment,
    target: &str,
    bundle: &str,
    sign: bool,
) -> Vec<CommandSpec> {
    let mut tauri = vec![
        environment.tauri_cli.to_str().expect("UTF-8 CLI"),
        "build",
        "--ci",
        "--target",
        target,
        "--bundles",
        bundle,
    ];
    if !sign {
        tauri.extend(["--config", "tauri.nightly.conf.json"]);
    }
    vec![
        command(
            environment,
            "selected-cargo",
            &[
                "build",
                "--locked",
                "--release",
                "-p",
                "gateway",
                "--target",
                target,
            ],
            OutputMode::Inherit,
        ),
        CommandSpec {
            program: gateway(environment, target),
            args: strings(&["--version"]),
            current_dir: environment.workspace_root.clone(),
            output_mode: OutputMode::Capture,
        },
        CommandSpec {
            current_dir: environment
                .workspace_root
                .join("crates")
                .join("workshop")
                .join("desktop"),
            ..command(environment, "selected-node", &tauri, OutputMode::Inherit)
        },
    ]
}

fn signer_command(environment: &BuildEnvironment, archive: &Path) -> CommandSpec {
    command(
        environment,
        "selected-node",
        &[
            environment.tauri_cli.to_str().expect("UTF-8 CLI"),
            "signer",
            "sign",
            "--app-version",
            VERSION,
            archive.to_str().expect("UTF-8 archive"),
        ],
        OutputMode::Inherit,
    )
}

fn request(target: &str, sign: bool) -> InstallerRequest {
    InstallerRequest {
        target: Some(target.to_owned()),
        sign,
    }
}

#[test]
fn windows_unsigned_builds_the_setup_on_the_unsigned_config() {
    let test_environment = environment();
    let environment = &test_environment.environment;
    let target = "x86_64-pc-windows-msvc";
    let setup =
        bundle(environment, target, "nsis").join(format!("PromptForge_{VERSION}_x64-setup.exe"));
    // A setup left by an earlier build at another version must not make
    // the collection ambiguous.
    write_file(
        &bundle(environment, target, "nsis").join("PromptForge_0.0.1_x64-setup.exe"),
        b"stale",
    );
    let mut runner = FakeRunner::with_responses(vec![
        gateway_built(gateway(environment, target)),
        version_printed(),
        bundled(test_environment.sidecar(target), vec![setup], success("")),
    ]);

    build_installer(&request(target, false), environment, &mut runner).expect("installer");

    assert_eq!(
        runner.commands,
        expected_commands(environment, target, "nsis", false)
    );
    let publish = output(environment, target).join("publish");
    assert!(
        publish
            .join(format!("PromptForge_{VERSION}_x64-setup.exe"))
            .is_file()
    );
    assert_eq!(std::fs::read_dir(&publish).expect("publish").count(), 1);
    assert!(!output(environment, target).join("payload").exists());
    assert!(!test_environment.sidecar(target).exists());
}

#[test]
fn windows_signed_publishes_the_setup_signature_without_a_gateway_archive() {
    let mut test_environment = environment();
    test_environment.environment.signing_key_set = true;
    let environment = &test_environment.environment;
    let target = "x86_64-pc-windows-msvc";
    let setup =
        bundle(environment, target, "nsis").join(format!("PromptForge_{VERSION}_x64-setup.exe"));
    let mut runner = FakeRunner::with_responses(vec![
        gateway_built(gateway(environment, target)),
        version_printed(),
        bundled(
            test_environment.sidecar(target),
            vec![collect_signature(&setup), setup],
            success(""),
        ),
    ]);

    build_installer(&request(target, true), environment, &mut runner).expect("installer");

    assert_eq!(
        runner.commands,
        expected_commands(environment, target, "nsis", true)
    );
    let publish = output(environment, target).join("publish");
    let setup = publish.join(format!("PromptForge_{VERSION}_x64-setup.exe"));
    assert!(setup.is_file());
    assert!(collect_signature(&setup).is_file());
}

#[test]
fn macos_signed_collects_the_payload_and_signs_the_gateway_archive() {
    let mut test_environment = environment();
    test_environment.environment.signing_key_set = true;
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
        gateway_built(gateway(environment, target)),
        version_printed(),
        bundled(
            test_environment.sidecar(target),
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
        std::fs::read(payload.join("promptforge-gateway")).expect("gateway payload"),
        b"gateway"
    );
    let publish = output_root.join("publish");
    let app_updater = publish.join(format!("PromptForge_{VERSION}_aarch64.app.tar.gz"));
    assert!(app_updater.is_file());
    assert!(collect_signature(&app_updater).is_file());
    assert!(collect_signature(&archive).is_file());
}

#[test]
fn linux_unsigned_from_the_host_collects_the_payload_only() {
    let test_environment = environment();
    let environment = &test_environment.environment;
    let target = "x86_64-unknown-linux-gnu";
    let appimage = bundle(environment, target, "appimage");
    let mut runner = FakeRunner::with_responses(vec![
        success(&format!("cargo 1.89.0\nhost: {target}\n")),
        gateway_built(gateway(environment, target)),
        version_printed(),
        bundled(
            test_environment.sidecar(target),
            vec![
                appimage.join(format!("PromptForge_{VERSION}_amd64.AppImage")),
                appimage.join("PromptForge.AppDir").join("AppRun"),
            ],
            success(""),
        ),
    ]);

    build_installer(
        &InstallerRequest {
            target: None,
            sign: false,
        },
        environment,
        &mut runner,
    )
    .expect("installer");

    let mut expected = vec![command(
        environment,
        "selected-cargo",
        &["-vV"],
        OutputMode::Capture,
    )];
    expected.extend(expected_commands(environment, target, "appimage", false));
    assert_eq!(runner.commands, expected);
    let output_root = output(environment, target);
    assert!(
        output_root
            .join("payload")
            .join("PromptForge.AppImage")
            .is_file()
    );
    assert!(
        output_root
            .join("payload")
            .join("promptforge-gateway")
            .is_file()
    );
    assert_eq!(
        std::fs::read_dir(output_root.join("publish"))
            .expect("publish")
            .count(),
        0
    );
}

#[test]
fn linux_signed_publishes_the_appimage_and_the_gateway_archive() {
    let mut test_environment = environment();
    test_environment.environment.signing_key_set = true;
    let environment = &test_environment.environment;
    let target = "aarch64-unknown-linux-gnu";
    let appimage = bundle(environment, target, "appimage")
        .join(format!("PromptForge_{VERSION}_aarch64.AppImage"));
    let archive = output(environment, target).join("publish").join(format!(
        "promptforge-gateway_{VERSION}_linux-aarch64.tar.gz"
    ));
    let signed_archive = archive.clone();
    let mut runner = FakeRunner::with_responses(vec![
        gateway_built(gateway(environment, target)),
        version_printed(),
        bundled(
            test_environment.sidecar(target),
            vec![collect_signature(&appimage), appimage],
            success(""),
        ),
        act(
            move || write_file(&collect_signature(&signed_archive), b"signature"),
            success(""),
        ),
    ]);

    build_installer(&request(target, true), environment, &mut runner).expect("installer");

    let publish = output(environment, target).join("publish");
    let published = publish.join(format!("PromptForge_{VERSION}_aarch64.AppImage"));
    assert!(published.is_file());
    assert!(collect_signature(&published).is_file());
    assert!(collect_signature(&archive).is_file());
    assert!(
        output(environment, target)
            .join("payload")
            .join("PromptForge.AppImage")
            .is_file()
    );
}

#[test]
fn a_signer_that_writes_no_signature_fails_the_build() {
    let mut test_environment = environment();
    test_environment.environment.signing_key_set = true;
    let environment = &test_environment.environment;
    let target = "x86_64-unknown-linux-gnu";
    let appimage = bundle(environment, target, "appimage")
        .join(format!("PromptForge_{VERSION}_amd64.AppImage"));
    let mut runner = FakeRunner::with_responses(vec![
        gateway_built(gateway(environment, target)),
        version_printed(),
        bundled(
            test_environment.sidecar(target),
            vec![collect_signature(&appimage), appimage],
            success(""),
        ),
        success(""),
    ]);

    let error = build_installer(&request(target, true), environment, &mut runner)
        .expect_err("missing signature");

    assert!(error.primary.contains("wrote no signature"), "{error}");
}

#[test]
fn preflight_failures_run_no_command() {
    let mut test_environment = environment();
    let target = "x86_64-unknown-linux-gnu";
    let mut runner = FakeRunner::default();

    let error = build_installer(
        &request(target, true),
        &test_environment.environment,
        &mut runner,
    )
    .expect_err("no signing key");
    assert!(
        error.primary.contains("TAURI_SIGNING_PRIVATE_KEY"),
        "{error}"
    );

    std::fs::remove_file(&test_environment.environment.tauri_cli).expect("remove CLI");
    test_environment.environment.signing_key_set = true;
    let error = build_installer(
        &request(target, true),
        &test_environment.environment,
        &mut runner,
    )
    .expect_err("no CLI");
    assert!(
        error.primary.contains("npm ci --prefix crates/workshop"),
        "{error}"
    );
    assert!(runner.commands.is_empty());
}

#[test]
fn an_unsupported_target_fails_before_building() {
    let test_environment = environment();
    let mut runner = FakeRunner::default();

    let error = build_installer(
        &request("x86_64-unknown-freebsd", false),
        &test_environment.environment,
        &mut runner,
    )
    .expect_err("unsupported target");

    assert!(error.primary.contains("x86_64-unknown-freebsd"), "{error}");
    assert!(runner.commands.is_empty());
}

#[test]
fn a_version_mismatch_stops_before_staging() {
    let test_environment = environment();
    let environment = &test_environment.environment;
    let target = "x86_64-unknown-linux-gnu";
    let mut runner = FakeRunner::with_responses(vec![
        gateway_built(gateway(environment, target)),
        success("promptforge-gateway 0.0.1\n"),
    ]);

    let error = build_installer(&request(target, false), environment, &mut runner)
        .expect_err("version mismatch");

    assert!(
        error
            .primary
            .contains("printed `promptforge-gateway 0.0.1`"),
        "{error}"
    );
    assert!(
        error
            .primary
            .contains(&format!("expected `promptforge-gateway {VERSION}`")),
        "{error}"
    );
    assert_eq!(runner.commands.len(), 2);
    assert!(!test_environment.sidecar(target).exists());
}

#[test]
fn a_failed_unstartable_or_interrupted_bundle_removes_the_sidecar_and_collects_nothing() {
    for (response, expected) in [
        (
            failure("bundle broke"),
            "Workshop bundle failed: bundle broke",
        ),
        (
            FakeResponse::SpawnFailed(io::ErrorKind::NotFound, "node not found"),
            "Workshop bundle could not start: node not found",
        ),
        (
            FakeResponse::InterruptedAfterStart,
            "Workshop bundle interrupted",
        ),
    ] {
        let test_environment = environment();
        let environment = &test_environment.environment;
        let target = "x86_64-pc-windows-msvc";
        let mut runner = FakeRunner::with_responses(vec![
            gateway_built(gateway(environment, target)),
            version_printed(),
            bundled(test_environment.sidecar(target), Vec::new(), response),
        ]);

        let error = build_installer(&request(target, false), environment, &mut runner)
            .expect_err("bundle failure");

        assert!(error.primary.contains(expected), "{error}");
        assert!(!test_environment.sidecar(target).exists());
        assert!(!output(environment, target).exists());
    }
}

fn collect_signature(path: &Path) -> PathBuf {
    crate::installer::collect::signature_path(path)
}
