//! Unit tests for the installer's platform table, bundle collection, and the
//! Gateway updater archive.

use std::io::Read as _;

use flate2::read::GzDecoder;

use super::*;
use crate::installer::{Arch, Platform, System};
use crate::installer::{archive, collect};

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[test]
fn maps_each_supported_triple_to_its_platform() {
    for (target, system, arch) in [
        ("x86_64-pc-windows-msvc", System::Windows, Arch::X86_64),
        ("aarch64-pc-windows-msvc", System::Windows, Arch::Aarch64),
        ("aarch64-apple-darwin", System::MacOs, Arch::Aarch64),
        ("x86_64-apple-darwin", System::MacOs, Arch::X86_64),
        ("x86_64-unknown-linux-gnu", System::Linux, Arch::X86_64),
        ("aarch64-unknown-linux-gnu", System::Linux, Arch::Aarch64),
    ] {
        assert_eq!(
            Platform::from_triple(target),
            Ok(Platform { system, arch }),
            "{target}"
        );
    }
    for target in [
        "x86_64-pc-windows-gnu",
        "x86_64-unknown-linux-musl",
        "i686-pc-windows-msvc",
        "x86_64-unknown-freebsd",
        "x86_64-uwp-windows-msvc",
        "aarch64-apple-ios",
        "x86_64-unknown-linux-gnux32",
    ] {
        let error = Platform::from_triple(target).expect_err(target);
        assert!(error.contains(target), "{error}");
    }
}

#[test]
fn a_missing_or_duplicated_bundle_file_names_the_glob_and_directory() {
    let temp = tempfile::tempdir().expect("temporary root");
    let release = temp.path().join("release");
    let nsis = release.join("bundle").join("nsis");
    std::fs::create_dir_all(&nsis).expect("bundle directory");
    let windows = Platform {
        system: System::Windows,
        arch: Arch::X86_64,
    };

    let error = collect::collect(
        temp.path(),
        &release,
        windows,
        false,
        &temp.path().join("out"),
    )
    .expect_err("missing setup");
    assert!(error.contains("`*-setup.exe`"), "{error}");
    assert!(error.contains(&nsis.display().to_string()), "{error}");
    assert!(error.contains("found 0"), "{error}");

    write_file(&nsis.join("a-setup.exe"), b"a");
    write_file(&nsis.join("b-setup.exe"), b"b");
    let error = collect::collect(
        temp.path(),
        &release,
        windows,
        false,
        &temp.path().join("out"),
    )
    .expect_err("duplicate setup");
    assert!(error.contains("found 2"), "{error}");
}

#[test]
fn a_missing_signature_under_sign_names_the_file() {
    let temp = tempfile::tempdir().expect("temporary root");
    let release = temp.path().join("release");
    let setup = release
        .join("bundle")
        .join("nsis")
        .join(format!("PromptForge_{VERSION}_x64-setup.exe"));
    write_file(&setup, b"setup");

    let error = collect::collect(
        temp.path(),
        &release,
        Platform {
            system: System::Windows,
            arch: Arch::X86_64,
        },
        true,
        &temp.path().join("out"),
    )
    .expect_err("missing signature");

    assert!(
        error.contains(&collect::signature_path(&setup).display().to_string()),
        "{error}"
    );
    assert!(
        error.contains("missing from the Tauri bundle output"),
        "{error}"
    );
}

#[test]
fn collection_replaces_an_earlier_output() {
    let temp = tempfile::tempdir().expect("temporary root");
    let release = temp.path().join("release");
    let output = temp.path().join("out");
    write_file(&output.join("publish").join("stale.exe"), b"stale");
    write_file(
        &release
            .join("bundle")
            .join("nsis")
            .join(format!("PromptForge_{VERSION}_x64-setup.exe")),
        b"setup",
    );

    collect::collect(
        temp.path(),
        &release,
        Platform {
            system: System::Windows,
            arch: Arch::X86_64,
        },
        false,
        &output,
    )
    .expect("collect");

    assert!(!output.join("publish").join("stale.exe").exists());
}

/// Each entry's path, mode, and contents, in archive order.
fn archive_entries(archive: &Path) -> Vec<(PathBuf, u32, Vec<u8>)> {
    let file = std::fs::File::open(archive).expect("open archive");
    let mut tar = tar::Archive::new(GzDecoder::new(file));
    let mut entries = Vec::new();
    for entry in tar.entries().expect("entries") {
        let mut entry = entry.expect("entry");
        let path = entry.path().expect("path").into_owned();
        let mode = entry.header().mode().expect("mode");
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).expect("bytes");
        entries.push((path, mode, bytes));
    }
    entries
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("mode");
}

#[cfg(unix)]
#[test]
fn the_linux_gateway_archive_holds_one_executable_entry_with_the_same_bytes() {
    let temp = tempfile::tempdir().expect("temporary root");
    let binary = temp.path().join("promptforge-gateway");
    let contents = b"\x7fELF gateway bytes".repeat(1000);
    write_file(&binary, &contents);
    set_mode(&binary, 0o755);
    let archive = temp.path().join("gateway.tar.gz");

    archive::write_archive(&binary, &archive).expect("archive");

    assert_eq!(
        archive_entries(&archive),
        vec![(PathBuf::from("promptforge-gateway"), 0o755, contents)]
    );
}

