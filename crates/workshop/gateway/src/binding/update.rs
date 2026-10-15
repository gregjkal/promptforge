//! Update requests derived from a gateway snapshot's validated identity,
//! and the gateway's answer read as an outcome.

use std::time::Instant;

use gateway_api_types::{UpdateAnswer, UpdateOutcome};

use super::{GatewaySnapshot, GatewayUpdater};

/// What the local gateway did when asked to update itself.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum GatewayUpdateOutcome {
    /// A newer release verified and is installing; the gateway restarts
    /// into this version.
    Updating(String),
    /// The gateway already runs the newest release.
    Current,
    /// Workshop's own updater owns the gateway's updates on this install.
    WorkshopOwned,
    /// Another update was already running.
    Busy,
    /// The update failed or was refused, for this reason; nothing was
    /// installed.
    Failed(String),
    /// The gateway has no update route: it predates self-update.
    Predates,
}

impl GatewayUpdateOutcome {
    /// Reads the gateway's `POST /update` answer: a 404 is a gateway that
    /// predates the route, and every other status carries an
    /// [`UpdateAnswer`] body.
    #[must_use]
    pub(crate) fn from_answer(status: u16, body: &[u8]) -> GatewayUpdateOutcome {
        if status == 404 {
            return GatewayUpdateOutcome::Predates;
        }
        let answer: UpdateAnswer = match serde_json::from_slice(body) {
            Ok(answer) => answer,
            Err(error) => {
                return GatewayUpdateOutcome::Failed(format!(
                    "the gateway answered {status} with a body that is not an update answer: {error}"
                ));
            }
        };
        match answer.outcome {
            UpdateOutcome::Updating => match answer.version {
                Some(version) => GatewayUpdateOutcome::Updating(version),
                None => GatewayUpdateOutcome::Failed(format!(
                    "the gateway answered {status} updating without naming the version"
                )),
            },
            UpdateOutcome::Current => GatewayUpdateOutcome::Current,
            UpdateOutcome::WorkshopOwned => GatewayUpdateOutcome::WorkshopOwned,
            UpdateOutcome::Busy => GatewayUpdateOutcome::Busy,
            UpdateOutcome::Failed => {
                GatewayUpdateOutcome::Failed(answer.message.unwrap_or_else(|| {
                    format!("the gateway answered {status} failed without a message")
                }))
            }
            _ => GatewayUpdateOutcome::Failed(format!(
                "the gateway answered {status} with an outcome this Workshop does not know"
            )),
        }
    }
}

impl GatewaySnapshot {
    /// Asks the validated local gateway this snapshot names to update
    /// itself, posting the authenticated `POST /update`, and reads its
    /// answer. No network I/O outlives `deadline`.
    ///
    /// Returns `Ok(None)` without sending a request when the snapshot came
    /// from explicit configuration, which grants no local authority, as
    /// [`GatewaySnapshot::request_shutdown`] does. The request is blocking
    /// I/O; an async caller runs it off the executor.
    ///
    /// # Errors
    /// Returns [`gateway_api_discovery::UpdateRequestError`] when the local
    /// gateway cannot be reached or its answer read before `deadline`.
    pub fn request_update(
        &self,
        deadline: Instant,
    ) -> Result<Option<GatewayUpdateOutcome>, gateway_api_discovery::UpdateRequestError> {
        let Some(identity) = self.identity.as_ref() else {
            return Ok(None);
        };
        let answer = gateway_api_discovery::request_update(identity, deadline)?;
        Ok(Some(GatewayUpdateOutcome::from_answer(
            answer.status,
            &answer.body,
        )))
    }
}

impl GatewayUpdater {
    /// Asks the validated local gateway in the current consumer snapshot
    /// to update itself, as that snapshot's
    /// [`GatewaySnapshot::request_update`] does.
    ///
    /// Returns `Ok(None)` without sending a request when the current
    /// gateway came from explicit configuration.
    ///
    /// # Errors
    /// Returns [`gateway_api_discovery::UpdateRequestError`] when the
    /// current local gateway cannot be reached or its answer read before
    /// `deadline`.
    pub fn request_update(
        &self,
        deadline: Instant,
    ) -> Result<Option<GatewayUpdateOutcome>, gateway_api_discovery::UpdateRequestError> {
        self.binding.snapshot().request_update(deadline)
    }
}
