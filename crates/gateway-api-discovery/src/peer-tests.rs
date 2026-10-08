use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use super::{
    GATEWAY_BUNDLE_NAME, Layout, WORKSHOP_APPIMAGE_NAME, WORKSHOP_BUNDLE_NAME, bundle_exe,
    first_file, gateway_candidates, installed_gateway, installed_workshop, translocated,
    workshop_candidates,
};

/// Plants an empty file at `root/relative`, creating its directories.
fn plant(root: &Path, relative: &Path) -> PathBuf {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().expect("a planted file has a parent"))
        .expect("create fixture directories");
    std::fs::write(&path, b"").expect("plant fixture file");
    path
}

fn gateway_for(layout: Layout, workshop_exe: &Path, appimage: Option<&OsStr>) -> Option<PathBuf> {
    first_file(gateway_candidates(layout, workshop_exe, appimage))
}

fn workshop_for(layout: Layout, gateway_exe: &Path) -> Option<PathBuf> {
    first_file(workshop_candidates(layout, gateway_exe))
}

#[test]
fn windows_peers_sit_beside_each_other() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let workshop = plant(root.path(), Path::new("promptforge-workshop.exe"));
    let gateway = plant(root.path(), Path::new("promptforge-gateway.exe"));

    assert_eq!(
        gateway_for(Layout::Windows, &workshop, None),
        Some(gateway.clone())
    );
    assert_eq!(workshop_for(Layout::Windows, &gateway), Some(workshop));
}

#[test]
fn a_development_tree_finds_both_peers_beside_each_other() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let debug = Path::new("target").join("debug");
    let workshop = plant(root.path(), &debug.join("promptforge-workshop"));
    let gateway = plant(root.path(), &debug.join("promptforge-gateway"));

    for layout in [Layout::MacOs, Layout::Linux] {
        assert_eq!(
            gateway_for(layout, &workshop, None),
            Some(gateway.clone()),
            "{layout:?}"
        );
        assert_eq!(
            workshop_for(layout, &gateway),
            Some(workshop.clone()),
            "{layout:?}"
        );
    }
}

#[test]
fn macos_peers_are_sibling_bundles() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let workshop = plant(
        root.path(),
        &bundle_exe(Path::new(""), WORKSHOP_BUNDLE_NAME, "promptforge-workshop"),
    );
    let gateway = plant(
        root.path(),
        &bundle_exe(Path::new(""), GATEWAY_BUNDLE_NAME, "promptforge-gateway"),
    );

    assert_eq!(
        gateway_for(Layout::MacOs, &workshop, None),
        Some(gateway.clone())
    );
    assert_eq!(workshop_for(Layout::MacOs, &gateway), Some(workshop));
}

#[test]
fn macos_bundle_lookup_needs_the_bundle_structure() {
    let root = tempfile::TempDir::new().expect("tempdir");
    plant(
        root.path(),
        &bundle_exe(Path::new(""), GATEWAY_BUNDLE_NAME, "promptforge-gateway"),
    );
    let loose = root
        .path()
        .join("Contents")
        .join("MacOS")
        .join("promptforge-workshop");

    assert_eq!(
        gateway_for(Layout::MacOs, &loose, None),
        None,
        "an executable outside a .app bundle has no bundle siblings"
    );
}

#[test]
fn linux_workshop_finds_the_gateway_beside_its_appimage() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let install = root.path().join("PromptForge");
    let appimage = plant(&install, Path::new(WORKSHOP_APPIMAGE_NAME));
    let gateway = plant(&install, Path::new("promptforge-gateway"));
    let mount = root
        .path()
        .join("mount")
        .join("usr")
        .join("bin")
        .join("promptforge-workshop");

    assert_eq!(
        gateway_for(Layout::Linux, &mount, Some(appimage.as_os_str())),
        Some(gateway)
    );
}

#[test]
fn linux_workshop_prefers_the_gateway_beside_its_appimage_to_one_in_its_mount() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let install = root.path().join("PromptForge");
    let appimage = plant(&install, Path::new(WORKSHOP_APPIMAGE_NAME));
    let installed = plant(&install, Path::new("promptforge-gateway"));
    let bin = root.path().join("mount").join("usr").join("bin");
    let workshop = plant(&bin, Path::new("promptforge-workshop"));
    plant(&bin, Path::new("promptforge-gateway"));

    assert_eq!(
        gateway_for(Layout::Linux, &workshop, Some(appimage.as_os_str())),
        Some(installed)
    );
}

#[test]
fn linux_workshop_ignores_a_relative_or_empty_appimage() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let workshop = root.path().join("bin").join("promptforge-workshop");

    for appimage in [WORKSHOP_APPIMAGE_NAME, ""] {
        assert_eq!(
            gateway_candidates(Layout::Linux, &workshop, Some(OsStr::new(appimage))),
            vec![root.path().join("bin").join("promptforge-gateway")],
            "$APPIMAGE {appimage:?} adds no candidate resolved against the working directory"
        );
    }
}

#[test]
fn translocation_is_read_from_the_executable_path() {
    assert!(translocated(Path::new(
        "/private/var/folders/xy/T/AppTranslocation/0A1B/d/PromptForge.app/Contents/MacOS/promptforge-workshop"
    )));
    assert!(!translocated(Path::new(
        "/Applications/PromptForge/PromptForge.app/Contents/MacOS/promptforge-workshop"
    )));
}

#[test]
fn linux_gateway_finds_the_appimage_by_name_in_its_own_directory() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let appimage = plant(root.path(), Path::new(WORKSHOP_APPIMAGE_NAME));
    let gateway = plant(root.path(), Path::new("promptforge-gateway"));

    assert_eq!(workshop_for(Layout::Linux, &gateway), Some(appimage));
}

#[test]
fn a_missing_peer_is_none_in_every_layout() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let workshop = plant(root.path(), Path::new("promptforge-workshop"));
    let gateway = plant(root.path(), Path::new("promptforge-gateway"));
    std::fs::remove_file(&workshop).expect("remove workshop");
    let alone = plant(
        &root.path().join("alone"),
        Path::new("promptforge-workshop"),
    );

    for layout in [Layout::Windows, Layout::MacOs, Layout::Linux] {
        assert_eq!(workshop_for(layout, &gateway), None, "{layout:?}");
        assert_eq!(gateway_for(layout, &alone, None), None, "{layout:?}");
    }
}

#[test]
fn a_missing_install_directory_is_none() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let absent = root.path().join("absent").join("promptforge-gateway");
    assert_eq!(installed_workshop(&absent), None);
    assert_eq!(installed_gateway(&absent, None), None);
}
