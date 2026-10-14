//! Detached process launch: a gateway Workshop starts, a gateway's
//! successor, and the Windows setup a gateway runs to update itself all
//! outlive the process that starts them.

use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, OnceLock};

#[cfg(windows)]
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
#[cfg(windows)]
const DETACHED_PROCESS: u32 = 0x0000_0008;
#[cfg(windows)]
const WINDOWS_DETACHED_CREATION_FLAGS: u32 = CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS;
#[cfg(windows)]
const WINDOWS_BREAKAWAY_CREATION_FLAGS: u32 =
    CREATE_BREAKAWAY_FROM_JOB | WINDOWS_DETACHED_CREATION_FLAGS;

/// Spawns `command` detached from the caller's lifetime: null stdio, its
/// own process group on Unix, and on Windows a new process group with no
/// console, outside the caller's job object where the job allows it.
///
/// The caller owns the returned [`Child`] and decides whether to wait for
/// it, kill it, or let it run on after the caller exits.
///
/// # Errors
/// Returns the spawn's I/O error.
pub fn spawn_detached(command: &mut Command) -> std::io::Result<Child> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        spawn_detached_windows_with(|flags| command.creation_flags(flags).spawn())
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0).spawn()
    }
    #[cfg(not(any(windows, unix)))]
    {
        command.spawn()
    }
}

/// Spawns `command` as [`spawn_detached`] does and hands the child to a
/// reaper thread, which records its exit status in `exit`, so a launch wait
/// can stop as soon as the child dies instead of running out its budget.
/// Returns the child's pid.
///
/// # Errors
/// Returns the spawn's I/O error. A reaper thread that cannot start leaves
/// the child running unreaped, and `exit` never fills.
pub fn spawn_detached_reaped(
    command: &mut Command,
    exit: Arc<OnceLock<ExitStatus>>,
) -> std::io::Result<u32> {
    let mut child = spawn_detached(command)?;
    let child_pid = child.id();
    if let Err(error) = std::thread::Builder::new().spawn(move || {
        if let Ok(status) = child.wait() {
            let _ = exit.set(status);
        }
    }) {
        eprintln!("could not spawn the reaper thread; the child goes unreaped: {error}");
    }
    Ok(child_pid)
}

/// Runs `spawn` with the breakaway creation flags, retrying once without
/// breakaway when the job object forbids it.
#[cfg(windows)]
pub(crate) fn spawn_detached_windows_with<T, Spawn>(mut spawn: Spawn) -> std::io::Result<T>
where
    Spawn: FnMut(u32) -> std::io::Result<T>,
{
    match spawn(WINDOWS_BREAKAWAY_CREATION_FLAGS) {
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            // GitHub-hosted runners can forbid breakaway from their job object.
            // https://github.com/actions/runner/issues/595
            spawn(WINDOWS_DETACHED_CREATION_FLAGS)
        }
        result => result,
    }
}

#[cfg(test)]
#[path = "spawn-tests.rs"]
mod tests;
