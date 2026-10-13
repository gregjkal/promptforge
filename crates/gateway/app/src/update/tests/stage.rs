use std::path::{Path, PathBuf};

use crate::update::UpdateError;
use crate::update::stage::{Download, Install, STAGING_DIR_NAME, Staging, download, extract};
use crate::update::verify::Verifier;

use super::{TestKey, serve, tar_gz};

#[test]
fn a_bundled_executable_installs_as_its_bundle() {
    let exe = Path::new(
        "/Applications/PromptForge/PromptForge Gateway.app/Contents/MacOS/promptforge-gateway",
    );
    let install = Install::locate(exe);
    assert_eq!(
        install.item(),
        Path::new("/Applications/PromptForge/PromptForge Gateway.app")
    );
    assert_eq!(install.executable(), exe);
    assert_eq!(
        install.executable_in(Path::new("/stage/PromptForge Gateway.app")),
        Path::new("/stage/PromptForge Gateway.app/Contents/MacOS/promptforge-gateway")
    );
}

#[test]
fn a_bare_executable_installs_as_itself() {
    let exe = Path::new("/home/user/PromptForge/promptforge-gateway");
    let install = Install::locate(exe);
    assert_eq!(install.item(), exe);
    assert_eq!(
        install.executable_in(Path::new("/stage/promptforge-gateway")),
        Path::new("/stage/promptforge-gateway")
    );
}

#[test]
fn staging_starts_fresh_beside_the_install_and_goes_with_its_guard() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let install = Install::locate(&dir.path().join("promptforge-gateway"));
    let stale = dir.path().join(STAGING_DIR_NAME).join("leftover");
    std::fs::create_dir_all(&stale).expect("an earlier update's leftover");

    let staging = Staging::create(&install).expect("stage");
    assert!(!stale.exists(), "an earlier staging is cleared");
    assert_eq!(
        staging.archive().parent(),
        Some(&*dir.path().join(STAGING_DIR_NAME))
    );
    drop(staging);
    assert!(
        !dir.path().join(STAGING_DIR_NAME).exists(),
        "a failed update removes its staging"
    );

    let kept = Staging::create(&install).expect("stage").into_path();
    assert!(kept.is_dir(), "the succession takes the staging over");
}

#[cfg(unix)]
#[test]
fn an_unwritable_location_is_named() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::TempDir::new().expect("tempdir");
    let locked = dir.path().join("locked");
    std::fs::create_dir(&locked).expect("create");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).expect("lock");
    let error = Staging::create(&Install::locate(&locked.join("promptforge-gateway")))
        .expect_err("a read-only directory");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).expect("unlock");
    assert!(
        matches!(&error, UpdateError::NotWritable { directory, .. } if *directory == locked.join(STAGING_DIR_NAME)),
        "{error}"
    );
}

fn extract_bytes(archive: &[u8]) -> (tempfile::TempDir, Result<PathBuf, UpdateError>) {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = dir.path().join("download.tar.gz");
    std::fs::write(&path, archive).expect("write the archive");
    let into = dir.path().join("extracted");
    let result = extract(&path, &into, "linux-x86_64-gateway");
    (dir, result)
}

#[test]
fn an_archive_extracts_to_its_one_top_level_item() {
    let archive = tar_gz(&[
        (
            "PromptForge Gateway.app/Contents/Info.plist",
            b"plist",
            0o644,
        ),
        (
            "PromptForge Gateway.app/Contents/MacOS/promptforge-gateway",
            b"binary",
            0o755,
        ),
    ]);
    let (dir, result) = extract_bytes(&archive);
    let item = result.expect("extract");
    assert_eq!(item, dir.path().join("extracted/PromptForge Gateway.app"));
    let exe = item.join("Contents/MacOS/promptforge-gateway");
    assert_eq!(std::fs::read(&exe).expect("read"), b"binary");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&exe)
            .expect("metadata")
            .permissions()
            .mode();
        assert_ne!(mode & 0o111, 0, "the executable bit survives");
    }
}

#[test]
fn an_archive_with_two_items_or_none_is_refused() {
    let (_dir, result) = extract_bytes(&tar_gz(&[("a", b"1", 0o644), ("b", b"2", 0o644)]));
    let error = result.expect_err("two items");
    assert!(error.to_string().contains("one top-level item"), "{error}");

    let (_dir, result) = extract_bytes(&tar_gz(&[]));
    let error = result.expect_err("empty");
    assert!(error.to_string().contains("empty"), "{error}");

    let (_dir, result) = extract_bytes(b"not gzip");
    assert!(result.is_err(), "not an archive");
}

