//! The `update_gateway` command's decision with an injected request: debug
//! builds ask nothing, a missing server or sidecar is no local gateway, and
//! each gateway outcome reaches the SPA.

use std::cell::Cell;
use std::time::Instant;

use workshop_server_api::GatewayUpdateOutcome;

use super::{GatewayUpdate, update_with};

/// A request that answers `answer` and counts its calls.
fn answering(
    calls: &Cell<usize>,
    answer: Result<Option<GatewayUpdateOutcome>, std::io::Error>,
) -> impl FnOnce(Instant) -> Result<Option<GatewayUpdateOutcome>, std::io::Error> + '_ {
    move |deadline| {
        calls.set(calls.get() + 1);
        assert!(deadline > Instant::now(), "the request gets time to answer");
        answer
    }
}

#[test]
fn a_debug_build_never_asks() {
    let calls = Cell::new(0);
    assert_eq!(
        update_with(
            true,
            Some(answering(&calls, Ok(Some(GatewayUpdateOutcome::Current))))
        ),
        GatewayUpdate::Development
    );
    assert_eq!(calls.get(), 0, "no request leaves a debug build");
}

#[test]
fn the_command_reports_development_exactly_in_debug_builds() {
    // The command passes `cfg!(debug_assertions)`; this test binary is
    // built with the same setting the command is.
    let reported = update_with(
        cfg!(debug_assertions),
        None::<fn(Instant) -> Result<Option<GatewayUpdateOutcome>, std::io::Error>>,
    );
    if cfg!(debug_assertions) {
        assert_eq!(reported, GatewayUpdate::Development);
    } else {
        assert_eq!(reported, GatewayUpdate::NoLocalGateway);
    }
}

#[test]
fn no_server_is_no_local_gateway() {
    assert_eq!(
        update_with(
            false,
            None::<fn(Instant) -> Result<Option<GatewayUpdateOutcome>, std::io::Error>>
        ),
        GatewayUpdate::NoLocalGateway
    );
}

#[test]
fn a_configured_gateway_is_no_local_gateway() {
    let calls = Cell::new(0);
    assert_eq!(
        update_with(false, Some(answering(&calls, Ok(None)))),
        GatewayUpdate::NoLocalGateway
    );
    assert_eq!(calls.get(), 1);
}

#[test]
fn every_gateway_outcome_reaches_the_spa() {
    let cases = [
        (
            GatewayUpdateOutcome::Updating("0.4.0".to_owned()),
            GatewayUpdate::Updating {
                version: "0.4.0".to_owned(),
            },
        ),
        (GatewayUpdateOutcome::Current, GatewayUpdate::Current),
        (
            GatewayUpdateOutcome::WorkshopOwned,
            GatewayUpdate::WorkshopOwned,
        ),
        (GatewayUpdateOutcome::Busy, GatewayUpdate::Busy),
        (
            GatewayUpdateOutcome::Failed("refused".to_owned()),
            GatewayUpdate::Failed {
                message: "refused".to_owned(),
            },
        ),
        (GatewayUpdateOutcome::Predates, GatewayUpdate::Predates),
    ];
    for (outcome, expected) in cases {
        let calls = Cell::new(0);
        assert_eq!(
            update_with(false, Some(answering(&calls, Ok(Some(outcome))))),
            expected
        );
    }
}

#[test]
fn an_unreachable_gateway_fails_naming_the_cause() {
    let calls = Cell::new(0);
    let reported = update_with(
        false,
        Some(answering(
            &calls,
            Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                "connection refused",
            )),
        )),
    );
    assert_eq!(
        reported,
        GatewayUpdate::Failed {
            message: "cannot reach the gateway's update route: connection refused".to_owned()
        }
    );
}

#[test]
fn the_spa_reads_a_tagged_outcome() {
    let json = |update: &GatewayUpdate| serde_json::to_value(update).expect("serialize");
    assert_eq!(
        json(&GatewayUpdate::Updating {
            version: "0.4.0".to_owned()
        }),
        serde_json::json!({"outcome": "updating", "version": "0.4.0"})
    );
    assert_eq!(
        json(&GatewayUpdate::Failed {
            message: "refused".to_owned()
        }),
        serde_json::json!({"outcome": "failed", "message": "refused"})
    );
    assert_eq!(
        json(&GatewayUpdate::NoLocalGateway),
        serde_json::json!({"outcome": "no_local_gateway"})
    );
    assert_eq!(
        json(&GatewayUpdate::WorkshopOwned),
        serde_json::json!({"outcome": "workshop_owned"})
    );
}
