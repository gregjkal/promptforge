//! `cargo workshop installer`: builds the Gateway and Workshop for one target
//! in release, bundles Workshop with the pinned Tauri CLI, and collects each
//! platform's installer payload and, under `--sign`, its updater files into
//! `target/installer/<triple>/`.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::Path;

use super::args::InstallerRequest;
use super::pipeline::{resolve_target, run_checked, with_staged_sidecar};
use super::sidecar::gateway_binary_name;
use super::{BuildEnvironment, BuildError, CommandRunner, CommandSpec, OutputMode, StepError};

#[path = "installer-collect.rs"]
pub(super) mod collect;

const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Applied to unsigned builds: it turns off `createUpdaterArtifacts`, which
/// would otherwise demand the release key.
const UNSIGNED_CONFIG: &str = "tauri.nightly.conf.json";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum System {
    Windows,
    MacOs,
    Linux,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Arch {
    X86_64,
    Aarch64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Platform {
    pub(super) system: System,
    pub(super) arch: Arch,
}

impl Platform {
    pub(super) fn from_triple(target: &str) -> Result<Self, String> {
        let parts: Vec<&str> = target.split('-').collect();
        let arch = match parts.first() {
            Some(&"x86_64") => Some(Arch::X86_64),
            Some(&"aarch64") => Some(Arch::Aarch64),
            _ => None,
        };
        let system = if parts.contains(&"windows") && parts.contains(&"msvc") {
            Some(System::Windows)
        } else if parts.contains(&"apple") && parts.contains(&"darwin") {
            Some(System::MacOs)
        } else if parts.contains(&"linux") && parts.contains(&"gnu") {
            Some(System::Linux)
        } else {
            None
        };
        match (system, arch) {
            (Some(system), Some(arch)) => Ok(Self { system, arch }),
            _ => Err(format!(
                "installer target `{target}` is not supported; supported: x86_64 or aarch64 \
                 with pc-windows-msvc, apple-darwin, or unknown-linux-gnu"
            )),
        }
    }

    /// The Tauri bundle format, which is also its output directory's name
    /// except for `app`.
    fn bundle(self) -> &'static str {
        match self.system {
            System::Windows => "nsis",
            System::MacOs => "app",
            System::Linux => "appimage",
        }
    }

    fn bundle_directory(self) -> &'static str {
        match self.system {
            System::MacOs => "macos",
            System::Windows | System::Linux => self.bundle(),
        }
    }
}

pub(super) fn build_installer(
    request: &InstallerRequest,
    environment: &BuildEnvironment,
    runner: &mut impl CommandRunner,
) -> Result<(), BuildError> {
    preflight(request, environment)?;
    let target = resolve_target(request.target.as_deref(), environment, runner)?;
    let platform = Platform::from_triple(&target).map_err(failure)?;
    let release = environment.target_root.join(&target).join("release");
    // A bundle left by an earlier build, possibly at another version, would
    // make the collection ambiguous.
    remove_directory(&release.join("bundle").join(platform.bundle_directory()))?;
    let gateway = release.join(gateway_binary_name(&target));
    with_staged_sidecar(
        environment,
        &target,
        runner,
        |runner| {
            run_checked(
                runner,
                &cargo_command(environment, &target),
                "Gateway build",
            )?;
            check_gateway_version(runner, environment, &gateway)?;
            Ok(gateway.clone())
        },
        |runner| {
            run_checked(
                runner,
                &tauri_build_command(environment, &target, platform, request.sign),
                "Workshop bundle",
            )
            .map(|_| ())
        },
    )?;
    let output = environment.target_root.join("installer").join(&target);
    let collected = collect::collect(&release, platform, request.sign, &output).map_err(failure)?;
    if let Some(archive) = collected.gateway_archive {
        run_checked(
            runner,
            &sign_command(environment, &archive),
            "Gateway updater archive signing",
        )?;
        let signature = collect::signature_path(&archive);
        if !signature.is_file() {
            return Err(failure(format!(
                "Gateway updater archive signing wrote no signature at {}",
                signature.display()
            )));
        }
    }
    println!("installer files: {}", output.display());
    Ok(())
}

