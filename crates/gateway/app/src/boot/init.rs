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
    /// Speech was requested, but the selected profile declares no speech
    /// model; the text names the selection's source and its remedy.
    #[error("{0}")]
    NoSpeechModels(String),
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

/// The remedy when no profile is selected.
const SELECT_A_PROFILE: &str = "select a profile whose `models` name a [[stt_model]], through \
     PROMPTFORGE_PROFILE or the state file's active_profile";

/// The [`InitRepr::NoSpeechModels`] text: what speech requires, the actual
/// selection named by the input that made it, and the remedy.
fn no_speech_models(path: &Path, environment: Option<&str>, config: &Config) -> String {
    let state_file = gateway_config::profile_state_path(path);
    let (actual, remedy) = match (config.active_profile(), config.stale_state_selection()) {
        (Some(profile), _) => {
            // PROMPTFORGE_PROFILE, when set, wins over the state file.
            let source = if environment.is_some() {
                "PROMPTFORGE_PROFILE".to_owned()
            } else {
                state_file.display().to_string()
            };
            (
                format!(
                    "{source} selects profile {}, which lists no [[stt_model]] in {}",
                    profile.name(),
                    path.display()
                ),
                "name at least one [[stt_model]] in that profile's `models`",
            )
        }
        (None, Some(stale)) => (
            format!(
                "{} selects profile \"{stale}\", which {} does not define (defined profiles: {})",
                state_file.display(),
                path.display(),
                crate::runner::defined_profiles(config)
            ),
            SELECT_A_PROFILE,
        ),
        (None, None) => (
            format!("{} selects no profile", path.display()),
            SELECT_A_PROFILE,
        ),
    };
    format!("speech-to-text requested, but {actual}; {remedy}, or rerun init with --no-stt")
}

/// Writes the default configuration when none exists and, when
/// `provision_speech` holds, downloads and verifies the whisper library,
/// the speech models, and the Silero model the selected profile declares.
/// Download progress prints to stdout: a line when each phase starts, a
/// line when a phase that reports a percent ends, and at most one percent
/// line per second.
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
/// generated, or loaded; when `provision_speech` holds but the selected
/// profile declares no speech model, as in a config generated under
/// `--no-stt`, which `init` never rewrites; or when a speech artifact
/// cannot be provisioned. Its source chain names the failing artifact and
/// cause.
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
    // default there: generating a config is what it is for. Only a path
    // that names nothing is missing: a dangling symlink, which create-new
    // would refuse to follow, or a path that cannot be inspected falls
    // through to the load, which names the failure.
    let path = match explicit {
        Some(path)
            if path
                .symlink_metadata()
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            generate_default(&path, stt)
        }
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
    if stt == InstallerStt::Omitted {
        return Ok(path);
    }
    if config.stt_models().is_empty() {
        let message = no_speech_models(&path, environment.as_deref(), &config);
        return Err(InitError(InitRepr::NoSpeechModels(message)));
    }
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
