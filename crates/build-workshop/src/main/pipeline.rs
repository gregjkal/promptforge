//! The build pipeline: host discovery, product builds, sidecar staging, and cleanup.

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;

use super::args::valid_target_triple;
use super::sidecar;
use super::{
    BuildEnvironment, BuildError, BuildRequest, CommandRunner, CommandSpec, OutputMode, Profile,
    StepError,
};

pub(super) fn build_workshop(
    request: &BuildRequest,
    environment: &BuildEnvironment,
    runner: &mut impl CommandRunner,
) -> Result<(), BuildError> {
    let target = resolve_target(request.target.as_deref(), environment, runner)?;
    with_staged_sidecar(
        environment,
        &target,
        runner,
        |runner| {
            run_checked(
                runner,
                &cargo_build_command(environment, request, "gateway"),
                "Gateway build",
            )?;
            let mut source = environment.target_root.clone();
            if request.target.is_some() {
                source.push(&target);
            }
            source.push(request.profile.directory());
            source.push(sidecar::gateway_binary_name(&target));
            Ok(source)
        },
        |runner| {
            run_checked(
                runner,
                &cargo_build_command(environment, request, "workshop"),
                "Workshop build",
            )
            .map(|_| ())
        },
    )
}

pub(super) fn resolve_target(
    target: Option<&str>,
    environment: &BuildEnvironment,
    runner: &mut impl CommandRunner,
) -> Result<String, BuildError> {
    match target {
        Some(target) => Ok(target.to_owned()),
        None => discover_host_target(environment, runner).map_err(BuildError::from),
    }
}

/// Runs `build_gateway`, stages the Gateway binary it returns as the Tauri
/// sidecar, runs `build_workshop`, and removes the sidecar whatever happened,
/// unless an interrupt arrived before anything was staged.
pub(super) fn with_staged_sidecar<R: CommandRunner>(
    environment: &BuildEnvironment,
    target: &str,
    runner: &mut R,
    build_gateway: impl FnOnce(&mut R) -> Result<PathBuf, StepError>,
    build_workshop: impl FnOnce(&mut R) -> Result<(), StepError>,
) -> Result<(), BuildError> {
    let mut staged = false;
    let primary = build_around_sidecar(
        environment,
        target,
        runner,
        &mut staged,
        build_gateway,
        build_workshop,
    );
    let mut primary = match primary {
        Err(error) if error.interrupted && !staged => return Err(error.into()),
        result => result,
    };
    if primary.is_ok() && runner.interruption_observed() {
        primary = Err(interrupted_after_completion());
    }
    let cleanup = sidecar::remove(&environment.workspace_root, target)
        .map(|_| ())
        .map_err(|error| StepError::failed(format!("Gateway sidecar cleanup failed: {error}")));
    if primary.is_ok() && cleanup.is_ok() && runner.interruption_observed() {
        primary = Err(interrupted_after_completion());
    }

    match (primary, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error.into()),
        (Err(primary), Err(cleanup)) => Err(BuildError {
            primary: primary.message,
            cleanup: Some(cleanup.message),
        }),
    }
}

fn build_around_sidecar<R: CommandRunner>(
    environment: &BuildEnvironment,
    target: &str,
    runner: &mut R,
    staged: &mut bool,
    build_gateway: impl FnOnce(&mut R) -> Result<PathBuf, StepError>,
    build_workshop: impl FnOnce(&mut R) -> Result<(), StepError>,
) -> Result<(), StepError> {
    let source = build_gateway(runner)?;
    if runner.interruption_observed() {
        return Err(StepError {
            message: "Gateway sidecar staging interrupted".to_owned(),
            interrupted: true,
        });
    }
    // A copy that fails partway may leave a file behind, so cleanup counts
    // from the attempt.
    *staged = true;
    sidecar::stage(&environment.workspace_root, target, &source)
        .map_err(|error| StepError::failed(format!("Gateway sidecar staging failed: {error}")))?;
    build_workshop(runner)
}

fn interrupted_after_completion() -> StepError {
    StepError {
        message: "Workshop build interrupted after child completion".to_owned(),
        interrupted: true,
    }
}

fn discover_host_target(
    environment: &BuildEnvironment,
    runner: &mut impl CommandRunner,
) -> Result<String, StepError> {
    let output = run_checked(
        runner,
        &CommandSpec {
            program: environment.cargo.clone(),
            args: vec![OsString::from("-vV")],
            current_dir: environment.workspace_root.clone(),
            output_mode: OutputMode::Capture,
        },
        "Cargo host discovery",
    )?;
    output
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .filter(|target| valid_target_triple(target))
        .map(str::to_owned)
        .ok_or_else(|| {
            StepError::failed(
                "Cargo host triple was absent or malformed in `cargo -vV` output".to_owned(),
            )
        })
}

fn cargo_build_command(
    environment: &BuildEnvironment,
    request: &BuildRequest,
    package: &str,
) -> CommandSpec {
    let mut args = vec![
        OsString::from("build"),
        OsString::from("-p"),
        OsString::from(package),
    ];
    if request.profile == Profile::Release {
        args.push(OsString::from("--release"));
    }
    if let Some(target) = &request.target {
        args.push(OsString::from("--target"));
        args.push(OsString::from(target));
    }
    CommandSpec {
        program: environment.cargo.clone(),
        args,
        current_dir: environment.workspace_root.clone(),
        output_mode: OutputMode::Inherit,
    }
}

pub(super) fn run_checked(
    runner: &mut impl CommandRunner,
    command: &CommandSpec,
    label: &str,
) -> Result<String, StepError> {
    let result = runner.run(command).map_err(|error| {
        if error.kind() == io::ErrorKind::Interrupted {
            StepError {
                message: format!("{label} interrupted: {error}"),
                interrupted: true,
            }
        } else {
            StepError::failed(format!("{label} could not start: {error}"))
        }
    })?;
    if result.success {
        return Ok(result.stdout);
    }
    let detail = if result.stderr.trim().is_empty() {
        String::new()
    } else {
        format!(": {}", result.stderr.trim())
    };
    Err(StepError::failed(format!("{label} failed{detail}")))
}
