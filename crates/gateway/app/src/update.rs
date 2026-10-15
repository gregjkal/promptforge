//! The gateway's self-update, from the release manifest Workshop's updater
//! also reads.
//!
//! [`apply`] is the one entry point; `POST /update` calls it. On macOS and
//! Linux it checks the manifest, downloads the platform's signed archive
//! into a staging directory beside the install while verifying its minisign
//! signature, extracts it, requires the staged executable to report the
//! advertised version, and swaps it in by rename while this process keeps
//! running. It then marks the discovery file and asks the serve loop to stop
//! with a [`Succession`]: once serving has ended, `main` hands the instance
//! lease to the new binary and restores the previous one if no gateway at the
//! new version appears. On Windows without a Workshop beside the gateway it
//! downloads the signed setup instead and starts it in update mode, which
//! stops and relaunches this gateway itself; with Workshop installed,
//! Workshop's updater owns updates, because the setup updates every
//! installed component.

use std::path::{Path, PathBuf};

use crate::AppState;
use crate::shutdown::{ShutdownSignal, UpdateTicket};

mod manifest;
pub(crate) mod notice;
mod stage;
mod succession;
mod swap;
mod verify;
mod windows;

pub(crate) use self::succession::stopped;
pub use self::succession::{Succession, SuccessionError, SuccessorShape};

/// The release manifest the gateway reads unless [`UPDATE_URL_ENV`]
/// overrides it: the one `tauri.conf.json`'s updater endpoint names.
const DEFAULT_UPDATE_URL: &str =
    "https://github.com/cppalliance/promptforge/releases/download/workshop-latest/latest.json";

/// Overrides [`DEFAULT_UPDATE_URL`], as `PROMPTFORGE_MODELS_SHEET_URL`
/// overrides the model sheet. The signature check is what makes the
/// override safe: only a newer release signed by the release key applies.
const UPDATE_URL_ENV: &str = "PROMPTFORGE_UPDATE_URL";

/// Replaces the version this gateway reports, in `test-fixtures` builds
/// only, so a test can run a copy that is older than the release it serves.
/// Children the update starts never inherit it.
#[cfg(feature = "test-fixtures")]
const TEST_VERSION_ENV: &str = "PROMPTFORGE_GATEWAY_TEST_VERSION";

/// The version this gateway reports: in `--version`, in its discovery file,
/// and to the update check.
#[must_use]
pub fn version() -> String {
    #[cfg(feature = "test-fixtures")]
    if let Some(version) = std::env::var(TEST_VERSION_ENV)
        .ok()
        .filter(|value| !value.is_empty())
    {
        return version;
    }
    env!("CARGO_PKG_VERSION").to_owned()
}

/// A command for a child the update starts, without the test-only
/// overrides, so a staged or successor gateway reports its own version.
fn child_command(exe: &Path) -> std::process::Command {
    let mut command = std::process::Command::new(exe);
    strip_test_overrides(&mut command);
    command
}

/// Removes the test-only version override from `command`'s environment;
/// does nothing outside `test-fixtures` builds.
#[cfg_attr(
    not(feature = "test-fixtures"),
    expect(
        unused_variables,
        reason = "only test-fixtures builds strip a variable"
    )
)]
fn strip_test_overrides(command: &mut std::process::Command) {
    #[cfg(feature = "test-fixtures")]
    command.env_remove(TEST_VERSION_ENV);
}

/// What [`apply`] concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Applied {
    /// A newer release is installing. On macOS and Linux it is swapped in
    /// and the succession is requested; on Windows its setup is running.
    Updating {
        /// The version being installed.
        version: String,
    },
    /// The gateway runs the newest release.
    Current {
        /// The version it runs.
        version: String,
    },
    /// Workshop, installed beside this gateway on Windows, owns updates.
    WorkshopOwned,
    /// Another update is already running.
    Busy,
}

