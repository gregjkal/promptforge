//! The tray's update notice: what the tray's `Update` item and status line
//! say about a newer release, and the one worker that finds out.
//!
//! The Windows and macOS trays run on the main thread with no runtime, so
//! the tray never calls the update flow itself. It reads [`Notice::state`]
//! on every tick and asks for an update with [`Notice::request_apply`], a
//! channel send that works from any thread. A worker task on the gateway's
//! runtime owns the checks: at startup and then every [`CHECK_INTERVAL`] it
//! runs the manifest check alone, and on a request it runs [`super::apply`],
//! the flow `POST /update` runs. Only tray runs start the worker, so a
//! headless gateway never checks.

use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::mpsc;

use super::manifest::{self, Check};
use super::{Applied, Environment, UpdateError, windows};
use crate::AppState;
use crate::error::error_chain;
use crate::runner::ServeOptions;
use crate::shutdown::ShutdownSignal;

/// How long the worker waits between scheduled checks.
pub(crate) const CHECK_INTERVAL: Duration = Duration::from_hours(24);

/// What the tray shows about updates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NoticeState {
    /// No check has finished yet.
    Unchecked,
    /// The last check found nothing newer than this running version.
    Current(String),
    /// The last check found this newer version.
    Available(String),
    /// A check or an update is running, the route's included.
    Busy,
    /// The last check or update failed, for this reason. It stays until
    /// the next check, scheduled or clicked.
    Failed(String),
    /// Workshop, installed beside this gateway on Windows, owns updates.
    WorkshopOwned,
}

/// The shared notice: the stored result of the last check or update, and
/// the request channel the tray sends on. Every clone shares one notice.
#[derive(Debug, Clone)]
pub(crate) struct Notice {
    shared: Arc<Shared>,
}

#[derive(Debug)]
struct Shared {
    /// The last result; never [`NoticeState::Busy`], which is derived.
    stored: Mutex<NoticeState>,
    /// Whether the worker is running a check or an update.
    working: AtomicBool,
    /// The tray's update requests.
    requests: mpsc::UnboundedSender<()>,
    /// The receiving end, until the worker takes it.
    receiver: Mutex<Option<mpsc::UnboundedReceiver<()>>>,
    /// The process's shutdown signal: its update claim marks an update
    /// the route started, and its firing ends the worker.
    shutdown: ShutdownSignal,
}

impl Notice {
    /// A notice that has checked nothing, reading `shutdown`'s update
    /// claim.
    pub(crate) fn new(shutdown: ShutdownSignal) -> Notice {
        let (requests, receiver) = mpsc::unbounded_channel();
        Notice {
            shared: Arc::new(Shared {
                stored: Mutex::new(NoticeState::Unchecked),
                working: AtomicBool::new(false),
                requests,
                receiver: Mutex::new(Some(receiver)),
                shutdown,
            }),
        }
    }

    /// What the tray shows now: [`NoticeState::Busy`] while the worker
    /// runs or another update holds the claim, else the last result. A
    /// failure outranks the claim, so a stranded install, which keeps the
    /// claim, still shows its error.
    pub(crate) fn state(&self) -> NoticeState {
        let stored = self
            .shared
            .stored
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let claimed =
            self.shared.shutdown.is_updating() && !matches!(stored, NoticeState::Failed(_));
        if self.shared.working.load(Ordering::Acquire) || claimed {
            NoticeState::Busy
        } else {
            stored
        }
    }

    /// Asks the worker to run the whole update flow. It never blocks, so
    /// any thread may call it.
    pub(crate) fn request_apply(&self) {
        // The receiver lives in the notice or the worker, so the send
        // fails only while the notice itself is being dropped.
        let _ = self.shared.requests.send(());
    }

    /// The request channel's receiving end, once: the worker's.
    pub(crate) fn take_requests(&self) -> Option<mpsc::UnboundedReceiver<()>> {
        self.shared
            .receiver
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// Records the last result.
    pub(super) fn store(&self, state: NoticeState) {
        *self
            .shared
            .stored
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = state;
    }

    /// Marks the worker busy until the guard drops.
    fn working(&self) -> Working<'_> {
        self.shared.working.store(true, Ordering::Release);
        Working(self)
    }
}

/// Clears the worker's busy mark on drop.
struct Working<'a>(&'a Notice);

impl Drop for Working<'_> {
    fn drop(&mut self) {
        self.0.shared.working.store(false, Ordering::Release);
    }
}

/// Starts the notice worker on `runtime` when `options` asks for it, as
/// tray runs do. The request channel is taken here, before the worker
/// first runs, so a second start finds nothing to take.
pub(crate) fn start(runtime: &tokio::runtime::Handle, options: &ServeOptions, state: &AppState) {
    if !options.update_notice {
        return;
    }
    let Some(requests) = state.update_notice.take_requests() else {
        return;
    };
    let notice = state.update_notice.clone();
    let state = state.clone();
    let sources = Sources {
        os: std::env::consts::OS,
        workshop: workshop_beside,
        check: check_release,
        apply: move || {
            let state = state.clone();
            async move { super::apply(&state).await }
        },
    };
    runtime.spawn(run(notice, requests, sources));
}

