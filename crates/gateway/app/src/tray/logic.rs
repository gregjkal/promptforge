//! The platform-independent tray rules: the menu layout, the status
//! label, the icon phase machine, the launch-at-login entry, and the icon
//! tinting. Every backend consumes these so the idiom cannot drift between
//! platforms, and every rule is unit-tested here without a tray - CI is
//! headless.

#[cfg(any(target_os = "windows", target_os = "macos", test))]
use std::path::Path;

use crate::runner::ServeOptions;
use crate::update::notice::NoticeState;

/// The serve options a tray run uses: the caller's, with the update
/// notice on, since the tray is what shows and installs an update.
pub(crate) fn tray_options(options: &ServeOptions) -> ServeOptions {
    let mut options = options.clone();
    options.update_notice = true;
    options
}

/// The tray icon's visual phases: grayed while starting, steady while
/// running, distinct on error. The status label and tooltip use the
/// matching word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrayPhase {
    /// No status poll has reported yet.
    Starting,
    /// The gateway is serving.
    Running,
    /// The serve loop has stopped.
    Error,
}

/// What one status poll observed about the gateway thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Poll {
    /// The gateway thread is alive and serving.
    Serving,
    /// The gateway thread has exited. Shutdown arrives only through the
    /// tray, so a finished thread means the serve loop failed.
    Stopped,
}

/// Maps a poll to the icon phase. The machine is memoryless on purpose:
/// the phase is recomputed from every poll and can never latch a stale
/// state, which is the "starting forever" bug class of event-driven tray
/// icons.
pub(crate) fn next_phase(poll: Poll) -> TrayPhase {
    match poll {
        Poll::Serving => TrayPhase::Running,
        Poll::Stopped => TrayPhase::Error,
    }
}

/// The status label at the top of the menu, also used as the tooltip.
/// While the gateway serves, the label reads the activity hub: a busy hub
/// reports its text ("Running - Downloading qwen 34%"), which includes an
/// update's download. An idle hub reports a newer release or a failed
/// update from the update `notice`, else the loaded models, e.g.
/// "Running - 2 models, 4.1 GB", with the VRAM total omitted when no local
/// or STT model declares any. The icon tints stay phase-driven by the
/// serve poll (grayed Starting, steady Running, red Error): the hub says
/// nothing about a stopped gateway, so the phase machine keeps the icon.
pub(crate) fn status_label(
    phase: TrayPhase,
    busy_text: Option<&str>,
    models: usize,
    vram_gb: f64,
    notice: &NoticeState,
) -> String {
    match phase {
        TrayPhase::Starting => "Starting".to_owned(),
        TrayPhase::Error => "Error - serving stopped".to_owned(),
        TrayPhase::Running => {
            if let Some(text) = busy_text {
                return format!("Running - {text}");
            }
            match notice {
                NoticeState::Available(version) => {
                    return format!("Running - Update available: {version}");
                }
                NoticeState::Failed(reason) | NoticeState::Stranded(reason) => {
                    return format!("Running - Update failed: {reason}");
                }
                _ => {}
            }
            let models = match models {
                1 => "1 model".to_owned(),
                n => format!("{n} models"),
            };
            if vram_gb > 0.0 {
                format!("Running - {models}, {vram_gb:.1} GB")
            } else {
                format!("Running - {models}")
            }
        }
    }
}

/// The status label cut to fit a Windows tray tooltip, which holds at
/// most 128 UTF-16 units with its terminating null
/// (<https://learn.microsoft.com/en-us/windows/win32/api/shellapi/ns-shellapi-notifyicondataw>).
/// A longer label, such as a failed update's full reason, would lose the
/// terminator; it ends in an ellipsis instead, and the menu's status item
/// keeps the whole text.
#[cfg(any(target_os = "windows", test))]
pub(crate) fn windows_tooltip(label: &str) -> std::borrow::Cow<'_, str> {
    const MAX_UNITS: usize = 127;
    if label.encode_utf16().count() <= MAX_UNITS {
        return std::borrow::Cow::Borrowed(label);
    }
    let mut units = 0;
    let mut cut = String::new();
    for character in label.chars() {
        units += character.len_utf16();
        // One unit stays free for the ellipsis.
        if units > MAX_UNITS - 1 {
            break;
        }
        cut.push(character);
    }
    cut.push('\u{2026}');
    std::borrow::Cow::Owned(cut)
}

