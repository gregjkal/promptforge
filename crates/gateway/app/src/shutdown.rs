//! The process-shutdown signal every part of the gateway watches.
//!
//! It is shared state: the serve loop selects on it beside the
//! caller-owned shutdown future, every open-ended response stream ends
//! when it fires so the graceful drain has nothing left to wait for, the
//! tray's status tick peeks at it, and `POST /shutdown` is only one of the
//! things that fires it. A self-update fires it too, after leaving its
//! [`Succession`] in the signal's slot for `main` to run once serving has
//! ended.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use tokio_util::sync::CancellationToken;

use crate::update::Succession;

/// The process-shutdown signal shared by the `POST /shutdown` route, the
/// serve loop (which selects on it alongside the caller-owned shutdown
/// future), and every open-ended response stream, which ends when it fires
/// so the graceful drain has nothing left to wait for.
///
/// Every clone shares the one underlying signal. It is a cancellation
/// token, not a notify: a `fire` wakes every waiter at once and stays
/// fired, so a stream that subscribes after the signal ends immediately.
#[derive(Debug, Clone, Default)]
pub(crate) struct ShutdownSignal {
    token: CancellationToken,
    succession: Arc<SuccessionSlot>,
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
/// [`UpdateTicket::keep`] holds the claim for the process's remaining life
/// once a succession is requested.
#[derive(Debug)]
pub(crate) struct UpdateTicket {
    slot: Arc<SuccessionSlot>,
    kept: bool,
}

impl UpdateTicket {
    /// Keeps the update claimed: this process is handing off, and no
    /// second update may start.
    pub(crate) fn keep(mut self) {
        self.kept = true;
    }
}

impl Drop for UpdateTicket {
    fn drop(&mut self) {
        if !self.kept {
            self.slot.updating.store(false, Ordering::Release);
        }
    }
}

impl ShutdownSignal {
    /// Fires the signal, starting the serve loop's graceful shutdown.
    pub(crate) fn fire(&self) {
        self.token.cancel();
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
    pub(crate) fn begin_update(&self) -> Option<UpdateTicket> {
        self.succession
            .updating
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| UpdateTicket {
                slot: Arc::clone(&self.succession),
                kept: false,
            })
    }

    /// Leaves `plan` for the end of serving, then fires the signal.
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
        self.fire();
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The tray's status tick reads `is_fired` synchronously to tell a
    /// requested shutdown apart from a serve-loop failure, and the update
    /// reads it before it swaps or starts the setup.
    #[test]
    fn fire_sets_the_synchronous_peek() {
        let signal = ShutdownSignal::default();
        assert!(!signal.is_fired(), "a fresh signal reads unfired");
        signal.fire();
        assert!(signal.is_fired(), "fire sets the peek before any wait");
    }

    #[test]
    fn one_update_runs_at_a_time_until_its_ticket_drops() {
        let signal = ShutdownSignal::default();
        let ticket = signal.begin_update().expect("the first update claims");
        assert!(
            signal.clone().begin_update().is_none(),
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
}
