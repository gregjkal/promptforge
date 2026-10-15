//! The tray's update notice worker, on paused `tokio::time` with an
//! injected check, update flow, and Workshop lookup.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gateway_config::Config;

use crate::runner::ServeOptions;
use crate::shutdown::ShutdownSignal;
use crate::update::manifest::{Check, Entry};
use crate::update::notice::{CHECK_INTERVAL, Notice, NoticeState, Sources, run, start};
use crate::update::{Applied, UpdateError};

/// Counts calls and answers each from a queue, repeating the last answer
/// once the queue holds one.
#[derive(Clone)]
struct Scripted<T> {
    calls: Arc<AtomicUsize>,
    answers: Arc<Mutex<VecDeque<T>>>,
}

impl<T: Clone> Scripted<T> {
    fn new(answers: impl IntoIterator<Item = T>) -> Scripted<T> {
        Scripted {
            calls: Arc::new(AtomicUsize::new(0)),
            answers: Arc::new(Mutex::new(answers.into_iter().collect())),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn next(&self) -> T {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut answers = self.answers.lock().expect("answers");
        if answers.len() > 1 {
            answers.pop_front().expect("an answer")
        } else {
            answers.front().cloned().expect("an answer")
        }
    }
}

/// A check answer, rebuilt per call since `UpdateError` is not `Clone`.
#[derive(Clone)]
enum CheckAnswer {
    Current(&'static str),
    Available(&'static str),
    Unreachable,
}

impl CheckAnswer {
    fn result(self) -> Result<Check, UpdateError> {
        match self {
            CheckAnswer::Current(version) => Ok(Check::Current {
                version: version.to_owned(),
            }),
            CheckAnswer::Available(version) => Ok(Check::Available {
                version: version.to_owned(),
                entry: Entry {
                    url: "http://127.0.0.1:1/gateway.tar.gz".to_owned(),
                    signature: String::new(),
                },
            }),
            CheckAnswer::Unreachable => Err(UpdateError::Manifest {
                url: "http://127.0.0.1:1/latest.json".to_owned(),
                reason: "the server answered 503".to_owned(),
            }),
        }
    }
}

/// The worker's injected parts.
struct Fixture {
    notice: Notice,
    shutdown: ShutdownSignal,
    workshop: Scripted<Option<PathBuf>>,
    check: Scripted<CheckAnswer>,
    apply: Scripted<Applied>,
}

impl Fixture {
    fn new(
        workshop: Option<PathBuf>,
        checks: impl IntoIterator<Item = CheckAnswer>,
        applied: Applied,
    ) -> Fixture {
        let shutdown = ShutdownSignal::default();
        Fixture {
            notice: Notice::new(shutdown.clone()),
            shutdown,
            workshop: Scripted::new([workshop]),
            check: Scripted::new(checks),
            apply: Scripted::new([applied]),
        }
    }

    /// Starts the worker for `os` on the test's runtime.
    fn start(&self, os: &'static str) -> tokio::task::JoinHandle<()> {
        let requests = self.notice.take_requests().expect("the request channel");
        let (workshop, check, apply) = (
            self.workshop.clone(),
            self.check.clone(),
            self.apply.clone(),
        );
        let sources = Sources {
            os,
            workshop: move || workshop.next(),
            check: move || {
                let answer = check.next();
                async move { answer.result() }
            },
            apply: move || {
                let applied = apply.next();
                async move { Ok(applied) }
            },
        };
        tokio::spawn(run(self.notice.clone(), requests, sources))
    }
}

/// Lets the worker run, letting paused time advance by `by`.
async fn settle(by: Duration) {
    tokio::time::sleep(by).await;
}

#[tokio::test(start_paused = true)]
async fn the_worker_checks_at_startup_and_then_once_a_day() {
    let fixture = Fixture::new(None, [CheckAnswer::Current("0.3.0")], Applied::Busy);
    let worker = fixture.start("macos");
    settle(Duration::from_secs(1)).await;
    assert_eq!(fixture.check.calls(), 1, "the startup check ran");
    assert_eq!(
        fixture.notice.state(),
        NoticeState::Current("0.3.0".to_owned())
    );
    settle(CHECK_INTERVAL.saturating_sub(Duration::from_secs(2))).await;
    assert_eq!(fixture.check.calls(), 1, "no second check within a day");
    settle(Duration::from_secs(2)).await;
    assert_eq!(fixture.check.calls(), 2, "the next check after a day");
    assert_eq!(
        fixture.workshop.calls(),
        0,
        "only Windows looks for Workshop"
    );
    fixture.shutdown.fire();
    worker.await.expect("the worker ends with the gateway");
}

#[tokio::test(start_paused = true)]
async fn a_newer_release_is_shown_and_nothing_installs_without_a_request() {
    let fixture = Fixture::new(
        None,
        [CheckAnswer::Available("0.4.0")],
        Applied::Updating {
            version: "0.4.0".to_owned(),
        },
    );
    let _worker = fixture.start("linux");
    settle(Duration::from_secs(1)).await;
    assert_eq!(
        fixture.notice.state(),
        NoticeState::Available("0.4.0".to_owned())
    );
    settle(CHECK_INTERVAL * 2).await;
    assert_eq!(
        fixture.apply.calls(),
        0,
        "the tray never applies on its own"
    );
}

#[tokio::test(start_paused = true)]
async fn a_request_runs_the_flow_once_and_stores_its_outcome() {
    let fixture = Fixture::new(
        None,
        [CheckAnswer::Available("0.4.0")],
        Applied::Current {
            version: "0.4.0".to_owned(),
        },
    );
    let _worker = fixture.start("macos");
    settle(Duration::from_secs(1)).await;
    // Two clicks before the worker wakes are one request for one update.
    fixture.notice.request_apply();
    fixture.notice.request_apply();
    settle(Duration::from_secs(1)).await;
    assert_eq!(fixture.apply.calls(), 1, "the flow ran once");
    assert_eq!(
        fixture.notice.state(),
        NoticeState::Current("0.4.0".to_owned())
    );
    assert_eq!(fixture.check.calls(), 1, "a request runs no extra check");
}

#[tokio::test(start_paused = true)]
async fn a_failed_check_is_shown_until_the_next_check_clears_it() {
    let fixture = Fixture::new(
        None,
        [CheckAnswer::Unreachable, CheckAnswer::Current("0.3.0")],
        Applied::Busy,
    );
    let _worker = fixture.start("linux");
    settle(Duration::from_secs(1)).await;
    let NoticeState::Failed(reason) = fixture.notice.state() else {
        panic!("a failed check is shown: {:?}", fixture.notice.state());
    };
    assert!(reason.contains("latest.json"), "{reason}");
    settle(CHECK_INTERVAL).await;
    assert_eq!(
        fixture.notice.state(),
        NoticeState::Current("0.3.0".to_owned()),
        "the next check replaces the failure"
    );
}

#[tokio::test(start_paused = true)]
async fn a_windows_gateway_beside_workshop_never_checks() {
    let fixture = Fixture::new(
        Some(PathBuf::from(
            r"C:\Program Files\PromptForge\promptforge-workshop.exe",
        )),
        [CheckAnswer::Available("0.4.0")],
        Applied::Busy,
    );
    let _worker = fixture.start("windows");
    settle(Duration::from_secs(1)).await;
    assert_eq!(fixture.notice.state(), NoticeState::WorkshopOwned);
    assert_eq!(fixture.check.calls(), 0, "Workshop's updater owns updates");
    assert_eq!(fixture.workshop.calls(), 1);
    settle(CHECK_INTERVAL).await;
    assert_eq!(
        fixture.workshop.calls(),
        2,
        "the peer is looked up again at every check"
    );
}

#[tokio::test(start_paused = true)]
async fn a_windows_gateway_without_workshop_checks() {
    let fixture = Fixture::new(None, [CheckAnswer::Available("0.4.0")], Applied::Busy);
    let _worker = fixture.start("windows");
    settle(Duration::from_secs(1)).await;
    assert_eq!(fixture.workshop.calls(), 1);
    assert_eq!(
        fixture.notice.state(),
        NoticeState::Available("0.4.0".to_owned())
    );
}

#[test]
fn an_update_holding_the_claim_shows_busy_unless_the_last_result_failed() {
    let shutdown = ShutdownSignal::default();
    let notice = Notice::new(shutdown.clone());
    assert_eq!(notice.state(), NoticeState::Unchecked);
    let ticket = shutdown.begin_update().expect("claim");
    assert_eq!(
        notice.state(),
        NoticeState::Busy,
        "an update the route started shows"
    );
    drop(ticket);
    assert_eq!(notice.state(), NoticeState::Unchecked);

    let failed = NoticeState::Failed("the previous gateway waits at x.previous".to_owned());
    notice.store(failed.clone());
    shutdown.begin_update().expect("claim").keep();
    assert_eq!(
        notice.state(),
        failed,
        "a stranded install keeps the claim and still shows its error"
    );
}

#[test]
fn serve_options_without_the_notice_start_no_worker() {
    let config = Config::from_toml_str(
        "config-version = 0\n[server]\nbind = \"127.0.0.1:0\"\napi_key = \"k\"\n",
    )
    .expect("config parses");
    // The runtime is never driven, so a started worker never fetches.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a runtime");
    let headless = crate::test_support::app_state(config.clone(), None);
    let options = ServeOptions::new(None, None::<crate::ProfileName>);
    start(runtime.handle(), &options, &headless);
    assert!(
        headless.update_notice.take_requests().is_some(),
        "no worker took the request channel"
    );

    let tray = crate::test_support::app_state(config, None);
    let mut options = options;
    options.update_notice = true;
    start(runtime.handle(), &options, &tray);
    assert!(
        tray.update_notice.take_requests().is_none(),
        "a tray run's worker holds the request channel"
    );
}
