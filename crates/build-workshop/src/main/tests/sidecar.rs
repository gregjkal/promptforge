//! Unit tests for the sidecar's name, staging, and removal.

use super::*;

#[test]
fn names_the_sidecar_with_tauri_s_target_suffix() {
    let root = Path::new("repo");
    let binaries = root
        .join("crates")
        .join("workshop")
        .join("desktop")
        .join("binaries");
    for (target, binary, staged) in [
        (
            "x86_64-pc-windows-msvc",
            "promptforge-gateway.exe",
            "promptforge-gateway-x86_64-pc-windows-msvc.exe",
        ),
        (
            "x86_64-pc-windows-gnu",
            "promptforge-gateway.exe",
            "promptforge-gateway-x86_64-pc-windows-gnu.exe",
        ),
        (
            "x86_64-unknown-linux-gnu",
            "promptforge-gateway",
            "promptforge-gateway-x86_64-unknown-linux-gnu",
        ),
        (
            "aarch64-unknown-linux-gnu",
            "promptforge-gateway",
            "promptforge-gateway-aarch64-unknown-linux-gnu",
        ),
        (
            "aarch64-apple-darwin",
            "promptforge-gateway",
            "promptforge-gateway-aarch64-apple-darwin",
        ),
        (
            "x86_64-apple-darwin",
            "promptforge-gateway",
            "promptforge-gateway-x86_64-apple-darwin",
        ),
    ] {
        assert_eq!(sidecar::gateway_binary_name(target), binary, "{target}");
        assert_eq!(
            sidecar::sidecar_path(root, target),
            binaries.join(staged),
            "{target}"
        );
    }
}

#[test]
fn stages_a_copy_and_removes_it() {
    let temp = tempfile::tempdir().expect("temporary root");
    let root = temp.path().join("repo");
    let source = temp.path().join("build").join("promptforge-gateway");
    write_file(&source, b"gateway bytes");
    let target = "x86_64-unknown-linux-gnu";

    let staged = sidecar::stage(&root, target, &source).expect("stage");

    assert_eq!(staged, sidecar::sidecar_path(&root, target));
    assert_eq!(std::fs::read(&staged).expect("staged"), b"gateway bytes");
    assert!(source.is_file(), "staging moved the source");
    assert_eq!(sidecar::remove(&root, target).expect("remove"), staged);
    assert!(!staged.exists());
}

#[cfg(unix)]
#[test]
fn staging_keeps_the_execute_bit() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempfile::tempdir().expect("temporary root");
    let source = temp.path().join("promptforge-gateway");
    write_file(&source, b"gateway");
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o755))
        .expect("make executable");

    let staged =
        sidecar::stage(&temp.path().join("repo"), "aarch64-apple-darwin", &source).expect("stage");

    let mode = std::fs::metadata(staged)
        .expect("staged")
        .permissions()
        .mode();
    assert_eq!(mode & 0o111, 0o111, "{mode:o}");
}

#[test]
fn removing_an_absent_sidecar_succeeds() {
    let temp = tempfile::tempdir().expect("temporary root");
    let target = "x86_64-pc-windows-msvc";

    let removed = sidecar::remove(temp.path(), target).expect("remove");

    assert_eq!(removed, sidecar::sidecar_path(temp.path(), target));
}

#[test]
fn refuses_a_missing_source_a_directory_and_a_wrong_name() {
    let temp = tempfile::tempdir().expect("temporary root");
    let root = temp.path().join("repo");
    let target = "x86_64-pc-windows-msvc";
    let missing = temp.path().join("promptforge-gateway.exe");
    let directory = temp.path().join("dir").join("promptforge-gateway.exe");
    std::fs::create_dir_all(&directory).expect("directory");
    let misnamed = temp.path().join("promptforge-gateway");
    write_file(&misnamed, b"gateway");

    for (source, expected) in [
        (&missing, "does not exist"),
        (&directory, "is not a file"),
        (
            &misnamed,
            "must be named promptforge-gateway.exe for x86_64-pc-windows-msvc",
        ),
    ] {
        let error = sidecar::stage(&root, target, source).expect_err("refused source");
        assert!(error.contains(expected), "{error}");
        assert!(error.contains(&source.display().to_string()), "{error}");
    }
    assert!(!sidecar::sidecar_path(&root, target).exists());
}

#[test]
fn only_windows_targets_bundle_the_sidecar_and_the_mode_refuses_the_rest() {
    assert!(sidecar::bundles_sidecar("x86_64-pc-windows-msvc"));
    assert!(sidecar::bundles_sidecar("aarch64-pc-windows-msvc"));
    for target in [
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
    ] {
        assert!(!sidecar::bundles_sidecar(target), "{target}");
        for args in [
            arguments(&["sidecar", "stage", "--target", target, "--source", "g"]),
            arguments(&["sidecar", "remove", "--target", target]),
        ] {
            let error = parse_arguments(&args).expect_err(target).to_string();
            assert!(
                error.contains(&format!("needs a Windows target, got `{target}`")),
                "{error}"
            );
        }
    }
}