fn preflight(request: &InstallerRequest, environment: &BuildEnvironment) -> Result<(), BuildError> {
    if !environment.tauri_cli.is_file() {
        return Err(failure(format!(
            "the Tauri CLI is missing at {}; run `npm ci --prefix crates/workshop` first",
            environment.tauri_cli.display()
        )));
    }
    if request.sign {
        preflight_signing(environment)?;
    }
    Ok(())
}

/// `tauri signer sign` accepts less than `tauri build`: it reads the key
/// variable only as the key's contents, and prompts for a password when its
/// variable is unset, so both are checked before the release build.
fn preflight_signing(environment: &BuildEnvironment) -> Result<(), BuildError> {
    let Some(key) = &environment.signing_key else {
        return Err(failure(
            "`--sign` needs TAURI_SIGNING_PRIVATE_KEY set to the release minisign key's \
             contents; it is unset or empty"
                .to_owned(),
        ));
    };
    if Path::new(key).is_file() {
        return Err(failure(format!(
            "`--sign` needs TAURI_SIGNING_PRIVATE_KEY set to the key's contents, not a path; it \
             names the file {}",
            Path::new(key).display()
        )));
    }
    if !environment.signing_password_set {
        return Err(failure(
            "`--sign` needs TAURI_SIGNING_PRIVATE_KEY_PASSWORD set, empty for a key without a \
             password; it is unset"
                .to_owned(),
        ));
    }
    Ok(())
}

fn check_gateway_version(
    runner: &mut impl CommandRunner,
    environment: &BuildEnvironment,
    gateway: &Path,
) -> Result<(), StepError> {
    let printed = run_checked(
        runner,
        &CommandSpec {
            program: gateway.to_path_buf(),
            args: vec![OsString::from("--version")],
            current_dir: environment.workspace_root.clone(),
            envs: Vec::new(),
            output_mode: OutputMode::Capture,
        },
        "Gateway version check",
    )?;
    let expected = format!("promptforge-gateway {VERSION}");
    if printed.trim() == expected {
        Ok(())
    } else {
        Err(StepError::failed(format!(
            "Gateway version check failed: {} printed `{}`, expected `{expected}`",
            gateway.display(),
            printed.trim()
        )))
    }
}

fn cargo_command(environment: &BuildEnvironment, target: &str) -> CommandSpec {
    CommandSpec {
        program: environment.cargo.clone(),
        args: [
            "build",
            "--locked",
            "--release",
            "-p",
            "gateway",
            "--target",
            target,
        ]
        .map(OsString::from)
        .to_vec(),
        current_dir: environment.workspace_root.clone(),
        envs: environment.cargo_envs(),
        output_mode: OutputMode::Inherit,
    }
}

fn tauri_build_command(
    environment: &BuildEnvironment,
    target: &str,
    platform: Platform,
    sign: bool,
) -> CommandSpec {
    let mut args = vec![environment.tauri_cli.clone().into_os_string()];
    args.extend(
        [
            "build",
            "--ci",
            "--target",
            target,
            "--bundles",
            platform.bundle(),
        ]
        .map(OsString::from),
    );
    if !sign {
        args.extend(["--config", UNSIGNED_CONFIG].map(OsString::from));
    }
    CommandSpec {
        program: environment.node.clone(),
        args,
        current_dir: environment
            .workspace_root
            .join("crates")
            .join("workshop")
            .join("desktop"),
        envs: environment.cargo_envs(),
        output_mode: OutputMode::Inherit,
    }
}

/// The CLI reads the key and its password from the same variables `tauri
/// build` does, and binds the signature to the version as `tauri build`
/// does for Workshop's files.
fn sign_command(environment: &BuildEnvironment, archive: &Path) -> CommandSpec {
    let mut args = vec![environment.tauri_cli.clone().into_os_string()];
    args.extend(["signer", "sign", "--app-version", VERSION].map(OsString::from));
    args.push(archive.as_os_str().to_owned());
    CommandSpec {
        program: environment.node.clone(),
        args,
        current_dir: environment.workspace_root.clone(),
        envs: Vec::new(),
        output_mode: OutputMode::Inherit,
    }
}

fn remove_directory(path: &Path) -> Result<(), BuildError> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(failure(format!("cannot clear {}: {error}", path.display()))),
    }
}

fn failure(message: String) -> BuildError {
    BuildError {
        primary: message,
        cleanup: None,
    }
}
