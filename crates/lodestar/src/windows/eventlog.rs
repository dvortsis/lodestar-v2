//! Errors to the Windows Event Log (`logging.event_log_errors`), for the
//! Windows service only.
//!
//! - **The text the log API shows, with less in it.** Lines are read from
//!   the ring as `/logs` reads it, so a kind the file mask hides is already
//!   `[hidden]` and the setup token is `[redacted]`. The Application log
//!   is readable by every local user, unlike the private log folder, so
//!   every data value (names, paths, addresses, peers, hashes, sizes) and
//!   every untyped field is `[hidden]` here as well: only the event, its
//!   message, labels and error kinds go out.
//! - **Errors only, at most [`PER_MINUTE`] a minute.** Any more are counted
//!   and one line says how many weren't sent.
//! - **It never holds up the logger.** It runs on a task of its own that
//!   reads the ring after it changes, and the sink hands each message to a
//!   thread of its own. When a write fails it stops, notes why once in
//!   `%SystemRoot%\Temp\lodestar-service.log`, and tries again only after
//!   the next settings save.
//! - The service installs the sink ([`install`]); without one (every other
//!   way Lodestar runs) [`follow`] does nothing. The service's refusal to
//!   start writes to the Event Log whatever the setting ([`write`]).

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use lodestar_api::dto::logs::{LogFieldKind, LogLine, LogValue};
use lodestar_api::logs::{LineFilter, RingRead};
use lodestar_log::Level;
use tokio::sync::watch;

#[cfg(test)]
use crate::logging::{Logs, Runtime};

/// The most errors sent in a minute.
pub const PER_MINUTE: u32 = 10;
/// The longest message sent, in bytes; longer ones are cut.
pub const MAX_MESSAGE: usize = 8_000;
/// What a value the Event Log doesn't get shows as.
pub const HIDDEN: &str = lodestar_log::HIDDEN;
/// The event id of a forwarded error; a refusal to start is `3`.
pub const ERROR_ID: u16 = 1;
/// The event id of a refusal to start.
pub const REFUSAL_ID: u16 = 3;
const WINDOW_MS: i64 = 60_000;
/// The most lines read at a time.
const BATCH: usize = 64;
/// Ring changes closer together than this are read together.
const COALESCE: Duration = Duration::from_secs(1);

/// The PowerShell that writes one Event Log entry, from the environment so
/// no text is ever parsed as a command.
pub const EVENT_SCRIPT: &str = "$ErrorActionPreference = 'Stop'
Write-EventLog -LogName Application -Source $env:LODESTAR_EVENT_SOURCE -EntryType Error -EventId ([int]$env:LODESTAR_EVENT_ID) -Message $env:LODESTAR_EVENT_MESSAGE";

/// Where messages go: the Event Log, or a test's list.
pub trait Sink: Send {
    /// Queues `message`; `Err` with why once a write has failed.
    fn send(&mut self, message: String) -> Result<(), String>;
}

/// What the forwarder does with the ring, given the setting.
pub struct Forwarder {
    sink: Box<dyn Sink>,
    cursor: u64,
    window_start: i64,
    sent: u32,
    suppressed: u64,
    on: bool,
    saves: u64,
    failed: bool,
}

impl Forwarder {
    pub fn new(sink: Box<dyn Sink>) -> Forwarder {
        Forwarder {
            sink,
            cursor: 0,
            window_start: i64::MIN,
            sent: 0,
            suppressed: 0,
            on: true,
            saves: 0,
            failed: false,
        }
    }

    /// The setting, and how many saves there have been: a save after a
    /// failure tries again.
    pub fn configure(&mut self, on: bool, saves: u64) {
        if saves != self.saves {
            self.saves = saves;
            self.failed = false;
        }
        self.on = on;
    }

    /// Reads what's new through `read` (cursor, filter, max) and sends the
    /// errors the setting and the limit allow. Returns why, the first time
    /// a write fails.
    pub fn forward(
        &mut self,
        read: impl Fn(u64, &LineFilter, usize) -> RingRead,
        now_ms: i64,
    ) -> Option<String> {
        let filter = LineFilter {
            level: Level::Error,
            ..LineFilter::default()
        };
        let active = self.on && !self.failed;
        // While off, the cursor still moves, so turning it on sends only
        // what comes after.
        let got = read(self.cursor, &filter, if active { BATCH } else { 0 });
        self.cursor = got.head;
        if !active {
            self.suppressed = 0;
            return None;
        }
        let mut messages = Vec::new();
        if now_ms.saturating_sub(self.window_start) >= WINDOW_MS {
            self.window_start = now_ms;
            self.sent = 0;
            if self.suppressed > 0 {
                messages.push(format!(
                    "Lodestar: {} more errors in the last minute weren't sent to the Event Log. The log file has them.",
                    self.suppressed
                ));
                self.suppressed = 0;
                self.sent = 1;
            }
        }
        let shown = got.lines.len() as u64;
        for (_, line) in got.lines {
            if self.sent < PER_MINUTE {
                self.sent += 1;
                messages.push(render(&line));
            } else {
                self.suppressed += 1;
            }
        }
        self.suppressed += got.matching.saturating_sub(shown);
        for m in messages {
            if let Err(why) = self.sink.send(m) {
                self.failed = true;
                return Some(why);
            }
        }
        None
    }
}

