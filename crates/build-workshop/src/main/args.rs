//! Command-line parsing for the build, installer, and sidecar modes.

use std::path::PathBuf;

use super::{BuildRequest, Profile};

pub(super) const USAGE: &str = "\
Build PromptForge Gateway and Workshop together, build a platform installer,
or stage the Gateway sidecar on its own.

USAGE:
    cargo workshop [--release] [--target <triple>]
    cargo workshop installer [--target <triple>] [--sign]
    cargo workshop sidecar stage --target <triple> --source <path>
    cargo workshop sidecar remove --target <triple>

OPTIONS:
    --release           Build both products with Cargo's release profile
    --target <triple>   Build for this target triple instead of the host
    --sign              Sign the updater files with TAURI_SIGNING_PRIVATE_KEY
                        and TAURI_SIGNING_PRIVATE_KEY_PASSWORD (unset is empty);
                        on macOS and Linux the key must be contents, not a
                        path, with TAURI_SIGNING_PRIVATE_KEY_PATH and
                        TAURI_PRIVATE_KEY_PATH unset
    --source <path>     The built Gateway binary to stage
    -h, --help          Print this help
";

#[derive(Debug, Eq, PartialEq)]
pub(super) enum Request {
    Build(BuildRequest),
    Installer(InstallerRequest),
    Sidecar(SidecarRequest),
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct InstallerRequest {
    pub(super) target: Option<String>,
    pub(super) sign: bool,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum SidecarRequest {
    Stage { target: String, source: PathBuf },
    Remove { target: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    Release,
    Target,
    Sign,
    Source,
}

impl Flag {
    const ALL: [Self; 4] = [Self::Release, Self::Target, Self::Sign, Self::Source];

    fn name(self) -> &'static str {
        match self {
            Self::Release => "--release",
            Self::Target => "--target",
            Self::Sign => "--sign",
            Self::Source => "--source",
        }
    }
}

#[derive(Debug, Default)]
struct Options {
    release: bool,
    target: Option<String>,
    sign: bool,
    source: Option<String>,
}

impl Options {
    fn parse(args: &[String], allowed: &[Flag]) -> Result<Self, anyhow::Error> {
        let mut options = Self::default();
        let mut index = 0;
        while index < args.len() {
            let argument = args[index].as_str();
            let flag = Flag::ALL
                .into_iter()
                .find(|flag| flag.name() == argument && allowed.contains(flag))
                .ok_or_else(|| usage_error(&format!("unsupported argument `{argument}`")))?;
            if options.seen(flag) {
                return Err(usage_error(&format!("duplicate argument `{argument}`")));
            }
            match flag {
                Flag::Release => options.release = true,
                Flag::Sign => options.sign = true,
                Flag::Target => {
                    let value = value_after(args, index, "a target triple")?;
                    if !valid_target_triple(value) {
                        return Err(usage_error(&format!(
                            "argument `--target` needs a valid target triple, got `{value}`"
                        )));
                    }
                    options.target = Some(value.to_owned());
                    index += 1;
                }
                Flag::Source => {
                    options.source = Some(value_after(args, index, "a path")?.to_owned());
                    index += 1;
                }
            }
            index += 1;
        }
        Ok(options)
    }

    fn seen(&self, flag: Flag) -> bool {
        match flag {
            Flag::Release => self.release,
            Flag::Target => self.target.is_some(),
            Flag::Sign => self.sign,
            Flag::Source => self.source.is_some(),
        }
    }
}

pub(super) fn parse_arguments(args: &[String]) -> Result<Request, anyhow::Error> {
    match args.first().map(String::as_str) {
        Some("installer") => {
            let options = Options::parse(&args[1..], &[Flag::Target, Flag::Sign])?;
            Ok(Request::Installer(InstallerRequest {
                target: options.target,
                sign: options.sign,
            }))
        }
        Some("sidecar") => parse_sidecar(&args[1..]),
        _ => {
            let options = Options::parse(args, &[Flag::Release, Flag::Target])?;
            Ok(Request::Build(BuildRequest {
                profile: if options.release {
                    Profile::Release
                } else {
                    Profile::Debug
                },
                target: options.target,
            }))
        }
    }
}

fn parse_sidecar(args: &[String]) -> Result<Request, anyhow::Error> {
    match args.first().map(String::as_str) {
        Some("stage") => {
            let options = Options::parse(&args[1..], &[Flag::Target, Flag::Source])?;
            let target = options
                .target
                .ok_or_else(|| usage_error("`sidecar stage` needs `--target <triple>`"))?;
            let source = options
                .source
                .ok_or_else(|| usage_error("`sidecar stage` needs `--source <path>`"))?;
            Ok(Request::Sidecar(SidecarRequest::Stage {
                target,
                source: PathBuf::from(source),
            }))
        }
        Some("remove") => {
            let options = Options::parse(&args[1..], &[Flag::Target])?;
            let target = options
                .target
                .ok_or_else(|| usage_error("`sidecar remove` needs `--target <triple>`"))?;
            Ok(Request::Sidecar(SidecarRequest::Remove { target }))
        }
        Some(action) => Err(usage_error(&format!(
            "`sidecar` needs `stage` or `remove`, got `{action}`"
        ))),
        None => Err(usage_error("`sidecar` needs `stage` or `remove`")),
    }
}

fn value_after<'a>(
    args: &'a [String],
    index: usize,
    expected: &str,
) -> Result<&'a str, anyhow::Error> {
    let name = &args[index];
    match args.get(index + 1) {
        Some(value) if !value.starts_with('-') => Ok(value),
        _ => Err(usage_error(&format!("argument `{name}` needs {expected}"))),
    }
}

fn usage_error(message: &str) -> anyhow::Error {
    anyhow::anyhow!("{message}\n\n{USAGE}")
}

pub(super) fn valid_target_triple(target: &str) -> bool {
    let parts: Vec<&str> = target.split('-').collect();
    parts.len() >= 3
        && parts.iter().all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_')
        })
}
