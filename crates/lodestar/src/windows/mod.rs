//! The Windows runtime (architecture §11): the service entrypoint,
//! below-normal priority, the ephemeral-port warning and the portable
//! build's Restart now ([`relaunch`]).
//!
//! `main` calls [`hook`] first. Elsewhere it's a no-op; the parsing and
//! policy code compiles and is tested on every OS, and only the parts
//! that touch Windows are `cfg(windows)`.
//!
//! The rest of §11's Windows list lives with the code it concerns:
//! `SO_LINGER(0)` on outbound peer sockets in `lodestar-peer-wire`, and
//! retried renames and deletes of mapped index files in `lodestar-search`
//! (B5a/B5b).

#![cfg_attr(not(windows), allow(dead_code))]

pub mod acl;
pub mod logfile;
pub mod ports;
pub mod priority;
pub mod service;
pub mod temp_log;

use std::process::ExitCode;

/// `main`'s hook, before its own argument parsing. For a serve command
/// line (none, `serve`, `--config <path>`) it runs the whole service when
/// the Service Control Manager started this process, and returns its exit
/// code; otherwise it starts the [`startup`] checks and returns `None` so
/// `main` carries on. Any other command, or arguments that don't parse
/// (`main` reports those), is left alone.
pub fn hook() -> Option<ExitCode> {
    #[cfg(windows)]
    {
        // A copy started by Restart now waits for the old one to hand
        // over and exit, or ends here if it doesn't.
        if let Some(code) = relaunch::wait_if_successor() {
            return Some(code);
        }
        // Restart now for an ordinary run; a service sets its own method,
        // which never runs this.
        crate::system::Restart::global().set_prepare(Box::new(relaunch::start_successor));
        let config_path = service::service_command(std::env::args_os())?;
        if let Some(code) = service::run_if_launched(config_path.clone()) {
            return Some(code);
        }
        // An unreadable config is `main`'s to report.
        if let Ok(config) =
            crate::config::Config::load(config_path.as_deref(), |k| std::env::var(k).ok())
        {
            startup(&config);
        }
    }
    None
}

/// Lowers the process priority and checks the port range, on a thread of
/// its own: both run a system tool, which shouldn't hold up the start.
/// They log once a subscriber is installed (`main` installs it just after
/// [`hook`]).
#[cfg(windows)]
pub fn startup(config: &crate::config::Config) {
    let max_sockets = config.pipeline_config().map_or(0, |p| p.pool.max_sockets);
    let _ = std::thread::Builder::new()
        .name("windows-startup".into())
        .spawn(move || {
            wait_for_subscriber();
            priority::apply();
            if max_sockets > 0 {
                ports::check(max_sockets);
            }
        });
}

