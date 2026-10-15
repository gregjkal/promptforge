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
use crate::update::notice::{
    CHECK_INTERVAL, Notice, NoticeState, RETRY_INTERVAL, Sources, run, start,
};
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

/// An update flow answer, rebuilt per call like [`CheckAnswer`].
#[derive(Clone)]
enum ApplyAnswer {
    Applied(Applied),
    /// The flow answers after a minute of paused time.
    Slow(Applied),
    Fails,
    /// The flow fails after a minute of paused time.
    SlowFails,
    Strands,
}

impl From<Applied> for ApplyAnswer {
    fn from(applied: Applied) -> ApplyAnswer {
        ApplyAnswer::Applied(applied)
    }
}

impl ApplyAnswer {
    async fn result(self) -> Result<Applied, UpdateError> {
        match self {
            ApplyAnswer::Applied(applied) => Ok(applied),
            ApplyAnswer::Slow(applied) => {
                tokio::time::sleep(Duration::from_mins(1)).await;
                Ok(applied)
            }
            ApplyAnswer::SlowFails => {
                tokio::time::sleep(Duration::from_mins(1)).await;
                Box::pin(ApplyAnswer::Fails.result()).await
            }
            ApplyAnswer::Fails => Err(UpdateError::Download {
                url: "http://127.0.0.1:1/gateway.tar.gz".to_owned(),
                reason: "the server answered 503".to_owned(),
            }),
            ApplyAnswer::Strands => Err(UpdateError::Stranded {
                installed: PathBuf::from("/opt/promptforge/promptforge-gateway"),
                previous: PathBuf::from("/opt/promptforge/promptforge-gateway.previous"),
                reason: "permission denied".to_owned(),
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
    apply: Scripted<ApplyAnswer>,
}

impl Fixture {
    fn new(
        workshop: Option<PathBuf>,
        checks: impl IntoIterator<Item = CheckAnswer>,
        applied: impl Into<ApplyAnswer>,
    ) -> Fixture {
        let shutdown = ShutdownSignal::default();
        Fixture {
            notice: Notice::new(shutdown.clone()),
            shutdown,
            workshop: Scripted::new([workshop]),
            check: Scripted::new(checks),
            apply: Scripted::new([applied.into()]),
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
            apply: move || apply.next().result(),
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
async fn a_failed_check_is_shown_until_the_retry_within_the_hour_clears_it() {
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
    settle(RETRY_INTERVAL.saturating_sub(Duration::from_secs(2))).await;
    assert_eq!(fixture.check.calls(), 1, "no retry before the hour");
    settle(Duration::from_secs(2)).await;
    assert_eq!(fixture.check.calls(), 2, "the failed check is retried");
    assert_eq!(
        fixture.notice.state(),
        NoticeState::Current("0.3.0".to_owned()),
        "the retry replaces the failure"
    );
    settle(CHECK_INTERVAL.saturating_sub(Duration::from_secs(2))).await;
    assert_eq!(
        fixture.check.calls(),
        2,
        "a passing check returns to the daily schedule"
    );
}

#[tokio::test(start_paused = true)]
async fn a_check_due_during_an_update_never_replaces_its_failure() {
    let fixture = Fixture::new(
        None,
        [CheckAnswer::Unreachable, CheckAnswer::Available("0.4.0")],
        ApplyAnswer::SlowFails,
    );
    let _worker = fixture.start("linux");
    settle(Duration::from_secs(1)).await;
    // The click lands half a minute before the retry, and the update runs
    // a minute, past it.
    settle(RETRY_INTERVAL.saturating_sub(Duration::from_secs(30))).await;
    fixture.notice.request_apply();
    settle(Duration::from_secs(61)).await;
    let NoticeState::Failed(reason) = fixture.notice.state() else {
        panic!(
            "the update's failure is shown: {:?}",
            fixture.notice.state()
        );
    };
    assert!(reason.contains("gateway.tar.gz"), "{reason}");
    assert_eq!(fixture.check.calls(), 1, "the due retry waits");
    settle(RETRY_INTERVAL).await;
    assert_eq!(
        fixture.check.calls(),
        2,
        "the retry comes an hour after the failed update"
    );
}

#[tokio::test(start_paused = true)]
async fn the_notice_is_busy_while_a_requested_update_runs() {
    let fixture = Fixture::new(
        None,
        [CheckAnswer::Available("0.4.0")],
        ApplyAnswer::Slow(Applied::Current {
            version: "0.4.0".to_owned(),
        }),
    );
    let _worker = fixture.start("macos");
    settle(Duration::from_secs(1)).await;
    fixture.notice.request_apply();
    settle(Duration::from_secs(1)).await;
    assert_eq!(fixture.notice.state(), NoticeState::Busy);
    settle(Duration::from_mins(1)).await;
    assert_eq!(
        fixture.notice.state(),
        NoticeState::Current("0.4.0".to_owned())
    );
}

#[tokio::test(start_paused = true)]
async fn a_started_update_shows_its_release_until_its_claim_shows_busy() {
    let fixture = Fixture::new(
        None,
        [CheckAnswer::Current("0.3.0")],
        Applied::Updating {
            version: "0.4.0".to_owned(),
        },
    );
    let _worker = fixture.start("linux");
    settle(Duration::from_secs(1)).await;
    fixture.notice.request_apply();
    settle(Duration::from_secs(1)).await;
    assert_eq!(
        fixture.notice.state(),
        NoticeState::Available("0.4.0".to_owned()),
        "a setup that ends without installing leaves the release clickable"
    );
    // The flow keeps its claim until the process hands off.
    fixture.shutdown.begin_update().expect("claim").keep();
    assert_eq!(fixture.notice.state(), NoticeState::Busy);
}

#[tokio::test(start_paused = true)]
async fn a_busy_answer_leaves_the_last_result() {
    let fixture = Fixture::new(None, [CheckAnswer::Current("0.3.0")], Applied::Busy);
    let _worker = fixture.start("linux");
    settle(Duration::from_secs(1)).await;
    fixture.notice.request_apply();
    settle(Duration::from_secs(1)).await;
    assert_eq!(fixture.apply.calls(), 1);
    assert_eq!(
        fixture.notice.state(),
        NoticeState::Current("0.3.0".to_owned())
    );
}

#[tokio::test(start_paused = true)]
async fn a_failed_update_is_shown_and_a_later_route_update_shows_busy() {
    let fixture = Fixture::new(None, [CheckAnswer::Available("0.4.0")], ApplyAnswer::Fails);
    let _worker = fixture.start("linux");
    settle(Duration::from_secs(1)).await;
    fixture.notice.request_apply();
    settle(Duration::from_secs(1)).await;
    let NoticeState::Failed(reason) = fixture.notice.state() else {
        panic!("a failed update is shown: {:?}", fixture.notice.state());
    };
    assert!(reason.contains("gateway.tar.gz"), "{reason}");
    let route = fixture.shutdown.begin_update().expect("claim");
    assert_eq!(
        fixture.notice.state(),
        NoticeState::Busy,
        "an update the route starts after a failure shows"
    );
    drop(route);
}

#[tokio::test(start_paused = true)]
async fn a_stranded_install_outranks_its_claim_and_every_later_check() {
    let fixture = Fixture::new(
        None,
        [CheckAnswer::Available("0.4.0")],
        ApplyAnswer::Strands,
    );
    let _worker = fixture.start("linux");
    settle(Duration::from_secs(1)).await;
    // The stranded flow keeps its claim for the process's life.
    fixture.shutdown.begin_update().expect("claim").keep();
    fixture.notice.request_apply();
    settle(Duration::from_secs(1)).await;
    let NoticeState::Stranded(reason) = fixture.notice.state() else {
        panic!("a stranded install is shown: {:?}", fixture.notice.state());
    };
    assert!(reason.contains("move it to"), "{reason}");
    settle(CHECK_INTERVAL * 2).await;
    assert_eq!(fixture.check.calls(), 1, "no check runs after a strand");
    assert!(
        matches!(fixture.notice.state(), NoticeState::Stranded(_)),
        "the strand stays shown"
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
fn an_update_holding_the_claim_shows_busy_over_the_last_result() {
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

    let failed = NoticeState::Failed("cannot fetch the release manifest".to_owned());
    notice.store(failed.clone());
    let ticket = shutdown.begin_update().expect("claim");
    assert_eq!(
        notice.state(),
        NoticeState::Busy,
        "a failed check does not hide an update the route started"
    );
    drop(ticket);
    assert_eq!(notice.state(), failed);
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
