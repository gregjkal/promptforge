use crate::update::UpdateError;
use crate::update::manifest::{Check, Entry, check, judge};

use super::{manifest_json, serve};

fn entry() -> Entry {
    Entry {
        url: "https://example.invalid/gateway.tar.gz".to_owned(),
        signature: "c2ln".to_owned(),
    }
}

#[test]
fn only_a_newer_version_is_offered() {
    let body = manifest_json(
        "0.4.0",
        "linux-x86_64-gateway",
        &entry().url,
        &entry().signature,
    );
    assert_eq!(
        judge(&body, "linux-x86_64-gateway", "0.3.1").expect("judge"),
        Check::Available {
            version: "0.4.0".to_owned(),
            entry: entry(),
        }
    );
    for running in ["0.4.0", "0.5.0", "v0.4.0"] {
        assert_eq!(
            judge(&body, "linux-x86_64-gateway", running).expect("judge"),
            Check::Current {
                version: running.to_owned(),
            },
            "running {running}"
        );
    }
}

#[test]
fn a_leading_v_is_read_as_the_tauri_updater_reads_it() {
    let body = manifest_json(
        "v0.4.0",
        "linux-x86_64-gateway",
        &entry().url,
        &entry().signature,
    );
    assert!(matches!(
        judge(&body, "linux-x86_64-gateway", "0.3.1").expect("judge"),
        Check::Available { version, .. } if version == "0.4.0"
    ));
}

#[test]
fn an_unusable_manifest_names_what_is_wrong() {
    let newer_without_key = manifest_json(
        "0.4.0",
        "darwin-aarch64-gateway",
        &entry().url,
        &entry().signature,
    );
    let error = judge(&newer_without_key, "linux-x86_64-gateway", "0.3.1").expect_err("no entry");
    assert!(error.contains("\"linux-x86_64-gateway\""), "{error}");

    let not_semver = manifest_json("latest", "linux-x86_64-gateway", "u", "s");
    let error = judge(&not_semver, "linux-x86_64-gateway", "0.3.1").expect_err("not semver");
    assert!(error.contains("\"latest\" is not semver"), "{error}");

    let error = judge(b"<html>", "linux-x86_64-gateway", "0.3.1").expect_err("not JSON");
    assert!(error.contains("latest.json"), "{error}");

    let broken_entry = serde_json::to_vec(&serde_json::json!({
        "version": "0.4.0",
        "platforms": { "linux-x86_64-gateway": { "url": 7 } },
    }))
    .expect("serialize");
    let error = judge(&broken_entry, "linux-x86_64-gateway", "0.3.1").expect_err("bad entry");
    assert!(error.contains("url and a signature"), "{error}");
}

#[test]
fn another_platforms_malformed_entry_does_not_stop_this_one() {
    let body = serde_json::to_vec(&serde_json::json!({
        "version": "0.4.0",
        "platforms": {
            "windows-x86_64": "not an object",
            "linux-x86_64-gateway": { "url": "u", "signature": "s" },
        },
    }))
    .expect("serialize");
    assert!(matches!(
        judge(&body, "linux-x86_64-gateway", "0.3.1").expect("judge"),
        Check::Available { .. }
    ));
}

#[tokio::test]
async fn the_check_fetches_and_judges_the_served_manifest() {
    let body = manifest_json("0.4.0", "linux-x86_64-gateway", "u", "s");
    let address = serve(vec![
        ("/latest.json", 200, body),
        ("/missing.json", 404, Vec::new()),
    ])
    .await;
    let client = reqwest::Client::new();
    let found = check(
        &client,
        &format!("http://{address}/latest.json"),
        "linux-x86_64-gateway",
        "0.3.1",
    )
    .await
    .expect("check");
    assert!(matches!(found, Check::Available { version, .. } if version == "0.4.0"));

    let url = format!("http://{address}/missing.json");
    let error = check(&client, &url, "linux-x86_64-gateway", "0.3.1")
        .await
        .expect_err("a 404 manifest");
    assert!(
        matches!(&error, UpdateError::Manifest { url: named, reason } if *named == url && reason.contains("404")),
        "{error}"
    );
}
