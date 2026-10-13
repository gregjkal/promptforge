//! The answer to `POST /update`: what the Gateway did about a newer
//! release.
//!
//! The route writes it, and a caller asking the Gateway to update itself
//! reads it, so both name an outcome with one vocabulary.

use serde::{Deserialize, Serialize};

/// What a `POST /update` request concluded.
///
/// Future additive fields are marked `#[serde(default)]` so a lagging
/// reader survives them; there is no schema version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateAnswer {
    /// What the Gateway did.
    pub outcome: UpdateOutcome,
    /// For [`UpdateOutcome::Updating`], the version the Gateway is moving
    /// to; for [`UpdateOutcome::Current`], the version it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Why the Gateway is not updating, for every other outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// The outcome an [`UpdateAnswer`] reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum UpdateOutcome {
    /// A newer release verified and is being installed; the Gateway
    /// restarts into it.
    Updating,
    /// The Gateway already runs the newest release.
    Current,
    /// Workshop installed beside this Gateway owns updates, which update
    /// both together.
    WorkshopOwned,
    /// Another update is already running.
    Busy,
    /// The update failed; the Gateway keeps running its current version.
    Failed,
}

#[cfg(test)]
#[path = "update-tests.rs"]
mod tests;
