//! Command-line parsing for the `promptforge-gateway` binary.

use std::ffi::OsString;
use std::path::PathBuf;

use gateway::{ProfileName, ServeOptions};

/// Why argument parsing stopped.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum ParseError {
    /// `-h`/`--help` was requested.
    Help,
    /// `--version` was requested.
    Version,
    /// The arguments were invalid; the string is the operator-facing reason.
    Usage(String),
}

/// What this launch does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Command {
    /// Serve (the default and only serving mode).
    Serve,
    /// Prints the diagnostics report and exits.
    Diagnostics,
    /// Writes the default config when absent and, when `speech` holds,
    /// provisions the speech artifacts it declares, then exits.
    Init {
        /// Whether to provision speech-to-text (`--no-stt` clears it).
        speech: bool,
    },
}

/// The parsed invocation: the serve options plus how the main thread runs.
#[derive(Debug)]
pub(super) struct Invocation {
    /// What the launch does.
    pub(super) command: Command,
    /// What to serve. Under [`Command::Diagnostics`] only the config path
    /// is meaningful: the report names it.
    pub(super) serve: ServeOptions,
    /// Whether the system tray occupies the main thread (default).
    /// `--no-tray` keeps the headless Ctrl-C loop for servers and CI.
    pub(super) tray: bool,
    /// Whether the launch came from the OS autostart entry (`--login`).
    pub(super) login: bool,
    /// Whether to print the Settings handoff URL to stdout (`--print-url`).
    /// Implies the headless loop: the flag exists for tray-less
    /// environments.
    pub(super) print_url: bool,
}

/// Parses the command line into a typed [`Invocation`].
///
/// The bare invocation serves; `diagnostics` and `init` are the subcommands. Uses `OsString`
/// operands so non-UTF-8 config paths survive. The config path
/// (`--config PATH`, falling back to `PROMPTFORGE_GATEWAY_CONFIG`) stays
/// optional: with neither set, the gateway discovers or generates the
/// boot config itself. `--profile NAME` is validated into a
/// [`ProfileName`] at parse time.
pub(super) fn parse_args(
    args: impl IntoIterator<Item = OsString>,
) -> Result<Invocation, ParseError> {
    let mut args = args.into_iter();
    let _binary = args.next();

    // A subcommand must come first.
    let mut args = args.peekable();
    match args.peek().and_then(|arg| arg.to_str()) {
        Some("diagnostics") => {
            args.next();
            return parse_diagnostics_args(args);
        }
        Some("init") => {
            args.next();
            return parse_init_args(args);
        }
        _ => {}
    }

    let mut profile: Option<ProfileName> = None;
    let mut config_path: Option<PathBuf> = None;
    let mut tray = true;
    let mut login = false;
    let mut print_url = false;
    let mut browser = false;

    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--config") => {
                let path = args
                    .next()
                    .ok_or_else(|| ParseError::Usage("--config requires a path".to_string()))?;
                if config_path.is_some() {
                    return Err(ParseError::Usage("--config accepts one path".to_string()));
                }
                config_path = Some(PathBuf::from(path));
            }
            Some("--profile") => {
                let name = args
                    .next()
                    .ok_or_else(|| ParseError::Usage("--profile requires a name".to_string()))?;
                let name = name.into_string().map_err(|_| {
                    ParseError::Usage("--profile name must be valid UTF-8".to_string())
                })?;
                let name = ProfileName::parse(&name)
                    .map_err(|error| ParseError::Usage(format!("invalid profile name: {error}")))?;
                profile = Some(name);
            }
            Some("--no-tray") => tray = false,
            Some("--login") => login = true,
            Some("--print-url") => print_url = true,
            Some("--browser") => browser = true,
            Some("-h" | "--help") => return Err(ParseError::Help),
            Some("--version") => return Err(ParseError::Version),
            Some(other) if other.starts_with('-') => {
                return Err(ParseError::Usage(format!("unknown flag {other}")));
            }
            _ => {
                return Err(ParseError::Usage(format!(
                    "unexpected argument {}",
                    arg.to_string_lossy()
                )));
            }
        }
    }

    let config_path =
        resolve_config_path(config_path, std::env::var_os("PROMPTFORGE_GATEWAY_CONFIG"));

    Ok(Invocation {
        command: Command::Serve,
        // `--login`'s contract is absolute - a login launch never opens a
        // browser - so it wins over `--browser`.
        serve: ServeOptions::new(config_path, profile).with_browser(browser && !login),
        tray,
        login,
        print_url,
    })
}

