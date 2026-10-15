use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use super::*;

const FIXTURE_TIMEOUT: Duration = Duration::from_secs(10);

/// A program that exits by itself once it reads end of input from a null
/// stdin.
fn self_exiting_program() -> PathBuf {
    if cfg!(windows) {
        std::env::var_os("ComSpec").map_or_else(|| PathBuf::from("cmd.exe"), PathBuf::from)
    } else {
        PathBuf::from("/bin/sh")
    }
}

#[test]
fn the_reaper_records_the_exit_status_of_a_detached_child() {
    let exit = Arc::new(OnceLock::new());
    spawn_detached_reaped(&mut Command::new(self_exiting_program()), Arc::clone(&exit))
        .expect("spawn a child that exits by itself");

    let deadline = Instant::now() + FIXTURE_TIMEOUT;
    while exit.get().is_none() {
        assert!(
            Instant::now() < deadline,
            "the reaper records the child's exit within {FIXTURE_TIMEOUT:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
#[test]
fn a_detached_child_belongs_to_the_caller_until_it_waits_or_kills() {
    let mut child =
        spawn_detached(Command::new("/bin/sleep").arg("30")).expect("spawn a long-lived child");
    assert!(child.try_wait().expect("poll").is_none(), "the child runs");
    child
        .kill()
        .expect("the caller can kill its detached child");
    assert!(!child.wait().expect("reap").success());
}

#[cfg(windows)]
#[test]
fn windows_detached_spawn_first_attempt_uses_all_required_flags() {
    let mut attempts = Vec::new();

    let child_pid = spawn_detached_windows_with(|flags| {
        attempts.push(flags);
        Ok(41)
    })
    .expect("the first spawn succeeds");

    assert_eq!(child_pid, 41);
    assert_eq!(attempts, [0x0100_0208]);
}

#[cfg(windows)]
#[test]
fn windows_detached_spawn_retries_access_denied_without_breakaway() {
    let mut attempts = Vec::new();

    let child_pid = spawn_detached_windows_with(|flags| {
        attempts.push(flags);
        if attempts.len() == 1 {
            Err(std::io::Error::from_raw_os_error(5))
        } else {
            Ok(42)
        }
    })
    .expect("access denied retries without breakaway");

    assert_eq!(child_pid, 42);
    assert_eq!(attempts, [0x0100_0208, 0x0000_0208]);
}

#[cfg(windows)]
#[test]
fn windows_detached_spawn_does_not_retry_after_success() {
    let mut attempts = 0;

    spawn_detached_windows_with(|_| {
        attempts += 1;
        Ok(43)
    })
    .expect("the first spawn succeeds");

    assert_eq!(attempts, 1);
}

#[cfg(windows)]
#[test]
fn windows_detached_spawn_does_not_retry_other_errors() {
    let mut attempts = 0;

    let error = spawn_detached_windows_with::<u32, _>(|_| {
        attempts += 1;
        Err(std::io::Error::from_raw_os_error(123))
    })
    .expect_err("non-permission errors propagate");

    assert_eq!(error.raw_os_error(), Some(123));
    assert_eq!(attempts, 1);
}
