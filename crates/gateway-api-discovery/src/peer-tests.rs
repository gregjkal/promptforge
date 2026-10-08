use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use super::{
    GATEWAY_BUNDLE_NAME, Layout, WORKSHOP_APPIMAGE_NAME, WORKSHOP_BUNDLE_NAME, bundle_exe,
    first_file, gateway_candidates, installed_gateway, installed_workshop, running_appimage,
    translocated, workshop_candidates,
};

/// Plants an empty file at `root/relative`, creating its directories.
fn plant(root: &Path, relative: &Path) -> PathBuf {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().expect("a planted file has a parent"))
        .expect("create fixture directories");
    std::fs::write(&path, b"").expect("plant fixture file");
    path
}

fn gateway_for(layout: Layout, workshop_exe: &Path, appimage: Option<&Path>) -> Option<PathBuf> {
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
        gateway_for(Layout::Linux, &mount, Some(appimage.as_path())),
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
        gateway_for(Layout::Linux, &workshop, Some(appimage.as_path())),
        Some(installed)
    );
}

#[test]
fn an_appimage_counts_only_when_workshop_runs_from_its_mount() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let mount = root.path().join(".mount_PromptXYZ");
    let workshop = mount.join("usr").join("bin").join("promptforge-workshop");
    let appimage = root.path().join("PromptForge").join(WORKSHOP_APPIMAGE_NAME);
    let other_mount = root.path().join(".mount_CodeXYZ");
    let other_appimage = root.path().join("Apps").join("Code.AppImage");
    let found = |appimage: &OsStr, appdir: &OsStr| {
        running_appimage(&workshop, Some(appimage), Some(appdir))
    };

    assert_eq!(
        found(appimage.as_os_str(), mount.as_os_str()),
        Some(appimage.clone())
    );
    assert_eq!(
        found(other_appimage.as_os_str(), other_mount.as_os_str()),
        None,
        "an $APPIMAGE inherited from another AppImage is ignored"
    );
    assert_eq!(
        found(OsStr::new(WORKSHOP_APPIMAGE_NAME), mount.as_os_str()),
        None,
        "a relative $APPIMAGE is ignored"
    );
    assert_eq!(
        found(OsStr::new(""), mount.as_os_str()),
        None,
        "empty $APPIMAGE"
    );
    assert_eq!(
        found(appimage.as_os_str(), OsStr::new("")),
        None,
        "empty $APPDIR"
    );
    assert_eq!(
        running_appimage(&workshop, Some(appimage.as_os_str()), None),
        None
    );
    assert_eq!(
        running_appimage(&workshop, None, Some(mount.as_os_str())),
        None
    );
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
    for layout in [Layout::Windows, Layout::MacOs, Layout::Linux] {
        let root = tempfile::TempDir::new().expect("tempdir");
        let workshop = plant(root.path(), Path::new(layout.workshop_exe()));
        let gateway = plant(root.path(), Path::new(layout.gateway_exe()));
        assert_eq!(
            workshop_for(layout, &gateway),
            Some(workshop.clone()),
            "{layout:?} control: both peers present"
        );
        std::fs::remove_file(&workshop).expect("remove workshop");
        std::fs::remove_file(&gateway).expect("remove gateway");
        let lone_gateway = plant(
            &root.path().join("gateway"),
            Path::new(layout.gateway_exe()),
        );
        let lone_workshop = plant(
            &root.path().join("workshop"),
            Path::new(layout.workshop_exe()),
        );

        assert_eq!(workshop_for(layout, &lone_gateway), None, "{layout:?}");
        assert_eq!(
            gateway_for(layout, &lone_workshop, None),
            None,
            "{layout:?}"
        );
    }
}

#[test]
fn a_missing_install_directory_is_none() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let absent = root.path().join("absent").join("promptforge-gateway");
    assert_eq!(installed_workshop(&absent), None);
    assert_eq!(installed_gateway(&absent, None), None);
}
