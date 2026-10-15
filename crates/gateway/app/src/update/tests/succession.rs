use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use gateway_api_discovery::{GatewayDiscoveryFile, GatewayInstanceLease};

use crate::ProfileName;
use crate::update::stage::Install;
use crate::update::succession::{Spawned, Timeouts, successor_args, watchdog_with};
use crate::update::swap::previous_path;
use crate::update::{Succession, SuccessionError};

const SHORT: Timeouts = Timeouts {
    successor: Duration::from_millis(300),
    restored: Duration::from_millis(300),
    poll: Duration::from_millis(10),
};

/// A swapped install in a temporary directory: the new item installed, the
/// old one moved aside, the staging directory present, and this process's
/// marked discovery record in the run directory.
struct Fixture {
    _dir: tempfile::TempDir,
    run_dir: PathBuf,
    installed: PathBuf,
    staging: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let run_dir = dir.path().join("run");
        let installed = dir.path().join("promptforge-gateway");
        let staging = dir.path().join(crate::update::stage::STAGING_DIR_NAME);
        std::fs::write(&installed, "new").expect("install the new item");
        std::fs::write(previous_path(&installed), "old").expect("move the old item aside");
        std::fs::create_dir(&staging).expect("staging");
        record("0.3.1", true)
            .write_to(&run_dir)
            .expect("the marked record");
        Fixture {
            _dir: dir,
            run_dir,
            installed,
            staging,
        }
    }

    fn succession(&self) -> Succession {
        Succession::new(
            Some(self.run_dir.clone()),
            &Install::locate(&self.installed),
            self.staging.clone(),
            "0.4.0".to_owned(),
            Some(PathBuf::from("/profile/gateway.toml")),
        )
    }

    fn lease(&self) -> GatewayInstanceLease {
        GatewayInstanceLease::try_acquire(&self.run_dir)
            .expect("lease")
            .expect("free")
    }

    fn marked(&self) -> bool {
        GatewayDiscoveryFile::read(&self.run_dir)
            .expect("read")
            .is_some_and(|file| file.restarting)
    }

    fn installed(&self) -> String {
        std::fs::read_to_string(&self.installed).expect("read the installed item")
    }

    fn publish(&self, version: &str) {
        record(version, false)
            .write_to(&self.run_dir)
            .expect("publish");
    }

    /// Whether an unmarked record at `version` (any when `None`) is on
    /// disk: the publication check without the live-gateway proof.
    fn published(&self, version: Option<&str>) -> bool {
        GatewayDiscoveryFile::read(&self.run_dir)
            .ok()
            .flatten()
            .is_some_and(|file| {
                !file.restarting && version.is_none_or(|version| file.version == version)
            })
    }
}

fn record(version: &str, restarting: bool) -> GatewayDiscoveryFile {
    GatewayDiscoveryFile {
        port: 1,
        api_key: "key".to_owned(),
        pid: std::process::id(),
        epoch: 1_757_000_000,
        version: version.to_owned(),
        started_at: "2026-10-13T00:00:00Z".to_owned(),
        restarting,
    }
}

/// A started gateway that has or has not exited, recording a kill.
struct Fake {
    exited: bool,
    killed: Rc<Cell<bool>>,
}

impl Spawned for Fake {
    fn exited(&mut self) -> bool {
        self.exited
    }

    fn kill(&mut self) {
        self.killed.set(true);
    }
}

/// What one spawn saw.
#[derive(Debug)]
struct Seen {
    arguments: Vec<OsString>,
    marked: bool,
    lease_free: bool,
}

fn seen(fixture: &Fixture, arguments: &[OsString]) -> Seen {
    Seen {
        arguments: arguments.to_vec(),
        marked: fixture.marked(),
        lease_free: GatewayInstanceLease::try_acquire(&fixture.run_dir)
            .expect("lease")
            .is_some(),
    }
}

