//! Coverage for a gateway handoff marked in the discovery file: boot waits,
//! supervision launches nothing, and a dead marker still recovers.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::time::Duration;

use gateway_api_discovery::{CancellationToken, GatewayDiscoveryFile, Resolution};

use super::{TestIdentity, dead_pid, live_file, probe_own_image, test_identity, workshop_exe};
use crate::gateway::boot::{
    GatewayPlan, RecoveryLaunch, WorkshopInstall, attach_or_launch, plan_gateway,
};
use crate::gateway::identity::GatewayAttachment;
use crate::gateway::supervisor::{
    SUPERVISION_BASE_DELAY, SupervisionProbe, SystemClock, probe_local_gateway, run_supervision,
    wait_for_launched_file_cancellable_with,
};

/// A handoff mark naming the test process, whose gateway stopped serving.
fn marked_file() -> GatewayDiscoveryFile {
    GatewayDiscoveryFile {
        restarting: true,
        ..live_file(1, "k")
    }
}

fn install(exe: &Path) -> WorkshopInstall {
    WorkshopInstall {
        exe: exe.to_path_buf(),
        appimage: None,
    }
}

#[test]
fn boot_awaits_a_live_handoff_instead_of_launching() {
    let run = tempfile::TempDir::new().expect("tempdir");
    marked_file().write_to(run.path()).expect("write the mark");
    let (_dir, exe) = workshop_exe(true);

    assert_eq!(
        plan_gateway(run.path(), &install(&exe), true, probe_own_image),
        GatewayPlan::AwaitRestart,
        "neither a launch nor the configured endpoint while the handoff lasts"
    );
    assert!(gateway_api_discovery::gateway_discovery_file_path(run.path()).exists());
}

#[test]
fn boot_launches_over_a_handoff_whose_gateway_died() {
    let run = tempfile::TempDir::new().expect("tempdir");
    GatewayDiscoveryFile {
        pid: dead_pid(),
        ..marked_file()
    }
    .write_to(run.path())
    .expect("write the mark");
    let (_dir, exe) = workshop_exe(true);

    let plan = plan_gateway(run.path(), &install(&exe), false, probe_own_image);
    assert!(matches!(plan, GatewayPlan::Launch(_)), "{plan:?}");
}

/// Runs boot's loop over a scripted resolution and launch, attaching only
/// `successor`; returns the outcome, the files offered for attachment, and
/// the pause count.
fn boot_through(
    resolutions: impl Fn(usize) -> Resolution,
    launch: impl FnMut(PathBuf) -> anyhow::Result<RecoveryLaunch>,
    successor: &GatewayDiscoveryFile,
) -> (
    anyhow::Result<GatewayAttachment>,
    Vec<GatewayDiscoveryFile>,
    usize,
) {
    let (_dir, exe) = workshop_exe(true);
    let mut resolves = 0;
    let mut attached = Vec::new();
    let mut pauses = 0;
    let outcome = attach_or_launch(
        Path::new("unused"),
        &install(&exe),
        false,
        |_| {
            resolves += 1;
            Ok(resolutions(resolves))
        },
        |file| {
            attached.push(file.clone());
            if file == *successor {
                Ok(GatewayAttachment::Config)
            } else {
                anyhow::bail!("the recorded gateway is handing off to a successor")
            }
        },
        launch,
        || pauses += 1,
    );
    (outcome, attached, pauses)
}

#[test]
fn boot_plans_again_when_the_resolved_gateway_changes_before_attaching() {
    let changed = live_file(54_377, "old");
    let successor = live_file(54_378, "new");

    let (outcome, attached, pauses) = boot_through(
        |resolve| match resolve {
            1 => Resolution::Attach(changed.clone()),
            2 => Resolution::Restarting,
            _ => Resolution::Attach(successor.clone()),
        },
        |_| panic!("a resolvable gateway is never launched over"),
        &successor,
    );

    assert!(matches!(outcome, Ok(GatewayAttachment::Config)));
    assert_eq!(attached, [changed, successor]);
    assert_eq!(pauses, 2, "the failed proof and the handoff each paused");
}

#[test]
fn boot_plans_again_when_the_launch_race_finds_a_gateway_that_changes() {
    let changed = live_file(54_377, "old");
    let successor = live_file(54_378, "new");
    let launched = changed.clone();

    let (outcome, attached, _) = boot_through(
        |resolve| match resolve {
            1 => Resolution::Absent,
            _ => Resolution::Attach(successor.clone()),
        },
        move |_| Ok(RecoveryLaunch::Attached(launched.clone())),
        &successor,
    );

    assert!(matches!(outcome, Ok(GatewayAttachment::Config)));
    assert_eq!(attached, [changed, successor]);
}

