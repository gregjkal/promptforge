//! Installed-peer lookup: where the gateway finds the Workshop it opens,
//! and where Workshop finds the gateway it launches.
//!
//! The installers lay the two programs out three ways:
//!
//! - Beside each other in one directory (Windows, development builds).
//! - On macOS, as sibling bundles in one directory: `PromptForge.app` and
//!   `PromptForge Gateway.app`, each running from `Contents/MacOS/`.
//! - On Linux, as `PromptForge.AppImage` beside `promptforge-gateway`.
//!   Workshop runs from the AppImage's mount, so it finds the gateway
//!   through `$APPIMAGE`, the path of the AppImage file, and looks there
//!   before its own directory: a gateway inside the mount loses its files
//!   when Workshop exits and the mount goes away. The gateway finds
//!   the AppImage by name in its own directory and never reads `$APPIMAGE`:
//!   a gateway started from its desktop entry or at login has none, and one
//!   Workshop launched may have inherited Workshop's.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The macOS Workshop bundle's directory name.
pub const WORKSHOP_BUNDLE_NAME: &str = "PromptForge.app";

/// The macOS gateway bundle's directory name.
pub const GATEWAY_BUNDLE_NAME: &str = "PromptForge Gateway.app";

/// The Linux Workshop AppImage's file name.
pub const WORKSHOP_APPIMAGE_NAME: &str = "PromptForge.AppImage";

/// The installer layout a lookup searches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "a build constructs only its own target's layout; tests construct all three"
    )
)]
enum Layout {
    Windows,
    MacOs,
    Linux,
}

impl Layout {
    #[cfg(target_os = "windows")]
    const CURRENT: Self = Self::Windows;
    #[cfg(target_os = "macos")]
    const CURRENT: Self = Self::MacOs;
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    const CURRENT: Self = Self::Linux;

    fn gateway_exe(self) -> &'static str {
        match self {
            Self::Windows => "promptforge-gateway.exe",
            Self::MacOs | Self::Linux => "promptforge-gateway",
        }
    }

    fn workshop_exe(self) -> &'static str {
        match self {
            Self::Windows => "promptforge-workshop.exe",
            Self::MacOs | Self::Linux => "promptforge-workshop",
        }
    }
}

/// Locates the installed gateway executable for the Workshop running from
/// `workshop_exe`.
///
/// `appimage` is Workshop's `$APPIMAGE`; a relative or empty value counts
/// as unset, so the lookup never resolves against the working directory.
/// Returns the first existing candidate: on Linux `promptforge-gateway`
/// beside the AppImage, then beside `workshop_exe`, then the sibling
/// gateway bundle on macOS.
#[must_use]
pub fn installed_gateway(workshop_exe: &Path, appimage: Option<&OsStr>) -> Option<PathBuf> {
    first_file(gateway_candidates(Layout::CURRENT, workshop_exe, appimage))
}

/// Locates the installed Workshop executable for the gateway running from
/// `gateway_exe`.
///
/// Returns the first existing candidate: beside `gateway_exe`, then the
/// sibling Workshop bundle on macOS, then `PromptForge.AppImage` beside
/// the gateway on Linux.
#[must_use]
pub fn installed_workshop(gateway_exe: &Path) -> Option<PathBuf> {
    first_file(workshop_candidates(Layout::CURRENT, gateway_exe))
}

fn first_file(candidates: Vec<PathBuf>) -> Option<PathBuf> {
    candidates.into_iter().find(|candidate| candidate.is_file())
}

fn gateway_candidates(
    layout: Layout,
    workshop_exe: &Path,
    appimage: Option<&OsStr>,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let appimage_dir = appimage
        .map(Path::new)
        .filter(|path| path.is_absolute())
        .and_then(Path::parent);
    if matches!(layout, Layout::Linux)
        && let Some(dir) = appimage_dir
    {
        candidates.push(dir.join(layout.gateway_exe()));
    }
    if let Some(dir) = workshop_exe.parent() {
        candidates.push(dir.join(layout.gateway_exe()));
    }
    if matches!(layout, Layout::MacOs)
        && let Some(dir) = bundle_dir(workshop_exe)
    {
        candidates.push(bundle_exe(dir, GATEWAY_BUNDLE_NAME, layout.gateway_exe()));
    }
    candidates
}

fn workshop_candidates(layout: Layout, gateway_exe: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let own_dir = gateway_exe.parent();
    if let Some(dir) = own_dir {
        candidates.push(dir.join(layout.workshop_exe()));
    }
    match layout {
        Layout::Windows => {}
        Layout::MacOs => {
            if let Some(dir) = bundle_dir(gateway_exe) {
                candidates.push(bundle_exe(dir, WORKSHOP_BUNDLE_NAME, layout.workshop_exe()));
            }
        }
        Layout::Linux => {
            if let Some(dir) = own_dir {
                candidates.push(dir.join(WORKSHOP_APPIMAGE_NAME));
            }
        }
    }
    candidates
}

/// The directory holding the `.app` bundle `exe` runs from, when `exe`
/// sits at `<dir>/<name>.app/Contents/MacOS/<exe>`.
fn bundle_dir(exe: &Path) -> Option<&Path> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    let is_bundle = macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension()? == "app";
    if is_bundle { bundle.parent() } else { None }
}

fn bundle_exe(dir: &Path, bundle: &str, exe: &str) -> PathBuf {
    dir.join(bundle).join("Contents").join("MacOS").join(exe)
}

#[cfg(test)]
#[path = "peer-tests.rs"]
mod tests;
