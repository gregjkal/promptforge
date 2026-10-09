//! The Gateway sidecar that Tauri's `externalBin` bundles: its path for a
//! target triple, staging a built Gateway binary there, and removing it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub(super) fn gateway_binary_name(target: &str) -> &'static str {
    if is_windows(target) {
        "promptforge-gateway.exe"
    } else {
        "promptforge-gateway"
    }
}

/// Tauri resolves `externalBin` entries with the target triple appended to
/// the file stem.
pub(super) fn sidecar_path(workspace_root: &Path, target: &str) -> PathBuf {
    let extension = if is_windows(target) { ".exe" } else { "" };
    workspace_root
        .join("crates")
        .join("workshop")
        .join("desktop")
        .join("binaries")
        .join(format!("promptforge-gateway-{target}{extension}"))
}

pub(super) fn stage(workspace_root: &Path, target: &str, source: &Path) -> Result<PathBuf, String> {
    let metadata = match fs::symlink_metadata(source) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(format!(
                "Gateway source binary does not exist: {}",
                source.display()
            ));
        }
        Err(error) => {
            return Err(format!(
                "Gateway source binary {} cannot be read: {error}",
                source.display()
            ));
        }
    };
    if !metadata.is_file() {
        return Err(format!(
            "Gateway source binary is not a file: {}",
            source.display()
        ));
    }
    let expected = gateway_binary_name(target);
    if source.file_name().and_then(|name| name.to_str()) != Some(expected) {
        return Err(format!(
            "Gateway source binary must be named {expected} for {target}, got {}",
            source.display()
        ));
    }
    let destination = sidecar_path(workspace_root, target);
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    fs::copy(source, &destination).map_err(|error| {
        format!(
            "cannot copy {} to {}: {error}",
            source.display(),
            destination.display()
        )
    })?;
    Ok(destination)
}

/// Removing a sidecar that was never staged succeeds, so cleanup can run
/// after any failure.
pub(super) fn remove(workspace_root: &Path, target: &str) -> Result<PathBuf, String> {
    let path = sidecar_path(workspace_root, target);
    match fs::remove_file(&path) {
        Ok(()) => Ok(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(path),
        Err(error) => Err(format!("cannot remove {}: {error}", path.display())),
    }
}

fn is_windows(target: &str) -> bool {
    target.split('-').any(|part| part == "windows")
}
