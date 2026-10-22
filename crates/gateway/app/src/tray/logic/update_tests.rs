//! The tray's update surfaces: the update item per notice state, its place
//! in the menu, and the status line each notice state reads.

use super::*;

#[test]
fn a_tray_run_turns_the_update_notice_on() {
    let options = ServeOptions::new(None, None::<crate::ProfileName>);
    assert!(!options.update_notice, "headless runs never check");
    let tray = tray_options(&options);
    assert!(tray.update_notice, "a tray run checks for updates");
    assert_eq!(tray.config_path, options.config_path);
}

#[test]
fn the_update_item_follows_every_notice_state() {
    let cases = [
        (NoticeState::Unchecked, "Check for Updates", true),
        (
            NoticeState::Failed("cannot fetch the release manifest".to_owned()),
            "Check for Updates",
            true,
        ),
        (
            NoticeState::Current("0.3.0".to_owned()),
            "Up to date (0.3.0)",
            true,
        ),
        (
            NoticeState::Available("0.4.0".to_owned()),
            "Update available: 0.4.0",
            true,
        ),
        (NoticeState::Busy, "Updating", false),
        (
            NoticeState::Stranded("move it to".to_owned()),
            "Update failed",
            false,
        ),
        (
            NoticeState::WorkshopOwned,
            "Updates come from Runvil Workshop",
            false,
        ),
    ];
    for (notice, label, enabled) in cases {
        assert_eq!(
            update_item(&notice),
            (label.to_owned(), enabled),
            "{notice:?}"
        );
    }
}

#[test]
fn the_update_item_sits_after_settings_before_the_first_separator() {
    let notice = NoticeState::Available("0.4.0".to_owned());
    let spec = menu_spec("Running", true, true, false, &notice);
    let settings = spec
        .iter()
        .position(|item| *item == MenuItemSpec::Settings)
        .expect("a Settings item");
    assert_eq!(
        spec[settings + 1],
        MenuItemSpec::Update {
            label: "Update available: 0.4.0".to_owned(),
            enabled: true,
        }
    );
    assert_eq!(
        spec[settings + 2],
        MenuItemSpec::Separator,
        "the first separator follows the update item"
    );
    assert!(
        !spec[..settings].contains(&MenuItemSpec::Separator),
        "no separator comes before it"
    );
}

#[test]
fn the_status_line_reads_each_notice_state_when_idle() {
    let label = |notice: &NoticeState| status_label(TrayPhase::Running, None, 2, 0.0, notice);
    assert_eq!(
        label(&NoticeState::Available("0.4.0".to_owned())),
        "Running - Update available: 0.4.0"
    );
    assert_eq!(
        label(&NoticeState::Failed(
            "cannot fetch the release manifest".to_owned()
        )),
        "Running - Update failed: cannot fetch the release manifest"
    );
    assert_eq!(
        label(&NoticeState::Stranded("move it to".to_owned())),
        "Running - Update failed: move it to"
    );
    for notice in [
        NoticeState::Unchecked,
        NoticeState::Current("0.3.0".to_owned()),
        NoticeState::Busy,
        NoticeState::WorkshopOwned,
    ] {
        assert_eq!(
            label(&notice),
            "Running - 2 models",
            "{notice:?} keeps the model summary"
        );
    }
}

#[test]
fn a_long_status_line_fits_the_windows_tooltip() {
    let short = "Running - Update failed: cannot fetch the release manifest";
    assert_eq!(windows_tooltip(short), short);
    let reason = "é".repeat(200);
    let label = format!("Running - Update failed: {reason}");
    let tip = windows_tooltip(&label);
    assert_eq!(tip.encode_utf16().count(), 127, "{tip}");
    assert!(tip.ends_with('\u{2026}'), "{tip}");
    assert!(label.starts_with(tip.trim_end_matches('\u{2026}')));
    let astral = "😀".repeat(100);
    let tip = windows_tooltip(&astral);
    assert_eq!(
        tip.chars().filter(|c| *c == '😀').count(),
        63,
        "63 two-unit characters and the ellipsis fill 127 units: {tip}"
    );
}

#[test]
fn busy_text_outranks_the_notice() {
    for notice in [
        NoticeState::Available("0.4.0".to_owned()),
        NoticeState::Failed("refused".to_owned()),
    ] {
        assert_eq!(
            status_label(
                TrayPhase::Running,
                Some("Downloading Runvil Gateway 0.4.0 34%"),
                2,
                0.0,
                &notice
            ),
            "Running - Downloading Runvil Gateway 0.4.0 34%",
            "{notice:?} waits behind the activity"
        );
    }
    assert_eq!(
        status_label(
            TrayPhase::Error,
            None,
            0,
            0.0,
            &NoticeState::Available("0.4.0".to_owned())
        ),
        "Error - serving stopped",
        "a stopped gateway reports the error, not the notice"
    );
}