#[test]
fn boot_fails_when_a_resolved_gateway_keeps_failing_its_second_proof() {
    let changed = live_file(54_377, "old");

    let (outcome, attached, _) = boot_through(
        |_| Resolution::Attach(changed.clone()),
        |_| panic!("a resolvable gateway is never launched over"),
        &live_file(54_378, "new"),
    );

    assert!(outcome.is_err());
    assert_eq!(attached.len(), 3, "boot gave up after three failed proofs");
}

#[test]
fn the_supervision_probe_reports_a_live_handoff() {
    let run = tempfile::TempDir::new().expect("tempdir");
    marked_file().write_to(run.path()).expect("write the mark");

    let probe = probe_local_gateway(run.path(), &CancellationToken::new(), |dir, _| {
        probe_own_image(dir)
    });
    assert!(matches!(probe, SupervisionProbe::Restarting));
}

#[test]
fn the_supervision_probe_reports_a_dead_handoff_missing() {
    let run = tempfile::TempDir::new().expect("tempdir");
    GatewayDiscoveryFile {
        pid: dead_pid(),
        ..marked_file()
    }
    .write_to(run.path())
    .expect("write the mark");

    let probe = probe_local_gateway(run.path(), &CancellationToken::new(), |dir, _| {
        probe_own_image(dir)
    });
    assert!(
        matches!(probe, SupervisionProbe::Missing),
        "a handoff whose gateway died recovers as today"
    );
    assert!(!gateway_api_discovery::gateway_discovery_file_path(run.path()).exists());
}

#[test]
fn supervision_never_recovers_during_a_handoff_then_attaches_to_the_successor() {
    const HANDOFF_POLLS: usize = 1_000;
    let original = test_identity(live_file(54_375, "old"));
    let successor = TestIdentity {
        file: live_file(54_376, "new"),
        process_boot: 7,
    };
    let polls = Cell::new(0_usize);
    let published = RefCell::new(Vec::new());
    let waits = RefCell::new(Vec::new());

    run_supervision(
        original,
        |current, _| {
            polls.set(polls.get() + 1);
            if !published.borrow().is_empty() {
                SupervisionProbe::Replacement(current.clone())
            } else if polls.get() <= HANDOFF_POLLS {
                SupervisionProbe::Restarting
            } else {
                SupervisionProbe::Replacement(successor.clone())
            }
        },
        |_| -> anyhow::Result<TestIdentity> { panic!("a handoff is never recovered by launching") },
        |identity, _| {
            published.borrow_mut().push(identity.clone());
            Ok(())
        },
        |delay, _| {
            waits.borrow_mut().push(delay);
            !published.borrow().is_empty()
        },
        &CancellationToken::new(),
    );

    assert!(polls.get() > HANDOFF_POLLS);
    assert_eq!(published.borrow().as_slice(), [successor]);
    let waits = waits.into_inner();
    assert_eq!(
        waits[..HANDOFF_POLLS],
        [SUPERVISION_BASE_DELAY; HANDOFF_POLLS],
        "every handoff wait is the base delay, with no backoff"
    );
}

#[test]
fn the_launched_gateway_wait_keeps_waiting_through_a_handoff() {
    let run = tempfile::TempDir::new().expect("tempdir");
    let successor = live_file(1, "k");
    successor.write_to(run.path()).expect("write");
    let resolves = Cell::new(0_usize);

    let attached = wait_for_launched_file_cancellable_with(
        run.path(),
        Duration::from_secs(10),
        &CancellationToken::new(),
        &SystemClock,
        |_, _, _| Ok(()),
        |_, _| {
            resolves.set(resolves.get() + 1);
            if resolves.get() <= 3 {
                Ok(Resolution::Restarting)
            } else {
                Ok(Resolution::Attach(successor.clone()))
            }
        },
        || None,
    )
    .expect("the successor attaches");

    assert_eq!(resolves.get(), 4, "each handoff sighting polled again");
    assert_eq!(attached, successor);
}

#[test]
fn the_launched_gateway_wait_judges_a_marked_file_without_a_health_probe() {
    let run = tempfile::TempDir::new().expect("tempdir");
    marked_file().write_to(run.path()).expect("write the mark");
    let resolves = Cell::new(0_usize);

    let error = wait_for_launched_file_cancellable_with(
        run.path(),
        Duration::from_millis(100),
        &CancellationToken::new(),
        &SystemClock,
        |_, _, _| panic!("a marked gateway may have stopped serving; it is not probed"),
        |_, _| {
            resolves.set(resolves.get() + 1);
            Ok(Resolution::Restarting)
        },
        || None,
    )
    .expect_err("the handoff outlasts the budget");

    assert!(resolves.get() > 0);
    assert!(
        format!("{error:#}").contains("marks a handoff to a successor"),
        "the timeout names the handoff: {error:#}"
    );
}
