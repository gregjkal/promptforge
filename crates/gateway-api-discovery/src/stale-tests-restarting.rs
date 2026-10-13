//! Unit tests for the handoff mark: resolution, the read-only checks,
//! marking, and removal.

use super::tests::{dead_pid, fixture_gateway, live_file, own_image_name};
use super::*;

use crate::file::{mark_restarting, remove_if_mine};
use crate::paths::gateway_discovery_file_path;

/// A marked gateway discovery file naming the test process.
fn marked_file(port: u16) -> GatewayDiscoveryFile {
    GatewayDiscoveryFile {
        restarting: true,
        ..live_file(port, "key")
    }
}

#[test]
fn a_marked_file_whose_process_lives_resolves_to_restarting_and_stays() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    // Port 1 never listens: the marking gateway has stopped serving.
    marked_file(1).write_to(dir.path()).expect("write");

    let resolution = resolve_named(dir.path(), &own_image_name()).expect("resolve");
    assert_eq!(resolution, Resolution::Restarting);
    let resolution =
        resolve_named_cancellable(dir.path(), &own_image_name(), &CancellationToken::new())
            .expect("resolve cancellable");
    assert_eq!(resolution, Resolution::Restarting);
    assert!(
        gateway_discovery_file_path(dir.path()).exists(),
        "a handoff's file is never removed while its gateway lives"
    );
}

#[test]
fn a_marked_file_whose_gateway_still_serves_resolves_to_restarting() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    marked_file(fixture_gateway("key"))
        .write_to(dir.path())
        .expect("write");

    let resolution = resolve_named(dir.path(), &own_image_name()).expect("resolve");
    assert_eq!(
        resolution,
        Resolution::Restarting,
        "the mark wins over a gateway that would otherwise attach"
    );
    let resolution =
        resolve_named_cancellable(dir.path(), &own_image_name(), &CancellationToken::new())
            .expect("resolve cancellable");
    assert_eq!(resolution, Resolution::Restarting);
}

#[test]
fn a_marked_file_whose_process_died_is_stale_and_removed() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let dead = GatewayDiscoveryFile {
        pid: dead_pid(),
        ..marked_file(1)
    };
    dead.write_to(dir.path()).expect("write");
    let resolution = resolve_named(dir.path(), &own_image_name()).expect("resolve");
    assert_eq!(resolution, Resolution::Stale(StaleReason::ProcessDead));
    assert!(!gateway_discovery_file_path(dir.path()).exists());

    dead.write_to(dir.path()).expect("write again");
    let resolution =
        resolve_named_cancellable(dir.path(), &own_image_name(), &CancellationToken::new())
            .expect("resolve cancellable");
    assert_eq!(resolution, Resolution::Stale(StaleReason::ProcessDead));
    assert!(!gateway_discovery_file_path(dir.path()).exists());
}

#[test]
fn a_marked_file_naming_another_program_is_stale() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    marked_file(1).write_to(dir.path()).expect("write");
    // The test process is alive but is not a promptforge-gateway: a
    // reused pid.
    let resolution = resolve(dir.path()).expect("resolve");
    assert_eq!(resolution, Resolution::Stale(StaleReason::ImageMismatch));
    assert!(!gateway_discovery_file_path(dir.path()).exists());
}

#[test]
fn the_read_only_checks_never_report_a_marked_file_live() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let file = marked_file(fixture_gateway("key"));
    file.write_to(dir.path()).expect("write");

    assert!(!is_running_named(dir.path(), &own_image_name()));
    assert!(!is_live(&file, &own_image_name()));
    assert!(
        !is_live_cancellable(&file, &own_image_name(), &CancellationToken::new())
            .expect("not cancelled")
    );
    assert!(is_restarting(&file, &own_image_name()));
    assert_eq!(
        ValidatedConnection::validate_named(file, &own_image_name()),
        Err(StaleReason::Restarting),
        "a validated connection never names a gateway mid-handoff"
    );
    assert!(gateway_discovery_file_path(dir.path()).exists());
}

