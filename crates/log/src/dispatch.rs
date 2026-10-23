//! Where an event goes once it's on: the binary's [`Sink`] (its outputs),
//! or, without one (tests, tools), `tracing` at the event's level with
//! every kind but the Private ones hidden. Also the gathered catalogues
//! and the runtime's status, which `GET /logs/catalog` reads.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, RwLock};
use std::time::Instant;

use crate::event::{Catalogue, Event, Kind, Level, Mask};
use crate::value::{Reference, Value, pairs_torrent};

/// One event occurrence on its way to the outputs.
#[derive(Debug, Clone, Copy)]
pub struct Record<'a> {
    pub event: &'static Event,
    pub fields: &'a [(&'static str, Value<'a>)],
}

impl Record<'_> {
    /// Whether a peer address in it sits next to a torrent.
    pub fn pairs_torrent(&self) -> bool {
        pairs_torrent(self.fields)
    }

    /// The message and its fields as one text line, under `mask`.
    pub fn text(&self, mask: Mask, reference: Option<Reference<'_>>) -> String {
        let pairs = self.pairs_torrent();
        let mut line = String::from(self.event.message);
        for (name, value) in self.fields {
            let _ = write!(line, " {name}={}", value.render(mask, pairs, reference));
        }
        line
    }
}

/// The binary's outputs.
pub trait Sink: Send + Sync {
    fn record(&self, record: &Record<'_>);
}

static SINK: OnceLock<&'static dyn Sink> = OnceLock::new();

/// Installs the process's outputs, once. Later calls are refused.
pub fn set_sink(sink: &'static dyn Sink) -> Result<(), &'static str> {
    SINK.set(sink)
        .map_err(|_| "a log sink is already installed")
}

/// Sends an event that's on to the outputs. [`crate::log_event!`] calls
/// it after the switch and the counter.
pub fn emit(event: &'static Event, fields: &[(&'static str, Value<'_>)]) {
    let record = Record { event, fields };
    match SINK.get() {
        Some(sink) => sink.record(&record),
        None => fallback(&record),
    }
}

/// No sink: `tracing`, Private only (no `ref:` without the run's key).
fn fallback(record: &Record<'_>) {
    let line = record.text(Mask::PRIVATE, None);
    let id = record.event.id;
    match record.event.level {
        Level::Error => tracing::error!(target: "lodestar_log", event = id, "{line}"),
        Level::Warn => tracing::warn!(target: "lodestar_log", event = id, "{line}"),
        Level::Info => tracing::info!(target: "lodestar_log", event = id, "{line}"),
        Level::Debug => tracing::debug!(target: "lodestar_log", event = id, "{line}"),
        Level::Trace => tracing::trace!(target: "lodestar_log", event = id, "{line}"),
    }
}

static CATALOGUES: OnceLock<&'static [&'static Catalogue]> = OnceLock::new();
static STARTED: OnceLock<Instant> = OnceLock::new();

/// Registers every crate's catalogue, once (the binary's explicit list;
/// §12.1: "no registry dependency"). Refused when an id repeats or a
/// second list comes.
pub fn register(catalogues: &'static [&'static Catalogue]) -> Result<(), String> {
    check_ids(catalogues)?;
    STARTED.get_or_init(Instant::now);
    match CATALOGUES.get() {
        Some(c) if std::ptr::eq(*c, catalogues) => Ok(()),
        Some(_) => Err("the log catalogues are already registered".into()),
        None => {
            let _ = CATALOGUES.set(catalogues);
            Ok(())
        }
    }
}

/// Every id once, and every id `component.name` in lower case.
pub fn check_ids(catalogues: &[&Catalogue]) -> Result<(), String> {
    let mut seen = std::collections::BTreeSet::new();
    for e in catalogues.iter().flat_map(|c| c.events.iter()) {
        let well_formed = e.id.split_once('.').is_some_and(|(a, b)| {
            !a.is_empty()
                && !b.is_empty()
                && e.id
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'.')
        });
        if !well_formed {
            return Err(format!("log event id {:?} isn't component.name", e.id));
        }
        if !seen.insert(e.id) {
            return Err(format!("log event id {:?} is declared twice", e.id));
        }
        if e.required && e.opt_in.is_some() {
            return Err(format!(
                "log event {:?} can't be both required and an opt-in",
                e.id
            ));
        }
    }
    Ok(())
}