/// Why an update failed. Every failure but [`UpdateError::Stranded`]
/// leaves the running gateway and its install as they were.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub(crate) enum UpdateError {
    /// This gateway is a development build, whose binary lives in a build
    /// tree; replacing it with a release would discard the build.
    #[error(
        "this gateway is a development build and updates by rebuilding; only a release build updates itself"
    )]
    Development,
    /// The running executable's path could not be read.
    #[error("cannot locate the running gateway executable")]
    Executable(#[source] std::io::Error),
    /// No release is published for this operating system and architecture.
    #[error("no gateway self-update is published for {os} on {arch}")]
    NoPlatform {
        /// The operating system.
        os: &'static str,
        /// The architecture.
        arch: &'static str,
    },
    /// The release manifest could not be fetched.
    #[error("cannot fetch the release manifest {url}")]
    Fetch {
        /// The manifest URL.
        url: String,
        /// The transport error.
        #[source]
        source: reqwest::Error,
    },
    /// The release manifest is not what the updater publishes.
    #[error("the release manifest {url} is unusable: {reason}")]
    Manifest {
        /// The manifest URL.
        url: String,
        /// What is wrong with it.
        reason: String,
    },
    /// The release archive or setup could not be downloaded.
    #[error("cannot download {url}: {reason}")]
    Download {
        /// The download URL.
        url: String,
        /// What failed.
        reason: String,
    },
    /// The download's signature does not verify against the release key.
    #[error("the {key} release does not verify against the gateway's release key: {reason}")]
    Signature {
        /// The manifest's platform key.
        key: &'static str,
        /// Why verification refused it.
        reason: String,
    },
    /// The executable sits inside another application's bundle, which the
    /// swap would replace and its clean-up delete.
    #[error(
        "the gateway runs inside {bundle}, not a standalone {name}; only a standalone gateway updates itself",
        name = gateway_api_discovery::GATEWAY_BUNDLE_NAME
    )]
    NotStandalone {
        /// The enclosing bundle.
        bundle: PathBuf,
    },
    /// The directory the update writes cannot be written: the staging
    /// directory beside the install, or the setup's download directory.
    #[error("the update directory {directory} is not writable")]
    NotWritable {
        /// The directory the update must write.
        directory: PathBuf,
        /// The I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The verified archive does not hold one gateway to install.
    #[error("the {key} release archive is unusable: {reason}")]
    Archive {
        /// The manifest's platform key.
        key: &'static str,
        /// What is wrong with it.
        reason: String,
    },
    /// The staged gateway does not report the advertised version.
    #[error(
        "the staged gateway reports \"{actual}\"; the release manifest advertises \"{required}\""
    )]
    StagedVersion {
        /// What `--version` must print.
        required: String,
        /// What it printed, or why it could not run.
        actual: String,
    },
    /// The staged gateway could not be renamed into place.
    #[error("cannot swap the update into {installed}; the previous gateway is in place")]
    Swap {
        /// The installed item.
        installed: PathBuf,
        /// The rename error.
        #[source]
        source: std::io::Error,
    },
    /// The swap failed and the previous gateway could not be moved back,
    /// so nothing is installed until it is moved back by hand.
    #[error(
        "cannot swap the update into {installed}, and the previous gateway could not be moved back ({reason}); it waits at {previous}: move it to {installed}"
    )]
    Stranded {
        /// The installed item's path, now empty.
        installed: PathBuf,
        /// Where the previous gateway waits.
        previous: PathBuf,
        /// Both rename errors.
        reason: String,
    },
    /// The Windows setup could not be started.
    #[error("cannot start the update setup {path}")]
    Setup {
        /// The downloaded setup.
        path: PathBuf,
        /// The spawn error.
        #[source]
        source: std::io::Error,
    },
    /// The gateway began shutting down before the swap.
    #[error("the gateway is shutting down; the update was not installed")]
    ShuttingDown,
    /// Blocking update work panicked or was cancelled.
    #[error("the update's file work did not finish: {0}")]
    Task(String),
}

/// What the update needs to know about the process and its release
/// channel, gathered once so tests can replace any of it.
#[derive(Debug, Clone)]
pub(crate) struct Environment {
    /// The release manifest URL.
    pub(crate) manifest_url: String,
    /// The release public key, as `tauri.conf.json` writes it.
    pub(crate) public_key: String,
    /// The version this gateway runs.
    pub(crate) version: String,
    /// The operating system, as `std::env::consts::OS` names it.
    pub(crate) os: &'static str,
    /// The architecture, as `std::env::consts::ARCH` names it.
    pub(crate) arch: &'static str,
    /// Whether this is a development build, which never updates itself.
    /// A `test-fixtures` build is a release stand-in for tests and smokes.
    pub(crate) development: bool,
}

impl Environment {
    /// The running process's environment.
    pub(crate) fn current() -> Environment {
        Environment {
            manifest_url: std::env::var(UPDATE_URL_ENV)
                .ok()
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| DEFAULT_UPDATE_URL.to_owned()),
            public_key: verify::release_key(),
            version: version(),
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            development: cfg!(debug_assertions) && !cfg!(feature = "test-fixtures"),
        }
    }
}

/// Checks for a newer release and installs it: the one entry point the
/// route and the tray share.
///
/// # Errors
/// Returns [`UpdateError`] naming what failed; the running gateway and its
/// install are left as they were, except after [`UpdateError::Stranded`].
pub(crate) async fn apply(state: &AppState) -> Result<Applied, UpdateError> {
    apply_with(state, &Environment::current()).await
}

