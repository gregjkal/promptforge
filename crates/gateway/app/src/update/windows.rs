//! The Windows update path. The gateway is a component of Workshop's NSIS
//! setup there, so it updates by running that setup in update mode: the
//! setup restores the gateway-only selection, skips every page and `init`,
//! stops this gateway, and relaunches it with `--login`. With Workshop
//! installed beside the gateway, Workshop's updater runs the same setup,
//! which updates both, and two concurrent setups must never run, so the
//! gateway leaves updates to Workshop.

use std::path::{Path, PathBuf};
use std::process::Child;
use std::thread::JoinHandle;

use super::UpdateError;
use super::stage::{Download, download, remove_item};
use crate::shutdown::{ShutdownSignal, UpdateTicket};

/// The setup's arguments: passive, update mode. Never `/R`, which would
/// launch a Workshop that is not installed.
pub(crate) const SETUP_ARGUMENTS: [&str; 2] = ["/P", "/UPDATE"];

/// The directory under the temporary directory the setup downloads into.
const SETUP_DIR_NAME: &str = "promptforge-gateway-update";

/// The setup's name when the download URL names no `.exe`.
const DEFAULT_SETUP_NAME: &str = "PromptForge-setup.exe";

/// How this gateway updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Route {
    /// Swap in the gateway's own archive (macOS and Linux).
    Swap,
    /// Run Workshop's setup in update mode (Windows, no Workshop).
    Setup,
    /// Leave updates to Workshop's updater (Windows, Workshop installed).
    WorkshopOwned,
}

/// The route for `os`. `workshop` looks for a Workshop installed beside
/// the gateway, and runs only on Windows.
pub(crate) fn route(os: &str, workshop: impl FnOnce() -> Option<PathBuf>) -> Route {
    if os != "windows" {
        Route::Swap
    } else if workshop().is_some() {
        Route::WorkshopOwned
    } else {
        Route::Setup
    }
}

/// The file name the setup at `url` is saved under: the URL's last segment
/// when it is a plain `.exe` name, else [`DEFAULT_SETUP_NAME`].
pub(crate) fn setup_file_name(url: &str) -> String {
    url.split(['?', '#'])
        .next()
        .and_then(|path| path.rsplit('/').next())
        .filter(|name| {
            name.to_ascii_lowercase().ends_with(".exe")
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        })
        .map_or_else(|| DEFAULT_SETUP_NAME.to_owned(), str::to_owned)
}

/// Downloads and verifies the setup into a fresh directory under the
/// temporary directory, returning its path.
///
/// # Errors
/// Returns [`UpdateError::NotWritable`] when the directory cannot be
/// created, and the download's errors.
pub(crate) async fn download_setup(setup: Download<'_>) -> Result<PathBuf, UpdateError> {
    let dir = std::env::temp_dir().join(SETUP_DIR_NAME);
    let not_writable = |source| UpdateError::NotWritable {
        directory: dir.clone(),
        source,
    };
    match remove_item(&dir) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(not_writable(error));
        }
        _ => {}
    }
    std::fs::create_dir_all(&dir).map_err(not_writable)?;
    let path = dir.join(setup_file_name(setup.url));
    download(setup, &path).await?;
    Ok(path)
}

/// Starts the verified setup detached, outside the gateway's job object,
/// so it outlives the gateway it stops, and holds the update `ticket`
/// until the setup exits. Once `shutdown` has fired it starts nothing: the
/// setup relaunches the gateway, and a quit must not come back.
///
/// # Errors
/// Returns [`UpdateError::ShuttingDown`] once `shutdown` has fired, and
/// [`UpdateError::Setup`] when the setup cannot start.
pub(crate) fn start_setup(
    path: &Path,
    ticket: UpdateTicket,
    shutdown: &ShutdownSignal,
) -> Result<(), UpdateError> {
    if shutdown.is_fired() {
        return Err(UpdateError::ShuttingDown);
    }
    let mut command = std::process::Command::new(path);
    command.args(SETUP_ARGUMENTS);
    let setup = gateway_api_discovery::spawn_detached(&mut command).map_err(|source| {
        UpdateError::Setup {
            path: path.to_path_buf(),
            source,
        }
    })?;
    release_on_exit(setup, ticket);
    Ok(())
}

/// Holds `ticket` until `setup` exits. A setup that installs stops this
/// gateway first, so an exit seen here means the update did not install,
/// and the claim is freed for the next attempt.
pub(crate) fn release_on_exit(mut setup: Child, ticket: UpdateTicket) -> Option<JoinHandle<()>> {
    let pid = setup.id();
    let watcher = std::thread::Builder::new()
        .name("gateway-update-setup".to_owned())
        .spawn(move || {
            match setup.wait() {
                Ok(status) => tracing::warn!(
                    "the update setup {pid} exited ({status}) and this gateway still runs; the update did not install"
                ),
                Err(error) => {
                    tracing::warn!("could not wait for the update setup {pid}: {error}");
                }
            }
            drop(ticket);
        });
    match watcher {
        Ok(handle) => Some(handle),
        Err(error) => {
            // The closure, and the ticket with it, dropped: the claim is
            // free while the setup may still run.
            tracing::warn!("could not watch the update setup {pid}: {error}");
            None
        }
    }
}
