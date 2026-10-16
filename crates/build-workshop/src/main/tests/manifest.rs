//! The `manifest` mode: its arguments, the release file set, the
//! signature checks, `latest.json`, `SHA256SUMS`, and the date.

use std::fmt::Write as _;
use std::path::PathBuf;

use tempfile::TempDir;

use super::*;
use crate::args::{Request, parse_arguments};

const BASE_URL: &str = "https://example.invalid/releases/download/v1";

fn key_pair() -> minisign::KeyPair {
    minisign::KeyPair::generate_unencrypted_keypair().expect("generate a key pair")
}

/// The public key as `tauri.conf.json` and `tauri signer generate` write it.
fn public(pair: &minisign::KeyPair) -> String {
    STANDARD.encode(pair.pk.to_box().expect("box the key").into_string())
}

/// A signature as the Tauri CLI writes a `.sig` file.
fn sign(pair: &minisign::KeyPair, data: &[u8], trusted: &str) -> String {
    let signature =
        minisign::sign(Some(&pair.pk), &pair.sk, data, Some(trusted), None).expect("sign");
    STANDARD.encode(signature.into_string())
}

fn release_comment(file: &str) -> String {
    format!("timestamp:1\tfile:{file}\tversion:{VERSION}")
}

/// A complete release signed with `pair`, beside a workspace whose
/// `tauri.conf.json` trusts `pair`.
struct Release {
    _temp: TempDir,
    assets: PathBuf,
    workspace: PathBuf,
    pair: minisign::KeyPair,
}

impl Release {
    fn new() -> Release {
        let temp = tempfile::tempdir().expect("temp dir");
        let assets = temp.path().join("assets");
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&assets).expect("assets dir");
        let pair = key_pair();
        let release = Release {
            _temp: temp,
            assets,
            workspace,
            pair,
        };
        release.trust(&public(&release.pair));
        let updaters = updaters().expect("updaters");
        for file in checksummed_files(&updaters) {
            fs::write(release.assets.join(&file), format!("contents of {file}")).expect("file");
        }
        for updater in &updaters {
            release.resign(
                &updater.file,
                &release_comment(&updater.file),
                &release.pair,
            );
        }
        release
    }

    /// Writes `key` as the workspace's `plugins.updater.pubkey`.
    fn trust(&self, key: &str) {
        let desktop = self.workspace.join("crates/workshop/desktop");
        fs::create_dir_all(&desktop).expect("desktop dir");
        let config = serde_json::json!({ "plugins": { "updater": { "pubkey": key } } });
        fs::write(desktop.join("tauri.conf.json"), config.to_string()).expect("config");
    }

    /// Signs `file` with `trusted` as the trusted comment; the trailing
    /// newline matches what the Tauri CLI leaves.
    fn resign(&self, file: &str, trusted: &str, pair: &minisign::KeyPair) {
        let data = fs::read(self.assets.join(file)).expect("read file");
        let signature = format!("{}\n", sign(pair, &data, trusted));
        fs::write(self.assets.join(format!("{file}.sig")), signature).expect("signature");
    }

    fn request(&self) -> ManifestRequest {
        ManifestRequest {
            assets: self.assets.clone(),
            base_url: format!("{BASE_URL}/"),
            public_key: None,
        }
    }

    fn write(&self) -> Result<(), String> {
        write(&self.request(), &self.workspace, 1_709_210_096)
    }

    fn manifest(&self) -> serde_json::Value {
        let text = fs::read_to_string(self.assets.join(MANIFEST)).expect("latest.json");
        serde_json::from_str(&text).expect("latest.json is JSON")
    }
}

fn gateway_archive(system: System, arch: Arch) -> String {
    gateway_archive_name(Platform { system, arch }).expect("a Gateway archive")
}

#[test]
fn the_complete_set_for_the_workspace_version_passes() {
    let release = Release::new();
    release.write().expect("a complete release");
    assert!(release.assets.join(MANIFEST).is_file());
    assert!(release.assets.join(CHECKSUMS).is_file());
    // A second run over its own output rewrites it.
    release.write().expect("a rerun");
}

#[test]
fn the_release_is_twenty_two_files() {
    let updaters = updaters().expect("updaters");
    assert_eq!(updaters.len(), 9);
    assert_eq!(checksummed_files(&updaters).len(), 13);
    assert_eq!(release_files(&updaters).len(), 22);
}

#[test]
fn a_missing_file_fails_naming_it() {
    let release = Release::new();
    let dmg = installer_name(RELEASE_PLATFORMS[1]);
    fs::remove_file(release.assets.join(&dmg)).expect("remove");
    let error = release.write().expect_err("a missing installer");
    assert!(
        error.contains(&format!("missing 1 of 22: {dmg}")),
        "{error}"
    );
    assert!(
        !release.assets.join(MANIFEST).exists(),
        "nothing is written"
    );
}

