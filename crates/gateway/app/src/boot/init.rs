//! `promptforge-gateway init`: first-run configuration and speech
//! provisioning at install time.
//!
//! The installers run it as the installing user once the Gateway component
//! is selected. It resolves the configuration exactly as a boot does,
//! generating the default when none exists and never rewriting an existing
//! file, then provisions the speech artifacts the selected profile declares
//! through the speech load's own preparation, so the first boot finds them
//! cached.

use std::path::{Path, PathBuf};

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
/// where the default is written when that file does not exist. The
/// configuration is then loaded as a boot loads it, so an unloadable file
/// fails `init` either way. Without `provision_speech` a generated default
/// declares no speech models and nothing is downloaded. Returns the
/// configuration path.
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
    init_in(
        explicit_config,
        Locations::gather,
        stt,
        boot_environment,
        provision,
    )
}

/// What a boot reads before loading `config_path`: the env file beside it,
/// then `PROMPTFORGE_PROFILE`, which that file may set.
fn boot_environment(config_path: &Path) -> Option<String> {
    crate::runner::load_env_file(&config_path.with_extension("env"));
    std::env::var("PROMPTFORGE_PROFILE").ok()
}

/// The testable body of [`init`]: `gather`, `environment`, and `provision`
/// are injected. `environment` receives the resolved configuration path and
/// returns the `PROMPTFORGE_PROFILE` value a boot would read.
fn init_in(
    explicit: Option<PathBuf>,
    gather: impl FnOnce() -> Result<Locations, BootError>,
    stt: InstallerStt,
    environment: impl FnOnce(&Path) -> Option<String>,
    provision: impl FnOnce(&Config) -> Result<(), InitError>,
) -> Result<PathBuf, InitError> {
    // Unlike a boot, which refuses a missing explicit file, init writes the
    // default there: generating a config is what it is for. A dangling
    // symlink is not missing: create-new would refuse to follow it.
    let path = match explicit {
        Some(path) if path.symlink_metadata().is_err() => generate_default(&path, stt),
        explicit => resolve_in(explicit, gather, stt),
    }
    .map_err(|error| InitError(InitRepr::Resolve(error)))?;
    let environment = environment(&path);
    let selection = ProfileSelection::new(None, environment.as_deref());
    let config = Config::load(&path, &selection).map_err(|source| {
        InitError(InitRepr::Load {
            path: path.clone(),
            source,
        })
    })?;
    if stt == InstallerStt::Included {
        provision(&config)?;
    }
    Ok(path)
}

/// Provisions the speech artifacts `config` declares, printing the artifact
/// store's progress text through [`progress::ProgressLines`].
#[cfg(feature = "stt")]
fn provision_speech_artifacts(config: &Config) -> Result<(), InitError> {
    if config.stt_models().is_empty() {
        println!("the selected profile declares no [[stt_model]]; no speech-to-text to provision");
        return Ok(());
    }
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