/// One tray menu entry, in display order. The backend materializes the
/// spec into native menu items; the layout is data so the idiom is
/// testable without a tray.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MenuItemSpec {
    /// The disabled status label on top.
    Status(String),
    /// Launches the workshop shell; disabled when no installed Workshop is
    /// found.
    Workshop {
        /// Whether the lookup found the installed Workshop.
        enabled: bool,
    },
    /// Opens the config SPA in the browser.
    Settings,
    /// Checks for and installs a gateway update; its text follows the
    /// update notice ([`update_item`]).
    Update {
        /// The item's text.
        label: String,
        /// Whether a click may start the update flow.
        enabled: bool,
    },
    /// A separator.
    Separator,
    /// The launch-at-login check item; the checked state is read from the
    /// OS, never from local config.
    LaunchAtLogin {
        /// Whether the platform's login store is available to this
        /// process. macOS disables the item when the gateway is a bare
        /// executable inside another app's bundle, where
        /// `SMAppService.mainApp` registration would target the wrong
        /// program.
        enabled: bool,
        /// Whether the OS autostart entry exists.
        checked: bool,
    },
    /// Quits the gateway. Always last.
    Quit,
}

/// The tray menu layout: status label on top, then Workshop, Settings,
/// and the update item, Launch at Login between separators, Quit last.
pub(crate) fn menu_spec(
    status: &str,
    workshop_enabled: bool,
    login_enabled: bool,
    login_checked: bool,
    notice: &NoticeState,
) -> Vec<MenuItemSpec> {
    let (label, enabled) = update_item(notice);
    vec![
        MenuItemSpec::Status(status.to_owned()),
        MenuItemSpec::Workshop {
            enabled: workshop_enabled,
        },
        MenuItemSpec::Settings,
        MenuItemSpec::Update { label, enabled },
        MenuItemSpec::Separator,
        MenuItemSpec::LaunchAtLogin {
            enabled: login_enabled,
            checked: login_checked,
        },
        MenuItemSpec::Separator,
        MenuItemSpec::Quit,
    ]
}

/// The update item's text and enabled bit for a notice. The item stays in
/// the menu and changes text, since a native menu item cannot be hidden
/// and a displayed menu is never rebuilt. Any enabled click runs the whole
/// update flow, which installs a newer release at once: the user asked.
pub(crate) fn update_item(notice: &NoticeState) -> (String, bool) {
    match notice {
        NoticeState::Unchecked | NoticeState::Failed(_) => ("Check for Updates".to_owned(), true),
        NoticeState::Current(version) => (format!("Up to date ({version})"), true),
        NoticeState::Available(version) => (format!("Update available: {version}"), true),
        NoticeState::Busy => ("Updating".to_owned(), false),
        NoticeState::Stranded(_) => ("Update failed".to_owned(), false),
        NoticeState::WorkshopOwned => ("Updates come from Runvil Workshop".to_owned(), false),
    }
}

/// The OS's launch-at-login store, behind a seam so the toggle logic is
/// testable without touching the registry. The Windows implementation
/// reads and writes the HKCU Run key through `crate::boot::registry`.
pub(crate) trait RunKeyStore {
    /// Reads the stored login command line, or `None` when absent.
    fn read(&self) -> Option<String>;
    /// Writes the login command line, creating the entry when absent.
    ///
    /// # Errors
    /// Returns the OS error when the entry cannot be written.
    fn write(&mut self, command: &str) -> std::io::Result<()>;
    /// Deletes the login entry. Deleting an absent entry succeeds.
    ///
    /// # Errors
    /// Returns the OS error when the entry cannot be deleted.
    fn delete(&mut self) -> std::io::Result<()>;
}