/// Waits up to 5 s for the global subscriber.
#[cfg(windows)]
fn wait_for_subscriber() {
    for _ in 0..100 {
        if tracing::dispatcher::has_been_set() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// A command for a tool in `%SystemRoot%\System32`, by absolute path (a
/// tool found through the search path could be anyone's), that opens no
/// console window.
#[cfg(windows)]
fn system32_command(relative: &str) -> Option<std::process::Command> {
    use std::os::windows::process::CommandExt;
    /// `CREATE_NO_WINDOW`.
    const NO_WINDOW: u32 = 0x0800_0000;
    let root = std::env::var_os("SystemRoot")?;
    let mut cmd =
        std::process::Command::new(std::path::Path::new(&root).join("System32").join(relative));
    cmd.creation_flags(NO_WINDOW);
    // A copy started by Restart now keeps the token in its own environment
    // (safe code can't remove it); children never need it.
    cmd.env_remove(relaunch::MARKER);
    Some(cmd)
}

/// Windows PowerShell 5.1 from System32, with its own module path: one
/// inherited from PowerShell 7 (a parent shell, a CI runner) points at
/// modules 5.1 can't load, and `Get-Acl` then fails to autoload.
#[cfg(windows)]
fn powershell_command() -> Option<std::process::Command> {
    let mut cmd = system32_command(r"WindowsPowerShell\v1.0\powershell.exe")?;
    cmd.env_remove("PSModulePath");
    Some(cmd)
}

/// Restart now for the portable build (architecture §10.10.6): a new copy
/// of the same program with the same arguments takes this one's place.
///
/// - **At the request** ([`start_successor`], run by
///   `Restart::request`), the new copy is started, never through a shell
///   and with no argument added. Its stdin is a pipe from this process,
///   and [`MARKER`] in its environment holds a one-time token. If it can't
///   be started, the request is dropped and this process keeps serving.
/// - **The new copy waits** ([`wait_if_successor`], first thing in
///   [`hook`]) for the token on stdin, which this process writes only once
///   its graceful shutdown is done ([`hand_over`]), then for end of file,
///   which comes when this process exits and Windows has released its
///   port, data lock and files. A stop instead of a restart (Ctrl+C, a
///   crash) closes the pipe without the token, and the new copy ends
///   quietly. Both waits have deadlines: [`GO_LIMIT`] for the shutdown and
///   [`EXIT_LIMIT`] for the exit, after which it goes ahead and a port
///   still held fails its start as any taken port would.
/// - **It shares this process's console** and output, so the window it
///   was started in shows the new copy, and Ctrl+C there stops it.
pub mod relaunch {
    use std::ffi::OsString;
    use std::io::Read;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::Duration;

    /// The new copy's environment variable holding the hand-over token.
    pub const MARKER: &str = "LODESTAR_RELAUNCH_TOKEN";
    /// How long the new copy waits for the graceful shutdown.
    pub const GO_LIMIT: Duration = Duration::from_secs(600);
    /// How long it then waits for the old process to exit.
    pub const EXIT_LIMIT: Duration = Duration::from_secs(60);
    /// The longest token line read.
    const MAX_LINE: usize = 128;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Handover {
        /// Handed over: start.
        Go,
        /// The old process stopped instead, or never handed over: end.
        Abandoned,
    }

    enum Step {
        Line(Vec<u8>),
        End,
    }

    /// The new copy's side: `Go` once `input` gives `token` on a line of
    /// its own within `go_limit`, then ends (or `exit_limit` passes).
    pub fn await_handover(
        input: impl Read + Send + 'static,
        token: &str,
        go_limit: Duration,
        exit_limit: Duration,
    ) -> Handover {
        let (tx, rx) = mpsc::channel();
        let reader = std::thread::Builder::new()
            .name("relaunch-wait".into())
            .spawn(move || {
                let mut input = input;
                let mut line = Vec::new();
                let mut byte = [0u8; 1];
                loop {
                    match input.read(&mut byte) {
                        Ok(1) if byte[0] == b'\n' => break,
                        Ok(1) if line.len() < MAX_LINE => line.push(byte[0]),
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                        // End of file, an error or an overlong line.
                        _ => {
                            let _ = tx.send(Step::End);
                            return;
                        }
                    }
                }
                let _ = tx.send(Step::Line(line));
                let mut sink = [0u8; 64];
                loop {
                    match input.read(&mut sink) {
                        Ok(0) => break,
                        Ok(_) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(_) => break,
                    }
                }
                let _ = tx.send(Step::End);
            });
        if reader.is_err() {
            return Handover::Abandoned;
        }
        match rx.recv_timeout(go_limit) {
            Ok(Step::Line(line))
                if line.strip_suffix(b"\r").unwrap_or(&line) == token.as_bytes() => {}
            _ => return Handover::Abandoned,
        }
        // The old process exits next; past the deadline, go ahead anyway.
        let _ = rx.recv_timeout(exit_limit);
        Handover::Go
    }

    /// The new copy: `exe` with `args`, the token in its environment, its
    /// stdin a pipe from this process, its output and console shared.
    pub fn command(exe: &Path, args: impl IntoIterator<Item = OsString>, token: &str) -> Command {
        let mut cmd = Command::new(exe);
        cmd.args(args).env(MARKER, token).stdin(Stdio::piped());
        cmd
    }

    /// A token no other process would write: this process's id and the
    /// time, which only the new copy is given.
    pub fn token() -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        format!("lodestar-{}-{nanos}", std::process::id())
    }

    #[cfg(windows)]
    pub use os::{hand_over, start_successor, wait_if_successor};

    #[cfg(windows)]
    mod os {
        use std::io::{IsTerminal, Write};
        use std::process::{Child, ExitCode};
        use std::sync::Mutex;

        use super::{EXIT_LIMIT, GO_LIMIT, Handover, MARKER};

        /// The copy waiting to take over, and its token.
        static SUCCESSOR: Mutex<Option<(Child, String)>> = Mutex::new(None);
        /// Its stdin once handed over, kept open until this process exits.
        static HANDED: Mutex<Option<Child>> = Mutex::new(None);

        /// For [`super::super::hook`]: `None` unless this process is a copy
        /// started by Restart now; then it waits, and `Some` ends it.
        pub fn wait_if_successor() -> Option<ExitCode> {
            let token = std::env::var(MARKER).ok().filter(|t| !t.is_empty())?;
            // Started from a console by hand, with the variable inherited:
            // there's no old process to wait for.
            if std::io::stdin().is_terminal() {
                return None;
            }
            match super::await_handover(std::io::stdin(), &token, GO_LIMIT, EXIT_LIMIT) {
                Handover::Go => None,
                Handover::Abandoned => Some(ExitCode::SUCCESS),
            }
        }

        fn spawn() -> std::io::Result<(Child, String)> {
            let exe = std::env::current_exe()?;
            let token = super::token();
            let child = super::command(&exe, std::env::args_os().skip(1), &token).spawn()?;
            Ok((child, token))
        }

        /// `Restart::request`'s step: starts the copy that takes over.
        pub fn start_successor() -> std::io::Result<()> {
            let started = spawn()?;
            let previous = SUCCESSOR
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .replace(started);
            // Only one request runs this; a leftover copy is told nothing,
            // so it ends once its stdin closes.
            drop(previous);
            Ok(())
        }

        fn send(mut child: Child, token: &str) -> std::io::Result<()> {
            if child.try_wait()?.is_some() {
                return Err(std::io::Error::other("the new copy has exited"));
            }
            let stdin = child
                .stdin
                .as_mut()
                .ok_or_else(|| std::io::Error::other("no pipe to the new copy"))?;
            stdin.write_all(format!("{token}\n").as_bytes())?;
            stdin.flush()?;
            *HANDED.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
            Ok(())
        }

        /// After the graceful shutdown: hands over to the waiting copy, or,
        /// if it's gone, to a new one, which starts once this process has
        /// exited. An error means no copy takes over.
        pub fn hand_over() -> std::io::Result<()> {
            let waiting = SUCCESSOR.lock().unwrap_or_else(|e| e.into_inner()).take();
            let handed = match waiting {
                Some((child, token)) => send(child, &token),
                None => Err(std::io::Error::other("no copy was started")),
            };
            if handed.is_err() {
                let (child, token) = spawn()?;
                send(child, &token)?;
            }
            lodestar_log::log_event!(crate::logging::events::APP_RELAUNCH);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::relaunch::*;
    use std::io::Read;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    const LONG: Duration = Duration::from_secs(10);

    /// A pipe end fed from a channel: `None` closes it.
    struct Pipe(mpsc::Receiver<Option<Vec<u8>>>, Vec<u8>);

    impl Read for Pipe {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            while self.1.is_empty() {
                match self.0.recv() {
                    Ok(Some(bytes)) => self.1 = bytes,
                    _ => return Ok(0),
                }
            }
            let n = buf.len().min(self.1.len());
            buf[..n].copy_from_slice(&self.1[..n]);
            self.1.drain(..n);
            Ok(n)
        }
    }

    fn pipe() -> (mpsc::Sender<Option<Vec<u8>>>, Pipe) {
        let (tx, rx) = mpsc::channel();
        (tx, Pipe(rx, Vec::new()))
    }

    #[test]
    fn the_new_copy_starts_once_handed_over_and_the_old_one_exits() {
        let (tx, input) = pipe();
        tx.send(Some(b"tok-1\n".to_vec())).unwrap();
        tx.send(None).unwrap();
        assert_eq!(await_handover(input, "tok-1", LONG, LONG), Handover::Go);
        // While the old process hasn't exited it waits, up to the limit.
        let (tx, input) = pipe();
        tx.send(Some(b"tok-1\n".to_vec())).unwrap();
        let limit = Duration::from_millis(300);
        let start = Instant::now();
        assert_eq!(await_handover(input, "tok-1", LONG, limit), Handover::Go);
        assert!(start.elapsed() >= limit);
        drop(tx);
    }

    #[test]
    fn a_stop_instead_of_a_restart_ends_the_new_copy() {
        for input in [&b""[..], b"tok", b"other\n", b"tok-1x\n"] {
            let (tx, from) = pipe();
            tx.send(Some(input.to_vec())).unwrap();
            tx.send(None).unwrap();
            assert_eq!(
                await_handover(from, "tok-1", LONG, LONG),
                Handover::Abandoned,
                "{input:?}"
            );
        }
        // An endless line is cut off.
        let (tx, input) = pipe();
        tx.send(Some(vec![b'a'; 4096])).unwrap();
        assert_eq!(await_handover(input, "a", LONG, LONG), Handover::Abandoned);
    }

    #[test]
    fn both_waits_have_deadlines() {
        // No hand-over in time: ends.
        let (_tx, input) = pipe();
        let start = Instant::now();
        let short = Duration::from_millis(100);
        assert_eq!(await_handover(input, "t", short, LONG), Handover::Abandoned);
        assert!(start.elapsed() < LONG);
        // A CRLF line counts.
        let (tx, input) = pipe();
        tx.send(Some(b"t\r\n".to_vec())).unwrap();
        tx.send(None).unwrap();
        assert_eq!(await_handover(input, "t", LONG, LONG), Handover::Go);
    }

    #[test]
    fn the_new_copy_gets_the_same_arguments_and_nothing_more() {
        let args = ["serve", "--config", "C:\\a b\\c.toml"].map(std::ffi::OsString::from);
        let cmd = command(std::path::Path::new("lodestar.exe"), args.clone(), "tok");
        assert_eq!(cmd.get_program(), "lodestar.exe");
        assert!(cmd.get_args().eq(args.iter().map(|a| a.as_os_str())));
        let envs: Vec<_> = cmd.get_envs().collect();
        assert_eq!(
            envs,
            vec![(
                std::ffi::OsStr::new(MARKER),
                Some(std::ffi::OsStr::new("tok"))
            )]
        );
        assert_ne!(token(), "");
    }
}