#[test]
fn remove_if_mine_leaves_a_marked_file() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let file = marked_file(1);
    file.write_to(dir.path()).expect("write");

    remove_if_mine(dir.path(), file.pid).expect("remove");
    assert!(
        gateway_discovery_file_path(dir.path()).exists(),
        "the marking gateway's own shutdown keeps its handoff mark"
    );
}

#[test]
fn mark_restarting_marks_only_the_callers_own_file() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let error = mark_restarting(dir.path(), std::process::id()).expect_err("no file");
    assert!(
        error.to_string().contains("no file exists"),
        "an absent file is named: {error}"
    );

    let file = live_file(1, "key");
    file.write_to(dir.path()).expect("write");
    let other = file.pid + 1;
    let error = mark_restarting(dir.path(), other).expect_err("another process's file");
    let message = error.to_string();
    assert!(
        message.contains(&other.to_string()) && message.contains(&file.pid.to_string()),
        "the refusal names both processes: {message}"
    );
    assert!(
        !GatewayDiscoveryFile::read(dir.path())
            .expect("read")
            .expect("present")
            .restarting,
        "a refused mark changes nothing"
    );

    mark_restarting(dir.path(), file.pid).expect("mark");
    let marked = GatewayDiscoveryFile::read(dir.path())
        .expect("read")
        .expect("present");
    assert_eq!(
        marked,
        GatewayDiscoveryFile {
            restarting: true,
            ..file
        }
    );
}

#[test]
fn the_mark_is_absent_from_unmarked_json_and_round_trips() {
    let unmarked = live_file(8081, "key");
    let json = serde_json::to_string(&unmarked).expect("serialize");
    assert!(
        !json.contains("restarting"),
        "an unmarked file serializes as it did before the field: {json}"
    );
    let parsed: GatewayDiscoveryFile = serde_json::from_str(&json).expect("parse");
    assert!(!parsed.restarting, "a file without the field is unmarked");

    let marked = marked_file(8081);
    let json = serde_json::to_string(&marked).expect("serialize");
    let parsed: GatewayDiscoveryFile = serde_json::from_str(&json).expect("parse");
    assert_eq!(parsed, marked);
}

#[test]
fn a_mark_written_while_validation_fails_is_not_removed() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    live_file(1, "key").write_to(dir.path()).expect("write");
    let run_dir = dir.path().to_owned();

    let resolution = resolve_named_cancellable_with_effects(
        dir.path(),
        &own_image_name(),
        &CancellationToken::new(),
        |_, _, _| {
            // The gateway marks its record and stops serving while the
            // proof is in flight, so the proof fails.
            mark_restarting(&run_dir, std::process::id()).expect("mark");
            Err(ValidationError::Stale(StaleReason::HealthFailed))
        },
        || {},
        remove_stale,
    )
    .expect("resolve");

    assert_eq!(resolution, Resolution::Restarting);
    assert!(
        GatewayDiscoveryFile::read(dir.path())
            .expect("read")
            .expect("the mark survives")
            .restarting
    );
}

#[test]
fn a_successor_published_after_a_dead_mark_was_judged_is_attached() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    GatewayDiscoveryFile {
        pid: dead_pid(),
        ..marked_file(1)
    }
    .write_to(dir.path())
    .expect("write");
    let successor = live_file(fixture_gateway("key"), "key");
    let mut published = false;

    let resolution = resolve_named_cancellable_with_effects(
        dir.path(),
        &own_image_name(),
        &CancellationToken::new(),
        ValidatedConnection::validate_named_cancellable,
        || {
            // The successor publishes between the judgment and the removal.
            if !std::mem::replace(&mut published, true) {
                successor.write_to(dir.path()).expect("publish");
            }
        },
        remove_stale,
    )
    .expect("resolve");

    assert_eq!(resolution, Resolution::Attach(successor.clone()));
    assert_eq!(
        GatewayDiscoveryFile::read(dir.path()).expect("read"),
        Some(successor),
        "the successor's file is never removed as the dead mark"
    );
}
