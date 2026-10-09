//! Builds the PromptForge Gateway, then Workshop or each platform's
//! installer. For Windows targets it stages the Gateway as Tauri's sidecar
//! around the Workshop build and removes it after; macOS and Linux payloads
//! carry the Gateway beside Workshop.
//! The `sidecar` mode stages or removes it on its own, for CI jobs that build
//! the Gateway themselves.

use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::process::{Child, ExitCode};
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};

#[path = "main/args.rs"]
mod args;
#[path = "main/installer.rs"]
mod installer;
#[path = "main/pipeline.rs"]
mod pipeline;
#[path = "main/runner.rs"]
mod runner;
#[path = "main/sidecar.rs"]
mod sidecar;

use args::{Request, SidecarRequest, parse_arguments};
use installer::build_installer;
use pipeline::build_workshop;
use runner::install_interrupt_handler;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Profile {
    Debug,
    Release,
}

impl Profile {
    fn directory(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Release => "release",
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct BuildRequest {
    profile: Profile,
    target: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OutputMode {
    Capture,
    Inherit,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CommandSpec {
    program: PathBuf,
    args: Vec<OsString>,
    current_dir: PathBuf,
    envs: Vec<(OsString, OsString)>,
    output_mode: OutputMode,
}

#[derive(Debug, Eq, PartialEq)]
struct CommandResult {
    success: bool,
    stdout: String,
    stderr: String,
}

trait CommandRunner {
    /// Refuses to start once an interrupt has been requested.
    fn run(&mut self, command: &CommandSpec) -> io::Result<CommandResult>;

    fn interruption_observed(&self) -> bool;
}

#[derive(Debug)]
struct ProcessRunner {
    interrupt: InterruptController,
}

#[derive(Clone, Debug)]
struct InterruptController {
    state: Arc<InterruptState>,
}

#[derive(Debug)]
struct InterruptState {
    generation: AtomicUsize,
    active_child: Mutex<Option<Child>>,
    termination_error: Mutex<Option<String>>,
}

#[derive(Debug)]
struct BuildEnvironment {
    workspace_root: PathBuf,
    target_root: PathBuf,
    /// Whether `CARGO_TARGET_DIR` chose `target_root`; otherwise Cargo's
    /// own configuration does, and children are left to it.
    target_root_from_env: bool,
    cargo: PathBuf,
    node: PathBuf,
    tauri_cli: PathBuf,
    signing_key: Option<OsString>,
    signing_password_set: bool,
    /// The first key path variable set, which `tauri signer sign` reads
    /// beside TAURI_SIGNING_PRIVATE_KEY and `tauri build` ignores.
    signing_key_path_variable: Option<&'static str>,
}

impl BuildEnvironment {
    fn discover() -> Result<Self, anyhow::Error> {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = manifest_dir
            .parent()
            .and_then(|crates| crates.parent())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "cannot derive the workspace root from {}",
                    manifest_dir.display()
                )
            })?
            .to_path_buf();
        let configured_target = std::env::var_os("CARGO_TARGET_DIR");
        let target_root_from_env = configured_target.is_some();
        let target_root = match configured_target {
            Some(value) if value.is_empty() => {
                return Err(anyhow::anyhow!("CARGO_TARGET_DIR must not be empty"));
            }
            Some(value) => {
                let path = PathBuf::from(value);
                if path.is_absolute() {
                    path
                } else {
                    std::env::current_dir()
                        .map_err(|error| {
                            anyhow::anyhow!("cannot read the current directory: {error}")
                        })?
                        .join(path)
                }
            }
            None => workspace_root.join("target"),
        };
        let cargo = std::env::var_os("CARGO")
            .or_else(|| option_env!("CARGO").map(OsString::from))
            .map(PathBuf::from)
            .ok_or_else(|| {
                anyhow::anyhow!("Cargo did not provide the executable used for this command")
            })?;
        let tauri_cli = workspace_root
            .join("crates")
            .join("workshop")
            .join("node_modules")
            .join("@tauri-apps")
            .join("cli")
            .join("tauri.js");
        let signing_key =
            std::env::var_os("TAURI_SIGNING_PRIVATE_KEY").filter(|key| !key.is_empty());
        let signing_password_set = std::env::var_os("TAURI_SIGNING_PRIVATE_KEY_PASSWORD").is_some();
        let signing_key_path_variable =
            ["TAURI_SIGNING_PRIVATE_KEY_PATH", "TAURI_PRIVATE_KEY_PATH"]
                .into_iter()
                .find(|name| std::env::var_os(name).is_some());
        Ok(Self {
            workspace_root,
            target_root,
            target_root_from_env,
            cargo,
            node: PathBuf::from("node"),
            tauri_cli,
            signing_key,
            signing_password_set,
            signing_key_path_variable,
        })
    }

    /// Cargo resolves a relative `CARGO_TARGET_DIR` against its own working
    /// directory, which differs between children (the Tauri CLI runs from
    /// `crates/workshop/desktop`), so every child that runs Cargo gets the
    /// absolute root this process reads its outputs from.
    fn cargo_envs(&self) -> Vec<(OsString, OsString)> {
        if !self.target_root_from_env {
            return Vec::new();
        }
        vec![(
            OsString::from("CARGO_TARGET_DIR"),
            self.target_root.clone().into_os_string(),
        )]
    }
}

#[derive(Debug, Eq, PartialEq)]
struct BuildError {
    primary: String,
    cleanup: Option<String>,
}

#[derive(Debug, Eq, PartialEq)]
struct StepError {
    message: String,
    interrupted: bool,
}

impl StepError {
    fn failed(message: String) -> Self {
        Self {
            message,
            interrupted: false,
        }
    }
}

impl From<StepError> for BuildError {
    fn from(error: StepError) -> Self {
        Self {
            primary: error.message,
            cleanup: None,
        }
    }
}

impl fmt::Display for BuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.primary)?;
        if let Some(cleanup) = &self.cleanup {
            write!(formatter, "\ncleanup also failed: {cleanup}")?;
        }
        Ok(())
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args
        .iter()
        .any(|argument| argument == "-h" || argument == "--help")
    {
        print!("{}", args::USAGE);
        return ExitCode::SUCCESS;
    }
    let request = match parse_arguments(&args) {
        Ok(request) => request,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let environment = match BuildEnvironment::discover() {
        Ok(environment) => environment,
        Err(error) => {
            eprintln!("build-workshop failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    let result = match &request {
        Request::Sidecar(request) => run_sidecar(request, &environment),
        Request::Build(request) => {
            with_process_runner(|runner| build_workshop(request, &environment, runner))
        }
        Request::Installer(request) => {
            with_process_runner(|runner| build_installer(request, &environment, runner))
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("build-workshop failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn with_process_runner(
    build: impl FnOnce(&mut ProcessRunner) -> Result<(), BuildError>,
) -> Result<(), String> {
    let interrupt = install_interrupt_handler().map_err(|error| error.to_string())?;
    build(&mut ProcessRunner::new(interrupt)).map_err(|error| error.to_string())
}

fn run_sidecar(request: &SidecarRequest, environment: &BuildEnvironment) -> Result<(), String> {
    let report = match request {
        SidecarRequest::Stage { target, source } => {
            format!(
                "staged {}",
                sidecar::stage(&environment.workspace_root, target, source)?.display()
            )
        }
        SidecarRequest::Remove { target } => format!(
            "removed {}",
            sidecar::remove(&environment.workspace_root, target)?.display()
        ),
    };
    println!("{report}");
    Ok(())
}

#[cfg(test)]
#[path = "main/tests.rs"]
mod tests;
