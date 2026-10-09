//! Moves Tauri's bundle output and places the Gateway (on macOS as its own
//! bundle) into the installer output directory under stable names, and
//! writes the Gateway updater archive.
//!
//! `publish/` holds what a release uploads; `payload/` (macOS and Linux)
//! holds what the installer packages install, under their installed names.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use gateway_api_discovery::{GATEWAY_BUNDLE_NAME, WORKSHOP_APPIMAGE_NAME, WORKSHOP_BUNDLE_NAME};

use super::archive::write_archive;
use super::bundle::{self, BundleSources};
use super::{Arch, Platform, System, VERSION};

const PRODUCT: &str = "PromptForge";
pub(crate) const GATEWAY: &str = "promptforge-gateway";

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Collected {
    /// The unsigned Gateway updater archive, written only under `--sign`.
    pub(crate) gateway_archive: Option<PathBuf>,
}

/// `workspace_root` holds the sources of the macOS Gateway bundle.
pub(crate) fn collect(
    workspace_root: &Path,
    release: &Path,
    platform: Platform,
    sign: bool,
    output: &Path,
) -> Result<Collected, String> {
    match fs::remove_dir_all(output) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("cannot clear {}: {error}", output.display())),
    }
    let publish = output.join("publish");
    create_directory(&publish)?;
    let bundle = release.join("bundle").join(platform.bundle_directory());
    match platform.system {
        System::Windows => {
            let setup = single_file(&bundle, "-setup.exe")?;
            let published = publish.join(format!(
                "{PRODUCT}_{VERSION}_{}-setup.exe",
                windows_arch(platform.arch)
            ));
            if sign {
                move_path(&signature_path(&setup), &signature_path(&published))?;
            }
            move_path(&setup, &published)?;
            Ok(Collected {
                gateway_archive: None,
            })
        }
        System::MacOs => {
            let payload = output.join("payload");
            create_directory(&payload)?;
            if sign {
                let archive = bundle.join(format!("{PRODUCT}.app.tar.gz"));
                let published = publish.join(format!(
                    "{PRODUCT}_{VERSION}_{}.app.tar.gz",
                    arch_name(platform.arch)
                ));
                move_path(&signature_path(&archive), &signature_path(&published))?;
                move_path(&archive, &published)?;
            }
            move_path(
                &bundle.join(format!("{PRODUCT}.app")),
                &payload.join(WORKSHOP_BUNDLE_NAME),
            )?;
            let gateway = payload.join(GATEWAY_BUNDLE_NAME);
            bundle::assemble(
                &BundleSources::in_workspace(workspace_root),
                &release.join(GATEWAY),
                &gateway,
            )?;
            archive_gateway(&gateway, platform, sign, &publish)
        }
        System::Linux => {
            let payload = output.join("payload");
            create_directory(&payload)?;
            let appimage = single_file(&bundle, ".AppImage")?;
            if sign {
                let published = publish.join(format!(
                    "{PRODUCT}_{VERSION}_{}.AppImage",
                    linux_arch(platform.arch)
                ));
                move_path(&signature_path(&appimage), &signature_path(&published))?;
                copy_file(&appimage, &published)?;
            }
            move_path(&appimage, &payload.join(WORKSHOP_APPIMAGE_NAME))?;
            let gateway = payload.join(GATEWAY);
            copy_executable(&release.join(GATEWAY), &gateway)?;
            archive_gateway(&gateway, platform, sign, &publish)
        }
    }
}

pub(crate) fn signature_path(path: &Path) -> PathBuf {
    let mut signature = OsString::from(path.as_os_str());
    signature.push(".sig");
    PathBuf::from(signature)
}

/// Under `--sign`, writes the Gateway payload item, the binary or the
/// bundle, into the updater archive `gateway-update` swaps in.
fn archive_gateway(
    gateway: &Path,
    platform: Platform,
    sign: bool,
    publish: &Path,
) -> Result<Collected, String> {
    if !sign {
        return Ok(Collected {
            gateway_archive: None,
        });
    }
    let os = match platform.system {
        System::MacOs => "darwin",
        System::Linux => "linux",
        System::Windows => "windows",
    };
    let archive = publish.join(format!(
        "{GATEWAY}_{VERSION}_{os}-{}.tar.gz",
        arch_name(platform.arch)
    ));
    write_archive(gateway, &archive)
        .map_err(|error| format!("cannot write {}: {error}", archive.display()))?;
    Ok(Collected {
        gateway_archive: Some(archive),
    })
}

fn single_file(directory: &Path, suffix: &str) -> Result<PathBuf, String> {
    let entries = fs::read_dir(directory).map_err(|error| {
        format!(
            "cannot read the Tauri bundle directory {}: {error}",
            directory.display()
        )
    })?;
    let mut matches = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|error| format!("cannot read {}: {error}", directory.display()))?
            .path();
        let named = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(suffix));
        if named && path.is_file() {
            matches.push(path);
        }
    }
    match matches.as_slice() {
        [path] => Ok(path.clone()),
        _ => Err(format!(
            "expected exactly one `*{suffix}` in {}, found {}",
            directory.display(),
            matches.len()
        )),
    }
}

fn move_path(source: &Path, destination: &Path) -> Result<(), String> {
    if !source.exists() {
        return Err(format!(
            "{} is missing from the Tauri bundle output",
            source.display()
        ));
    }
    fs::rename(source, destination).map_err(|error| {
        format!(
            "cannot move {} to {}: {error}",
            source.display(),
            destination.display()
        )
    })
}

/// Copies a binary the installer packages install as a program, with mode
/// `0755` whatever the source's mode.
pub(crate) fn copy_executable(source: &Path, destination: &Path) -> Result<(), String> {
    copy_file(source, destination)?;
    set_executable(destination)
        .map_err(|error| format!("cannot make {} executable: {error}", destination.display()))
}

#[cfg(unix)]
fn set_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
}

/// Only macOS and Linux payloads hold the Gateway outside Workshop.
#[cfg(not(unix))]
fn set_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn copy_file(source: &Path, destination: &Path) -> Result<(), String> {
    fs::copy(source, destination).map(|_| ()).map_err(|error| {
        format!(
            "cannot copy {} to {}: {error}",
            source.display(),
            destination.display()
        )
    })
}

pub(crate) fn create_directory(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|error| format!("cannot create {}: {error}", path.display()))
}

fn arch_name(arch: Arch) -> &'static str {
    match arch {
        Arch::X86_64 => "x86_64",
        Arch::Aarch64 => "aarch64",
    }
}

/// Tauri's NSIS and AppImage file names use these spellings.
fn windows_arch(arch: Arch) -> &'static str {
    match arch {
        Arch::X86_64 => "x64",
        Arch::Aarch64 => "arm64",
    }
}

fn linux_arch(arch: Arch) -> &'static str {
    match arch {
        Arch::X86_64 => "amd64",
        Arch::Aarch64 => "aarch64",
    }
}
