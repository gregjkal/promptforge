//! The entry point's early outcomes, and the install step on a fixture
//! install. No test runs `apply_with` past the staging, because there the
//! update would replace the test binary itself; the end-to-end test in
//! `tests/it/update.rs` covers the whole flow on a copy.

use gateway_config::Config;

#[cfg(unix)]
use crate::update::install_staged;
#[cfg(unix)]
use crate::update::stage::{Install, STAGING_DIR_NAME, Staging};
use crate::update::{Applied, Environment, UpdateError, apply_with, settle};

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

/// Every path that reports `Updating`, the swap's and the Windows setup's,
/// keeps the claim, since the succession or the setup ends this process.
#[test]
fn an_update_under_way_keeps_its_claim_and_any_other_end_frees_it() {
    let updating = Ok(Applied::Updating {
        version: "0.4.0".to_owned(),
    });
    let held = state();
    settle(held.shutdown.begin_update().expect("claim"), &updating);
    assert!(
        held.shutdown.begin_update().is_none(),
        "a second update waits for the restart"
    );

    for ended in [
        Ok(Applied::Current {
            version: "0.3.1".to_owned(),
        }),
        Err(UpdateError::ShuttingDown),
    ] {
        let freed = state();
        settle(freed.shutdown.begin_update().expect("claim"), &ended);
        assert!(freed.shutdown.begin_update().is_some(), "{ended:?}");
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
    state.shutdown.fire();
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
