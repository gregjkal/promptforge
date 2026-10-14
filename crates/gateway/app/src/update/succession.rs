//! The succession: once serving has ended, the old process hands the
//! instance lease to the gateway it swapped in, and owns the outcome.
//!
//! "Handoff" already names the `/auth` browser handoff and a second
//! launch's handoff to the running gateway, so the restart is called the
//! succession. The old process marked its discovery record before it
//! stopped serving, so every reader waits instead of launching. It drops
//! the lease, starts the installed path detached with `--login`, and waits
//! for any validated, unmarked gateway at the new version, whoever started
//! it. If none appears it kills its successor, restores the previous item,
//! starts that, and waits again with the mark still in place; if that also
//! fails it withdraws the mark and leaves recovery to the next launch. A
//! watchdog bounds the whole succession, since readers wait on a mark with
//! no timeout of their own.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::time::{Duration, Instant};

use gateway_api_discovery::{GatewayDiscoveryFile, GatewayInstanceLease, ValidatedConnection};
use gateway_config::ProfileName;

use super::manifest::bare_version;
use super::stage::{Install, remove_item};
use super::swap;
use crate::Stopped;
use crate::api_error::StartupError;

/// How long the successor and, after a rollback, the restored gateway each
/// have to publish. A gateway publishes about a second after start, since
/// provisioning is queued after the bind.
const PUBLISH_TIMEOUT: Duration = Duration::from_secs(30);

/// How often the old process reads the discovery record while it waits.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The bound on the whole succession from the mark: both publication waits
/// plus the serve loop's drain and teardown, with margin.
const WATCHDOG_TIMEOUT: Duration = Duration::from_secs(90);

/// The waits of one succession.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Timeouts {
    /// The successor's publication wait.
    pub(crate) successor: Duration,
    /// The restored gateway's publication wait.
    pub(crate) restored: Duration,
    /// The pause between reads of the discovery record.
    pub(crate) poll: Duration,
}

const TIMEOUTS: Timeouts = Timeouts {
    successor: PUBLISH_TIMEOUT,
    restored: PUBLISH_TIMEOUT,
    poll: POLL_INTERVAL,
};

/// A swapped-in gateway waiting to take over from this process, returned
/// by the serve loop in [`crate::Stopped::Succession`].
#[derive(Debug)]
pub struct Succession {
    record: Option<PathBuf>,
    pid: u32,
    installed: PathBuf,
    executable: PathBuf,
    staging: PathBuf,
    version: String,
    config_path: Option<PathBuf>,
}

/// How the old process ran, which its successor repeats.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SuccessorShape {
    /// The command-line profile override, when one was given.
    pub profile: Option<ProfileName>,
    /// Whether the old process ran without the tray.
    pub headless: bool,
}

impl SuccessorShape {
    /// The shape of a process that ran with `profile` and, when
    /// `headless`, without the tray.
    #[must_use]
    pub fn new(profile: Option<ProfileName>, headless: bool) -> SuccessorShape {
        SuccessorShape { profile, headless }
    }
}

/// Why a succession did not end with the new version serving.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SuccessionError {
    /// The new version never served, and the previous gateway was
    /// restored and serves again.
    #[error(
        "no gateway at version {version} served within the wait ({reason}); the previous gateway was restored"
    )]
    RolledBack {
        /// The version that was swapped in.
        version: String,
        /// Why the successor counted as failed.
        reason: String,
    },
    /// Neither the new version nor the restored previous gateway served.
    /// The restart mark was withdrawn, so the next launch recovers.
    #[error(
        "no gateway at version {version} served ({reason}), and the previous gateway did not come back ({restore}); the next launch starts the installed gateway"
    )]
    Failed {
        /// The version that was swapped in.
        version: String,
        /// Why the successor counted as failed.
        reason: String,
        /// Why the restore failed.
        restore: String,
    },
}

/// A started gateway the old process may have to kill.
pub(crate) trait Spawned {
    /// Whether the process has exited.
    fn exited(&mut self) -> bool;
    /// Kills the process and reaps it.
    fn kill(&mut self);
}

impl Spawned for Child {
    fn exited(&mut self) -> bool {
        !matches!(self.try_wait(), Ok(None))
    }

    fn kill(&mut self) {
        if let Err(error) = Child::kill(self) {
            tracing::warn!(
                "could not kill the successor gateway {}: {error}",
                self.id()
            );
        }
        let _ = self.wait();
    }
}

