//! `promptforge-gateway init`: first-run configuration and speech
//! provisioning at install time.
//!
//! The installers run it as the installing user once the Gateway component
//! is selected. It resolves the configuration exactly as a boot does,
//! generating the default when none exists and never rewriting an existing
//! file, then provisions the speech artifacts the selected profile declares
//! through the speech load's own preparation, so the first boot finds them
//! cached.

use std::path::PathBuf;

use gateway_config::{Config, ProfileSelection};

use super::{BootError, InstallerStt, Locations, generate_default, resolve_in};

#[cfg(feature = "stt")]
#[path = "init-progress.rs"]
mod progress;

/// Why `init` failed.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct InitError(InitRepr);

#[derive(Debug, thiserror::Error)]
enum InitRepr {
    #[error("resolve the gateway configuration")]
    Resolve(#[source] BootError),
    #[error("load the gateway configuration {}", path.display())]
    Load {
        path: PathBuf,
        #[source]
        source: gateway_config::ConfigError,
    },
    #[cfg(feature = "stt")]
    #[error("provision speech-to-text for the selected profile")]
    Speech(#[source] gateway_stt::SpeechError),
    #[cfg(not(feature = "stt"))]
    #[error(
        "speech-to-text requested, but this gateway was built without the `stt` Cargo feature; \
         rerun init with --no-stt"
    )]
    SpeechUnavailable,
}

/// Writes the default configuration when none exists and, when
/// `provision_speech` holds, downloads and verifies the whisper library,
/// the speech models, and the Silero model the selected profile declares.
/// Download progress prints to stdout, one line per phase start and end
/// and at most one percent line per second.
///
/// `explicit_config` wins over discovery, as it does for a boot, and is
/// where the default is written when that file does not exist. Without
/// `provision_speech` a generated default declares no speech models and
/// nothing is downloaded. Returns the configuration path.
///
/// # Errors
/// Returns [`InitError`] when the configuration cannot be resolved,
/// generated, or loaded, or when a speech artifact cannot be provisioned;
/// its source chain names the failing artifact and cause.
pub fn init(
    explicit_config: Option<PathBuf>,
    provision_speech: bool,
) -> Result<PathBuf, InitError> {
    #[cfg(not(feature = "stt"))]
    if provision_speech {
        return Err(InitError(InitRepr::SpeechUnavailable));
    }
    let stt = if provision_speech {
        InstallerStt::Included
    } else {
        InstallerStt::Omitted
    };
    #[cfg(feature = "stt")]
    let provision = provision_speech_artifacts;
    #[cfg(not(feature = "stt"))]
    let provision = |_: &Config| -> Result<(), InitError> { Ok(()) };
    let environment = std::env::var("PROMPTFORGE_PROFILE").ok();
    init_in(
        explicit_config,
        Locations::gather,
        stt,
        environment.as_deref(),
        provision,
    )
}

/// The testable body of [`init`]: `gather` and `provision` are injected,
/// and `environment` is the `PROMPTFORGE_PROFILE` value a boot would read.
fn init_in(
    explicit: Option<PathBuf>,
    gather: impl FnOnce() -> Result<Locations, BootError>,
    stt: InstallerStt,
    environment: Option<&str>,
    provision: impl FnOnce(&Config) -> Result<(), InitError>,
) -> Result<PathBuf, InitError> {
    // Unlike a boot, which refuses a missing explicit file, init writes the
    // default there: generating a config is what it is for.
    let path = match explicit {
        Some(path) if !path.exists() => generate_default(&path, stt),
        explicit => resolve_in(explicit, gather, stt),
    }
    .map_err(|error| InitError(InitRepr::Resolve(error)))?;
    if stt == InstallerStt::Omitted {
        return Ok(path);
    }
    crate::runner::load_env_file(&path.with_extension("env"));
    let selection = ProfileSelection::new(None, environment);
    let config = Config::load(&path, &selection).map_err(|source| {
        InitError(InitRepr::Load {
            path: path.clone(),
            source,
        })
    })?;
    provision(&config)?;
    Ok(path)
}

/// Provisions the speech artifacts `config` declares, printing the artifact
/// store's progress text through [`progress::ProgressLines`].
#[cfg(feature = "stt")]
fn provision_speech_artifacts(config: &Config) -> Result<(), InitError> {
    let hub = gateway_progress::ProgressHub::new();
    let activity = std::sync::Arc::new(hub.begin("Provisioning speech-to-text"));
    let printer = progress::Printer::spawn(hub.subscribe());
    let result = gateway_stt::provision(
        config,
        Some(&activity),
        &tokio_util::sync::CancellationToken::new(),
    );
    drop(activity);
    if let Some(printer) = printer {
        printer.finish(result.is_ok());
    }
    result.map_err(|source| InitError(InitRepr::Speech(source)))
}

#[cfg(test)]
#[path = "init-tests.rs"]
mod tests;
