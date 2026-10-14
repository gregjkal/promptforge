//! The swap by rename. A running process keeps executing its open file, so
//! the installed item moves aside and the staged one takes its place while
//! the gateway still serves.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use super::UpdateError;

/// `<name>.previous` beside `installed`: where the replaced item waits
/// until the succession is known to have worked.
pub(crate) fn previous_path(installed: &Path) -> PathBuf {
    let mut name = installed
        .file_name()
        .map(OsString::from)
        .unwrap_or_default();
    name.push(gateway_api_discovery::SUPERSEDED_SUFFIX);
    installed.with_file_name(name)
}

/// Moves `installed` to [`previous_path`] and `staged` into its place.
///
/// # Errors
/// Returns [`UpdateError::Swap`] when either rename fails; a failed second
/// rename moves the original back first, and [`UpdateError::Stranded`]
/// when that also fails.
pub(crate) fn swap(installed: &Path, staged: &Path) -> Result<(), UpdateError> {
    swap_with(installed, staged, |from, to| std::fs::rename(from, to))
}

/// [`swap`] with the rename injected, so a test can fail either one.
pub(crate) fn swap_with(
    installed: &Path,
    staged: &Path,
    mut rename: impl FnMut(&Path, &Path) -> io::Result<()>,
) -> Result<(), UpdateError> {
    let previous = previous_path(installed);
    let failed = |source| UpdateError::Swap {
        installed: installed.to_path_buf(),
        source,
    };
    // A leftover from an earlier update whose cleanup failed.
    match super::stage::remove_item(&previous) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(failed(error)),
        _ => {}
    }
    rename(installed, &previous).map_err(failed)?;
    if let Err(error) = rename(staged, installed) {
        if let Err(restore) = rename(&previous, installed) {
            return Err(UpdateError::Stranded {
                installed: installed.to_path_buf(),
                previous,
                reason: format!("{error}; moving it back: {restore}"),
            });
        }
        return Err(failed(error));
    }
    Ok(())
}

/// Puts the replaced item back: the installed item moves into
/// `rejected`, then [`previous_path`] returns to `installed`.
///
/// # Errors
/// Returns the failing rename's error. When the second rename fails, the
/// new item is moved back so something stays installed.
pub(crate) fn restore(installed: &Path, rejected: &Path) -> io::Result<()> {
    restore_with(installed, rejected, |from, to| std::fs::rename(from, to))
}

/// [`restore`] with the rename injected.
pub(crate) fn restore_with(
    installed: &Path,
    rejected: &Path,
    mut rename: impl FnMut(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    rename(installed, rejected)?;
    if let Err(error) = rename(&previous_path(installed), installed) {
        if let Err(back) = rename(rejected, installed) {
            tracing::error!(
                "could not move {} back to {} after a failed restore: {back}",
                rejected.display(),
                installed.display()
            );
        }
        return Err(error);
    }
    Ok(())
}