#[cfg(unix)]
#[test]
fn a_bundle_tree_archives_sorted_at_the_root_with_its_modes() {
    let temp = tempfile::tempdir().expect("temporary root");
    let bundle = temp.path().join("PromptForge Gateway.app");
    let contents = bundle.join("Contents");
    write_file(&contents.join("Info.plist"), b"plist");
    write_file(
        &contents.join("MacOS").join("promptforge-gateway"),
        b"gateway",
    );
    set_mode(&contents.join("MacOS").join("promptforge-gateway"), 0o700);
    write_file(&contents.join("Resources").join("icon.icns"), b"icon");
    let archive = temp.path().join("gateway.tar.gz");

    archive::write_archive(&bundle, &archive).expect("archive");

    let root = PathBuf::from("PromptForge Gateway.app");
    let contents = root.join("Contents");
    assert_eq!(
        archive_entries(&archive),
        vec![
            (root.clone(), 0o755, Vec::new()),
            (contents.clone(), 0o755, Vec::new()),
            (contents.join("Info.plist"), 0o644, b"plist".to_vec()),
            (contents.join("MacOS"), 0o755, Vec::new()),
            (
                contents.join("MacOS").join("promptforge-gateway"),
                0o755,
                b"gateway".to_vec()
            ),
            (contents.join("Resources"), 0o755, Vec::new()),
            (
                contents.join("Resources").join("icon.icns"),
                0o644,
                b"icon".to_vec()
            ),
        ]
    );
}

#[cfg(unix)]
#[test]
fn a_symlink_in_the_payload_is_refused_naming_its_path() {
    let temp = tempfile::tempdir().expect("temporary root");
    let bundle = temp.path().join("PromptForge Gateway.app");
    write_file(&bundle.join("real"), b"real");
    let link = bundle.join("link");
    std::os::unix::fs::symlink("real", &link).expect("symlink");

    let error = archive::write_archive(&bundle, &temp.path().join("gateway.tar.gz"))
        .expect_err("symlink refused")
        .to_string();

    assert!(error.contains(&link.display().to_string()), "{error}");
}

/// A Tauri macOS bundle output and a release Gateway; with `sign`, also
/// Workshop's updater archive and its signature.
fn macos_release(release: &Path, sign: bool) {
    let bundle = release.join("bundle").join("macos");
    write_file(
        &bundle
            .join("PromptForge.app")
            .join("Contents")
            .join("MacOS")
            .join("promptforge-workshop"),
        b"workshop",
    );
    if sign {
        write_file(&bundle.join("PromptForge.app.tar.gz"), b"app archive");
        write_file(&bundle.join("PromptForge.app.tar.gz.sig"), b"app signature");
    }
    write_file(&release.join("promptforge-gateway"), b"gateway");
}

#[cfg(unix)]
#[test]
fn macos_payload_holds_the_two_bundles_and_the_signed_archive_the_gateway_bundle() {
    let temp = tempfile::tempdir().expect("temporary root");
    let release = temp.path().join("release");
    let output = temp.path().join("out");
    macos_release(&release, true);
    let platform = Platform {
        system: System::MacOs,
        arch: Arch::Aarch64,
    };

    let collected =
        collect::collect(&repository_root(), &release, platform, true, &output).expect("collect");

    let payload = output.join("payload");
    let mut names: Vec<_> = std::fs::read_dir(&payload)
        .expect("payload")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    names.sort();
    assert_eq!(names, ["PromptForge Gateway.app", "PromptForge.app"]);
    let archive = output.join("publish").join(format!(
        "promptforge-gateway_{VERSION}_darwin-aarch64.tar.gz"
    ));
    assert_eq!(
        collected.gateway_archive.as_deref(),
        Some(archive.as_path())
    );
    let executable = PathBuf::from("PromptForge Gateway.app")
        .join("Contents")
        .join("MacOS")
        .join("promptforge-gateway");
    let entries = archive_entries(&archive);
    assert_eq!(entries[0].0, PathBuf::from("PromptForge Gateway.app"));
    assert!(
        entries.iter().any(|(path, mode, bytes)| *path == executable
            && *mode == 0o755
            && bytes == b"gateway"),
        "{entries:?}"
    );
}

#[test]
fn macos_unsigned_payload_writes_no_archive() {
    let temp = tempfile::tempdir().expect("temporary root");
    let release = temp.path().join("release");
    let output = temp.path().join("out");
    macos_release(&release, false);
    let platform = Platform {
        system: System::MacOs,
        arch: Arch::X86_64,
    };

    let collected =
        collect::collect(&repository_root(), &release, platform, false, &output).expect("collect");

    assert_eq!(collected.gateway_archive, None);
    let payload = output.join("payload");
    assert!(payload.join("PromptForge Gateway.app").is_dir());
    assert!(!payload.join("promptforge-gateway").exists());
    assert_eq!(
        std::fs::read_dir(output.join("publish"))
            .expect("publish")
            .count(),
        0
    );
}
