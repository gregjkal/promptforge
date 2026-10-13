//! Coverage for a gateway handoff marked in the discovery file: boot waits,
//! supervision launches nothing, and a dead marker still recovers.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::time::Duration;

use gateway_api_discovery::{CancellationToken, GatewayDiscoveryFile, Resolution};

use super::{TestIdentity, dead_pid, live_file, probe_own_image, test_identity, workshop_exe};
use crate::gateway::boot::{GatewayPlan, WorkshopInstall, plan_gateway};
use crate::gateway::supervisor::{
    SupervisionProbe, SystemClock, probe_local_gateway, run_supervision,
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
    /// More polls than any fixed interval the supervisor uses would allow.
    const HANDOFF_POLLS: usize = 1_000;
    let original = test_identity(live_file(54_375, "old"));
    let successor = TestIdentity {
        file: live_file(54_376, "new"),
        process_boot: 7,
    };
    let polls = Cell::new(0_usize);
    let published = RefCell::new(Vec::new());

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
        |_, _| !published.borrow().is_empty(),
        &CancellationToken::new(),
    );

    assert!(polls.get() > HANDOFF_POLLS);
    assert_eq!(published.borrow().as_slice(), [successor]);
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
