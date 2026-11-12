//! Running a child process with a deadline: the update helper's `msiexec`
//! and both Event Log notes (the update helper's and the service's refusal
//! note) go through [`supervise`], so a hung child is always killed and
//! reaped rather than waited on for good (rv-702's N2).
//!
//! This module writes nothing itself: it runs the command it's handed.
//! The Event Log writers are `update/windows/helper.rs` and
//! `windows/service.rs`, which `tests/legal_facts.rs` checks against the
//! Legal Notice's section 5, so the writer's name stays out of this file.
//!
//! Cross-platform: the waiting and the kill are `std`'s, so the tests run
//! on every OS with a real child.

use std::io;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The longest a writer waits for its Event Log note to be written. One
/// note takes far less; past it the note is given up (it's best effort),
/// so a hung writer never holds up a rollback, a restart or the service's
/// refusal.
pub const EVENT_LOG_WAIT: Duration = Duration::from_secs(10);

/// How often [`run_bounded`] looks at the child.
const POLL: Duration = Duration::from_millis(50);

/// A started child, as [`supervise`] sees it.
pub trait Started {
    /// `Some(code)` once it has ended (`Some(None)`: with no exit code).
    fn ended(&mut self) -> io::Result<Option<Option<i32>>>;
    /// Ends it and reaps it.
    fn end(&mut self);
}

impl Started for std::process::Child {
    fn ended(&mut self) -> io::Result<Option<Option<i32>>> {
        Ok(self.try_wait()?.map(|s| s.code()))
    }
    /// Reaped only once it's killed (an ended child counts as killed), so
    /// a kill that fails never leaves this waiting.
    fn end(&mut self) {
        if self.kill().is_ok() {
            let _ = self.wait();
        }
    }
}

/// Waits for a started child (`what` names it in errors), ending it at
/// `deadline`. Every error comes after it started, so it's `TimedOut` (the
/// deadline) or `Interrupted` (no exit code, or it couldn't be waited on):
/// the update helper never answers one as "not started" (P-4).
pub fn supervise(
    child: &mut dyn Started,
    what: &str,
    deadline: Duration,
    poll: Duration,
    now: &dyn Fn() -> Instant,
    sleep: &mut dyn FnMut(Duration),
) -> io::Result<i32> {
    let start = now();
    loop {
        match child.ended() {
            Ok(Some(Some(code))) => return Ok(code),
            Ok(Some(None)) => {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    format!("{what} ended without a code"),
                ));
            }
            Err(e) => {
                child.end();
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    format!("waiting for {what}: {e}"),
                ));
            }
            Ok(None) => {}
        }
        if now().saturating_duration_since(start) >= deadline {
            child.end();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("{what} ran past its deadline"),
            ));
        }
        sleep(poll);
    }
}

/// Runs `cmd` with no input or output, for at most `limit`: past it the
/// child is killed and reaped, and the call answers `TimedOut`. Through
/// [`supervise`], so the bound and the kill are the ones msiexec gets.
pub fn run_bounded(mut cmd: Command, what: &str, limit: Duration) -> io::Result<i32> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    supervise(
        &mut child,
        what,
        limit,
        POLL,
        &Instant::now,
        &mut std::thread::sleep,
    )
}

/// A child that never exits, on either platform: for the tests of every
/// bounded writer.
#[cfg(test)]
pub fn never_exits() -> Command {
    if cfg!(windows) {
        let mut c = Command::new("powershell.exe");
        c.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Start-Sleep -Seconds 600",
        ]);
        c
    } else {
        let mut c = Command::new("sleep");
        c.arg("600");
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A child that never exits is killed at the limit and the call
    /// answers `TimedOut`, well within the test's 10 s deadline; one that
    /// ends answers its code.
    #[test]
    fn a_hung_child_is_killed_at_its_bound() {
        let start = Instant::now();
        let e = run_bounded(never_exits(), "sleep", Duration::from_millis(300)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut, "{e}");
        assert!(e.to_string().contains("sleep ran past its deadline"), "{e}");
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "{:?}",
            start.elapsed()
        );

        let quick = if cfg!(windows) {
            let mut c = Command::new("cmd.exe");
            c.args(["/C", "exit 3"]);
            c
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", "exit 3"]);
            c
        };
        assert_eq!(run_bounded(quick, "exit", EVENT_LOG_WAIT).unwrap(), 3);
        assert!(EVENT_LOG_WAIT >= Duration::from_secs(10));
    }
}