/// Parses what may follow `diagnostics`: at most `--config PATH`. Every
/// other flag belongs to a serving launch and is a usage error here.
fn parse_diagnostics_args(args: impl Iterator<Item = OsString>) -> Result<Invocation, ParseError> {
    let mut args = args;
    let mut config_path: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--config") => {
                let path = args
                    .next()
                    .ok_or_else(|| ParseError::Usage("--config requires a path".to_string()))?;
                if config_path.is_some() {
                    return Err(ParseError::Usage("--config accepts one path".to_string()));
                }
                config_path = Some(PathBuf::from(path));
            }
            Some("-h" | "--help") => return Err(ParseError::Help),
            _ => {
                return Err(ParseError::Usage(format!(
                    "diagnostics accepts only --config PATH, got {}",
                    arg.to_string_lossy()
                )));
            }
        }
    }
    let config_path =
        resolve_config_path(config_path, std::env::var_os("PROMPTFORGE_GATEWAY_CONFIG"));
    Ok(Invocation {
        command: Command::Diagnostics,
        serve: ServeOptions::new(config_path, None),
        tray: false,
        login: false,
        print_url: false,
    })
}

/// Parses what may follow `init`: `--no-stt` and at most `--config PATH`.
/// Every other flag belongs to a serving launch and is a usage error here.
fn parse_init_args(args: impl Iterator<Item = OsString>) -> Result<Invocation, ParseError> {
    let mut args = args;
    let mut config_path: Option<PathBuf> = None;
    let mut speech = true;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--config") => {
                let path = args
                    .next()
                    .ok_or_else(|| ParseError::Usage("--config requires a path".to_string()))?;
                if config_path.is_some() {
                    return Err(ParseError::Usage("--config accepts one path".to_string()));
                }
                config_path = Some(PathBuf::from(path));
            }
            Some("--no-stt") => speech = false,
            Some("-h" | "--help") => return Err(ParseError::Help),
            _ => {
                return Err(ParseError::Usage(format!(
                    "init accepts only --no-stt and --config PATH, got {}",
                    arg.to_string_lossy()
                )));
            }
        }
    }
    let config_path =
        resolve_config_path(config_path, std::env::var_os("PROMPTFORGE_GATEWAY_CONFIG"));
    Ok(Invocation {
        command: Command::Init { speech },
        serve: ServeOptions::new(config_path, None),
        tray: false,
        login: false,
        print_url: false,
    })
}

/// Resolves the config path: the `--config` flag wins, then the
/// `PROMPTFORGE_GATEWAY_CONFIG` environment variable - but only when it
/// names an existing file. A stale env var warns and falls through to boot
/// discovery: ambient state rots in ways a typed CLI path does not, and a
/// forgotten variable must not hard-fail a first-run boot. A `--config`
/// path is deliberate, so a missing file there stays an error
/// downstream.
///
/// Tests pass both sources explicitly and never touch the process
/// environment (edition 2024 makes `set_var` unsafe); the existence check
/// touches only the paths the test itself creates.
pub(super) fn resolve_config_path(cli: Option<PathBuf>, env: Option<OsString>) -> Option<PathBuf> {
    cli.or_else(|| {
        let path = PathBuf::from(env?);
        if path.is_file() {
            return Some(path);
        }
        tracing::warn!(
            path = %path.display(),
            "PROMPTFORGE_GATEWAY_CONFIG names no file; falling back to discovery"
        );
        None
    })
}
