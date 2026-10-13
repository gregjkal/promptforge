//! Boot planning and detached Gateway spawn.

use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::{Arc, OnceLock};

use anyhow::Context as _;
use gateway_api_discovery::{
    CancellationToken, GatewayDiscoveryFile, Resolution, SidecarError, ValidatedConnection,
};
use workshop_server_api::Config;

use super::identity::GatewayAttachment;
use super::supervisor::{
    RECOVERY_POLL_INTERVAL, RecoveryCandidate, RecoveryOwnership, launch_and_attach_cancellable,
};

#[cfg(windows)]
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
#[cfg(windows)]
const DETACHED_PROCESS: u32 = 0x0000_0008;
#[cfg(windows)]
const WINDOWS_DETACHED_CREATION_FLAGS: u32 = CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS;
#[cfg(windows)]
const WINDOWS_BREAKAWAY_CREATION_FLAGS: u32 =
    CREATE_BREAKAWAY_FROM_JOB | WINDOWS_DETACHED_CREATION_FLAGS;

/// What the boot decision concluded.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum GatewayPlan {
    /// A live gateway discovery file exists.
    Attach(GatewayDiscoveryFile),
    /// The running gateway is handing off to a successor: plan again once
    /// the handoff ends, instead of launching a rival.
    AwaitRestart,
    /// No live file exists and a sibling Gateway can be launched.
    Launch(PathBuf),
    /// Explicit configuration is the only available endpoint.
    ConfigOnly,
    /// No attachment path exists.
    Fail,
}

/// The result of a launch election, retaining the spawned pid only for the
/// branch that actually created a child.
#[derive(Debug)]
pub(super) enum RecoveryLaunch {
    /// A race winner had already published a live Gateway.
    Attached(GatewayDiscoveryFile),
    /// This process spawned a child and observed a gateway discovery file.
    Launched {
        child_pid: u32,
        file: GatewayDiscoveryFile,
    },
}

/// Connects the Gateway for boot.
///
/// # Errors
/// Returns an error when no attachment path exists or launch fails.
pub(crate) fn ensure_gateway(config: &Config) -> anyhow::Result<GatewayAttachment> {
    let install = WorkshopInstall::current()?;
    let explicit = !config.gateway.base_url.is_empty();
    let Some(run_dir) = gateway_api_discovery::default_run_dir() else {
        return if explicit {
            Ok(GatewayAttachment::Config)
        } else {
            Err(no_gateway_error(&install))
        };
    };
    attach_or_launch(
        &run_dir,
        &install,
        explicit,
        gateway_api_discovery::resolve,
        validated_attachment,
        |exe| {
            launch_and_attach_cancellable(&run_dir, &exe, &CancellationToken::new())
                .and_then(validated_recovery_attachment)
                .context("launch the sidecar gateway")
        },
        || std::thread::sleep(RECOVERY_POLL_INTERVAL),
    )
}

/// Plans until the plan settles, with resolution, the attachment proof, the
/// launch, and the pause between plans injected for tests.
///
/// A handoff pauses and plans again, with no timeout of its own: the mark
/// lasts only while the marking gateway lives, and that gateway bounds its
/// waits. A resolved file whose second proof fails also plans again: the
/// gateway changed between the two proofs, as when a handoff begins, and
/// the next plan sees what it became.
pub(super) fn attach_or_launch(
    run_dir: &Path,
    install: &WorkshopInstall,
    explicit: bool,
    mut resolve: impl FnMut(&Path) -> Result<Resolution, SidecarError>,
    mut attach: impl FnMut(GatewayDiscoveryFile) -> anyhow::Result<GatewayAttachment>,
    launch: impl FnOnce(PathBuf) -> anyhow::Result<GatewayAttachment>,
    mut pause: impl FnMut(),
) -> anyhow::Result<GatewayAttachment> {
    let mut announced = false;
    loop {
        match plan_gateway(run_dir, install, explicit, &mut resolve) {
            GatewayPlan::AwaitRestart => {
                if !std::mem::replace(&mut announced, true) {
                    eprintln!(
                        "the local gateway is handing off to a successor; waiting for it \
                         before attaching"
                    );
                }
                pause();
            }
            GatewayPlan::Attach(file) => match attach(file) {
                Ok(attachment) => return Ok(attachment),
                Err(error) => {
                    eprintln!("the resolved gateway changed before attaching: {error:#}");
                }
            },
            GatewayPlan::ConfigOnly => return Ok(GatewayAttachment::Config),
            GatewayPlan::Fail => return Err(no_gateway_error(install)),
            GatewayPlan::Launch(exe) => return launch(exe),
        }
    }
}

/// Retains the selected process proof instead of reducing it to file fields.
fn validated_attachment(file: GatewayDiscoveryFile) -> anyhow::Result<GatewayAttachment> {
    ValidatedConnection::validate(file)
        .map(GatewayAttachment::Sidecar)
        .context("validate the selected gateway process identity")
}

fn validated_recovery_attachment(recovery: RecoveryLaunch) -> anyhow::Result<GatewayAttachment> {
    match recovery {
        RecoveryLaunch::Attached(file) => validated_attachment(file),
        RecoveryLaunch::Launched { child_pid, file } => {
            let validated = ValidatedConnection::validate(file)
                .context("validate the launched gateway process identity")?;
            Ok(
                match RecoveryCandidate::authenticate(child_pid, validated) {
                    RecoveryOwnership::Owned(candidate) => GatewayAttachment::Launched(candidate),
                    RecoveryOwnership::Unowned(unowned) => GatewayAttachment::Sidecar(unowned),
                },
            )
        }
    }
}

