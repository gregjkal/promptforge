//! The process-shutdown signal every part of the gateway watches.
//!
//! It is shared state: the serve loop selects on it beside the
//! caller-owned shutdown future, every open-ended response stream ends
//! when it fires so the graceful drain has nothing left to wait for, the
//! tray's status tick peeks at it, and `POST /shutdown` is only one of the
//! things that fires it. A self-update fires it too, after leaving its
//! [`Succession`] in the signal's slot for `main` to run once serving has
//! ended.

use std::{
    io::Read,
    path::PathBuf,
    process::ExitCode,
    sync::{
        Arc, Mutex, OnceLock, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
};

use tokio_util::sync::CancellationToken;

use crate::update::Succession;

/// The exit status a gateway uses to ask its supervisor for a restart.
/// Private to the supervisor and its child; distinct from 0 and 1 and
/// below the 128+N range shells use for signal deaths.
pub const RESTART_REQUESTED: u8 = 75;

/// Why the gateway is shutting down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownReason {
    /// Stop serving and exit.
    Stop,
    /// Stop serving and exit, requesting a restart from the supervisor.
    Restart,
}

impl ShutdownReason {
    /// Exit code associated with each exit state.
    ///
    /// A `Stop` is [ExitCode::SUCCESS]
    /// A `Restart` is [RESTART_REQUESTED]
    #[must_use]
    pub fn exit_code(&self) -> ExitCode {
        match self {
            Self::Stop => ExitCode::SUCCESS,
            Self::Restart => ExitCode::from(RESTART_REQUESTED),
        }
    }
}

/// The process-shutdown signal shared by the `POST /shutdown` route, the
/// serve loop (which selects on it alongside the caller-owned shutdown
/// future), and every open-ended response stream, which ends when it fires
/// so the graceful drain has nothing left to wait for.
///
/// Every holder shares the one signal through an `Arc`. It is a
/// cancellation token, not a notify: a `fire` wakes every waiter at once
/// and stays fired, so a stream that subscribes after the signal ends
/// immediately.
#[derive(Debug, Default)]
pub(crate) struct ShutdownSignal {
    token: CancellationToken,
    reason: OnceLock<ShutdownReason>,
    succession: SuccessionSlot,
}

/// What a self-update leaves for the end of serving.
#[derive(Debug, Default)]
struct SuccessionSlot {
    /// The requested succession; the first request wins.
    plan: Mutex<Option<Succession>>,
    /// Whether an update is running, so a second one is refused.
    updating: AtomicBool,
    /// The run directory holding this process's discovery record.
    record: OnceLock<PathBuf>,
}

/// The right to run the one update at a time. Dropping it ends the update;
/// [`UpdateTicket::keep`] holds the claim for the process's remaining life,
/// once a succession is requested or an install is stranded.
#[derive(Debug)]
pub(crate) struct UpdateTicket {
    signal: Arc<ShutdownSignal>,
    kept: bool,
}

impl UpdateTicket {
    /// Keeps the update claimed: no second update may start in this
    /// process.
    pub(crate) fn keep(mut self) {
        self.kept = true;
    }
}

impl Drop for UpdateTicket {
    fn drop(&mut self) {
        if !self.kept {
            self.signal
                .succession
                .updating
                .store(false, Ordering::Release);
        }
    }
}

impl ShutdownSignal {
    /// Fires the signal, starting the serve loop's graceful shutdown.
    /// If fire is called more then once, only the first call's `reason` is kept,
    /// so the serve loop's own re-fire never turns a restart into a stop or vice versa.
    pub(crate) fn fire(&self, reason: ShutdownReason) {
        let _ = self.reason.set(reason);
        self.token.cancel();
    }

    /// Why the signal fired, or `None` if it has not.
    pub(crate) fn reason(&self) -> Option<ShutdownReason> {
        self.reason.get().copied()
    }

    /// Whether the signal has been fired; the tray's status tick reads it
    /// to tell a requested shutdown apart from a serve-loop failure, and
    /// the update reads it before it swaps or starts the setup.
    pub(crate) fn is_fired(&self) -> bool {
        self.token.is_cancelled()
    }

    /// Resolves once the signal has fired.
    pub(crate) async fn fired(&self) {
        self.token.cancelled().await;
    }