/// The registered catalogues (empty before [`register`]).
pub fn catalogues() -> &'static [&'static Catalogue] {
    CATALOGUES.get().copied().unwrap_or(&[])
}

/// Every registered event.
pub fn events() -> impl Iterator<Item = &'static Event> {
    catalogues().iter().flat_map(|c| c.events.iter().copied())
}

pub fn find(id: &str) -> Option<&'static Event> {
    events().find(|e| e.id == id)
}

/// An event's lines per hour since start, from its counter (on or off).
pub fn rate_per_hour(event: &Event) -> u64 {
    let secs = STARTED.get().map_or(0, |s| s.elapsed().as_secs()).max(60);
    event.count().saturating_mul(3600) / secs
}

/// An output (§12.1): each has its own level and data kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Output {
    Console,
    File,
    Live,
    EventLog,
}

impl Output {
    pub const ALL: [Output; 4] = [
        Output::Console,
        Output::File,
        Output::Live,
        Output::EventLog,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Output::Console => "console",
            Output::File => "file",
            Output::Live => "live",
            Output::EventLog => "event_log",
        }
    }
}

static WRITTEN: [(AtomicU64, AtomicU64); 4] = [
    (AtomicU64::new(0), AtomicU64::new(0)),
    (AtomicU64::new(0), AtomicU64::new(0)),
    (AtomicU64::new(0), AtomicU64::new(0)),
    (AtomicU64::new(0), AtomicU64::new(0)),
];

/// Counts a line an output wrote, for the volume estimates.
pub fn note_written(output: Output, bytes: usize) {
    let (lines, total) = &WRITTEN[output as usize];
    lines.fetch_add(1, Ordering::Relaxed);
    total.fetch_add(bytes as u64, Ordering::Relaxed);
}

/// An output's average line length so far, or `default` before any line.
pub fn average_line(output: Output, default: u64) -> u64 {
    let (lines, total) = &WRITTEN[output as usize];
    match lines.load(Ordering::Relaxed) {
        0 => default,
        n => total.load(Ordering::Relaxed) / n,
    }
}

/// One data kind's state, as the runtime last applied it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KindStatus {
    pub kind: Kind,
    /// Shown on each output, by [`Output::ALL`]'s order.
    pub outputs: [bool; 4],
    /// When it turns off: 0 off, -1 until turned off, else Unix seconds.
    pub until: i64,
}

/// One output's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputStatus {
    pub output: Output,
    pub enabled: bool,
    pub level: Level,
    /// `text` or `json` (console); `jsonl` (file).
    pub format: Option<&'static str>,
    /// `private` or `file` (console).
    pub detail: Option<&'static str>,
}

/// What the runtime applied: the presets, the kinds and the outputs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Status {
    /// `errors`… `everything`, or `custom`.
    pub verbosity: &'static str,
    /// A timed verbosity ("Detailed for 30 minutes") and its deadline.
    pub temporary: Option<(&'static str, i64)>,
    /// `private`, `hashes`, `full` or `custom`.
    pub data: &'static str,
    pub kinds: Vec<KindStatus>,
    pub outputs: Vec<OutputStatus>,
}

static STATUS: RwLock<Option<Status>> = RwLock::new(None);

/// Publishes what the runtime applied.
pub fn set_status(status: Status) {
    *STATUS.write().unwrap_or_else(|e| e.into_inner()) = Some(status);
}

/// What the runtime last applied; `None` before it started (tests,
/// tools).
pub fn status() -> Option<Status> {
    STATUS.read().unwrap_or_else(|e| e.into_inner()).clone()
}