#[test]
fn a_successor_that_publishes_takes_over_and_the_old_items_go() {
    let fixture = Fixture::new();
    let spawns = RefCell::new(Vec::new());
    let result = fixture.succession().run_with(
        fixture.lease(),
        &successor_args(None, None, true),
        |exe: &Path, arguments: &[OsString]| {
            assert_eq!(exe, fixture.installed);
            spawns.borrow_mut().push(seen(&fixture, arguments));
            fixture.publish("0.4.0");
            Ok(Fake {
                exited: false,
                killed: Rc::default(),
            })
        },
        |version, _| fixture.published(version),
        SHORT,
    );
    assert_eq!(result.expect("the successor serves"), "0.4.0");
    let spawns = spawns.into_inner();
    assert_eq!(spawns.len(), 1);
    assert!(
        spawns[0].marked,
        "the mark holds until the successor publishes"
    );
    assert!(
        spawns[0].lease_free,
        "the successor takes the lease while the old process still runs"
    );
    assert!(spawns[0].arguments.contains(&OsString::from("--login")));
    assert!(!previous_path(&fixture.installed).exists());
    assert!(!fixture.staging.exists());
    assert_eq!(fixture.installed(), "new");
}

/// Runs a succession whose successor never publishes: `first` is what its
/// spawn returns, and the restored gateway publishes at once.
fn roll_back(first: io::Result<Fake>) -> (Fixture, Vec<Seen>, Result<String, SuccessionError>) {
    let fixture = Fixture::new();
    let spawns = RefCell::new(Vec::new());
    let mut first = Some(first);
    let result = fixture.succession().run_with(
        fixture.lease(),
        &successor_args(None, None, false),
        |_: &Path, arguments: &[OsString]| {
            spawns.borrow_mut().push(seen(&fixture, arguments));
            if let Some(successor) = first.take() {
                return successor;
            }
            assert_eq!(fixture.installed(), "old", "the restore precedes its spawn");
            fixture.publish("0.3.1");
            Ok(Fake {
                exited: false,
                killed: Rc::default(),
            })
        },
        |version, _| fixture.published(version),
        SHORT,
    );
    (fixture, spawns.into_inner(), result)
}

#[test]
fn a_successor_that_never_publishes_is_killed_and_the_previous_gateway_restored() {
    let killed = Rc::new(Cell::new(false));
    let (fixture, spawns, result) = roll_back(Ok(Fake {
        exited: false,
        killed: Rc::clone(&killed),
    }));
    assert!(
        matches!(result, Err(SuccessionError::RolledBack { .. })),
        "{result:?}"
    );
    assert!(killed.get(), "the hung successor is killed");
    assert_eq!(spawns.len(), 2);
    assert!(
        spawns.iter().all(|spawn| spawn.marked),
        "the mark holds through the rollback until the restored copy publishes"
    );
    assert!(
        spawns
            .iter()
            .all(|spawn| spawn.arguments.contains(&OsString::from("--login"))),
        "both spawns open no browser"
    );
    assert_eq!(fixture.installed(), "old");
    assert!(
        !fixture.staging.exists(),
        "the rejected item goes with the staging"
    );
}

#[test]
fn a_successor_that_exits_early_is_rolled_back_without_a_kill() {
    let killed = Rc::new(Cell::new(false));
    let (fixture, _spawns, result) = roll_back(Ok(Fake {
        exited: true,
        killed: Rc::clone(&killed),
    }));
    assert!(
        matches!(result, Err(SuccessionError::RolledBack { .. })),
        "{result:?}"
    );
    assert!(!killed.get(), "an exited successor needs no kill");
    assert_eq!(fixture.installed(), "old");
}

#[test]
fn a_successor_that_does_not_start_is_rolled_back() {
    let (fixture, spawns, result) = roll_back(Err(io::Error::other("injected")));
    let error = result.expect_err("a rollback");
    assert!(error.to_string().contains("did not start"), "{error}");
    assert_eq!(spawns.len(), 2);
    assert_eq!(fixture.installed(), "old");
}

