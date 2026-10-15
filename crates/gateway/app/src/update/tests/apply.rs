//! The entry point's early outcomes, and the install step on a fixture
//! install. No test runs `apply_with` past the staging, because there the
//! update would replace the test binary itself; the end-to-end test in
//! `tests/it/update.rs` covers the whole flow on a copy.

use gateway_config::Config;

use crate::update::stage::Install;
#[cfg(unix)]
use crate::update::stage::{STAGING_DIR_NAME, Staging};
use crate::update::{Applied, Environment, UpdateError, apply_with, end_failed_install};
#[cfg(unix)]
use crate::update::{install_staged, windows::release_on_exit};

use super::{manifest_json, serve};

fn state() -> crate::AppState {
    let config = Config::from_toml_str(
        "config-version = 0\n[server]\nbind = \"127.0.0.1:0\"\napi_key = \"k\"\n",
    )
    .expect("config parses");
    crate::test_support::app_state(config, None)
}

fn environment(manifest_url: String) -> Environment {
    Environment {
        manifest_url,
        public_key: super::TestKey::generate().public(),
        version: "0.3.1".to_owned(),
        os: "linux",
        arch: "x86_64",
    }
}

#[tokio::test]
async fn a_second_update_while_one_runs_is_busy() {
    let state = state();
    let _running = state.shutdown.begin_update().expect("claim");
    let applied = apply_with(
        &state,
        &environment("http://127.0.0.1:1/latest.json".to_owned()),
    )
    .await
    .expect("busy is an answer");
    assert_eq!(applied, Applied::Busy);
}

#[tokio::test]
async fn a_current_gateway_downloads_nothing_and_frees_the_claim() {
    let address = serve(vec![(
        "/latest.json",
        200,
        manifest_json("0.3.1", "linux-x86_64-gateway", "http://127.0.0.1:1/x", "s"),
    )])
    .await;
    let state = state();
    let environment = environment(format!("http://{address}/latest.json"));
    assert_eq!(
        apply_with(&state, &environment).await.expect("apply"),
        Applied::Current {
            version: "0.3.1".to_owned()
        }
    );
    assert!(
        state.shutdown.begin_update().is_some(),
        "a finished check frees the claim"
    );
    assert!(!state.shutdown.is_fired(), "nothing restarts");
}

#[tokio::test]
async fn an_unsigned_release_is_refused_naming_the_platform_key() {
    let address = serve(vec![(
        "/latest.json",
        200,
        manifest_json("0.4.0", "linux-x86_64-gateway", "http://127.0.0.1:1/x", ""),
    )])
    .await;
    let state = state();
    let error = apply_with(
        &state,
        &environment(format!("http://{address}/latest.json")),
    )
    .await
    .expect_err("no signature");
    assert!(
        matches!(
            &error,
            UpdateError::Signature {
                key: "linux-x86_64-gateway",
                ..
            }
        ),
        "{error}"
    );
    assert!(
        state.shutdown.begin_update().is_some(),
        "a failure frees the claim"
    );
}

