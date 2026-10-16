//! `cargo workshop manifest`: checks that a directory holds exactly one
//! release's files for every platform, verifies each updater file's
//! signature against the updater public key, and writes the release's
//! `latest.json` and `SHA256SUMS` into it.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read as _;
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use gateway_api_discovery::update_platform_key;
use sha2::{Digest as _, Sha256};

use super::args::ManifestRequest;
use super::installer::collect::{
    arch_name, gateway_archive_name, installer_name, workshop_updater_name,
};
use super::installer::ifw::utc_date;
use super::installer::{Arch, Platform, System, VERSION};

pub(crate) const MANIFEST: &str = "latest.json";
pub(crate) const CHECKSUMS: &str = "SHA256SUMS";
const SIGNATURE_SUFFIX: &str = ".sig";
const HEX: &[u8; 16] = b"0123456789abcdef";

/// Every platform a release ships an installer for.
pub(crate) const RELEASE_PLATFORMS: [Platform; 5] = [
    Platform {
        system: System::Windows,
        arch: Arch::X86_64,
    },
    Platform {
        system: System::MacOs,
        arch: Arch::Aarch64,
    },
    Platform {
        system: System::MacOs,
        arch: Arch::X86_64,
    },
    Platform {
        system: System::Linux,
        arch: Arch::X86_64,
    },
    Platform {
        system: System::Linux,
        arch: Arch::Aarch64,
    },
];

/// One signed updater file and the `latest.json` key it is published under.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Updater {
    pub(crate) key: String,
    pub(crate) file: String,
}

/// The release's nine signed updater files: Workshop's on each platform,
/// under the Tauri updater's `{os}-{arch}` key, and the Gateway's archive
/// on macOS and Linux, under the key the Gateway reads its entry by.
pub(crate) fn updaters() -> Result<Vec<Updater>, String> {
    let mut updaters = Vec::new();
    for platform in RELEASE_PLATFORMS {
        let arch = arch_name(platform.arch);
        let (tauri_os, rust_os) = match platform.system {
            System::Windows => ("windows", "windows"),
            System::MacOs => ("darwin", "macos"),
            System::Linux => ("linux", "linux"),
        };
        updaters.push(Updater {
            key: format!("{tauri_os}-{arch}"),
            file: workshop_updater_name(platform),
        });
        if let Some(file) = gateway_archive_name(platform) {
            let key = update_platform_key(rust_os, arch).ok_or_else(|| {
                format!(
                    "the Gateway on {rust_os} {arch} reads no update key, yet the release \
                     publishes its archive {file}"
                )
            })?;
            updaters.push(Updater {
                key: key.to_owned(),
                file,
            });
        }
    }
    Ok(updaters)
}

/// The files `SHA256SUMS` lists, sorted: the five installers and the other
/// updater files. The Windows setup is both.
pub(crate) fn checksummed_files(updaters: &[Updater]) -> Vec<String> {
    let files: BTreeSet<String> = RELEASE_PLATFORMS
        .into_iter()
        .map(installer_name)
        .chain(updaters.iter().map(|updater| updater.file.clone()))
        .collect();
    files.into_iter().collect()
}

/// Every file the release publishes besides the two this mode writes.
pub(crate) fn release_files(updaters: &[Updater]) -> BTreeSet<String> {
    checksummed_files(updaters)
        .into_iter()
        .chain(
            updaters
                .iter()
                .map(|updater| format!("{}{SIGNATURE_SUFFIX}", updater.file)),
        )
        .collect()
}

