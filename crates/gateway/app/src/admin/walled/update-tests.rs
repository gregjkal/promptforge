use std::net::SocketAddr;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::header::AUTHORIZATION;
use axum::http::{Method, Request, Response, StatusCode};
use gateway_api_types::{UpdateAnswer, UpdateOutcome};
use gateway_config::Config;
use tower::ServiceExt;

use super::answer;
use crate::test_support::app_state;
use crate::update::{Applied, UpdateError};
use crate::{AppState, build_router};

fn state(trust_loopback: bool) -> AppState {
    let config = Config::from_toml_str(&format!(
        "config-version = 0\n\
         [server]\nbind = \"127.0.0.1:0\"\napi_key = \"test-token\"\n\
         trust_loopback = {trust_loopback}\n",
    ))
    .expect("config parses");
    app_state(config, None)
}

/// Sends `POST /update` through the router with the given bearer key and
/// peer address planted, as the walled route requires.
async fn send(state: &AppState, key: Option<&str>, peer: &str) -> Response<Body> {
    let mut builder = Request::builder().method(Method::POST).uri("/update");
    if let Some(key) = key {
        builder = builder.header(AUTHORIZATION, format!("Bearer {key}"));
    }
    let mut request = builder.body(Body::empty()).expect("request builds");
    let peer: SocketAddr = peer.parse().expect("a socket address");
    request.extensions_mut().insert(ConnectInfo(peer));
    build_router(state.clone(), None)
        .oneshot(request)
        .await
        .expect("the router is infallible")
}

async fn body(response: Response<Body>) -> UpdateAnswer {
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("read the body");
    serde_json::from_slice(&bytes).expect("an UpdateAnswer")
}

/// The handler is reached without touching the network: an update held
/// by the test answers every accepted caller with `409`.
#[tokio::test]
async fn an_accepted_caller_reaches_the_update_and_a_concurrent_one_gets_409() {
    let state = state(false);
    let _running = state.shutdown.begin_update().expect("claim");
    let response = send(&state, Some("test-token"), "127.0.0.1:50000").await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(body(response).await.outcome, UpdateOutcome::Busy);
    assert!(
        !state.shutdown.is_fired(),
        "a refused update leaves the server up"
    );
}

#[tokio::test]
async fn a_trusted_loopback_caller_needs_no_credentials() {
    let state = state(true);
    let _running = state.shutdown.begin_update().expect("claim");
    let response = send(&state, None, "127.0.0.1:50000").await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_missing_or_wrong_key_is_refused() {
    let state = state(false);
    for key in [None, Some("wrong")] {
        let response = send(&state, key, "127.0.0.1:50000").await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "key {key:?}");
    }
}

#[tokio::test]
async fn a_lan_peer_is_refused_even_with_the_key() {
    let state = state(true);
    let response = send(&state, Some("test-token"), "198.51.100.7:44821").await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[test]
fn each_result_has_its_status_and_answer() {
    let (status, reply) = answer(Ok(Applied::Updating {
        version: "0.4.0".to_owned(),
    }));
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(
        (reply.outcome, reply.version.as_deref()),
        (UpdateOutcome::Updating, Some("0.4.0"))
    );

    let (status, reply) = answer(Ok(Applied::Current {
        version: "0.4.0".to_owned(),
    }));
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reply.outcome, UpdateOutcome::Current);

    let (status, reply) = answer(Ok(Applied::WorkshopOwned));
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reply.outcome, UpdateOutcome::WorkshopOwned);
    assert!(
        reply
            .message
            .is_some_and(|message| message.contains("Workshop"))
    );

    let (status, reply) = answer(Err(UpdateError::Signature {
        key: "linux-x86_64-gateway",
        reason: "the signature was made with another key".to_owned(),
    }));
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(reply.outcome, UpdateOutcome::Failed);
    assert!(reply.message.is_some_and(
        |message| message.contains("linux-x86_64-gateway") && message.contains("another key")
    ));

    let (status, reply) = answer(Err(UpdateError::NotWritable {
        directory: "/usr/local/bin/.promptforge-gateway-update".into(),
        source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
    }));
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        reply
            .message
            .is_some_and(|message| message.contains("/usr/local/bin/.promptforge-gateway-update")),
        "the unwritable directory is named"
    );
}