#[test]
fn another_new_version_gateway_counts_as_the_successor() {
    let fixture = Fixture::new();
    let killed = Rc::new(Cell::new(false));
    let mut spawns = 0;
    let result = fixture.succession().run_with(
        fixture.lease(),
        &successor_args(None, None, false),
        |_: &Path, _: &[OsString]| {
            spawns += 1;
            // An older Workshop's supervisor started the installed path
            // first; the successor lost the lease and exited.
            fixture.publish("0.4.0");
            Ok(Fake {
                exited: true,
                killed: Rc::clone(&killed),
            })
        },
        |version, _| fixture.published(version),
        SHORT,
    );
    assert_eq!(result.expect("the other gateway serves"), "0.4.0");
    assert_eq!(spawns, 1, "no rollback over a healthy new-version gateway");
    assert_eq!(fixture.installed(), "new");
}

#[test]
fn a_failed_rollback_withdraws_the_mark() {
    let fixture = Fixture::new();
    let result = fixture.succession().run_with(
        fixture.lease(),
        &successor_args(None, None, false),
        |_: &Path, _: &[OsString]| {
            Ok(Fake {
                exited: true,
                killed: Rc::default(),
            })
        },
        |version, _| fixture.published(version),
        SHORT,
    );
    assert!(
        matches!(result, Err(SuccessionError::Failed { .. })),
        "{result:?}"
    );
    assert!(
        GatewayDiscoveryFile::read(&fixture.run_dir)
            .expect("read")
            .is_none(),
        "readers stop waiting on a succession that ended"
    );
}

#[test]
fn the_successor_repeats_the_old_process_shape_and_always_logs_in() {
    let profile = ProfileName::parse("work").expect("a profile name");
    let config = Path::new("/profile/gateway.toml");
    let os = |args: &[&str]| args.iter().map(OsString::from).collect::<Vec<_>>();
    assert_eq!(successor_args(None, None, false), os(&["--login"]));
    assert_eq!(
        successor_args(Some(config), Some(&profile), true),
        os(&[
            "--config",
            "/profile/gateway.toml",
            "--profile",
            "work",
            "--no-tray",
            "--login"
        ])
    );
    assert_eq!(
        successor_args(Some(config), None, false),
        os(&["--config", "/profile/gateway.toml", "--login"])
    );
}

#[test]
fn the_watchdog_withdraws_the_mark_and_ends_the_process() {
    let fixture = Fixture::new();
    let (expired_tx, expired_rx) = mpsc::channel();
    let started = Instant::now();
    let watchdog = watchdog_with(
        fixture.run_dir.clone(),
        std::process::id(),
        Duration::from_millis(50),
        move || {
            let _ = expired_tx.send(());
        },
    )
    .expect("the watchdog starts");
    expired_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the watchdog expires");
    watchdog.join().expect("the watchdog thread ends");
    assert!(started.elapsed() >= Duration::from_millis(50));
    assert!(
        GatewayDiscoveryFile::read(&fixture.run_dir)
            .expect("read")
            .is_none(),
        "the mark is withdrawn before the process ends"
    );
}

#[test]
fn a_requested_succession_outlives_a_serve_error() {
    let fixture = Fixture::new();
    let failed = || {
        Err(crate::api_error::StartupError::serve(
            crate::api_error::ServeError::io(io::Error::other("the drain failed")),
        ))
    };
    assert!(matches!(
        crate::update::stopped(failed(), Some(fixture.succession()), None),
        Ok(crate::Stopped::Succession(_))
    ));
    assert!(matches!(
        crate::update::stopped(Ok(()), Some(fixture.succession()), None),
        Ok(crate::Stopped::Succession(_))
    ));
    assert!(crate::update::stopped(failed(), None, None).is_err());
    assert!(matches!(
        crate::update::stopped(Ok(()), None, None),
        Ok(crate::Stopped::Quit)
    ));
}

#[test]
fn a_pending_succession_wins_over_a_restart_and_a_restart_over_a_quit() {
    use crate::ShutdownReason::{Restart, Stop};
    let fixture = Fixture::new();
    assert!(matches!(
        crate::update::stopped(Ok(()), Some(fixture.succession()), Some(Restart)),
        Ok(crate::Stopped::Succession(_))
    ));
    assert!(matches!(
        crate::update::stopped(Ok(()), None, Some(Restart)),
        Ok(crate::Stopped::Restart)
    ));
    assert!(matches!(
        crate::update::stopped(Ok(()), None, Some(Stop)),
        Ok(crate::Stopped::Quit)
    ));
}
