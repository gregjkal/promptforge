//! Unit tests for the installer's platform table, bundle collection, and the
//! Gateway updater archive.

use std::io::Read as _;

use flate2::read::GzDecoder;

use super::*;
use crate::installer::collect;
use crate::installer::{Arch, Platform, System};

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

    let error = collect::collect(&release, windows, false, &temp.path().join("out"))
        .expect_err("missing setup");
    assert!(error.contains("`*-setup.exe`"), "{error}");
    assert!(error.contains(&nsis.display().to_string()), "{error}");
    assert!(error.contains("found 0"), "{error}");

    write_file(&nsis.join("a-setup.exe"), b"a");
    write_file(&nsis.join("b-setup.exe"), b"b");
    let error = collect::collect(&release, windows, false, &temp.path().join("out"))
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

#[test]
fn the_gateway_archive_holds_one_executable_entry_with_the_same_bytes() {
    let temp = tempfile::tempdir().expect("temporary root");
    let binary = temp.path().join("promptforge-gateway");
    let contents = b"\x7fELF gateway bytes".repeat(1000);
    write_file(&binary, &contents);
    let archive = temp.path().join("gateway.tar.gz");

    collect::write_gateway_archive(&binary, &archive).expect("archive");

    let file = std::fs::File::open(&archive).expect("open archive");
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
    assert_eq!(
        entries,
        vec![(PathBuf::from("promptforge-gateway"), 0o755, contents)]
    );
}
