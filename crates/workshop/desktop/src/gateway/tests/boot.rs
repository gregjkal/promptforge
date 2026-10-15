//! Boot planning and one-shot launch coverage.

use std::time::Duration;

use gateway_api_discovery::GatewayDiscoveryFile;

use super::{
    GATEWAY_EXE_NAME, dead_pid, fixture_gateway, live_file, owned_candidate, probe_own_image,
    probe_read_failure, workshop_exe,
};
use crate::gateway::boot::{GatewayPlan, WorkshopInstall, no_gateway_error, plan_gateway};
use crate::gateway::identity::GatewayAttachment;

#[path = "boot-launch-wait.rs"]
mod launch_wait;

const FIXTURE_PHASE_TIMEOUT: Duration = Duration::from_secs(10);

/// A Workshop at `exe`, running from no AppImage.
fn install(exe: &std::path::Path) -> WorkshopInstall {
    WorkshopInstall {
        exe: exe.to_path_buf(),
        appimage: None,
    }
}

fn workshop_server(
    port: u16,
    api_key: &str,
) -> (tempfile::TempDir, workshop_server_api::ServerHandle) {
    let state_dir = tempfile::TempDir::new().expect("create Workshop state directory");
    let server = workshop_server_api::fixtures::spawn(workshop_server_api::Config {
        gateway: workshop_server_api::GatewayConfig {
            base_url: format!("http://127.0.0.1:{port}"),
            api_key: api_key.to_owned(),
        },
        server: workshop_server_api::ServerConfig {
            bind: "127.0.0.1:0".to_owned(),
            state_dir: state_dir.path().to_owned(),
        },
        agents: workshop_server_api::AgentsConfig::default(),
    })
    .expect("spawn Workshop fixture");
    (state_dir, server)
}

#[test]
fn a_live_file_attaches_without_looking_for_a_sibling_exe() {
    let run = tempfile::TempDir::new().expect("tempdir");
    let file = live_file(fixture_gateway("key"), "key");
    file.write_to(run.path()).expect("write");
    let (_dir, exe) = workshop_exe(false);

    match plan_gateway(run.path(), &install(&exe), false, probe_own_image) {
        GatewayPlan::Attach(attached) => assert_eq!(attached, file),
        other => panic!("a live gateway must be attached, not {other:?}"),
    }
}

#[test]
fn no_file_and_a_sibling_exe_launches() {
    let run = tempfile::TempDir::new().expect("tempdir");
    let (_dir, exe) = workshop_exe(true);

    match plan_gateway(run.path(), &install(&exe), false, probe_own_image) {
        GatewayPlan::Launch(gateway) => {
            assert_eq!(gateway, exe.with_file_name(GATEWAY_EXE_NAME));
        }
        other => panic!("a full install with no running gateway must launch, not {other:?}"),
    }
}

#[test]
fn no_file_and_no_sibling_exe_falls_through_to_explicit_config() {
    let run = tempfile::TempDir::new().expect("tempdir");
    let (_dir, exe) = workshop_exe(false);

    assert_eq!(
        plan_gateway(run.path(), &install(&exe), true, probe_own_image),
        GatewayPlan::ConfigOnly,
        "a Workshop-only install attaches to the configured LAN gateway"
    );
}

#[test]
fn no_file_no_sibling_exe_and_no_config_fails() {
    let run = tempfile::TempDir::new().expect("tempdir");
    let (_dir, exe) = workshop_exe(false);

    assert_eq!(
        plan_gateway(run.path(), &install(&exe), false, probe_own_image),
        GatewayPlan::Fail,
        "nothing to connect to must fail loud, not serve a broken window"
    );
}

#[test]
fn a_stale_file_is_cleaned_and_the_sibling_exe_launches() {
    let run = tempfile::TempDir::new().expect("tempdir");
    GatewayDiscoveryFile {
        pid: dead_pid(),
        ..live_file(1, "k")
    }
    .write_to(run.path())
    .expect("write");
    let (_dir, exe) = workshop_exe(true);

    let plan = plan_gateway(run.path(), &install(&exe), false, probe_own_image);
    assert!(
        matches!(plan, GatewayPlan::Launch(_)),
        "a stale file must not block the relaunch: {plan:?}"
    );
    assert!(
        !gateway_api_discovery::gateway_discovery_file_path(run.path()).exists(),
        "the stale file was cleaned"
    );
}

#[test]
fn a_stale_file_with_no_sibling_exe_falls_through_to_explicit_config() {
    let run = tempfile::TempDir::new().expect("tempdir");
    GatewayDiscoveryFile {
        pid: dead_pid(),
        ..live_file(1, "k")
    }
    .write_to(run.path())
    .expect("write");
    let (_dir, exe) = workshop_exe(false);

    assert_eq!(
        plan_gateway(run.path(), &install(&exe), true, probe_own_image),
        GatewayPlan::ConfigOnly,
        "a stale file must not wedge the LAN fallback"
    );
    assert!(
        !gateway_api_discovery::gateway_discovery_file_path(run.path()).exists(),
        "the stale file was cleaned"
    );
}

