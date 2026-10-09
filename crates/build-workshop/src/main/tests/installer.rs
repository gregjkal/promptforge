//! Unit tests for the installer command sequence, its preflight, and its
//! failure paths.

use super::*;

pub(super) const VERSION: &str = env!("CARGO_PKG_VERSION");

fn release(environment: &BuildEnvironment, target: &str) -> PathBuf {
    environment.target_root.join(target).join("release")
}

pub(super) fn gateway(environment: &BuildEnvironment, target: &str) -> PathBuf {
    release(environment, target).join(sidecar::gateway_binary_name(target))
}

pub(super) fn bundle(environment: &BuildEnvironment, target: &str, directory: &str) -> PathBuf {
    release(environment, target).join("bundle").join(directory)
}

pub(super) fn output(environment: &BuildEnvironment, target: &str) -> PathBuf {
    environment.target_root.join("installer").join(target)
}

pub(super) fn node_found() -> FakeResponse {
    success("v22.0.0\n")
}

pub(super) fn version_printed() -> FakeResponse {
    success(&format!("promptforge-gateway {VERSION}\n"))
}

/// A Tauri build that writes `files`, with the sidecar staged exactly when
/// `target`'s Workshop bundle carries the Gateway.
pub(super) fn bundled(
    sidecar: PathBuf,
    target: &str,
    files: Vec<PathBuf>,
    response: FakeResponse,
) -> FakeResponse {
    let staged = sidecar::bundles_sidecar(target);
    act(
        move || {
            assert_eq!(
                sidecar.is_file(),
                staged,
                "sidecar staging during the bundle"
            );
            for file in files {
                write_file(&file, b"bundle");
            }
        },
        response,
    )
}

pub(super) fn expected_commands(
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
            "selected-node",
            &["--version"],
            OutputMode::Capture,
        ),
        cargo_build(
            environment,
            &[
                "build",
                "--locked",
                "--release",
                "-p",
                "gateway",
                "--target",
                target,
            ],
        ),
        CommandSpec {
            program: gateway(environment, target),
            args: strings(&["--version"]),
            current_dir: environment.workspace_root.clone(),
            envs: Vec::new(),
            output_mode: OutputMode::Capture,
        },
        CommandSpec {
            current_dir: environment
                .workspace_root
                .join("crates")
                .join("workshop")
                .join("desktop"),
            envs: target_root_env(environment),
            ..command(environment, "selected-node", &tauri, OutputMode::Inherit)
        },
    ]
}

pub(super) fn signer_command(environment: &BuildEnvironment, archive: &Path) -> CommandSpec {
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

pub(super) fn request(target: &str, sign: bool) -> InstallerRequest {
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
        node_found(),
        gateway_built(gateway(environment, target)),
        version_printed(),
        bundled(
            test_environment.sidecar(target),
            target,
            vec![setup],
            success(""),
        ),
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
    set_signing_key(&mut test_environment.environment);
    // `tauri build`, the only signer on Windows, reads a key path's file
    // and ignores the key path variables.
    let key_file = test_environment.environment.workspace_root.join("key");
    write_file(&key_file, b"dW50cnVzdGVkIGNvbW1lbnQ6");
    test_environment.environment.signing_key = Some(key_file.into_os_string());
    test_environment.environment.signing_key_path_variable = Some("TAURI_SIGNING_PRIVATE_KEY_PATH");
    let environment = &test_environment.environment;
    let target = "x86_64-pc-windows-msvc";
    let setup =
        bundle(environment, target, "nsis").join(format!("PromptForge_{VERSION}_x64-setup.exe"));
    let mut runner = FakeRunner::with_responses(vec![
        node_found(),
        gateway_built(gateway(environment, target)),
        version_printed(),
        bundled(
            test_environment.sidecar(target),
            target,
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
fn linux_unsigned_from_the_host_collects_the_payload_only() {
    let test_environment = environment();
    let environment = &test_environment.environment;
    let target = "x86_64-unknown-linux-gnu";
    let appimage = bundle(environment, target, "appimage");
    let mut runner = FakeRunner::with_responses(vec![
        success(&format!("cargo 1.89.0\nhost: {target}\n")),
        node_found(),
        gateway_built(gateway(environment, target)),
        version_printed(),
        bundled(
            test_environment.sidecar(target),
            target,
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
    set_signing_key(&mut test_environment.environment);
    test_environment.environment.signing_password_set = false;
    let environment = &test_environment.environment;
    let target = "aarch64-unknown-linux-gnu";
    let appimage = bundle(environment, target, "appimage")
        .join(format!("PromptForge_{VERSION}_aarch64.AppImage"));
    let archive = output(environment, target).join("publish").join(format!(
        "promptforge-gateway_{VERSION}_linux-aarch64.tar.gz"
    ));
    let signed_archive = archive.clone();
    let mut runner = FakeRunner::with_responses(vec![
        node_found(),
        gateway_built(gateway(environment, target)),
        version_printed(),
        bundled(
            test_environment.sidecar(target),
            target,
            vec![collect_signature(&appimage), appimage],
            success(""),
        ),
        act(
            move || write_file(&collect_signature(&signed_archive), b"signature"),
            success(""),
        ),
    ]);

    build_installer(&request(target, true), environment, &mut runner).expect("installer");

    assert_eq!(
        runner.commands.last().expect("signer").envs,
        vec![(
            OsString::from("TAURI_SIGNING_PRIVATE_KEY_PASSWORD"),
            OsString::new(),
        )],
        "an unset password signs as empty, as `tauri build --ci` does"
    );
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
fn preflight_failures_run_no_command() {
    let mut test_environment = environment();
    let target = "x86_64-unknown-linux-gnu";
    let key_file = test_environment.environment.workspace_root.join("key");
    let mut runner = FakeRunner::default();
    let mut refusal = |environment: &BuildEnvironment, case: &str| {
        build_installer(&request(target, true), environment, &mut runner)
            .expect_err(case)
            .primary
    };

    let error = refusal(&test_environment.environment, "no signing key");
    assert!(error.contains("TAURI_SIGNING_PRIVATE_KEY set"), "{error}");

    // Relative paths too: `tauri build` resolves them from its own working
    // directory, so whether they exist from here says nothing.
    set_signing_key(&mut test_environment.environment);
    for key in [
        key_file.into_os_string(),
        OsString::from("../../../updater.key"),
    ] {
        test_environment.environment.signing_key = Some(key);
        let error = refusal(&test_environment.environment, "a key path");
        assert!(error.contains("such as a path"), "{error}");
    }

    set_signing_key(&mut test_environment.environment);
    test_environment.environment.signing_key_path_variable = Some("TAURI_PRIVATE_KEY_PATH");
    let error = refusal(&test_environment.environment, "a key path variable");
    assert!(
        error.contains("needs TAURI_PRIVATE_KEY_PATH unset"),
        "{error}"
    );
    test_environment.environment.signing_key_path_variable = None;

    set_signing_key(&mut test_environment.environment);
    std::fs::remove_file(&test_environment.environment.tauri_cli).expect("remove CLI");
    let error = refusal(&test_environment.environment, "no CLI");
    assert!(error.contains("npm ci --prefix crates/workshop"), "{error}");
    assert!(runner.commands.is_empty());
}

pub(super) fn collect_signature(path: &Path) -> PathBuf {
    crate::installer::collect::signature_path(path)
}