impl Succession {
    /// The succession for `install`, swapped to `version` with its staging
    /// directory at `staging`. `record` is the run directory holding this
    /// process's discovery record, when it has one.
    pub(crate) fn new(
        record: Option<PathBuf>,
        install: &Install,
        staging: PathBuf,
        version: String,
        config_path: Option<PathBuf>,
    ) -> Succession {
        Succession {
            record,
            pid: std::process::id(),
            installed: install.item().to_path_buf(),
            executable: install.executable(),
            staging,
            version,
            config_path,
        }
    }

    /// Marks this process's discovery record as handing off and starts the
    /// watchdog. Runs before serving stops, so the serve loop's own cleanup
    /// leaves the record for the successor.
    pub(crate) fn mark(&self) {
        let Some(run_dir) = &self.record else {
            tracing::warn!("this gateway published no discovery record; nothing to mark");
            return;
        };
        match gateway_api_discovery::mark_restarting(run_dir, self.pid) {
            Ok(()) => {
                watchdog_with(run_dir.clone(), self.pid, WATCHDOG_TIMEOUT, || {
                    std::process::exit(1)
                });
            }
            Err(error) => tracing::warn!(
                "could not mark the discovery record for the restart: {error}; readers may start the new gateway themselves"
            ),
        }
    }

    /// Hands `lease` to the installed gateway, run as `shape` with
    /// `environment`, and waits for it; restores the previous gateway when
    /// the new one never serves. Returns the version that serves.
    ///
    /// # Errors
    /// Returns [`SuccessionError`] when the new version did not serve.
    pub fn run(
        self,
        lease: GatewayInstanceLease,
        shape: &SuccessorShape,
        environment: &[(OsString, OsString)],
    ) -> Result<String, SuccessionError> {
        let arguments = successor_args(
            self.config_path.as_deref(),
            shape.profile.as_ref(),
            shape.headless,
        );
        let record = self.record.clone();
        let spawn = |exe: &Path, arguments: &[OsString]| {
            let mut command = std::process::Command::new(exe);
            command
                .env_clear()
                .envs(environment.iter().map(|(name, value)| (name, value)))
                .args(arguments);
            super::strip_test_overrides(&mut command);
            gateway_api_discovery::spawn_detached(&mut command)
        };
        let serving = |version: Option<&str>, deadline: Instant| {
            record
                .as_deref()
                .is_some_and(|run_dir| serving(run_dir, version, deadline))
        };
        self.run_with(lease, &arguments, spawn, serving, TIMEOUTS)
    }

    /// [`Succession::run`] with the spawn, the publication check, and the
    /// waits injected.
    pub(crate) fn run_with<C: Spawned>(
        self,
        lease: GatewayInstanceLease,
        arguments: &[OsString],
        mut spawn: impl FnMut(&Path, &[OsString]) -> io::Result<C>,
        mut serving: impl FnMut(Option<&str>, Instant) -> bool,
        timeouts: Timeouts,
    ) -> Result<String, SuccessionError> {
        // The lease goes first: the successor takes it at once instead of
        // losing it to this process and exiting.
        drop(lease);
        let reason = match spawn(&self.executable, arguments) {
            Ok(mut successor) => {
                if self.record.is_none() {
                    tracing::warn!(
                        "started PromptForge Gateway {} with no discovery record to confirm it by",
                        self.version
                    );
                    self.clean_up();
                    return Ok(self.version);
                }
                if wait(
                    &mut serving,
                    Some(&self.version),
                    timeouts.successor,
                    timeouts.poll,
                ) {
                    tracing::info!("PromptForge Gateway {} took over", self.version);
                    self.clean_up();
                    return Ok(self.version);
                }
                if !successor.exited() {
                    successor.kill();
                }
                format!("none published within {:?}", timeouts.successor)
            }
            Err(error) => format!("{} did not start: {error}", self.executable.display()),
        };
        tracing::error!(
            "PromptForge Gateway {} did not take over ({reason}); restoring the previous gateway",
            self.version
        );
        let restore = self.restore(arguments, &mut spawn, &mut serving, timeouts);
        match restore {
            Ok(()) => Err(SuccessionError::RolledBack {
                version: self.version,
                reason,
            }),
            Err(restore) => {
                self.unmark();
                Err(SuccessionError::Failed {
                    version: self.version,
                    reason,
                    restore,
                })
            }
        }
    }