#[test]
fn a_resolve_error_still_launches_the_sibling_exe() {
    let run = tempfile::TempDir::new().expect("tempdir");
    let (_dir, exe) = workshop_exe(true);

    let plan = plan_gateway(run.path(), &install(&exe), false, probe_read_failure);
    assert!(
        matches!(plan, GatewayPlan::Launch(_)),
        "a discovery error must not read as no-gateway: {plan:?}"
    );
}

#[test]
fn the_sibling_probe_finds_only_the_gateway_exe_beside_the_desktop_app() {
    let (_dir, with) = workshop_exe(true);
    assert_eq!(
        install(&with).gateway(),
        Some(with.with_file_name(GATEWAY_EXE_NAME)),
        "the installed sibling is found"
    );
    let (_dir, without) = workshop_exe(false);
    assert_eq!(
        install(&without).gateway(),
        None,
        "a Workshop-only install has no sibling"
    );
}

#[test]
fn the_no_gateway_error_names_both_remedies() {
    let (_dir, workshop) = workshop_exe(false);
    let message = no_gateway_error(&install(&workshop)).to_string();
    let beside = workshop.with_file_name(GATEWAY_EXE_NAME);
    assert!(
        message.contains(&beside.display().to_string()),
        "the error names each path searched: {message}"
    );
    assert!(
        message.contains("workshop.toml"),
        "the error names the explicit-config remedy: {message}"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_translocated_workshop_is_told_to_move_the_app() {
    let exe = std::path::Path::new(
        "/private/var/folders/xy/T/AppTranslocation/0A1B/d/PromptForge.app/Contents/MacOS/promptforge-workshop",
    );
    let message = no_gateway_error(&install(exe)).to_string();
    assert!(
        message.contains("Applications/PromptForge"),
        "the error names the move remedy and the install folder: {message}"
    );
    assert!(
        message.contains("workshop.toml"),
        "the error names the explicit-config remedy: {message}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_workshop_in_an_appimage_names_then_finds_the_gateway_beside_it() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let appimage = root.path().join("PromptForge").join("PromptForge.AppImage");
    std::fs::create_dir_all(appimage.parent().expect("parent")).expect("create install dir");
    std::fs::write(&appimage, b"").expect("plant the AppImage");
    let install = WorkshopInstall {
        exe: root
            .path()
            .join("mount")
            .join("usr")
            .join("bin")
            .join("promptforge-workshop"),
        appimage: Some(appimage.clone()),
    };
    let beside = appimage.with_file_name(GATEWAY_EXE_NAME);

    let message = no_gateway_error(&install).to_string();
    assert!(
        message.contains(&beside.display().to_string()),
        "the error names the path beside the AppImage: {message}"
    );
    super::plant_executable(&beside);
    assert_eq!(install.gateway(), Some(beside));
}

#[test]
fn matching_boot_publication_survives_closure_and_server_teardown() {
    let mut launched = super::validated_gateway("launched-key");
    let identity = launched.validate("launched-key", 1_778_000_001, "2026-09-08T18:00:01Z");
    let attachment = GatewayAttachment::Launched(owned_candidate(identity.clone()));
    let (_state_dir, server) = workshop_server(launched.port(), "launched-key");

    let attachment = attachment.reconcile_publication(Some(identity));
    server.gateway_updater().close_publication();
    drop(attachment);
    let outcome = server.shutdown().expect("server teardown continues");

    assert_eq!(outcome, workshop_server_api::Termination::Graceful);
    assert!(
        !launched.received_shutdown(Duration::from_millis(100)),
        "closure cannot clean up the exact child already published at boot"
    );
}

#[test]
fn closed_boot_publication_cleans_unpublished_owner_then_continues_teardown() {
    let mut launched = super::validated_gateway("launched-key");
    let published = super::validated_gateway("published-key");
    let launched_identity =
        launched.validate("launched-key", 1_778_000_001, "2026-09-08T18:00:01Z");
    let published_identity =
        published.validate("published-key", 1_778_000_002, "2026-09-08T18:00:02Z");
    let attachment = GatewayAttachment::Launched(owned_candidate(launched_identity));
    let (_state_dir, server) = workshop_server(published.port(), "published-key");

    server.gateway_updater().close_publication();
    let attachment = attachment.reconcile_publication(Some(published_identity));
    drop(attachment);
    assert!(
        launched.received_shutdown(FIXTURE_PHASE_TIMEOUT),
        "a closed boot cleans only its unpublished authenticated child"
    );
    assert_eq!(
        server.shutdown().expect("server teardown continues"),
        workshop_server_api::Termination::Graceful
    );
}
