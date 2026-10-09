//! Unit tests for argument parsing and the build command sequence.

use std::collections::VecDeque;
use std::path::Path;

use tempfile::TempDir;

use super::*;
use args::InstallerRequest;

#[path = "tests/collect.rs"]
mod collect_tests;
#[path = "tests/failures.rs"]
mod failures;
#[path = "tests/installer.rs"]
mod installer_tests;
#[path = "tests/sidecar.rs"]
mod sidecar_tests;

enum FakeResponse {
    Completed(CommandResult),
    CompletedAndInterrupted,
    InterruptedBeforeStart,
    InterruptedAfterStart,
    SpawnFailed(io::ErrorKind, &'static str),
    /// Runs the effect, standing in for what the real child does to the
    /// filesystem, then answers with the inner response.
    Act(Box<dyn FnOnce()>, Box<FakeResponse>),
}

impl fmt::Debug for FakeResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Completed(result) => write!(formatter, "Completed({result:?})"),
            Self::CompletedAndInterrupted => formatter.write_str("CompletedAndInterrupted"),
            Self::InterruptedBeforeStart => formatter.write_str("InterruptedBeforeStart"),
            Self::InterruptedAfterStart => formatter.write_str("InterruptedAfterStart"),
            Self::SpawnFailed(kind, message) => {
                write!(formatter, "SpawnFailed({kind:?}, {message})")
            }
            Self::Act(_, response) => write!(formatter, "Act({response:?})"),
        }
    }
}

#[derive(Debug, Default)]
struct FakeRunner {
    responses: VecDeque<FakeResponse>,
    commands: Vec<CommandSpec>,
    interruption_observed: bool,
}

impl FakeRunner {
    fn with_responses(responses: Vec<FakeResponse>) -> Self {
        Self {
            responses: responses.into(),
            commands: Vec::new(),
            interruption_observed: false,
        }
    }

    fn answer(&mut self, response: FakeResponse) -> io::Result<CommandResult> {
        match response {
            FakeResponse::Completed(result) => Ok(result),
            FakeResponse::CompletedAndInterrupted => {
                self.interruption_observed = true;
                Ok(CommandResult {
                    success: true,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
            FakeResponse::InterruptedBeforeStart | FakeResponse::InterruptedAfterStart => {
                self.interruption_observed = true;
                Err(io::Error::new(io::ErrorKind::Interrupted, "interrupted"))
            }
            FakeResponse::SpawnFailed(kind, message) => Err(io::Error::new(kind, message)),
            FakeResponse::Act(effect, response) => {
                effect();
                self.answer(*response)
            }
        }
    }
}

impl CommandRunner for FakeRunner {
    fn run(&mut self, command: &CommandSpec) -> io::Result<CommandResult> {
        self.commands.push(command.clone());
        if self.interruption_observed {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "interrupted before child start",
            ));
        }
        let response = self.responses.pop_front().expect("unexpected command");
        self.answer(response)
    }

    fn interruption_observed(&self) -> bool {
        self.interruption_observed
    }
}

fn success(stdout: &str) -> FakeResponse {
    FakeResponse::Completed(CommandResult {
        success: true,
        stdout: stdout.to_owned(),
        stderr: String::new(),
    })
}

fn failure(stderr: &str) -> FakeResponse {
    FakeResponse::Completed(CommandResult {
        success: false,
        stdout: String::new(),
        stderr: stderr.to_owned(),
    })
}

fn act(effect: impl FnOnce() + 'static, response: FakeResponse) -> FakeResponse {
    FakeResponse::Act(Box::new(effect), Box::new(response))
}

/// A Cargo build that writes the Gateway binary where the pipeline looks.
fn gateway_built(path: PathBuf) -> FakeResponse {
    act(move || write_file(&path, b"gateway"), success(""))
}

/// A Workshop build that asserts the sidecar is staged while it runs.
fn workshop_built_with_sidecar(sidecar: PathBuf, response: FakeResponse) -> FakeResponse {
    act(
        move || assert!(sidecar.is_file(), "sidecar not staged during the build"),
        response,
    )
}

fn write_file(path: &Path, contents: &[u8]) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("parent directory");
    std::fs::write(path, contents).expect("write file");
}

struct TestEnvironment {
    _temp: TempDir,
    environment: BuildEnvironment,
}

impl TestEnvironment {
    fn sidecar(&self, target: &str) -> PathBuf {
        sidecar::sidecar_path(&self.environment.workspace_root, target)
    }
}

