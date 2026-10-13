//! The entry point's early outcomes. Each test stops before the staging,
//! because past it the update would replace the test binary itself; the
//! end-to-end test in `tests/it/update.rs` covers the swap on a copy.

use gateway_config::Config;

use crate::update::{Applied, Environment, UpdateError, apply_with};

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
