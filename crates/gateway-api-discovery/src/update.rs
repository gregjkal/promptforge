//! The `POST /update` request: how a reader asks the gateway to update
//! itself.
//!
//! Workshop is the caller: after its own update check it posts the current
//! validated connection's bearer key to the gateway's update route, which
//! answers once it knows the outcome, before any restart. Raw HTTP/1.0 over
//! `TcpStream` like the shutdown request, and the whole answer is returned
//! as it arrived: the caller reads the body as the gateway's update answer.
//!
//! [`update_platform_key`] names the release manifest entry each
//! platform's gateway updates from.

use crate::ValidatedConnection;
use crate::health::{self, ProbeError};
use std::time::Instant;

/// The update route's path on the gateway.
const UPDATE_PATH: &str = "/update";

/// The most of the answer read, head included. The gateway's answer is a
/// short JSON object; anything longer is cut here.
const ANSWER_LIMIT: usize = 16 * 1024;

/// The key of the `latest.json` entry that updates a gateway running on
/// `os` and `arch` (spelled as [`std::env::consts`] spells them), or
/// `None` for a platform no release serves. macOS and Linux read the
/// gateway's own archive; Windows reads Workshop's setup, which carries
/// the gateway as a component. The `-gateway` suffix never collides with
/// the Tauri updater's `{os}-{arch}-{installer}` keys. The gateway reads
/// its entry by this key and the release build writes the entries under
/// it, so the two cannot drift apart.
#[must_use]
pub fn update_platform_key(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => Some("darwin-aarch64-gateway"),
        ("macos", "x86_64") => Some("darwin-x86_64-gateway"),
        ("linux", "x86_64") => Some("linux-x86_64-gateway"),
        ("linux", "aarch64") => Some("linux-aarch64-gateway"),
        ("windows", "x86_64") => Some("windows-x86_64"),
        _ => None,
    }
}

/// The gateway's answer to `POST /update`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateResponse {
    /// The HTTP status code.
    pub status: u16,
    /// The body, at most what fits in 16 KiB with the head.
    pub body: Vec<u8>,
}

/// A failure of [`request_update`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum UpdateRequestError {
    /// The request could not be delivered or the answer read.
    #[error("{operation}")]
    Io {
        /// The failed operation.
        operation: &'static str,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// The answer does not start with an HTTP status line.
    #[error("the gateway's update answer has no HTTP status line")]
    Malformed,
}

/// Posts `POST /update` to the validated local Gateway, presenting its
/// bearer key, and returns the answer's status and body. No network I/O
/// outlives `deadline`. The gateway answers after it has downloaded and
/// installed a newer release and before it restarts into it, so the
/// deadline must leave room for the download; a request that runs out of
/// time does not stop the update.
///
/// # Errors
/// Returns [`UpdateRequestError::Io`] when the gateway cannot be connected
/// or its answer cannot be read before `deadline`, and
/// [`UpdateRequestError::Malformed`] when the answer is not HTTP.
pub fn request_update(
    connection: &ValidatedConnection,
    deadline: Instant,
) -> Result<UpdateResponse, UpdateRequestError> {
    let file = connection.gateway_discovery_file();
    request_update_at(&format!("127.0.0.1:{}", file.port), &file.api_key, deadline)
}

fn request_update_at(
    address: &str,
    api_key: &str,
    deadline: Instant,
) -> Result<UpdateResponse, UpdateRequestError> {
    let answer = health::request_until(
        address,
        "POST",
        UPDATE_PATH,
        Some(api_key),
        deadline,
        ANSWER_LIMIT,
    )
    .map_err(|error| match error {
        ProbeError::Io { operation, source } => UpdateRequestError::Io { operation, source },
        ProbeError::UnexpectedStatus { .. } => UpdateRequestError::Malformed,
    })?;
    parse(&answer).ok_or(UpdateRequestError::Malformed)
}

/// Splits a raw HTTP answer into its status code and body.
fn parse(answer: &[u8]) -> Option<UpdateResponse> {
    let status_line = answer.split(|byte| *byte == b'\n').next()?;
    let mut words = status_line.split(u8::is_ascii_whitespace);
    if !words.next()?.starts_with(b"HTTP/") {
        return None;
    }
    let status = std::str::from_utf8(words.next()?).ok()?.parse().ok()?;
    let body = answer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map_or_else(Vec::new, |end| answer[end + 4..].to_vec());
    Some(UpdateResponse { status, body })
}

#[cfg(test)]
#[path = "update-tests.rs"]
mod tests;
