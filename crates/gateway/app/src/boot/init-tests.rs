use std::cell::Cell;
use std::path::Path;

use super::super::{CONFIG_FILE_NAME, Locations};
use super::{InstallerStt, init_in};

/// Fixed locations rooted at a tempdir; only the profile directory exists
/// in none of them, so first-run generation writes there.
fn locations(root: &Path) -> Locations {
    Locations {
        exe_dir: root.join("exe"),
        cwd: root.join("cwd"),
        home: root.join("home"),
    }
}

fn profile_config(root: &Path) -> std::path::PathBuf {
    root.join("home")
        .join(".promptforge")
        .join(CONFIG_FILE_NAME)
}

#[test]
fn init_writes_the_stt_config_and_provisions_its_models() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let mut provisioned = Vec::new();

    let path = init_in(
        None,
        || Ok(locations(temp.path())),
        InstallerStt::Included,
        None,
        |config| {
            provisioned = config
                .stt_models()
                .iter()
                .map(|model| model.name().to_owned())
                .collect();
            Ok(())
        },
    )
    .expect("init succeeds");

    assert_eq!(path, profile_config(temp.path()));
    let written = std::fs::read_to_string(&path).expect("read the generated config");
    assert!(written.contains("[[stt_model]]"), "{written}");
    assert_eq!(provisioned, ["whisper-base-en", "whisper-small-en"]);
}

#[test]
fn init_without_stt_writes_no_models_and_never_provisions() {
    let temp = tempfile::TempDir::new().expect("tempdir");

    let path = init_in(
        None,
        || Ok(locations(temp.path())),
        InstallerStt::Omitted,
        None,
        |_| panic!("--no-stt must never provision"),
    )
    .expect("init succeeds");

    let written = std::fs::read_to_string(&path).expect("read the generated config");
    assert!(!written.contains("[[stt_model]]"), "{written}");
}

#[test]
fn init_leaves_an_existing_config_byte_identical() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let first = init_in(
        None,
        || Ok(locations(temp.path())),
        InstallerStt::Omitted,
        None,
        |_| Ok(()),
    )
    .expect("first init succeeds");
    let before = std::fs::read(&first).expect("read the config");

    let provisions = Cell::new(0);
    for stt in [InstallerStt::Included, InstallerStt::Omitted] {
        let again = init_in(
            None,
            || Ok(locations(temp.path())),
            stt,
            None,
            |_| {
                provisions.set(provisions.get() + 1);
                Ok(())
            },
        )
        .expect("a rerun succeeds");
        assert_eq!(again, first);
        assert_eq!(
            std::fs::read(&again).expect("read the config"),
            before,
            "{stt:?}"
        );
    }
    assert_eq!(
        provisions.get(),
        1,
        "only the STT rerun provisions what the config declares"
    );
}

#[test]
fn a_missing_explicit_config_is_generated_there() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let explicit = temp.path().join("nested").join("custom.toml");

    let path = init_in(
        Some(explicit.clone()),
        || panic!("an explicit path never gathers locations"),
        InstallerStt::Omitted,
        None,
        |_| panic!("--no-stt must never provision"),
    )
    .expect("init succeeds");

    assert_eq!(path, explicit);
    let written = std::fs::read_to_string(&explicit).expect("the default was written");
    assert!(written.contains("[server]"), "{written}");
    assert!(!written.contains("[[stt_model]]"), "{written}");
}

#[test]
fn an_explicit_config_wins_and_a_bad_one_names_its_path() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let explicit = temp.path().join("custom.toml");
    std::fs::write(&explicit, "not toml [").expect("write fixture");

    let error = init_in(
        Some(explicit.clone()),
        || panic!("an explicit path never gathers locations"),
        InstallerStt::Included,
        None,
        |_| panic!("an unloadable config never provisions"),
    )
    .expect_err("a malformed config fails");

    assert!(
        error.to_string().contains(&explicit.display().to_string()),
        "{error}"
    );
}

#[cfg(feature = "stt")]
#[test]
fn a_provision_failure_surfaces_its_cause() {
    use super::{InitError, InitRepr};

    let temp = tempfile::TempDir::new().expect("tempdir");

    let error = init_in(
        None,
        || Ok(locations(temp.path())),
        InstallerStt::Included,
        None,
        |_| {
            Err(InitError(InitRepr::Speech(
                gateway_stt::SpeechError::MissingInterim,
            )))
        },
    )
    .expect_err("the provision failure fails init");

    assert!(error.to_string().contains("speech-to-text"), "{error}");
    let cause = std::error::Error::source(&error)
        .map(ToString::to_string)
        .unwrap_or_default();
    assert!(cause.contains("interim"), "{cause}");
}

#[cfg(feature = "stt")]
mod progress_lines {
    use std::time::{Duration, Instant};

    use super::super::progress::ProgressLines;

    #[test]
    fn a_long_download_prints_at_most_one_line_per_second_and_per_five_points() {
        let mut lines = ProgressLines::default();
        let start = Instant::now();
        let mut printed = lines.offer("Provisioning speech-to-text", start);
        // A 600 MB download at 60 MB/s: one percent every 100 ms.
        let mut percent_lines = Vec::new();
        for percent in 0..=100u64 {
            let now = start + Duration::from_millis(100 * percent);
            for line in lines.offer(&format!("Downloading ggml-small.en.bin {percent}%"), now) {
                if line.ends_with('%') {
                    percent_lines.push((now, line.clone()));
                }
                printed.push(line);
            }
        }
        printed.extend(lines.finish());

        assert_eq!(printed[0], "Provisioning speech-to-text");
        assert_eq!(printed[1], "Provisioning speech-to-text: done");
        assert_eq!(printed[2], "Downloading ggml-small.en.bin 0%");
        assert_eq!(
            printed.last().map(String::as_str),
            Some("Downloading ggml-small.en.bin: done")
        );
        // The start line plus at most one line per elapsed second.
        assert!(percent_lines.len() <= 11, "{printed:#?}");
        for pair in percent_lines.windows(2) {
            let (earlier, later) = (&pair[0], &pair[1]);
            assert!(
                later.0 - earlier.0 >= Duration::from_secs(1),
                "{printed:#?}"
            );
        }
    }

    #[test]
    fn a_phase_without_a_percent_prints_its_start_and_end_once() {
        let mut lines = ProgressLines::default();
        let now = Instant::now();
        assert_eq!(lines.offer("Verifying ggml-base.en.bin", now).len(), 1);
        assert!(lines.offer("Verifying ggml-base.en.bin", now).is_empty());
        assert!(
            lines.offer("", now).is_empty(),
            "an ended activity prints nothing"
        );
        assert_eq!(
            lines.finish().as_deref(),
            Some("Verifying ggml-base.en.bin: done")
        );
        assert_eq!(lines.finish(), None);
    }
}
