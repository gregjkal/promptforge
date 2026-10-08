use std::cell::Cell;
use std::path::Path;

use super::super::{CONFIG_FILE_NAME, Locations, STT_MODELS_TOML};
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
        |_| None,
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
        |_| None,
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
        InstallerStt::Included,
        |_| None,
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
            |_| None,
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
fn speech_on_a_config_without_speech_models_fails_and_leaves_it() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let path = init_in(
        None,
        || Ok(locations(temp.path())),
        InstallerStt::Omitted,
        |_| None,
        |_| panic!("--no-stt must never provision"),
    )
    .expect("the --no-stt init succeeds");
    let before = std::fs::read(&path).expect("read the config");

    let error = init_in(
        None,
        || Ok(locations(temp.path())),
        InstallerStt::Included,
        |_| None,
        |_| panic!("a profile without speech models never provisions"),
    )
    .expect_err("speech cannot be added to a config that declares none");

    let message = error.to_string();
    assert!(message.contains(&path.display().to_string()), "{message}");
    assert!(message.contains("selects profile default"), "{message}");
    assert!(message.contains("--no-stt"), "{message}");
    assert_eq!(std::fs::read(&path).expect("read the config"), before);
}

#[test]
fn a_missing_explicit_config_is_generated_there() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let explicit = temp.path().join("nested").join("custom.toml");

    let path = init_in(
        Some(explicit.clone()),
        || panic!("an explicit path never gathers locations"),
        InstallerStt::Omitted,
        |_| None,
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
        |_| None,
        |_| panic!("an unloadable config never provisions"),
    )
    .expect_err("a malformed config fails");

    assert!(
        error.to_string().contains(&explicit.display().to_string()),
        "{error}"
    );
}

#[test]
fn init_without_stt_still_refuses_an_unloadable_config() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let explicit = temp.path().join("custom.toml");
    std::fs::write(&explicit, "not toml [").expect("write fixture");

    let error = init_in(
        Some(explicit.clone()),
        || panic!("an explicit path never gathers locations"),
        InstallerStt::Omitted,
        |_| None,
        |_| panic!("--no-stt must never provision"),
    )
    .expect_err("a malformed config fails under --no-stt too");

    assert!(
        error.to_string().contains(&explicit.display().to_string()),
        "{error}"
    );
}

#[cfg(unix)]
#[test]
fn a_dangling_explicit_symlink_fails_instead_of_generating() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let explicit = temp.path().join("link.toml");
    std::os::unix::fs::symlink(temp.path().join("absent.toml"), &explicit)
        .expect("create the symlink");

    let error = init_in(
        Some(explicit.clone()),
        || panic!("an explicit path never gathers locations"),
        InstallerStt::Omitted,
        |_| None,
        |_| panic!("--no-stt must never provision"),
    )
    .expect_err("a dangling symlink is not a config");

    assert!(
        error.to_string().contains(&explicit.display().to_string()),
        "{error}"
    );
    assert!(
        !temp.path().join("absent.toml").exists(),
        "init never writes through the symlink"
    );
    assert!(
        !explicit.with_extension("state.toml").exists(),
        "init records no profile for a config it did not write"
    );
}

#[test]
fn the_profile_environment_is_read_for_the_resolved_path_and_selects() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let explicit = temp.path().join("custom.toml");
    std::fs::write(
        &explicit,
        format!(
            "config-version = 0\n\
             [server]\nbind = \"127.0.0.1:0\"\napi_key = \"k\"\n\
             {STT_MODELS_TOML}\n\
             [[profile]]\nname = \"default\"\nmodels = []\n\
             [[profile]]\nname = \"travel\"\n\
             models = [\"whisper-base-en\", \"whisper-small-en\"]\n"
        ),
    )
    .expect("write fixture");
    let mut provisioned = Vec::new();

    init_in(
        Some(explicit.clone()),
        || panic!("an explicit path never gathers locations"),
        InstallerStt::Included,
        |path| {
            // A boot loads `<config>.env` first, and it may set the profile.
            assert_eq!(path, explicit);
            Some("travel".to_owned())
        },
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

    assert_eq!(provisioned, ["whisper-base-en", "whisper-small-en"]);
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
        |_| None,
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
    fn a_long_download_prints_at_most_one_line_per_second() {
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
        assert_eq!(printed[1], "Downloading ggml-small.en.bin 0%");
        assert_eq!(
            printed.last().map(String::as_str),
            Some("Downloading ggml-small.en.bin: done")
        );
        // The start line plus at most one line per elapsed second.
        assert!(percent_lines.len() <= 11, "{printed:#?}");
        assert!(percent_lines.len() >= 5, "{printed:#?}");
        for pair in percent_lines.windows(2) {
            let (earlier, later) = (&pair[0], &pair[1]);
            assert!(
                later.0 - earlier.0 >= Duration::from_secs(1),
                "{printed:#?}"
            );
        }
    }

    #[test]
    fn a_slow_download_prints_at_most_one_line_per_five_points() {
        let mut lines = ProgressLines::default();
        let start = Instant::now();
        let mut percents = Vec::new();
        // One percent every two seconds: the time gate always passes.
        for percent in 0..=100u64 {
            let now = start + Duration::from_secs(2 * percent);
            for line in lines.offer(&format!("Downloading ggml-base.en.bin {percent}%"), now) {
                let digits = line
                    .rsplit_once(' ')
                    .and_then(|(_, last)| last.strip_suffix('%'))
                    .expect("a percent line");
                percents.push(digits.parse::<u64>().expect("a whole percent"));
            }
        }

        assert_eq!(percents, (0..=100).step_by(5).collect::<Vec<_>>());
    }

    #[test]
    fn a_status_without_a_percent_prints_no_end_line() {
        let mut lines = ProgressLines::default();
        let now = Instant::now();
        let mut printed = Vec::new();
        for text in [
            "Provisioning speech-to-text",
            "Provisioning whisper library",
            "Verifying whisper-cpu.zip 0%",
            "Downloading ggml-base.en.bin",
            "",
        ] {
            printed.extend(lines.offer(text, now));
        }
        printed.extend(lines.finish());

        assert_eq!(
            printed,
            [
                "Provisioning speech-to-text",
                "Provisioning whisper library",
                "Verifying whisper-cpu.zip 0%",
                "Verifying whisper-cpu.zip: done",
                "Downloading ggml-base.en.bin",
            ]
        );
        assert_eq!(lines.finish(), None);
    }
}