/// One line as the Event Log gets it: the console's text format without
/// the time (the Event Log has its own), every value but labels and error
/// kinds hidden, cut to [`MAX_MESSAGE`] bytes.
pub fn render(l: &LogLine) -> String {
    use std::fmt::Write as _;
    let source = l.event.as_deref().or(l.target.as_deref()).unwrap_or("");
    let mut out = format!("{}: {}", printable(source), printable(&l.msg));
    for f in &l.fields {
        let value = match (&f.kind, &f.value) {
            (LogFieldKind::Label | LogFieldKind::Error, LogValue::Text(t)) => printable(t),
            (LogFieldKind::Label | LogFieldKind::Error, LogValue::Flag(b)) => b.to_string(),
            _ => HIDDEN.to_string(),
        };
        let _ = write!(out, " {}={value}", printable(&f.name));
    }
    if let Some(r) = &l.repeated {
        let _ = write!(out, " (repeated {} times)", r.count);
    }
    if out.len() > MAX_MESSAGE {
        let mut end = MAX_MESSAGE;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
    }
    out
}

/// `text` on one line: control and bidirectional characters dropped.
fn printable(text: &str) -> String {
    text.chars()
        .filter(|c| {
            !c.is_control() && !matches!(*c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .collect()
}

/// The setting as [`follow`] last saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Setting {
    on: bool,
    saves: u64,
}

static SINK: Mutex<Option<Box<dyn Sink>>> = Mutex::new(None);
static SETTING: OnceLock<watch::Sender<Setting>> = OnceLock::new();

/// The service's sink, before logging's [`follow`] first runs.
pub fn install(sink: Box<dyn Sink>) {
    *SINK.lock().unwrap_or_else(|e| e.into_inner()) = Some(sink);
}

/// Called by the logging settings on every wake: starts the forwarder the
/// first time, when a sink is installed, and hands it the setting.
pub fn follow(on: bool, saved: bool) {
    if let Some(tx) = SETTING.get() {
        tx.send_modify(|s| {
            s.on = on;
            s.saves += u64::from(saved);
        });
        return;
    }
    let Some(sink) = SINK.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        return;
    };
    let (tx, rx) = watch::channel(Setting { on, saves: 0 });
    if SETTING.set(tx).is_ok() {
        tokio::spawn(drive(Forwarder::new(sink), rx));
    }
}

/// The forwarder's task: reads after every ring change (at most once a
/// [`COALESCE`]), every setting change and every minute, for the summary.
async fn drive(mut fwd: Forwarder, mut setting: watch::Receiver<Setting>) {
    // The installed runtime's ring, as `/logs` reads it.
    let logs = crate::logging::source();
    let (_listening, mut ring) = logs.listen();
    loop {
        let s = *setting.borrow_and_update();
        fwd.configure(s.on, s.saves);
        if let Some(why) = fwd.forward(
            |c, f, m| logs.after(c, f, m),
            crate::logging::unix_now() * 1000,
        ) {
            note_failure(&why);
        }
        tokio::select! {
            changed = setting.changed() => if changed.is_err() { break },
            changed = ring.changed() => {
                if changed.is_err() {
                    break;
                }
                tokio::time::sleep(COALESCE).await;
            }
            () = tokio::time::sleep(Duration::from_millis(WINDOW_MS as u64)) => {}
        }
    }
}

#[cfg(windows)]
fn note_failure(why: &str) {
    let _ = super::temp_log::append(
        "errors stopped going to the Event Log until the next settings save",
        why,
    );
}

#[cfg(not(windows))]
fn note_failure(_why: &str) {}

/// Writes one entry to the Application log as the installer's `Lodestar`
/// source, waiting for it; `Err` with why it failed.
#[cfg(windows)]
pub fn write(id: u16, message: &str) -> Result<(), String> {
    let mut cmd = super::powershell_command().ok_or("SystemRoot isn't set")?;
    let out = cmd
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            EVENT_SCRIPT,
        ])
        .env("LODESTAR_EVENT_SOURCE", super::service::NAME)
        .env("LODESTAR_EVENT_ID", id.to_string())
        .env("LODESTAR_EVENT_MESSAGE", message)
        .output()
        .map_err(|e| format!("PowerShell didn't start: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        // PowerShell's error is about the source or the log, never the
        // message, which it only passes on.
        let err = String::from_utf8_lossy(&out.stderr);
        Err(printable(
            err.lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("Write-EventLog failed"),
        ))
    }
}

/// The service's sink: a thread that writes each message in turn, so a
/// slow PowerShell never holds up the forwarder. At most [`PER_MINUTE`]
/// plus one are waiting; any more are dropped.
#[cfg(windows)]
pub struct Thread {
    tx: std::sync::mpsc::SyncSender<String>,
    failed: std::sync::Arc<Mutex<Option<String>>>,
}

#[cfg(windows)]
impl Thread {
    pub fn start() -> Option<Thread> {
        let (tx, rx) = std::sync::mpsc::sync_channel::<String>(PER_MINUTE as usize + 1);
        let failed = std::sync::Arc::new(Mutex::new(None));
        let seen = failed.clone();
        std::thread::Builder::new()
            .name("event-log".into())
            .spawn(move || {
                for message in rx {
                    if let Err(why) = write(ERROR_ID, &message) {
                        *seen.lock().unwrap_or_else(|e| e.into_inner()) = Some(why);
                    }
                }
            })
            .ok()?;
        Some(Thread { tx, failed })
    }
}

#[cfg(windows)]
impl Sink for Thread {
    fn send(&mut self, message: String) -> Result<(), String> {
        if let Some(why) = self.failed.lock().unwrap_or_else(|e| e.into_inner()).take() {
            return Err(why);
        }
        match self.tx.try_send(message) {
            Ok(()) | Err(std::sync::mpsc::TrySendError::Full(_)) => Ok(()),
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                Err("the Event Log thread stopped".into())
            }
        }
    }
}

#[cfg(test)]
#[path = "eventlog_tests.rs"]
mod tests;