/// The Windows setup holds the claim while it runs, and an exit this
/// gateway outlives (a setup that did not install) frees it again.
#[cfg(unix)]
#[test]
fn a_running_setup_holds_the_claim_until_it_exits() {
    let state = state();
    let ticket = state.shutdown.begin_update().expect("claim");
    // The stand-in exits only once the test closes its stdin.
    let mut setup = std::process::Command::new("sh")
        .args(["-c", "read _; exit 3"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("a stand-in setup");
    let release = setup.stdin.take().expect("the stand-in's stdin");
    let watcher = release_on_exit(setup, ticket).expect("the watcher starts");
    assert!(
        state.shutdown.begin_update().is_none(),
        "a second update waits for the setup"
    );
    drop(release);
    watcher.join().expect("the watcher ends");
    assert!(
        state.shutdown.begin_update().is_some(),
        "a setup that exited without installing frees the claim"
    );
}

#[test]
fn only_a_stranded_install_keeps_the_claim_after_failing() {
    let stranded = UpdateError::Stranded {
        installed: "/opt/promptforge/promptforge-gateway".into(),
        previous: "/opt/promptforge/promptforge-gateway.previous".into(),
        reason: "swap: injected; restore: injected".to_owned(),
    };
    let held = state();
    end_failed_install(held.shutdown.begin_update().expect("claim"), &stranded);
    assert!(
        held.shutdown.begin_update().is_none(),
        "this process runs from the moved-aside copy and refuses another update"
    );
    let freed = state();
    end_failed_install(
        freed.shutdown.begin_update().expect("claim"),
        &UpdateError::ShuttingDown,
    );
    assert!(freed.shutdown.begin_update().is_some());
}

#[test]
fn an_executable_inside_another_bundle_is_refused() {
    let workshop = Install::locate(std::path::Path::new(
        "/Applications/PromptForge.app/Contents/MacOS/promptforge-gateway",
    ));
    let error = workshop
        .require_standalone()
        .expect_err("Workshop's bundle");
    assert!(
        matches!(&error, UpdateError::NotStandalone { bundle }
            if bundle == std::path::Path::new("/Applications/PromptForge.app")),
        "{error}"
    );
    for standalone in [
        "/Applications/PromptForge Gateway.app/Contents/MacOS/promptforge-gateway",
        "/usr/local/bin/promptforge-gateway",
    ] {
        Install::locate(std::path::Path::new(standalone))
            .require_standalone()
            .expect(standalone);
    }
}

/// A staged release whose archive holds `version`'s executable, beside an
/// installed one, as `apply` leaves them after a verified download.
#[cfg(unix)]
fn staged(dir: &std::path::Path, version: &str) -> (Install, Staging) {
    let script = format!("#!/bin/sh\necho 'promptforge-gateway {version}'\n");
    let install = Install::locate(&dir.join("promptforge-gateway"));
    std::fs::write(install.item(), "installed").expect("install");
    let staging = Staging::create(&install).expect("stage");
    std::fs::write(
        staging.archive(),
        super::tar_gz(&[("promptforge-gateway", script.as_bytes(), 0o755)]),
    )
    .expect("the verified download");
    (install, staging)
}

#[cfg(unix)]
#[test]
fn a_verified_release_is_swapped_in() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let (install, staging) = staged(dir.path(), "0.4.0");
    let staging = install_staged(
        staging,
        &install,
        "0.4.0",
        "linux-x86_64-gateway",
        &state().shutdown,
    )
    .expect("install");
    assert!(
        std::fs::read_to_string(install.item())
            .expect("read")
            .contains("0.4.0")
    );
    assert!(
        staging.into_path().is_dir(),
        "the succession gets the staging"
    );
}

#[cfg(unix)]
#[test]
fn a_gateway_shutting_down_does_not_swap_the_update_in() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let (install, staging) = staged(dir.path(), "0.4.0");
    let state = state();
    state.shutdown.fire(crate::ShutdownReason::Stop);
    let error = install_staged(
        staging,
        &install,
        "0.4.0",
        "linux-x86_64-gateway",
        &state.shutdown,
    )
    .expect_err("shutting down");
    assert!(matches!(error, UpdateError::ShuttingDown), "{error}");
    assert_eq!(
        std::fs::read_to_string(install.item()).expect("read"),
        "installed",
        "the install is untouched"
    );
    assert!(
        !dir.path().join(STAGING_DIR_NAME).exists(),
        "the staging goes with the refused update"
    );
}

#[tokio::test]
async fn an_unpublished_platform_is_named() {
    let state = state();
    let mut environment = environment("http://127.0.0.1:1/latest.json".to_owned());
    environment.os = "freebsd";
    let error = apply_with(&state, &environment)
        .await
        .expect_err("no platform");
    assert_eq!(
        error.to_string(),
        "no gateway self-update is published for freebsd on x86_64"
    );
}