    fn restore<C: Spawned>(
        &self,
        arguments: &[OsString],
        spawn: &mut impl FnMut(&Path, &[OsString]) -> io::Result<C>,
        serving: &mut impl FnMut(Option<&str>, Instant) -> bool,
        timeouts: Timeouts,
    ) -> Result<(), String> {
        swap::restore(&self.installed, &self.staging.join("rejected")).map_err(|error| {
            format!(
                "cannot move {} back to {}: {error}",
                swap::previous_path(&self.installed).display(),
                self.installed.display()
            )
        })?;
        spawn(&self.executable, arguments)
            .map_err(|error| format!("{} did not start: {error}", self.executable.display()))?;
        if !wait(serving, None, timeouts.restored, timeouts.poll) {
            return Err(format!("none published within {:?}", timeouts.restored));
        }
        tracing::info!("the previous gateway serves again");
        if let Err(error) = std::fs::remove_dir_all(&self.staging) {
            tracing::warn!("could not remove {}: {error}", self.staging.display());
        }
        Ok(())
    }

    /// Removes the moved-aside previous item and the staging directory
    /// once the new version serves.
    fn clean_up(&self) {
        for path in [swap::previous_path(&self.installed), self.staging.clone()] {
            match remove_item(&path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => {
                    tracing::warn!("could not remove {}: {error}", path.display());
                }
                _ => {}
            }
        }
    }

    /// Withdraws this process's restart mark, so readers stop waiting.
    fn unmark(&self) {
        if let Some(run_dir) = &self.record
            && let Err(error) = gateway_api_discovery::remove_marked_if_mine(run_dir, self.pid)
        {
            tracing::error!("could not withdraw the restart mark: {error}");
        }
    }
}

/// How serving ended, given the succession a self-update requested. The
/// succession wins over a serve error: the update is already swapped in,
/// and only the succession starts it.
pub(crate) fn stopped(
    served: Result<(), StartupError>,
    succession: Option<Succession>,
) -> Result<Stopped, StartupError> {
    let Some(succession) = succession else {
        return served.map(|()| Stopped::Quit);
    };
    if let Err(error) = served {
        tracing::error!(
            "serving ended with an error after the update was installed: {}; restarting into it",
            crate::error::error_chain(&error)
        );
    }
    Ok(Stopped::Succession(succession))
}

/// The successor's arguments, rebuilt from how the old process ran: the
/// resolved config path (on macOS the default lookup beside the executable
/// would look inside the swapped bundle), the profile override, `--no-tray`
/// for a headless process, and always `--login`, so no succession opens a
/// browser, even when the successor loses the lease.
pub(crate) fn successor_args(
    config: Option<&Path>,
    profile: Option<&ProfileName>,
    headless: bool,
) -> Vec<OsString> {
    let mut arguments = Vec::new();
    if let Some(config) = config {
        arguments.push(OsString::from("--config"));
        arguments.push(config.as_os_str().to_owned());
    }
    if let Some(profile) = profile {
        arguments.push(OsString::from("--profile"));
        arguments.push(OsString::from(profile.as_str()));
    }
    if headless {
        arguments.push(OsString::from("--no-tray"));
    }
    arguments.push(OsString::from("--login"));
    arguments
}

/// Whether a validated, unmarked gateway serves from `run_dir`, at
/// `version` when one is required.
fn serving(run_dir: &Path, version: Option<&str>, deadline: Instant) -> bool {
    let Ok(Some(file)) = GatewayDiscoveryFile::read(run_dir) else {
        return false;
    };
    if file.restarting || version.is_some_and(|version| bare_version(&file.version) != version) {
        return false;
    }
    ValidatedConnection::validate_before(file, deadline).is_ok()
}

/// Polls `serving` until it holds or `timeout` passes.
fn wait(
    serving: &mut impl FnMut(Option<&str>, Instant) -> bool,
    version: Option<&str>,
    timeout: Duration,
    poll: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if serving(version, deadline) {
            return true;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        std::thread::sleep(poll.min(remaining));
    }
}

/// Starts the watchdog thread: after `timeout` it withdraws `pid`'s restart
/// mark in `run_dir` and calls `expire`, which in production exits the
/// process. A succession that ends first ends the process, and the thread
/// with it.
pub(crate) fn watchdog_with(
    run_dir: PathBuf,
    pid: u32,
    timeout: Duration,
    expire: impl FnOnce() + Send + 'static,
) -> Option<std::thread::JoinHandle<()>> {
    let spawned = std::thread::Builder::new()
        .name("gateway-succession-watchdog".to_owned())
        .spawn(move || {
            std::thread::sleep(timeout);
            tracing::error!(
                "the restart into the new gateway did not finish within {timeout:?}; withdrawing the restart mark and exiting"
            );
            if let Err(error) = gateway_api_discovery::remove_marked_if_mine(&run_dir, pid) {
                tracing::error!("could not withdraw the restart mark: {error}");
            }
            expire();
        });
    match spawned {
        Ok(handle) => Some(handle),
        Err(error) => {
            tracing::error!("could not start the restart watchdog: {error}");
            None
        }
    }
}
