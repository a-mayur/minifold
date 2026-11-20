//! Running the external programs so an aborted job stops them right away.

use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Like `Command::output`, but the program is killed as soon as `cancel` is set
/// (then the result is an `Interrupted` error, not the killed program's output).
pub fn output(cmd: &mut Command, cancel: &AtomicBool) -> io::Result<Output> {
    let child = in_own_group(cmd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let out = until_done(child.id(), cancel, || child.wait_with_output())?;
    if cancel.load(Ordering::Relaxed) {
        return Err(aborted());
    }
    Ok(out)
}

pub fn aborted() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "aborted by the user")
}

/// Starts the program in its own process group, so killing the group also stops what it
/// started (`timeout` runs gs and ffmpeg as its children).
pub fn in_own_group(cmd: &mut Command) -> &mut Command {
    cmd.process_group(0)
}

/// Runs `wait` while a watcher kills process group `pid` once `cancel` is set.
/// `pid` must come from a command started with `in_own_group`.
pub fn until_done<T>(pid: u32, cancel: &AtomicBool, wait: impl FnOnce() -> T) -> T {
    let done = AtomicBool::new(false);
    std::thread::scope(|s| {
        s.spawn(|| {
            while !done.load(Ordering::Relaxed) {
                if cancel.load(Ordering::Relaxed) {
                    tracing::info!(pid, "aborted: stopping the program");
                    // SAFETY: kill(2) has no memory effects; a negative pid addresses the process group.
                    unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let result = wait();
        done.store(true, Ordering::Relaxed);
        result
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Instant;

    #[test]
    fn runs_to_completion_when_not_cancelled() {
        let out = output(Command::new("sh").args(["-c", "echo hi; echo err >&2"]), &AtomicBool::new(false)).unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout, b"hi\n");
        assert_eq!(out.stderr, b"err\n");
    }

    #[test]
    fn cancel_kills_the_program_and_its_children() {
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            flag.store(true, Ordering::Relaxed);
        });
        let started = Instant::now();
        // Like `timeout … gs`: the long-running program is a child of the one we start.
        let err = output(Command::new("timeout").args(["60", "sleep", "30"]), &cancel).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Interrupted);
        assert!(started.elapsed() < Duration::from_secs(5), "stopped quickly, took {:?}", started.elapsed());
    }
}