/// Whether the OS autostart entry exists. The state comes from the OS
/// alone - never local config - because the user can revoke it
/// externally.
pub(crate) fn launch_at_login(store: &dyn RunKeyStore) -> bool {
    store.read().is_some()
}

/// The login command line for the gateway executable: the quoted path
/// (install paths contain spaces) plus `--login` - the bare invocation
/// serves, and `--login` marks a login-triggered start so it never opens
/// a browser. This is the Windows Run-key shape, whose
/// parser has no escape layer; the desktop-entry Exec shape is
/// `linux::exec_command`. Gated on its callers: the Windows and macOS
/// backends (macOS's store ignores the command but the call sites share
/// `set_launch_at_login`), plus the tests.
#[cfg(any(target_os = "windows", target_os = "macos", test))]
pub(crate) fn run_key_command(exe: &Path) -> String {
    format!("\"{}\" --login", exe.display())
}

/// Sets or clears the OS autostart entry, returning the state now in
/// effect. The command is the login command for the caller's platform
/// (`run_key_command` on Windows, `linux::exec_command` on Linux);
/// macOS's store ignores it.
///
/// # Errors
/// Returns the OS error when the entry cannot be written or deleted; the
/// reported state is then unchanged.
pub(crate) fn set_launch_at_login(
    store: &mut dyn RunKeyStore,
    command: &str,
    enable: bool,
) -> std::io::Result<bool> {
    if enable {
        store.write(command)?;
        Ok(true)
    } else {
        store.delete()?;
        Ok(false)
    }
}

#[cfg(any(target_os = "macos", test))]
pub(crate) mod macos;

#[cfg(any(target_os = "linux", test))]
pub(crate) mod linux;

/// The grayed variant of an RGBA icon: each pixel's luma with the alpha
/// untouched, for the Starting phase.
/// Gated on its callers, the Windows and Linux backends, plus the tests.
#[cfg(any(target_os = "windows", target_os = "linux", test))]
#[expect(
    clippy::cast_possible_truncation,
    reason = "the fixed-point luma sums to at most 255"
)]
pub(crate) fn grayed(rgba: &[u8]) -> Vec<u8> {
    tint(rgba, |r, g, b| {
        // Rec. 601 luma in fixed point.
        let luma = ((299 * u32::from(r) + 587 * u32::from(g) + 114 * u32::from(b)) / 1000) as u8;
        (luma, luma, luma)
    })
}

/// The error variant of an RGBA icon: red-dominant with the alpha
/// untouched, for the Error phase. `r / 2 + 128` cannot overflow.
/// Gated on its callers, the Windows and Linux backends, plus the tests.
#[cfg(any(target_os = "windows", target_os = "linux", test))]
pub(crate) fn error_tint(rgba: &[u8]) -> Vec<u8> {
    tint(rgba, |r, g, b| (r / 2 + 128, g / 3, b / 3))
}

/// Maps every pixel's RGB channels through `f`, preserving alpha.
#[cfg(any(target_os = "windows", target_os = "linux", test))]
fn tint(rgba: &[u8], f: impl Fn(u8, u8, u8) -> (u8, u8, u8)) -> Vec<u8> {
    debug_assert!(
        rgba.len().is_multiple_of(4),
        "an RGBA buffer is whole pixels"
    );
    rgba.as_chunks::<4>()
        .0
        .iter()
        .flat_map(|px| {
            let (r, g, b) = f(px[0], px[1], px[2]);
            [r, g, b, px[3]]
        })
        .collect()
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod status_tests;

#[cfg(test)]
mod update_tests;
