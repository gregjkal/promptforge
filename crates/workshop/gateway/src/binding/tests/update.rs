//! Binding update requests: who may ask the gateway to update, and how its
//! answer reads as an outcome.

use super::*;

use std::time::{Duration, Instant};

use gateway_api_types::{UpdateAnswer, UpdateOutcome};

use crate::GatewayUpdateOutcome;

fn soon() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

fn body(outcome: UpdateOutcome, version: Option<&str>, message: Option<&str>) -> Vec<u8> {
    serde_json::to_vec(&UpdateAnswer {
        outcome,
        version: version.map(str::to_owned),
        message: message.map(str::to_owned),
    })
    .expect("serialize an answer")
}

#[test]
fn a_configured_snapshot_sends_no_update_request() {
    // Nothing listens at the documentation address, and no request is
    // tried: a configured gateway grants no local authority.
    let binding =
        GatewayBinding::new("http://192.0.2.10:8080", "configured-key").expect("binding builds");
    assert_eq!(
        binding
            .snapshot()
            .request_update(soon())
            .expect("a configured Gateway is an intentional no-op"),
        None
    );
    assert_eq!(
        binding
            .updater()
            .request_update(soon())
            .expect("a configured Gateway is an intentional no-op"),
        None
    );
}

#[test]
fn a_sidecar_snapshot_posts_the_update_and_reads_the_answer() {
    let gateway = crate::test_gateway::ValidatedGateway::spawn("update-key");
    let identity = validated_connection(
        &gateway,
        "update-key",
        1_778_000_001,
        "2026-09-07T18:00:01Z",
    );
    let base_url = format!("http://127.0.0.1:{}", identity.port());
    let binding = GatewayBinding::new_with_identity(&base_url, "update-key", Some(identity))
        .expect("binding builds");
    assert_eq!(
        binding
            .updater()
            .request_update(soon())
            .expect("the authenticated update is answered"),
        Some(GatewayUpdateOutcome::Updating("0.4.0".to_owned()))
    );
}

#[test]
fn every_update_answer_maps_to_its_outcome() {
    let cases = [
        (
            202,
            body(UpdateOutcome::Updating, Some("0.4.0"), None),
            GatewayUpdateOutcome::Updating("0.4.0".to_owned()),
        ),
        (
            200,
            body(UpdateOutcome::Current, Some("0.3.0"), None),
            GatewayUpdateOutcome::Current,
        ),
        (
            200,
            body(UpdateOutcome::WorkshopOwned, None, Some("Workshop updates")),
            GatewayUpdateOutcome::WorkshopOwned,
        ),
        (
            409,
            body(UpdateOutcome::Busy, None, Some("another update")),
            GatewayUpdateOutcome::Busy,
        ),
        (
            502,
            body(
                UpdateOutcome::Failed,
                None,
                Some("cannot fetch the release manifest"),
            ),
            GatewayUpdateOutcome::Failed("cannot fetch the release manifest".to_owned()),
        ),
    ];
    for (status, body, outcome) in cases {
        assert_eq!(
            GatewayUpdateOutcome::from_answer(status, &body),
            outcome,
            "{status}"
        );
    }
}

#[test]
fn a_gateway_without_the_route_predates_self_update() {
    assert_eq!(
        GatewayUpdateOutcome::from_answer(404, b""),
        GatewayUpdateOutcome::Predates
    );
}

#[test]
fn a_malformed_answer_fails_naming_the_status() {
    let GatewayUpdateOutcome::Failed(message) =
        GatewayUpdateOutcome::from_answer(500, b"Internal Server Error")
    else {
        panic!("a body that is not an update answer is a failure");
    };
    assert!(
        message.starts_with("the gateway answered 500 with a body that is not an update answer"),
        "{message}"
    );
}

#[test]
fn an_updating_answer_without_a_version_fails() {
    let GatewayUpdateOutcome::Failed(message) =
        GatewayUpdateOutcome::from_answer(202, &body(UpdateOutcome::Updating, None, None))
    else {
        panic!("an update to an unnamed version is a failure");
    };
    assert!(message.contains("202"), "{message}");
}