/// A gzip of one raw tar header naming `path` with `kind`, written by hand
/// because `tar::Builder` refuses traversal paths.
fn raw_entry(path: &str, kind: tar::EntryType, link: &str) -> Vec<u8> {
    let mut header = tar::Header::new_old();
    header.as_old_mut().name[..path.len()].copy_from_slice(path.as_bytes());
    header.as_old_mut().linkname[..link.len()].copy_from_slice(link.as_bytes());
    header.set_size(0);
    header.set_mode(0o644);
    header.set_entry_type(kind);
    header.set_cksum();
    let mut tar = header.as_bytes().to_vec();
    tar.extend_from_slice(&[0; 1024]);
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut encoder, &tar).expect("gzip");
    encoder.finish().expect("finish")
}

#[test]
fn traversal_entries_and_links_are_refused() {
    for path in ["../escape", "/absolute", "item/../../escape"] {
        let (dir, result) = extract_bytes(&raw_entry(path, tar::EntryType::Regular, ""));
        let error = result.expect_err(path);
        assert!(
            error.to_string().contains("leaves the archive"),
            "{path}: {error}"
        );
        assert!(!dir.path().join("escape").exists(), "{path} wrote nothing");
    }
    for kind in [tar::EntryType::Symlink, tar::EntryType::Link] {
        let (_dir, result) = extract_bytes(&raw_entry("item", kind, "/etc/passwd"));
        let error = result.expect_err("a link");
        assert!(
            error.to_string().contains("only files and directories"),
            "{kind:?}: {error}"
        );
    }
}

#[cfg(unix)]
#[test]
fn the_staged_executable_must_print_the_advertised_version() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::TempDir::new().expect("tempdir");
    let exe = dir.path().join("promptforge-gateway");
    std::fs::write(&exe, "#!/bin/sh\necho 'promptforge-gateway 0.4.0'\n").expect("write");
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    crate::update::stage::require_version(&exe, "0.4.0").expect("the advertised version");
    let error = crate::update::stage::require_version(&exe, "0.5.0").expect_err("another version");
    assert!(
        matches!(&error, UpdateError::StagedVersion { required, actual }
            if required == "promptforge-gateway 0.5.0" && actual == "promptforge-gateway 0.4.0"),
        "{error}"
    );
    let error = crate::update::stage::require_version(&dir.path().join("absent"), "0.4.0")
        .expect_err("nothing to run");
    assert!(error.to_string().contains("did not run"), "{error}");
}

#[tokio::test]
async fn a_download_lands_only_when_its_signature_verifies() {
    let key = TestKey::generate();
    let archive = b"the release archive".to_vec();
    let address = serve(vec![("/gateway.tar.gz", 200, archive.clone())]).await;
    let url = format!("http://{address}/gateway.tar.gz");
    let hub = gateway_progress::ProgressHub::new();
    let activity = hub.begin("Downloading");
    let dir = tempfile::TempDir::new().expect("tempdir");
    let to = dir.path().join("download.tar.gz");

    let good = Verifier::new(&key.public(), &key.sign_version(&archive, "0.4.0"), "0.4.0")
        .expect("decode");
    download(
        Download {
            url: &url,
            key: "linux-x86_64-gateway",
            verifier: &good,
            activity: &activity,
            label: "Downloading".to_owned(),
        },
        &to,
    )
    .await
    .expect("a signed download");
    assert_eq!(std::fs::read(&to).expect("read"), archive);
    assert_eq!(hub.current().text, "Downloading 100%");

    let bad = Verifier::new(&key.public(), &key.sign_version(b"other", "0.4.0"), "0.4.0")
        .expect("decode");
    let error = download(
        Download {
            url: &url,
            key: "linux-x86_64-gateway",
            verifier: &bad,
            activity: &activity,
            label: "Downloading".to_owned(),
        },
        &to,
    )
    .await
    .expect_err("a signature over other bytes");
    assert!(
        matches!(
            &error,
            UpdateError::Signature {
                key: "linux-x86_64-gateway",
                ..
            }
        ),
        "{error}"
    );
}