/// Chooses attach, a wait through a handoff, launch, configured fallback,
/// or failure.
pub(super) fn plan_gateway(
    run_dir: &Path,
    install: &WorkshopInstall,
    explicit_config: bool,
    resolve: impl FnOnce(&Path) -> Result<Resolution, SidecarError>,
) -> GatewayPlan {
    match resolve(run_dir) {
        Ok(Resolution::Attach(file)) => return GatewayPlan::Attach(file),
        Ok(Resolution::Restarting) => return GatewayPlan::AwaitRestart,
        Ok(_) => {}
        Err(error) => {
            eprintln!("could not resolve the gateway discovery file: {error}");
        }
    }
    match install.gateway() {
        Some(exe) => GatewayPlan::Launch(exe),
        None if explicit_config => GatewayPlan::ConfigOnly,
        None => GatewayPlan::Fail,
    }
}

/// Where this Workshop runs from, which decides where the gateway lookup
/// searches. Read from the process once, so planning never reads the
/// environment.
#[derive(Debug)]
pub(super) struct WorkshopInstall {
    /// Workshop's executable.
    pub(super) exe: PathBuf,
    /// The AppImage Workshop runs from, per
    /// [`gateway_api_discovery::running_appimage`].
    pub(super) appimage: Option<PathBuf>,
}

impl WorkshopInstall {
    /// Reads the running Workshop's executable, `$APPIMAGE`, and `$APPDIR`.
    ///
    /// # Errors
    /// Returns an error when the executable path cannot be read.
    pub(super) fn current() -> anyhow::Result<Self> {
        let exe = std::env::current_exe().context("locate the executable")?;
        let appimage = gateway_api_discovery::running_appimage(
            &exe,
            std::env::var_os("APPIMAGE").as_deref(),
            std::env::var_os("APPDIR").as_deref(),
        );
        Ok(Self { exe, appimage })
    }

    /// The installed gateway executable, when one exists.
    pub(super) fn gateway(&self) -> Option<PathBuf> {
        gateway_api_discovery::installed_gateway(&self.exe, self.appimage.as_deref())
    }

    /// Why the lookup found no gateway and what to do, for the boot failure
    /// and the supervisor's recovery failure: a translocated Workshop cannot
    /// see the gateway beside it, so the remedy is moving the app; otherwise
    /// every path searched and the install step.
    pub(super) fn missing_gateway(&self) -> String {
        use gateway_api_discovery::{GATEWAY_BUNDLE_NAME, WORKSHOP_BUNDLE_NAME};
        if cfg!(target_os = "macos") && gateway_api_discovery::translocated(&self.exe) {
            return format!(
                "macOS runs {WORKSHOP_BUNDLE_NAME} from a translocated copy at {}, where \
                 {GATEWAY_BUNDLE_NAME} is not visible; {}; if {GATEWAY_BUNDLE_NAME} is not \
                 installed, install the Gateway component",
                self.exe.display(),
                gateway_api_discovery::translocation_remedy(
                    WORKSHOP_BUNDLE_NAME,
                    GATEWAY_BUNDLE_NAME
                )
            );
        }
        let searched =
            gateway_api_discovery::gateway_search_paths(&self.exe, self.appimage.as_deref())
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
        format!("Workshop found no executable gateway at {searched}; install the Gateway component")
    }
}

/// Builds the loud boot failure naming both supported remedies.
pub(super) fn no_gateway_error(install: &WorkshopInstall) -> anyhow::Error {
    anyhow::anyhow!(
        "no gateway configured or running. {}. Alternatively, set gateway.base_url and \
         gateway.api_key in workshop.toml to attach to a gateway over the network",
        install.missing_gateway()
    )
}

#[cfg(windows)]
pub(super) fn spawn_detached_windows_with<T, Spawn>(mut spawn: Spawn) -> std::io::Result<T>
where
    Spawn: FnMut(u32) -> std::io::Result<T>,
{
    match spawn(WINDOWS_BREAKAWAY_CREATION_FLAGS) {
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            // Hosted runners can forbid breakaway from their job object.
            // https://github.com/actions/runner/issues/595
            spawn(WINDOWS_DETACHED_CREATION_FLAGS)
        }
        result => result,
    }
}

fn detached_command(exe: &Path) -> std::process::Command {
    let mut command = std::process::Command::new(exe);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command
}

/// Spawns the Gateway detached from the desktop app's lifetime. The reaper
/// thread records the child's exit status in `exit`, so a launch wait can
/// stop as soon as the child dies instead of running out its budget.
pub(super) fn spawn_detached(exe: &Path, exit: Arc<OnceLock<ExitStatus>>) -> std::io::Result<u32> {
    #[cfg(windows)]
    let mut child = spawn_detached_windows_with(|flags| {
        use std::os::windows::process::CommandExt as _;
        detached_command(exe).creation_flags(flags).spawn()
    })?;
    #[cfg(unix)]
    let mut child = {
        use std::os::unix::process::CommandExt as _;
        let mut command = detached_command(exe);
        command.process_group(0);
        command.spawn()?
    };
    #[cfg(not(any(windows, unix)))]
    let mut child = detached_command(exe).spawn()?;
    let child_pid = child.id();
    if let Err(error) = std::thread::Builder::new().spawn(move || {
        if let Ok(status) = child.wait() {
            let _ = exit.set(status);
        }
    }) {
        eprintln!("could not spawn the gateway reaper thread; the child goes unreaped: {error}");
    }
    Ok(child_pid)
}