    /// Claims the one running update, or `None` when one already runs.
    pub(crate) fn begin_update(self: &Arc<Self>) -> Option<UpdateTicket> {
        self.succession
            .updating
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| UpdateTicket {
                signal: Arc::clone(self),
                kept: false,
            })
    }

    /// Leaves `plan` for the end of serving, then fires the signal with a
    /// stop: the pending succession, not the reason, tells `main` what
    /// runs next.
    pub(crate) fn request_succession(&self, plan: Succession) {
        let mut slot = self
            .succession
            .plan
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if slot.is_none() {
            *slot = Some(plan);
        }
        drop(slot);
        self.fire(ShutdownReason::Stop);
    }

    /// The requested succession, taken once serving has ended.
    pub(crate) fn take_succession(&self) -> Option<Succession> {
        self.succession
            .plan
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// Records the run directory this process publishes its discovery
    /// record in, which a succession marks.
    pub(crate) fn set_record_dir(&self, run_dir: PathBuf) {
        let _ = self.succession.record.set(run_dir);
    }

    /// The run directory holding this process's discovery record.
    pub(crate) fn record_dir(&self) -> Option<PathBuf> {
        self.succession.record.get().cloned()
    }
}

/// Starts the supervised gateway's watchdog.
///
/// The watchdog covers the case where the supervisor dies without stopping its
/// child, for example when it's killed. The watchdog is a thread that blocks
/// reading stdin and fires ShutdownReason::Stop when it reads end-of-file or
/// reading errors out. The gateway process then shuts down gracefully,
/// including its llama-server processes.
///
/// The supervisor holds the write end of the supervised Gateway processes
/// stdin pipe. When the supervisor process ends for any reason, the OS closes
/// that end, and the child's next read from stdin returns end-of-file.
pub(crate) fn stdin_watchdog(shutdown_signal: Arc<ShutdownSignal>) {
    let watchdog_thread = std::thread::Builder::new()
        .name("gateway-stdin-watch".to_string())
        .spawn(move || {

            // Reads `input` until it ends, then fires `shutdown_signal` with a stop. A
            // supervised gateway's stdin is a pipe its supervisor holds open, so the
            // end of input means the supervisor is gone and the gateway must not
            // outlive it. A read error ends the watch the same way: the gateway can
            // no longer tell whether its supervisor is alive.
            // 
            // From https://doc.rust-lang.org/std/io/trait.Read.html#errors:
            // An error of the `ErrorKind::Interrupted` kind is non-fatal and the
            // read operation should be retried if there is nothing else to do
            let mut buffer = [0_u8; 256];
            let mut stdin = std::io::stdin();

            loop {
                match stdin.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(error) => {
                        tracing::warn!("watchdog reading stdin failed: {error}; treating the supervisor as gone");
                        break;
                    }
                }
            }

            tracing::info!("the supervisor closed stdin; shutting down");
            shutdown_signal.fire(ShutdownReason::Stop);
        });

    if let Err(e) = watchdog_thread {
        tracing::error!(
            "failed to spawn the stdin watch thread: {e}; the gateway will not stop if its supervisor dies"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tray's status tick reads `is_fired` synchronously to tell a
    /// requested shutdown apart from a serve-loop failure; the method is
    /// gated on the tray backends like its only callers.
    #[test]
    fn fire_sets_the_synchronous_peek() {
        let signal = ShutdownSignal::default();
        assert!(!signal.is_fired(), "a fresh signal reads unfired");
        signal.fire(ShutdownReason::Stop);
        assert!(signal.is_fired(), "fire sets the peek before any wait");
    }

    #[test]
    fn an_unfired_signal_has_no_reason() {
        let signal = ShutdownSignal::default();

        assert_eq!(signal.reason(), None);
    }

    #[test]
    fn one_update_runs_at_a_time_until_its_ticket_drops() {
        let signal = Arc::new(ShutdownSignal::default());
        let ticket = signal.begin_update().expect("the first update claims");
        assert!(
            signal.begin_update().is_none(),
            "a second update is refused while one runs"
        );
        drop(ticket);
        let ticket = signal
            .begin_update()
            .expect("a finished update frees the claim");
        ticket.keep();
        assert!(
            signal.begin_update().is_none(),
            "a kept claim refuses every later update"
        );
    }

    #[test]
    fn the_first_reason_survives_a_later_fire() {
        let signal = ShutdownSignal::default();

        signal.fire(ShutdownReason::Restart);
        signal.fire(ShutdownReason::Stop);

        assert!(signal.is_fired());
        assert_eq!(signal.reason(), Some(ShutdownReason::Restart));
    }
}