/// Where the worker's answers come from, so tests can replace each.
pub(crate) struct Sources<W, C, A> {
    /// The operating system, as `std::env::consts::OS` names it.
    pub(crate) os: &'static str,
    /// Looks for a Workshop installed beside the gateway; read only on
    /// Windows, before every check.
    pub(crate) workshop: W,
    /// The manifest check alone.
    pub(crate) check: C,
    /// The whole update flow.
    pub(crate) apply: A,
}

/// The worker: checks at once, then every [`CHECK_INTERVAL`], and runs
/// the update flow once per burst of requests, until the gateway shuts
/// down. The timer is `tokio::time`, so tests drive it with paused time.
pub(crate) async fn run<W, C, CF, A, AF>(
    notice: Notice,
    mut requests: mpsc::UnboundedReceiver<()>,
    mut sources: Sources<W, C, A>,
) where
    W: Fn() -> Option<PathBuf>,
    C: FnMut() -> CF,
    CF: Future<Output = Result<Check, UpdateError>>,
    A: FnMut() -> AF,
    AF: Future<Output = Result<Applied, UpdateError>>,
{
    let shutdown = notice.shared.shutdown.clone();
    let mut next = tokio::time::Instant::now();
    loop {
        tokio::select! {
            () = shutdown.fired() => return,
            () = tokio::time::sleep_until(next) => {
                check_once(&notice, &mut sources).await;
                next = tokio::time::Instant::now() + CHECK_INTERVAL;
            }
            request = requests.recv() => {
                if request.is_none() {
                    return;
                }
                apply_once(&notice, &mut sources).await;
                // Clicks queued while the update ran asked for the same
                // update; one run answers them all.
                while requests.try_recv().is_ok() {}
            }
        }
    }
}

/// One scheduled check, skipped on Windows when Workshop owns updates.
async fn check_once<W, C, CF, A>(notice: &Notice, sources: &mut Sources<W, C, A>)
where
    W: Fn() -> Option<PathBuf>,
    C: FnMut() -> CF,
    CF: Future<Output = Result<Check, UpdateError>>,
{
    if windows::route(sources.os, &sources.workshop) == windows::Route::WorkshopOwned {
        notice.store(NoticeState::WorkshopOwned);
        return;
    }
    let _working = notice.working();
    let state = match (sources.check)().await {
        Ok(Check::Current { version }) => {
            tracing::info!("the update check found PromptForge Gateway {version} is current");
            NoticeState::Current(version)
        }
        Ok(Check::Available { version, .. }) => {
            tracing::info!(
                "the update check found PromptForge Gateway {version}; the tray's Update item installs it"
            );
            NoticeState::Available(version)
        }
        Err(error) => {
            tracing::warn!("the update check failed: {}", error_chain(&error));
            NoticeState::Failed(error.to_string())
        }
    };
    notice.store(state);
}

/// One requested update, the flow `POST /update` runs.
async fn apply_once<W, C, A, AF>(notice: &Notice, sources: &mut Sources<W, C, A>)
where
    A: FnMut() -> AF,
    AF: Future<Output = Result<Applied, UpdateError>>,
{
    let _working = notice.working();
    match (sources.apply)().await {
        // The claim shows the update as busy until this process hands off
        // or the Windows setup ends; a setup that ends without installing
        // leaves the release available to try again.
        Ok(Applied::Updating { version }) => notice.store(NoticeState::Available(version)),
        Ok(Applied::Current { version }) => notice.store(NoticeState::Current(version)),
        Ok(Applied::WorkshopOwned) => notice.store(NoticeState::WorkshopOwned),
        // Another update holds the claim, and the notice shows it busy;
        // the last result stays for when it ends.
        Ok(Applied::Busy) => {}
        Err(error) => {
            tracing::warn!("the update failed: {}", error_chain(&error));
            notice.store(NoticeState::Failed(error.to_string()));
        }
    }
}

/// The manifest check for the running process.
async fn check_release() -> Result<Check, UpdateError> {
    let environment = Environment::current();
    let key = manifest::platform_key(environment.os, environment.arch).ok_or(
        UpdateError::NoPlatform {
            os: environment.os,
            arch: environment.arch,
        },
    )?;
    let client = gateway_protocol::http_util::bounded_client();
    manifest::check(
        &client,
        &environment.manifest_url,
        key,
        &environment.version,
    )
    .await
}

/// The Workshop installed beside this gateway, when there is one.
fn workshop_beside() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    gateway_api_discovery::installed_workshop(&exe)
}