#[test]
fn an_extra_file_fails_naming_it() {
    let release = Release::new();
    fs::write(release.assets.join("notes.txt"), "stray").expect("stray file");
    let error = release.write().expect_err("an extra file");
    assert!(error.contains("unexpected: notes.txt"), "{error}");
}

#[test]
fn a_file_named_for_another_version_fails_naming_it() {
    let release = Release::new();
    let setup = installer_name(RELEASE_PLATFORMS[0]);
    let other = setup.replace(VERSION, "9.9.9");
    fs::rename(release.assets.join(&setup), release.assets.join(&other)).expect("rename");
    let error = release.write().expect_err("another version's file");
    assert!(
        error.contains(&format!("missing 1 of 22: {setup}")),
        "{error}"
    );
    assert!(error.contains(&format!("unexpected: {other}")), "{error}");
}

#[test]
fn a_wrong_key_fails_naming_every_platform_key_and_file() {
    let release = Release::new();
    release.trust(&public(&key_pair()));
    let error = release.write().expect_err("signed with another key");
    assert!(error.contains("9 of 9 updater signatures fail"), "{error}");
    for updater in updaters().expect("updaters") {
        assert!(
            error.contains(&format!(
                "{} ({}): the signature was made with another key",
                updater.key, updater.file
            )),
            "{error}"
        );
    }
}

#[test]
fn a_tampered_file_fails_naming_its_platform_key() {
    let release = Release::new();
    let appimage = workshop_updater_name(RELEASE_PLATFORMS[4]);
    fs::write(release.assets.join(&appimage), "tampered").expect("tamper");
    let error = release.write().expect_err("a tampered file");
    assert!(error.contains("1 of 9 updater signatures fail"), "{error}");
    assert!(
        error.contains(&format!(
            "linux-aarch64 ({appimage}): the signature does not match the file"
        )),
        "{error}"
    );
}

#[test]
fn a_trusted_comment_without_the_version_fails_naming_its_platform_key() {
    let release = Release::new();
    let archive = gateway_archive(System::MacOs, Arch::Aarch64);
    release.resign(
        &archive,
        &format!("timestamp:1\tfile:{archive}"),
        &release.pair,
    );
    let error = release.write().expect_err("no version field");
    assert!(
        error.contains(&format!(
            "darwin-aarch64-gateway ({archive}): the signature's trusted comment must carry \
             \"version:{VERSION}\""
        )),
        "{error}"
    );
}

#[test]
fn a_trusted_comment_with_another_version_fails_naming_its_platform_key() {
    let release = Release::new();
    let setup = installer_name(RELEASE_PLATFORMS[0]);
    release.resign(
        &setup,
        &format!("timestamp:1\tfile:{setup}\tversion:9.9.9"),
        &release.pair,
    );
    let error = release.write().expect_err("another version");
    assert!(
        error.contains(&format!(
            "windows-x86_64 ({setup}): the signature's trusted comment"
        )),
        "{error}"
    );
    assert!(error.contains("version:9.9.9"), "{error}");
}

#[test]
fn the_public_key_option_replaces_the_tauri_config_key() {
    let release = Release::new();
    let key_file = release.workspace.join("run.key.pub");
    fs::write(&key_file, public(&release.pair)).expect("key file");
    release.trust(&public(&key_pair()));
    let request = ManifestRequest {
        public_key: Some(key_file.clone()),
        ..release.request()
    };
    write(&request, &release.workspace, 0).expect("the key file's key verifies");

    let other = release.workspace.join("other.key.pub");
    fs::write(&other, public(&key_pair())).expect("other key file");
    let request = ManifestRequest {
        public_key: Some(other.clone()),
        ..release.request()
    };
    let error = write(&request, &release.workspace, 0).expect_err("the file's key is used");
    assert!(error.contains(&other.display().to_string()), "{error}");
}

#[test]
fn latest_json_carries_the_version_and_nine_entries() {
    let release = Release::new();
    release.write().expect("a complete release");
    let manifest = release.manifest();
    assert_eq!(manifest["version"], VERSION);
    assert_eq!(manifest["notes"], format!("PromptForge {VERSION}"));
    assert_eq!(manifest["pub_date"], "2024-02-29T12:34:56Z");
    let platforms = manifest["platforms"].as_object().expect("platforms");
    let keys: Vec<&str> = platforms.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        [
            "darwin-aarch64",
            "darwin-aarch64-gateway",
            "darwin-x86_64",
            "darwin-x86_64-gateway",
            "linux-aarch64",
            "linux-aarch64-gateway",
            "linux-x86_64",
            "linux-x86_64-gateway",
            "windows-x86_64",
        ]
    );
    for updater in updaters().expect("updaters") {
        let entry = &platforms[&updater.key];
        assert_eq!(entry["url"], format!("{BASE_URL}/{}", updater.file));
        let signature = fs::read_to_string(release.assets.join(format!("{}.sig", updater.file)))
            .expect("signature");
        assert_eq!(entry["signature"], signature.trim());
    }
}

