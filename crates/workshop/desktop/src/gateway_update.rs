//! The `update_gateway` command: after its own update check, the SPA asks
//! the local sidecar gateway to update itself.
//!
//! The SPA reaches the gateway only through the server's `/gateway/api`
//! proxy, which refuses everything outside the admin and cache routes on
//! purpose, so the request goes through the desktop app, as Quit's
//! `/shutdown` does: it posts the authenticated `POST /update` through the
//! server's current validated Gateway snapshot. A configured LAN Gateway
//! grants no local authority, so nothing is sent to it. Debug builds never
//! ask: an update would replace a development tree's freshly built
//! gateway with the released one.

use std::sync::PoisonError;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Manager as _, Wry};
use workshop_server_api::GatewayUpdateOutcome;

use crate::ServerSlot;

/// How long the request may take. The gateway answers only after it has
/// downloaded and installed a newer release, so the bound leaves room for
/// the download; a request that runs out of time does not stop the update.
const UPDATE_DEADLINE: Duration = Duration::from_mins(5);

/// What the SPA learns from `update_gateway`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub(crate) enum GatewayUpdate {
    /// The gateway is installing this newer version and restarts into it.
    Updating {
        /// The version it moves to.
        version: String,
    },
    /// The gateway already runs the newest release.
    Current,
    /// This Workshop's own updater updates the gateway with it.
    WorkshopOwned,
    /// Another gateway update was already running.
    Busy,
    /// The update failed, was refused, or its answer never came.
    Failed {
        /// Why.
        message: String,
    },
    /// The gateway predates self-update.
    Predates,
    /// No local sidecar gateway: the server has not started, or the
    /// Gateway is a configured LAN endpoint.
    NoLocalGateway,
    /// A debug build, which never asks.
    Development,
}

impl From<GatewayUpdateOutcome> for GatewayUpdate {
    fn from(outcome: GatewayUpdateOutcome) -> Self {
        match outcome {
            GatewayUpdateOutcome::Updating(version) => Self::Updating { version },
            GatewayUpdateOutcome::Current => Self::Current,
            GatewayUpdateOutcome::WorkshopOwned => Self::WorkshopOwned,
            GatewayUpdateOutcome::Busy => Self::Busy,
            GatewayUpdateOutcome::Failed(message) => Self::Failed { message },
            GatewayUpdateOutcome::Predates => Self::Predates,
            _ => Self::Failed {
                message: "the gateway answered with an outcome this Workshop does not know"
                    .to_owned(),
            },
        }
    }
}

/// Asks the local sidecar gateway to update itself and reports what it
/// did. It never fails: every failure is an outcome the SPA records.
#[tauri::command]
pub(crate) async fn update_gateway(app: AppHandle<Wry>) -> GatewayUpdate {
    let gateway = app.try_state::<ServerSlot>().and_then(|slot| {
        slot.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(workshop_server_api::ServerHandle::gateway_updater)
    });
    let development = cfg!(debug_assertions);
    // The request is blocking socket I/O.
    tauri::async_runtime::spawn_blocking(move || {
        update_with(
            development,
            gateway.map(|gateway| move |deadline| gateway.request_update(deadline)),
        )
    })
    .await
    .unwrap_or_else(|error| GatewayUpdate::Failed {
        message: format!("the gateway update request did not finish: {error}"),
    })
}

/// The command's decision, with the request injected: a debug build asks
/// nothing, no server or no sidecar is [`GatewayUpdate::NoLocalGateway`],
/// and an undeliverable request is [`GatewayUpdate::Failed`].
fn update_with<R, E>(development: bool, request: Option<R>) -> GatewayUpdate
where
    R: FnOnce(Instant) -> Result<Option<GatewayUpdateOutcome>, E>,
    E: std::error::Error,
{
    if development {
        return GatewayUpdate::Development;
    }
    let Some(request) = request else {
        return GatewayUpdate::NoLocalGateway;
    };
    match request(Instant::now() + UPDATE_DEADLINE) {
        Ok(Some(outcome)) => outcome.into(),
        Ok(None) => GatewayUpdate::NoLocalGateway,
        Err(error) => GatewayUpdate::Failed {
            message: format!(
                "cannot reach the gateway's update route: {}",
                error_chain(&error)
            ),
        },
    }
}

/// `error` and its sources, colon-separated.
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
#[path = "gateway_update-tests.rs"]
mod tests;