fn environment() -> TestEnvironment {
    let temp = tempfile::tempdir().expect("temporary workspace");
    let workspace_root = temp.path().join("repo");
    let target_root = temp.path().join("cargo-target");
    let tauri_cli = temp.path().join("tauri-cli").join("tauri.js");
    std::fs::create_dir_all(&workspace_root).expect("workspace root");
    write_file(&tauri_cli, b"");
    TestEnvironment {
        _temp: temp,
        environment: BuildEnvironment {
            workspace_root,
            target_root,
            target_root_from_env: true,
            cargo: PathBuf::from("selected-cargo"),
            node: PathBuf::from("selected-node"),
            tauri_cli,
            signing_key: None,
            signing_password_set: false,
            signing_key_path_variable: None,
        },
    }
}

fn strings(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

fn command(
    environment: &BuildEnvironment,
    program: &str,
    args: &[&str],
    output_mode: OutputMode,
) -> CommandSpec {
    CommandSpec {
        program: PathBuf::from(program),
        args: strings(args),
        current_dir: environment.workspace_root.clone(),
        envs: Vec::new(),
        output_mode,
    }
}

/// The environment a child that runs Cargo gets: the test target root
/// stands in for an absolute `CARGO_TARGET_DIR`.
fn target_root_env(environment: &BuildEnvironment) -> Vec<(OsString, OsString)> {
    vec![(
        OsString::from("CARGO_TARGET_DIR"),
        environment.target_root.clone().into_os_string(),
    )]
}

/// A Cargo build, which runs with the absolute target root.
fn cargo_build(environment: &BuildEnvironment, args: &[&str]) -> CommandSpec {
    CommandSpec {
        envs: target_root_env(environment),
        ..command(environment, "selected-cargo", args, OutputMode::Inherit)
    }
}

/// Sets a key and its password, both of which the release secrets set.
fn set_signing_key(environment: &mut BuildEnvironment) {
    environment.signing_key = Some(OsString::from("dW50cnVzdGVkIGNvbW1lbnQ6"));
    environment.signing_password_set = true;
}

fn arguments(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn parses_only_the_documented_profile_and_target_options() {
    assert_eq!(
        parse_arguments(&[]).expect("debug request"),
        Request::Build(BuildRequest {
            profile: Profile::Debug,
            target: None,
        })
    );
    assert_eq!(
        parse_arguments(&arguments(&[
            "--target",
            "aarch64-apple-darwin",
            "--release"
        ]))
        .expect("release target request"),
        Request::Build(BuildRequest {
            profile: Profile::Release,
            target: Some("aarch64-apple-darwin".to_owned()),
        })
    );
}

#[test]
fn parses_the_installer_and_sidecar_modes() {
    assert_eq!(
        parse_arguments(&arguments(&["installer"])).expect("installer"),
        Request::Installer(InstallerRequest {
            target: None,
            sign: false,
        })
    );
    assert_eq!(
        parse_arguments(&arguments(&[
            "installer",
            "--sign",
            "--target",
            "x86_64-pc-windows-msvc",
        ]))
        .expect("signed installer"),
        Request::Installer(InstallerRequest {
            target: Some("x86_64-pc-windows-msvc".to_owned()),
            sign: true,
        })
    );
    assert_eq!(
        parse_arguments(&arguments(&[
            "sidecar",
            "stage",
            "--source",
            "target/debug/promptforge-gateway",
            "--target",
            "x86_64-unknown-linux-gnu",
        ]))
        .expect("stage"),
        Request::Sidecar(SidecarRequest::Stage {
            target: "x86_64-unknown-linux-gnu".to_owned(),
            source: PathBuf::from("target/debug/promptforge-gateway"),
        })
    );
    assert_eq!(
        parse_arguments(&arguments(&[
            "sidecar",
            "remove",
            "--target",
            "x86_64-pc-windows-msvc",
        ]))
        .expect("remove"),
        Request::Sidecar(SidecarRequest::Remove {
            target: "x86_64-pc-windows-msvc".to_owned(),
        })
    );
}

#[test]
fn rejects_product_features_and_other_unsupported_arguments() {
    for args in [
        arguments(&["--features", "local"]),
        arguments(&["--profile", "dist"]),
        arguments(&["gateway"]),
        arguments(&["--sign"]),
        arguments(&["installer", "--release"]),
        arguments(&["installer", "--source", "gateway"]),
        arguments(&[
            "sidecar",
            "remove",
            "--target",
            "x86_64-pc-windows-msvc",
            "--source",
            "x",
        ]),
        arguments(&["sidecar", "stage", "--release"]),
    ] {
        let error = parse_arguments(&args)
            .expect_err("unsupported argument")
            .to_string();
        assert!(error.contains("unsupported argument"), "{args:?}: {error}");
        assert!(error.contains(args::USAGE), "{error}");
    }
}

#[test]
fn rejects_duplicate_incomplete_or_malformed_options() {
    for args in [
        arguments(&["--release", "--release"]),
        arguments(&["--target"]),
        arguments(&[
            "--target",
            "x86_64-pc-windows-msvc",
            "--target",
            "x86_64-unknown-linux-gnu",
        ]),
        arguments(&["--target", "../outside"]),
        arguments(&["installer", "--sign", "--sign"]),
        arguments(&["installer", "--target", "--sign"]),
        arguments(&["sidecar"]),
        arguments(&["sidecar", "copy"]),
        arguments(&["sidecar", "stage", "--target", "x86_64-unknown-linux-gnu"]),
        arguments(&["sidecar", "stage", "--source", "gateway"]),
        arguments(&["sidecar", "remove"]),
    ] {
        assert!(parse_arguments(&args).is_err(), "{args:?}");
    }
}

#[test]
fn default_build_derives_host_and_stages_around_the_workshop_build() {
    let test_environment = environment();
    let environment = &test_environment.environment;
    let triple = "x86_64-pc-windows-msvc";
    let source = environment
        .target_root
        .join("debug")
        .join("promptforge-gateway.exe");
    let mut runner = FakeRunner::with_responses(vec![
        success("cargo 1.89.0\nhost: x86_64-pc-windows-msvc\n"),
        gateway_built(source),
        workshop_built_with_sidecar(test_environment.sidecar(triple), success("")),
    ]);

    build_workshop(
        &BuildRequest {
            profile: Profile::Debug,
            target: None,
        },
        environment,
        &mut runner,
    )
    .expect("Workshop build");

    assert_eq!(
        runner.commands,
        vec![
            command(environment, "selected-cargo", &["-vV"], OutputMode::Capture),
            cargo_build(environment, &["build", "-p", "gateway"]),
            cargo_build(environment, &["build", "-p", "workshop"]),
        ]
    );
    assert!(!test_environment.sidecar(triple).exists(), "sidecar left");
}

#[test]
fn explicit_release_target_uses_target_output_without_a_host_probe() {
    let test_environment = environment();
    let environment = &test_environment.environment;
    let triple = "aarch64-unknown-linux-gnu";
    let source = environment
        .target_root
        .join(triple)
        .join("release")
        .join("promptforge-gateway");
    let mut runner = FakeRunner::with_responses(vec![
        gateway_built(source),
        workshop_built_with_sidecar(test_environment.sidecar(triple), success("")),
    ]);

    build_workshop(
        &BuildRequest {
            profile: Profile::Release,
            target: Some(triple.to_owned()),
        },
        environment,
        &mut runner,
    )
    .expect("Workshop build");

    assert_eq!(
        runner.commands,
        vec![
            cargo_build(
                environment,
                &["build", "-p", "gateway", "--release", "--target", triple]
            ),
            cargo_build(
                environment,
                &["build", "-p", "workshop", "--release", "--target", triple]
            ),
        ]
    );
    assert!(!test_environment.sidecar(triple).exists(), "sidecar left");
}

#[test]
fn a_target_root_from_cargo_configuration_is_left_to_cargo() {
    let mut test_environment = environment();
    test_environment.environment.target_root_from_env = false;
    let environment = &test_environment.environment;
    let triple = "aarch64-unknown-linux-gnu";
    let source = environment
        .target_root
        .join(triple)
        .join("release")
        .join("promptforge-gateway");
    let mut runner = FakeRunner::with_responses(vec![gateway_built(source), success("")]);

    build_workshop(
        &BuildRequest {
            profile: Profile::Release,
            target: Some(triple.to_owned()),
        },
        environment,
        &mut runner,
    )
    .expect("Workshop build");

    assert_eq!(runner.commands.len(), 2);
    assert!(
        runner
            .commands
            .iter()
            .all(|command| command.envs.is_empty())
    );
}
