//! The release manifest check: fetch `latest.json`, read this platform's
//! entry, and compare its version with the running one. It downloads
//! nothing else, so a periodic check can run it alone.

use gateway_protocol::http_util::{MAX_JSON_BODY, read_bytes_capped};
use serde::Deserialize;

use super::UpdateError;

/// One platform's entry in the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct Entry {
    /// Where the signed archive or setup is published.
    pub(crate) url: String,
    /// The download's minisign signature, as standard base64 of the
    /// signature text.
    pub(crate) signature: String,
}

/// What the manifest says about this gateway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Check {
    /// The manifest offers nothing newer than the running version.
    Current {
        /// The running version.
        version: String,
    },
    /// The manifest offers a newer version for this platform.
    Available {
        /// The advertised version, without a leading `v`.
        version: String,
        /// This platform's entry.
        entry: Entry,
    },
}

/// The fields of `latest.json` the gateway reads. Other platforms'
/// entries stay unparsed, so one malformed entry for another platform
/// cannot stop this one's update.
#[derive(Deserialize)]
struct Manifest {
    version: String,
    platforms: std::collections::BTreeMap<String, serde_json::Value>,
}

/// `text` without the leading `v` a release tag may carry, as the Tauri
/// updater reads it.
pub(crate) fn bare_version(text: &str) -> &str {
    text.strip_prefix('v').unwrap_or(text)
}

/// Fetches the manifest at `url` and judges it for `key` against the
/// running `version`.
///
/// # Errors
/// Returns [`UpdateError::Fetch`] for a transport failure and
/// [`UpdateError::Manifest`] for a non-success status or a manifest
/// [`judge`] refuses.
pub(crate) async fn check(
    client: &reqwest::Client,
    url: &str,
    key: &'static str,
    version: &str,
) -> Result<Check, UpdateError> {
    let fetch = |source| UpdateError::Fetch {
        url: url.to_owned(),
        source,
    };
    let response = client.get(url).send().await.map_err(fetch)?;
    let status = response.status();
    if !status.is_success() {
        return Err(UpdateError::Manifest {
            url: url.to_owned(),
            reason: format!("the server answered {status}, required a 2xx success"),
        });
    }
    let body = read_bytes_capped(response, MAX_JSON_BODY)
        .await
        .map_err(fetch)?;
    judge(&body, key, version).map_err(|reason| UpdateError::Manifest {
        url: url.to_owned(),
        reason,
    })
}

/// Reads `body` as a manifest and compares its version for `key` with the
/// running `version`. Only a strictly newer version is offered.
///
/// # Errors
/// Returns the reason the manifest is unusable: not the updater's JSON, a
/// version that is not semver, or no entry for `key` when one is newer.
pub(crate) fn judge(body: &[u8], key: &str, version: &str) -> Result<Check, String> {
    let manifest: Manifest = serde_json::from_slice(body)
        .map_err(|error| format!("it is not the updater's latest.json: {error}"))?;
    let advertised = bare_version(&manifest.version);
    let offered = semver::Version::parse(advertised)
        .map_err(|error| format!("version \"{}\" is not semver: {error}", manifest.version))?;
    let running = semver::Version::parse(bare_version(version))
        .map_err(|error| format!("the running version \"{version}\" is not semver: {error}"))?;
    if offered <= running {
        return Ok(Check::Current {
            version: version.to_owned(),
        });
    }
    let value = manifest
        .platforms
        .get(key)
        .ok_or_else(|| format!("version {advertised} has no \"{key}\" entry"))?;
    let entry = Entry::deserialize(value).map_err(|error| {
        format!("the \"{key}\" entry needs a url and a signature string: {error}")
    })?;
    Ok(Check::Available {
        version: advertised.to_owned(),
        entry,
    })
}