async fn apply_with(state: &AppState, environment: &Environment) -> Result<Applied, UpdateError> {
    let Some(ticket) = state.shutdown.begin_update() else {
        return Ok(Applied::Busy);
    };
    update(state, environment, ticket).await
}

/// The update `ticket` claims. Every early end drops the ticket and frees
/// the claim; the swap and a stranded install keep it for the process's
/// remaining life, and the Windows setup holds it until the setup exits.
async fn update(
    state: &AppState,
    environment: &Environment,
    ticket: UpdateTicket,
) -> Result<Applied, UpdateError> {
    if environment.development {
        return Err(UpdateError::Development);
    }
    let exe = std::env::current_exe().map_err(UpdateError::Executable)?;
    let route = windows::route(environment.os, || {
        gateway_api_discovery::installed_workshop(&exe)
    });
    if route == windows::Route::WorkshopOwned {
        return Ok(Applied::WorkshopOwned);
    }
    let key = manifest::platform_key(environment.os, environment.arch).ok_or(
        UpdateError::NoPlatform {
            os: environment.os,
            arch: environment.arch,
        },
    )?;
    let client = gateway_protocol::http_util::bounded_client();
    let (version, entry) = match manifest::check(
        &client,
        &environment.manifest_url,
        key,
        &environment.version,
    )
    .await?
    {
        manifest::Check::Current { version } => return Ok(Applied::Current { version }),
        manifest::Check::Available { version, entry } => (version, entry),
    };
    let verifier = verify::Verifier::new(&environment.public_key, &entry.signature, &version)
        .map_err(|reason| UpdateError::Signature { key, reason })?;
    let label = format!("Downloading PromptForge Gateway {version}");
    let activity = state.hub.begin(label.clone());
    let download = stage::Download {
        url: &entry.url,
        key,
        verifier: &verifier,
        activity: &activity,
        label,
    };
    if route == windows::Route::Setup {
        let setup = windows::download_setup(download).await?;
        windows::start_setup(&setup, ticket, &state.shutdown)?;
        tracing::info!(
            "started the PromptForge {version} setup {}; it restarts this gateway",
            setup.display()
        );
        return Ok(Applied::Updating { version });
    }
    let install = stage::Install::locate(&exe);
    install.require_standalone()?;
    let staging = stage::Staging::create(&install)?;
    stage::download(download, &staging.archive()).await?;
    activity.set_text(format!("Installing PromptForge Gateway {version}"));
    let staging = {
        let (install, version, shutdown) =
            (install.clone(), version.clone(), state.shutdown.clone());
        match run_blocking(move || install_staged(staging, &install, &version, key, &shutdown))
            .await
        {
            Ok(staging) => staging,
            Err(error) => {
                end_failed_install(ticket, &error);
                return Err(error);
            }
        }
    };
    drop(activity);
    tracing::info!(
        "installed PromptForge Gateway {version} at {}; restarting into it",
        install.item().display()
    );
    let succession = Succession::new(
        state.shutdown.record_dir(),
        &install,
        staging.into_path(),
        version.clone(),
        state.config.as_ref().map(|config| config.path.clone()),
    );
    succession.mark();
    ticket.keep();
    state.commands.cancel_active();
    state.shutdown.request_succession(succession);
    Ok(Applied::Updating { version })
}

/// Extracts the verified download in `staging`, checks the staged
/// executable's version, and swaps it into `install`, unless `shutdown`
/// has fired: a gateway asked to quit does not restart into the update.
///
/// The staging guard moves in, so it is removed only after the swap is
/// over, even when the runtime drops the waiting update meanwhile.
fn install_staged(
    staging: stage::Staging,
    install: &stage::Install,
    version: &str,
    key: &'static str,
    shutdown: &ShutdownSignal,
) -> Result<stage::Staging, UpdateError> {
    let staged = stage::extract(&staging.archive(), &staging.extracted(), key)?;
    stage::require_version(&install.executable_in(&staged), version)?;
    if shutdown.is_fired() {
        return Err(UpdateError::ShuttingDown);
    }
    swap::swap(install.item(), &staged)?;
    Ok(staging)
}

/// Ends `ticket`'s update after a failed install, unless the install is
/// stranded: this process then runs from the moved-aside copy, which on
/// Linux a later update would locate and swap in place of the install.
fn end_failed_install(ticket: UpdateTicket, error: &UpdateError) {
    if matches!(error, UpdateError::Stranded { .. }) {
        ticket.keep();
    }
}

/// Runs file work off the runtime's worker threads.
async fn run_blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, UpdateError> + Send + 'static,
) -> Result<T, UpdateError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| UpdateError::Task(error.to_string()))?
}

#[cfg(test)]
mod tests;