#[test]
fn every_gateway_reads_its_entry_by_the_discovery_key() {
    let release = Release::new();
    release.write().expect("a complete release");
    let manifest = release.manifest();
    for (os, arch, system, archive_arch) in [
        ("macos", "aarch64", System::MacOs, Arch::Aarch64),
        ("macos", "x86_64", System::MacOs, Arch::X86_64),
        ("linux", "x86_64", System::Linux, Arch::X86_64),
        ("linux", "aarch64", System::Linux, Arch::Aarch64),
    ] {
        let key = update_platform_key(os, arch).expect("a published key");
        assert_eq!(
            manifest["platforms"][key]["url"],
            format!("{BASE_URL}/{}", gateway_archive(system, archive_arch)),
            "{os} on {arch}"
        );
    }
    let windows = update_platform_key("windows", "x86_64").expect("a published key");
    assert_eq!(
        manifest["platforms"][windows]["url"],
        format!("{BASE_URL}/{}", installer_name(RELEASE_PLATFORMS[0])),
        "the Windows gateway updates from the setup"
    );
}

#[test]
fn sha256sums_lists_thirteen_sorted_files_with_their_digests() {
    let release = Release::new();
    release.write().expect("a complete release");
    let text = fs::read_to_string(release.assets.join(CHECKSUMS)).expect("SHA256SUMS");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 13);
    let mut names = Vec::new();
    for line in lines {
        let (digest, name) = line.split_once("  ").expect("two-space separator");
        let data = fs::read(release.assets.join(name)).expect("listed file");
        let expected = Sha256::digest(&data)
            .iter()
            .fold(String::new(), |mut hex, byte| {
                write!(hex, "{byte:02x}").expect("write to a String");
                hex
            });
        assert_eq!(digest, expected, "{name}");
        names.push(name.to_owned());
    }
    // The installers and the other updater files, sorted, with no signature.
    assert_eq!(names, checksummed_files(&updaters().expect("updaters")));
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
}

#[test]
fn every_release_name_is_url_safe() {
    for name in release_files(&updaters().expect("updaters")) {
        assert!(
            name.chars()
                .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character)),
            "{name}"
        );
    }
}

#[test]
fn a_base_url_without_a_host_is_refused() {
    let release = Release::new();
    let request = ManifestRequest {
        base_url: "releases/v1".to_owned(),
        ..release.request()
    };
    let error = write(&request, &release.workspace, 0).expect_err("no scheme");
    assert!(
        error.contains("`--base-url` must be an http or https URL"),
        "{error}"
    );
}

#[test]
fn pub_date_is_rfc_3339_in_utc() {
    assert_eq!(pub_date(0), "1970-01-01T00:00:00Z");
    assert_eq!(pub_date(1_709_210_096), "2024-02-29T12:34:56Z");
    assert_eq!(pub_date(1_767_225_599), "2025-12-31T23:59:59Z");
    assert_eq!(pub_date(1_767_225_600), "2026-01-01T00:00:00Z");
}

fn arguments(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn the_manifest_mode_parses() {
    let request = parse_arguments(&arguments(&[
        "manifest",
        "--assets",
        "dist",
        "--base-url",
        BASE_URL,
        "--public-key",
        "run.key.pub",
    ]))
    .expect("parses");
    assert_eq!(
        request,
        Request::Manifest(ManifestRequest {
            assets: PathBuf::from("dist"),
            base_url: BASE_URL.to_owned(),
            public_key: Some(PathBuf::from("run.key.pub")),
        })
    );
    let request = parse_arguments(&arguments(&[
        "manifest",
        "--base-url",
        BASE_URL,
        "--assets",
        "dist",
    ]))
    .expect("parses without a key file");
    assert!(
        matches!(
            request,
            Request::Manifest(ManifestRequest {
                public_key: None,
                ..
            })
        ),
        "{request:?}"
    );
}

#[test]
fn manifest_argument_errors_are_usage_errors() {
    for (args, message) in [
        (
            &["manifest", "--base-url", BASE_URL][..],
            "`manifest` needs `--assets <dir>`",
        ),
        (
            &["manifest", "--assets", "dist"][..],
            "`manifest` needs `--base-url <url>`",
        ),
        (
            &[
                "manifest",
                "--assets",
                "a",
                "--assets",
                "b",
                "--base-url",
                BASE_URL,
            ][..],
            "duplicate argument `--assets`",
        ),
        (
            &[
                "manifest",
                "--assets",
                "dist",
                "--base-url",
                BASE_URL,
                "--sign",
            ][..],
            "unsupported argument `--sign`",
        ),
        (
            &["manifest", "--assets", "--base-url", BASE_URL][..],
            "argument `--assets` needs a directory",
        ),
    ] {
        let error = parse_arguments(&arguments(args))
            .expect_err("a usage error")
            .to_string();
        assert!(error.contains(message), "{args:?}: {error}");
        assert!(error.contains("USAGE:"), "{args:?}: {error}");
    }
}