/// Verifies `request.assets` as this version's release and writes
/// `latest.json` and `SHA256SUMS` into it, dated `now` (seconds since the
/// Unix epoch). The public key is `request.public_key`'s contents, or else
/// `plugins.updater.pubkey` in `workspace_root`'s Workshop `tauri.conf.json`.
pub(crate) fn write(
    request: &ManifestRequest,
    workspace_root: &Path,
    now: u64,
) -> Result<(), String> {
    let base_url = base_url(&request.base_url)?;
    let updaters = updaters()?;
    check_file_set(&request.assets, &release_files(&updaters))?;
    let (public_key, key_source) = public_key(request, workspace_root)?;
    let mut platforms = serde_json::Map::new();
    let mut failures = Vec::new();
    for updater in &updaters {
        match verify(&public_key, &request.assets, &updater.file) {
            Ok(signature) => {
                platforms.insert(
                    updater.key.clone(),
                    serde_json::json!({
                        "url": format!("{base_url}/{}", updater.file),
                        "signature": signature,
                    }),
                );
            }
            Err(reason) => failures.push(format!("{} ({}): {reason}", updater.key, updater.file)),
        }
    }
    if !failures.is_empty() {
        return Err(format!(
            "{} of {} updater signatures fail against the public key in {key_source}, \
             which every installed copy trusts; each must verify and carry \
             \"version:{VERSION}\":\n  {}",
            failures.len(),
            updaters.len(),
            failures.join("\n  ")
        ));
    }
    let manifest = serde_json::json!({
        "version": VERSION,
        "notes": format!("PromptForge {VERSION}"),
        "pub_date": pub_date(now),
        "platforms": platforms,
    });
    let manifest = serde_json::to_string_pretty(&manifest)
        .map_err(|error| format!("cannot render {MANIFEST}: {error}"))?;
    let checksums = checksummed_files(&updaters)
        .into_iter()
        .map(|file| {
            sha256_hex(&request.assets.join(&file)).map(|digest| format!("{digest}  {file}\n"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    write_file(&request.assets.join(MANIFEST), &(manifest + "\n"))?;
    write_file(&request.assets.join(CHECKSUMS), &checksums.concat())
}

fn base_url(value: &str) -> Result<&str, String> {
    let trimmed = value.trim_end_matches('/');
    let has_host = ["https://", "http://"].iter().any(|scheme| {
        trimmed
            .strip_prefix(scheme)
            .is_some_and(|rest| !rest.is_empty())
    });
    if has_host {
        Ok(trimmed)
    } else {
        Err(format!(
            "`--base-url` must be an http or https URL with a host, got `{value}`"
        ))
    }
}

/// Requires `assets` to hold `expected` as regular files and nothing else,
/// except an earlier run's `latest.json` and `SHA256SUMS`, which this run
/// rewrites.
fn check_file_set(assets: &Path, expected: &BTreeSet<String>) -> Result<(), String> {
    let entries = fs::read_dir(assets).map_err(|error| {
        format!(
            "cannot read the release directory {}: {error}",
            assets.display()
        )
    })?;
    let mut present = BTreeSet::new();
    let mut unexpected = BTreeSet::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot read {}: {error}", assets.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == MANIFEST || name == CHECKSUMS {
            continue;
        }
        if expected.contains(&name) && entry.path().is_file() {
            present.insert(name);
        } else {
            unexpected.insert(name);
        }
    }
    let missing: Vec<&str> = expected
        .iter()
        .filter(|name| !present.contains(*name))
        .map(String::as_str)
        .collect();
    if missing.is_empty() && unexpected.is_empty() {
        return Ok(());
    }
    let mut problems = Vec::new();
    if !missing.is_empty() {
        problems.push(format!(
            "missing {} of {}: {}",
            missing.len(),
            expected.len(),
            missing.join(", ")
        ));
    }
    if !unexpected.is_empty() {
        problems.push(format!(
            "unexpected: {}",
            unexpected.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    Err(format!(
        "{} must hold exactly the {} release files of PromptForge {VERSION} as regular files; \
         {}",
        assets.display(),
        expected.len(),
        problems.join("; ")
    ))
}

fn public_key(
    request: &ManifestRequest,
    workspace_root: &Path,
) -> Result<(minisign_verify::PublicKey, String), String> {
    let (encoded, source) = if let Some(path) = &request.public_key {
        let contents = fs::read_to_string(path)
            .map_err(|error| format!("cannot read the public key {}: {error}", path.display()))?;
        (contents, path.display().to_string())
    } else {
        let config = workspace_root
            .join("crates")
            .join("workshop")
            .join("desktop")
            .join("tauri.conf.json");
        let text = fs::read_to_string(&config)
            .map_err(|error| format!("cannot read {}: {error}", config.display()))?;
        let value: serde_json::Value = serde_json::from_str(&text)
            .map_err(|error| format!("{} is not JSON: {error}", config.display()))?;
        let key = value
            .pointer("/plugins/updater/pubkey")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                format!(
                    "{} has no string at plugins.updater.pubkey",
                    config.display()
                )
            })?;
        (
            key.to_owned(),
            format!("{} (plugins.updater.pubkey)", config.display()),
        )
    };
    let text =
        base64_text(&encoded, "public key").map_err(|reason| format!("{source}: {reason}"))?;
    let key = minisign_verify::PublicKey::decode(&text)
        .map_err(|error| format!("{source}: the public key is not a minisign key: {error}"))?;
    Ok((key, source))
}

/// Checks `file`'s signature as the Gateway does before it applies an
/// update, and returns the signature as `latest.json` carries it.
fn verify(key: &minisign_verify::PublicKey, assets: &Path, file: &str) -> Result<String, String> {
    let signature_path = assets.join(format!("{file}{SIGNATURE_SUFFIX}"));
    let encoded = fs::read_to_string(&signature_path)
        .map_err(|error| format!("cannot read {}: {error}", signature_path.display()))?;
    let encoded = encoded.trim().to_owned();
    let signature = minisign_verify::Signature::decode(&base64_text(&encoded, "signature")?)
        .map_err(|error| format!("the signature is not a minisign signature: {error}"))?;
    let required = format!("version:{VERSION}");
    if !signature
        .trusted_comment()
        .split('\t')
        .any(|field| field == required)
    {
        return Err(format!(
            "the signature's trusted comment must carry \"{required}\"; it reads \"{}\"",
            signature.trusted_comment()
        ));
    }
    let mut stream = key
        .verify_stream(&signature)
        .map_err(|error| reason(&error))?;
    read_chunks(&assets.join(file), |chunk| stream.update(chunk))?;
    stream.finalize().map_err(|error| reason(&error))?;
    Ok(encoded)
}

fn reason(error: &minisign_verify::Error) -> String {
    match error {
        minisign_verify::Error::UnexpectedKeyId => {
            "the signature was made with another key".to_owned()
        }
        minisign_verify::Error::InvalidSignature => {
            "the signature does not match the file".to_owned()
        }
        other => format!("the signature cannot be checked: {other}"),
    }
}

fn base64_text(encoded: &str, what: &str) -> Result<String, String> {
    let bytes = STANDARD
        .decode(encoded.trim())
        .map_err(|error| format!("the {what} is not standard base64: {error}"))?;
    String::from_utf8(bytes).map_err(|_| format!("the {what} does not decode to text"))
}

fn sha256_hex(path: &Path) -> Result<String, String> {
    let mut hasher = Sha256::new();
    read_chunks(path, |chunk| hasher.update(chunk))?;
    let mut hex = String::with_capacity(64);
    for byte in hasher.finalize() {
        hex.push(char::from(HEX[usize::from(byte >> 4)]));
        hex.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Ok(hex)
}

fn read_chunks(path: &Path, mut consume: impl FnMut(&[u8])) -> Result<(), String> {
    let mut file =
        fs::File::open(path).map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    let mut buffer = vec![0_u8; 1 << 20];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        if read == 0 {
            return Ok(());
        }
        consume(&buffer[..read]);
    }
}

fn write_file(path: &Path, contents: &str) -> Result<(), String> {
    fs::write(path, contents).map_err(|error| format!("cannot write {}: {error}", path.display()))
}

/// RFC 3339 in UTC, to the second, for `seconds` since the Unix epoch.
pub(crate) fn pub_date(seconds: u64) -> String {
    let of_day = seconds % 86_400;
    format!(
        "{}T{:02}:{:02}:{:02}Z",
        utc_date(seconds),
        of_day / 3_600,
        of_day % 3_600 / 60,
        of_day % 60
    )
}

#[cfg(test)]
#[path = "tests/manifest.rs"]
mod tests;
