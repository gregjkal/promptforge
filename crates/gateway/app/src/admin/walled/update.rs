//! The `POST /update` route, the remote face of the self-update in
//! [`crate::update`].
//!
//! It carries `/shutdown`'s authority: behind the shared loopback wall and
//! bearer auth, so any local process can call it, as it can call
//! `/shutdown`. That is safe because the update applies only a newer
//! release whose signature verifies against the embedded release key; the
//! worst a caller can cause is the restart an update needs anyway. The
//! route answers after the swap and before the succession: like
//! `/shutdown`'s `202`, the answer drains ahead of the shutdown the
//! succession starts.

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::{Method, StatusCode};
use axum::routing::post;
use gateway_api_types::{UpdateAnswer, UpdateOutcome};

use crate::AppState;
use crate::auth::LoopbackCaller;
use crate::registry::RouteInfo;
use crate::update::{Applied, UpdateError};

const UPDATE: RouteInfo = RouteInfo::walled("/update", &[Method::POST]);

/// The update route, as the registry sees it.
pub(super) const ROUTES: &[RouteInfo] = &[UPDATE];

/// The update route.
pub(super) fn routes() -> Router<AppState> {
    Router::new().route(UPDATE.path, post(admin_update))
}

/// The `POST /update` route: `202` while moving to a newer version, `200`
/// when current or when Workshop owns updates, `409` while another update
/// runs, and an error status naming what failed.
async fn admin_update(
    State(state): State<AppState>,
    _caller: LoopbackCaller,
) -> (StatusCode, Json<UpdateAnswer>) {
    // The update runs as its own task: a caller that disconnects drops
    // this handler, and an update dropped between its swap and its
    // succession would leave the new gateway installed but never started.
    let update = tokio::spawn(async move { crate::update::apply(&state).await });
    let result = update
        .await
        .unwrap_or_else(|error| Err(UpdateError::Task(error.to_string())));
    let (status, answer) = answer(result);
    (status, Json(answer))
}

/// The status and body for an update's result.
pub(super) fn answer(result: Result<Applied, UpdateError>) -> (StatusCode, UpdateAnswer) {
    let reply = |outcome, version, message| UpdateAnswer {
        outcome,
        version,
        message,
    };
    match result {
        Ok(Applied::Updating { version }) => (
            StatusCode::ACCEPTED,
            reply(UpdateOutcome::Updating, Some(version), None),
        ),
        Ok(Applied::Current { version }) => (
            StatusCode::OK,
            reply(UpdateOutcome::Current, Some(version), None),
        ),
        Ok(Applied::WorkshopOwned) => (
            StatusCode::OK,
            reply(
                UpdateOutcome::WorkshopOwned,
                None,
                Some(
                    "updates come from PromptForge Workshop on this install: its updater updates the gateway with it"
                        .to_owned(),
                ),
            ),
        ),
        Ok(Applied::Busy) => (
            StatusCode::CONFLICT,
            reply(
                UpdateOutcome::Busy,
                None,
                Some("another update is already running".to_owned()),
            ),
        ),
        Err(error) => {
            tracing::warn!("the gateway update failed: {}", error_chain(&error));
            (
                failure_status(&error),
                reply(UpdateOutcome::Failed, None, Some(error_chain(&error))),
            )
        }
    }
}

/// `502` when the release channel served something unusable, `500` when
/// this machine could not apply a good release.
fn failure_status(error: &UpdateError) -> StatusCode {
    match error {
        UpdateError::Fetch { .. }
        | UpdateError::Manifest { .. }
        | UpdateError::Download { .. }
        | UpdateError::Signature { .. }
        | UpdateError::Archive { .. }
        | UpdateError::StagedVersion { .. } => StatusCode::BAD_GATEWAY,
        UpdateError::Executable(_)
        | UpdateError::NoPlatform { .. }
        | UpdateError::NotWritable { .. }
        | UpdateError::Swap { .. }
        | UpdateError::Setup { .. }
        | UpdateError::Task(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// The error and its sources on one line, so the answer names the cause.
fn error_chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

#[cfg(test)]
#[path = "update-tests.rs"]
mod tests;
