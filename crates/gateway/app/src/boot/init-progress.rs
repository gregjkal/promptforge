//! Progress lines for `init`: the artifact store's activity text, thinned
//! for an installer's details pane and CI logs.

use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gateway_api_types::Progress;
use tokio::sync::watch;

/// The shortest gap between two percent lines of one phase.
const MIN_INTERVAL: Duration = Duration::from_secs(1);

/// The smallest percent advance a new percent line reports.
const MIN_STEP: u64 = 5;

/// How often the printer thread samples the activity text.
const POLL: Duration = Duration::from_millis(100);

/// Decides which activity texts print. A phase is the text without its
/// trailing ` NN%`: a new phase prints a line ending the previous one and
/// a line starting itself; within a phase a percent line prints only when
/// a second has passed and the percent moved at least five points.
#[derive(Debug, Default)]
pub(super) struct ProgressLines {
    phase: Option<String>,
    percent: u64,
    printed: Option<Instant>,
}

impl ProgressLines {
    /// The lines `text`, observed at `now`, prints.
    pub(super) fn offer(&mut self, text: &str, now: Instant) -> Vec<String> {
        let text = text.trim();
        if text.is_empty() {
            return Vec::new();
        }
        let (label, percent) = split_percent(text);
        if self.phase.as_deref() != Some(label) {
            let mut lines: Vec<String> = self.finish().into_iter().collect();
            self.phase = Some(label.to_owned());
            self.percent = percent.unwrap_or(0);
            self.printed = Some(now);
            lines.push(text.to_owned());
            return lines;
        }
        let Some(percent) = percent else {
            return Vec::new();
        };
        let due = self
            .printed
            .is_none_or(|printed| now.saturating_duration_since(printed) >= MIN_INTERVAL);
        if due && percent >= self.percent.saturating_add(MIN_STEP) {
            self.percent = percent;
            self.printed = Some(now);
            return vec![text.to_owned()];
        }
        Vec::new()
    }

    /// The line ending the current phase, if one is open.
    pub(super) fn finish(&mut self) -> Option<String> {
        self.printed = None;
        self.percent = 0;
        self.phase.take().map(|label| format!("{label}: done"))
    }
}

/// Splits a trailing ` NN%` off `text`.
fn split_percent(text: &str) -> (&str, Option<u64>) {
    if let Some((label, last)) = text.rsplit_once(' ')
        && let Some(digits) = last.strip_suffix('%')
        && let Ok(percent) = digits.parse::<u64>()
    {
        return (label, Some(percent));
    }
    (text, None)
}

/// A thread printing [`ProgressLines`] from a progress subscription to
/// stdout until [`Printer::finish`].
pub(super) struct Printer {
    stop: mpsc::Sender<bool>,
    thread: JoinHandle<()>,
}

impl Printer {
    /// Starts the thread; `None` when it cannot be spawned, so provisioning
    /// proceeds without progress lines.
    pub(super) fn spawn(mut progress: watch::Receiver<Progress>) -> Option<Printer> {
        let (stop, stopped) = mpsc::channel::<bool>();
        let thread = std::thread::Builder::new()
            .name("init-progress".to_owned())
            .spawn(move || {
                let mut lines = ProgressLines::default();
                let opening = progress.borrow_and_update().text.clone();
                for line in lines.offer(&opening, Instant::now()) {
                    println!("{line}");
                }
                loop {
                    let stopping = match stopped.recv_timeout(POLL) {
                        Ok(succeeded) => Some(succeeded),
                        Err(mpsc::RecvTimeoutError::Timeout) => None,
                        Err(mpsc::RecvTimeoutError::Disconnected) => Some(false),
                    };
                    if progress.has_changed().unwrap_or(false) {
                        let text = progress.borrow_and_update().text.clone();
                        for line in lines.offer(&text, Instant::now()) {
                            println!("{line}");
                        }
                    }
                    if let Some(succeeded) = stopping {
                        // A failed phase ends with the caller's error, not
                        // a done line.
                        if succeeded && let Some(line) = lines.finish() {
                            println!("{line}");
                        }
                        return;
                    }
                }
            })
            .ok()?;
        Some(Printer { stop, thread })
    }

    /// Prints the last phase's end line when the work `succeeded`, then
    /// joins the thread.
    pub(super) fn finish(self, succeeded: bool) {
        let _ = self.stop.send(succeeded);
        let _ = self.thread.join();
    }
}
