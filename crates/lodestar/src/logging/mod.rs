//! The binary's logging runtime (architecture §12.1): it gathers every
//! crate's catalogue, renders each line for each output by that output's
//! data kinds, and writes the console and the JSONL file.
//!
//! - **One runtime per process,** installed by `settings::log::init` as
//!   `lodestar_log`'s sink. Typed events arrive through [`Runtime`]'s
//!   `Sink` impl; lines the code didn't type (third-party crates, and the
//!   first-party ones the follow-up sweep converts) through [`Forward`],
//!   a `tracing` layer behind the reloadable filter, scrubbed of URL
//!   queries, passkeys and hash-like runs.
//! - **The console** always writes, on Private kinds unless "same as the
//!   file" is chosen, and never gets an opt-in.
//! - **The file** opens when `settings::log::follow` first applies the
//!   section. Lines before that are kept in a bounded buffer, rendered on
//!   the defaults (Private), and written when it opens.
//! - Never a deadlock on itself: an event the runtime raises about its own
//!   file (disk full, a failed rotation) is written after its locks are
//!   released, and never to the file it's about.

#[cfg(test)]
mod bench;
mod entry;
pub mod events;
mod file;
mod files;
mod fold;
mod format;
mod history;
mod layer;
mod names;
mod preview;
mod read;
mod refs;
mod ring;
mod source;
pub mod state;
mod view;
mod zip;

use std::cell::Cell;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use lodestar_log::{Catalogue, Component, Event, Kind, Level, Mask, Output, Record, Sink};

use entry::Entry;
#[cfg(test)]
pub use file::CURRENT;
pub use file::Flags;
use file::{FileSink, WriteError};
use fold::{Table, Verdict};
use format::{Stamp, Untyped};
pub use layer::Forward;
pub use preview::preview;
use refs::Refs;
use ring::Ring;
#[cfg(test)]
pub use source::Logs;
pub use source::source;
use state::Applied;

/// Every crate's catalogue (§12.1: gathered here, no registry dependency).
/// A crate that declares events adds one line.
pub static CATALOGUES: &[&Catalogue] = &[
    &events::CATALOGUE,
    &lodestar_api::log_events::CATALOGUE,
    &lodestar_api::log_events::INTEGRATIONS,
    &lodestar_pipeline::log_events::CATALOGUE,
    &lodestar_search::log_events::CATALOGUE,
];

/// A registered event by id.
pub fn find(id: &str) -> Option<&'static Event> {
    CATALOGUES
        .iter()
        .flat_map(|c| c.events.iter().copied())
        .find(|e| e.id == id)
}

/// Every first-party `tracing` target and its component, most specific
/// first (the untyped lines' levels follow their component's). Every
/// workspace crate has an entry (`tests::targets_cover_every_crate`).
pub const TARGETS: &[(&str, Component)] = &[
    ("lodestar::subscriptions", Component::Filters),
    ("lodestar::floor", Component::Filters),
    ("lodestar_api::auth", Component::Auth),
    ("lodestar", Component::System),
    ("lodestar_log", Component::System),
    ("lodestar_update", Component::System),
    // The fixture generator, linked into `serve --fixture`.
    ("lodestar_bench", Component::System),
    ("lodestar_api", Component::Api),
    ("lodestar_dht", Component::Dht),
    ("lodestar_peer_wire", Component::Fetch),
    ("lodestar_utp", Component::Fetch),
    ("lodestar_sources", Component::Sources),
    ("lodestar_sources_importers", Component::Sources),
    ("lodestar_tracker_scrape", Component::Sources),
    ("lodestar_pipeline", Component::Pipeline),
    ("lodestar_store", Component::Pipeline),
    ("lodestar_classify", Component::Pipeline),
    ("lodestar_release_parser", Component::Pipeline),
    ("lodestar_metainfo", Component::Pipeline),
    ("lodestar_bencode", Component::Pipeline),
    ("lodestar_search", Component::Search),
    ("lodestar_filter", Component::Filters),
    ("lodestar_integrations", Component::Integrations),
    ("lodestar_cluster", Component::Workers),
];

/// Whether `target` is Lodestar's own code.
pub fn first_party(target: &str) -> bool {
    let krate = target.split("::").next().unwrap_or(target);
    krate == "lodestar" || krate.starts_with("lodestar_")
}

/// The component a first-party target belongs to.
pub fn component_of(target: &str) -> Option<Component> {
    TARGETS
        .iter()
        .find(|(t, _)| {
            target == *t
                || target
                    .strip_prefix(t)
                    .is_some_and(|rest| rest.starts_with("::"))
        })
        .map(|(_, c)| *c)
}

pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Start-up lines kept for the file before it opens.
const STARTUP_LINES: usize = 2_000;
const STARTUP_BYTES: usize = 2 << 20;

enum FileState {
    /// Before `follow` opens it.
    Buffering {
        lines: Vec<(String, Flags)>,
        bytes: usize,
        dropped: u64,
    },
    Open(FileSink),
    /// Turned off in the settings.
    Off,
    /// Stopped by a full disk, until a restart.
    Stopped,
}

/// Something the runtime says about itself, once its locks are released.
#[derive(Debug)]
enum Note {
    DiskFull,
    RotationFailed(std::io::Error),
    FileError(std::io::Error),
    Unavailable(std::path::PathBuf, std::io::Error),
    StartupDropped(u64),
    OverCap,
    Shared(std::path::PathBuf),
}

thread_local! {
    /// Set while the runtime reports on itself: those lines skip the file.
    static NOTING: Cell<bool> = const { Cell::new(false) };
    /// Set while this thread is inside the runtime (holding its locks).
    static IN_SINK: Cell<bool> = const { Cell::new(false) };
}

/// Whether this thread is writing a line now: a panic here must not log
/// through the runtime, whose locks it may hold.
pub fn in_sink() -> bool {
    IN_SINK.with(Cell::get)
}

/// Marks this thread as inside the runtime until dropped.
struct Inside(bool);

impl Inside {
    fn enter() -> Inside {
        Inside(IN_SINK.with(|c| c.replace(true)))
    }
}

impl Drop for Inside {
    fn drop(&mut self) {
        let was = self.0;
        IN_SINK.with(|c| c.set(was));
    }
}

/// The outputs.
pub struct Runtime {
    refs: Option<Refs>,
    console: Mutex<Box<dyn Write + Send>>,
    file: Mutex<FileState>,
    applied: RwLock<Applied>,
    /// When the last file error was reported (one a minute at most).
    file_error_at: Mutex<Option<std::time::Instant>>,
    /// The folder the file first opened in: a new one applies at a restart.
    folder: OnceLock<std::path::PathBuf>,
    /// When the previous run was last alive, from the index (Unix seconds;
    /// 0 unknown), read when the file opens.
    last_alive: std::sync::atomic::AtomicI64,
    /// The process's runtime: its state is the events' switches and the
    /// catalogue's status. A test's own runtime leaves both alone.
    installed: std::sync::atomic::AtomicBool,
    /// The first half of every line's id: 16 hex digits from the OS RNG.
    epoch: String,
    /// The live log's records.
    ring: Ring,
    /// Flood folding: the console's table, and the file's and the ring's.
    folds: [Mutex<Table>; 2],
    /// Off only in tests that count identical lines.
    folding: AtomicBool,
    /// When the current file last took a line (Unix millis; 0 none yet).
    last_file_line: AtomicI64,
    /// The rotate worker of the file opened this run, for deletions.
    jobs: Mutex<Option<file::Jobs>>,
}

/// Whether file logging is writing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    On,
    Off,
    Stopped,
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime").finish_non_exhaustive()
    }
}

static RUNTIME: OnceLock<&'static Runtime> = OnceLock::new();

/// The installed runtime, if `init` ran.
pub fn runtime() -> Option<&'static Runtime> {
    RUNTIME.get().copied()
}

/// Installs the process's runtime as `lodestar_log`'s sink, once. A second
/// call returns the first.
pub fn install(console: Box<dyn Write + Send>, applied: Applied) -> &'static Runtime {
    let _ = lodestar_log::register(CATALOGUES);
    RUNTIME.get_or_init(|| {
        let rt: &'static Runtime = Box::leak(Box::new(Runtime::new(console, applied)));
        rt.installed
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = lodestar_log::set_sink(rt);
        rt.publish();
        rt.start_timer();
        rt
    })
}

fn read<T>(l: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    l.read().unwrap_or_else(|e| e.into_inner())
}

fn lock<T: ?Sized>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A kind that turned on or off between two states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Switched {
    pub kind: Kind,
    pub on: bool,
    /// Its deadline when it turned on (-1 until turned off).
    pub until: i64,
}

/// A fresh epoch: 16 hex digits from the OS RNG (the clock's nanoseconds
/// without one, which only has to differ from the last run's).
fn new_epoch() -> String {
    let mut b = [0u8; 8];
    let n = match getrandom::fill(&mut b) {
        Ok(()) => u64::from_le_bytes(b),
        Err(_) => std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64),
    };
    format!("{n:016x}")
}

/// How often the timer flushes fold summaries whose window ended.
const FOLD_TICK: Duration = Duration::from_millis(250);

impl Runtime {
    pub fn new(console: Box<dyn Write + Send>, applied: Applied) -> Runtime {
        Runtime::with_window(console, applied, fold::WINDOW)
    }

    /// With another fold window: tests of the timer only.
    pub fn with_window(
        console: Box<dyn Write + Send>,
        applied: Applied,
        window: Duration,
    ) -> Runtime {
        Runtime {
            epoch: new_epoch(),
            ring: Ring::new(applied.file_mask),
            folds: [
                Mutex::new(Table::new(window)),
                Mutex::new(Table::new(window)),
            ],
            folding: AtomicBool::new(true),
            last_file_line: AtomicI64::new(0),
            jobs: Mutex::new(None),
            refs: Refs::new(),
            console: Mutex::new(console),
            file: Mutex::new(FileState::Buffering {
                lines: Vec::new(),
                bytes: 0,
                dropped: 0,
            }),
            applied: RwLock::new(applied),
            file_error_at: Mutex::new(None),
            installed: std::sync::atomic::AtomicBool::new(false),
            folder: OnceLock::new(),
            last_alive: std::sync::atomic::AtomicI64::new(0),
        }
    }

    fn reference(&self) -> Option<impl Fn(&[u8; 20]) -> String + '_> {
        self.refs.as_ref().map(|r| move |h: &[u8; 20]| r.of(h))
    }

    /// The state now applied.
    pub fn applied(&self) -> Applied {
        read(&self.applied).clone()
    }

    fn publish(&self) {
        if !self.installed.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        let a = read(&self.applied);
        a.switch(CATALOGUES.iter().flat_map(|c| c.events.iter().copied()));
        lodestar_log::set_status(a.status());
    }

    /// Applies a new state: the event switches, the masks, the levels and
    /// the file's limits. Returns the kinds that turned on or off.
    pub fn apply(&self, applied: Applied) -> Vec<Switched> {
        let _inside = Inside::enter();
        let before = std::mem::replace(
            &mut *self.applied.write().unwrap_or_else(|e| e.into_inner()),
            applied.clone(),
        );
        self.publish();
        let mut file = lock(&self.file);
        match (&mut *file, applied.file_enabled) {
            (FileState::Open(sink), true) => sink.set_limits(&applied.file),
            (FileState::Open(_), false) => *file = FileState::Off,
            _ => {}
        }
        let was_off = matches!(*file, FileState::Off);
        drop(file);
        if was_off && applied.file_enabled {
            self.open_file();
        }
        let switched = switched(&before, &applied);
        // The ring follows at once: what may show from now on, and the
        // kinds that turned off (a save, or a deadline through `follow`)
        // purged from what it holds.
        let off = switched
            .iter()
            .filter(|s| !s.on)
            .fold(Mask::NONE, |m, s| m.with(s.kind));
        let reference = self.reference();
        let refs = reference
            .as_ref()
            .map(|f| f as &dyn Fn(&[u8; 20]) -> String);
        self.ring.purge(applied.file_mask, off, refs);
        switched
    }

    /// Opens the file in the applied folder (once; the folder applies at a
    /// restart), and writes the start-up lines kept so far.
    pub fn open_file(&self) {
        let _inside = Inside::enter();
        let applied = self.applied();
        let mut notes = Vec::new();
        let mut file = lock(&self.file);
        let buffered = match &mut *file {
            FileState::Buffering { lines, dropped, .. } => {
                Some((std::mem::take(lines), std::mem::take(dropped)))
            }
            FileState::Off => Some((Vec::new(), 0)),
            FileState::Open(_) | FileState::Stopped => None,
        };
        if let Some((lines, dropped)) = buffered {
            if !applied.file_enabled {
                *file = FileState::Off;
            } else {
                let mut cfg = applied.file.clone();
                cfg.dir = self.folder.get_or_init(|| cfg.dir.clone()).clone();
                let sink_dir = cfg.dir.clone();
                match FileSink::open(cfg) {
                    Ok(mut sink) => {
                        if sink.shared {
                            notes.push(Note::Shared(sink_dir.clone()));
                        }
                        self.last_alive
                            .store(sink.last_alive(), std::sync::atomic::Ordering::Relaxed);
                        *lock(&self.jobs) = Some(sink.jobs());
                        let mut stopped = false;
                        for (line, flags) in &lines {
                            match sink.write(line, *flags) {
                                Ok(rotation) => {
                                    lodestar_log::note_written(Output::File, line.len() + 1);
                                    notes.extend(rotation.map(Note::RotationFailed));
                                }
                                Err(e) => {
                                    let note = write_note(e);
                                    stopped = matches!(note, Some(Note::DiskFull));
                                    notes.extend(note);
                                    if stopped {
                                        break;
                                    }
                                }
                            }
                        }
                        *file = match stopped {
                            true => FileState::Stopped,
                            false => FileState::Open(sink),
                        };
                        if dropped > 0 {
                            notes.push(Note::StartupDropped(dropped));
                        }
                    }
                    Err(e) => {
                        *file = FileState::Off;
                        notes.push(Note::Unavailable(applied.file.dir.clone(), e));
                    }
                }
            }
        }
        drop(file);
        self.note(notes);
    }

    /// When the previous run was last alive (0 unknown).
    pub fn last_alive(&self) -> i64 {
        self.last_alive.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Records in the index that this run is alive at `now`, so the next
    /// start knows which deadlines passed while it was stopped.
    pub fn heartbeat(&self, now: i64) {
        let _inside = Inside::enter();
        if let FileState::Open(sink) = &mut *lock(&self.file) {
            sink.alive(now);
        }
    }

    fn write_console(&self, line: &str) {
        let mut console = lock(&self.console);
        let _ = console
            .write_all(line.as_bytes())
            .and_then(|()| console.write_all(b"\n"))
            .and_then(|()| console.flush());
        lodestar_log::note_written(Output::Console, line.len() + 1);
    }

    fn write_file(&self, line: String, flags: Flags) -> Vec<Note> {
        let mut notes = Vec::new();
        let len = line.len() + 1;
        let mut file = lock(&self.file);
        match &mut *file {
            FileState::Buffering {
                lines,
                bytes,
                dropped,
            } => {
                if lines.len() < STARTUP_LINES && *bytes + len <= STARTUP_BYTES {
                    *bytes += len;
                    lines.push((line, flags));
                } else {
                    *dropped += 1;
                }
            }
            FileState::Open(sink) => match sink.write(&line, flags) {
                Ok(rotation) => {
                    lodestar_log::note_written(Output::File, len);
                    self.last_file_line
                        .store(Stamp::now().millis, Ordering::Relaxed);
                    notes.extend(rotation.map(Note::RotationFailed));
                }
                Err(e) => {
                    let note = write_note(e);
                    if matches!(note, Some(Note::DiskFull)) {
                        *file = FileState::Stopped;
                    }
                    notes.extend(note);
                }
            },
            FileState::Off | FileState::Stopped => {}
        }
        notes
    }

    /// Reports `notes` through the catalogue, with the runtime's locks
    /// released and the file skipped.
    fn note(&self, notes: Vec<Note>) {
        if notes.is_empty() || NOTING.with(Cell::get) {
            return;
        }
        NOTING.with(|n| n.set(true));
        for note in notes {
            match note {
                Note::DiskFull => lodestar_log::log_event!(events::LOGGING_DISK_FULL),
                Note::RotationFailed(e) => lodestar_log::log_event!(
                    events::LOGGING_ROTATION_FAILED,
                    error = lodestar_log::Error::io(&e)
                ),
                Note::FileError(e) => {
                    let mut at = lock(&self.file_error_at);
                    if at.is_none_or(|t| t.elapsed().as_secs() >= 60) {
                        *at = Some(std::time::Instant::now());
                        drop(at);
                        lodestar_log::log_event!(
                            events::LOGGING_FILE_ERROR,
                            error = lodestar_log::Error::io(&e)
                        );
                    }
                }
                Note::Unavailable(dir, e) => lodestar_log::log_event!(
                    events::LOGGING_FILE_UNAVAILABLE,
                    path = lodestar_log::Path::new(&dir),
                    error = lodestar_log::Error::io(&e)
                ),
                Note::StartupDropped(n) => lodestar_log::log_event!(
                    events::LOGGING_STARTUP_DROPPED,
                    count = lodestar_log::Count(n)
                ),
                Note::OverCap => lodestar_log::log_event!(events::LOGGING_OVER_CAP),
                Note::Shared(dir) => lodestar_log::log_event!(
                    events::LOGGING_FOLDER_SHARED,
                    path = lodestar_log::Path::new(&dir)
                ),
            }
        }
        NOTING.with(|n| n.set(false));
    }

    /// A line the code didn't type, already through the `tracing` filter.
    pub fn record_untyped(&self, u: &Untyped) {
        let _inside = Inside::enter();
        let a = read(&self.applied);
        let line = Entry::untyped(Stamp::now(), u);
        let console = (u.level <= a.console_level).then(|| line.clone());
        let noting = NOTING.with(Cell::get);
        let file = a.file_enabled && u.level <= a.file_level && !noting;
        let (json, flags) = (a.console_json, flags_of(&a));
        drop(a);
        self.emit(console, json, line, file, flags, !noting);
    }

    /// Writes one record to every output: the console's line (if it
    /// passed the console's level) and the ring's and the file's, each
    /// folded by its own table when `foldable`.
    fn emit(
        &self,
        console: Option<Entry>,
        console_json: bool,
        line: Entry,
        file_on: bool,
        flags: Flags,
        foldable: bool,
    ) {
        let now = Instant::now();
        let folding = foldable && self.folding.load(Ordering::Relaxed);
        if let Some(c) = console {
            let mut due = Vec::new();
            let write = !folding
                || matches!(
                    lock(&self.folds[0]).check(&c, now, &mut due),
                    Verdict::Write(_)
                );
            for s in due {
                self.write_console_entry(&s, console_json);
            }
            if write {
                self.write_console_entry(&c, console_json);
            }
        }
        let mut due = Vec::new();
        let verdict = match folding {
            true => lock(&self.folds[1]).check(&line, now, &mut due),
            false => Verdict::Write(None),
        };
        if !due.is_empty() {
            self.keep_summaries(due);
        }
        if let Verdict::Write(key) = verdict {
            let seq = self.keep(line, file_on, flags);
            if let Some(key) = key {
                lock(&self.folds[1]).first(key, seq);
            }
        }
    }

    /// Writes summaries to the ring and, by their level, the file.
    fn keep_summaries(&self, due: Vec<Entry>) {
        let a = read(&self.applied);
        let (enabled, level, flags) = (a.file_enabled, a.file_level, flags_of(&a));
        drop(a);
        let noting = NOTING.with(Cell::get);
        for s in due {
            let file = enabled && s.level <= level && !noting;
            self.keep(s, file, flags);
        }
    }

    fn write_console_entry(&self, e: &Entry, json: bool) {
        let line = match json {
            true => e.json(None, None),
            false => e.text(),
        };
        self.write_console(&line);
    }

    /// Puts `e` in the ring (its own copy, when that differs) and writes
    /// its file line with its id. Returns its sequence.
    fn keep(&self, e: Entry, file_on: bool, flags: Flags) -> u64 {
        let reference = self.reference();
        let refs = reference
            .as_ref()
            .map(|f| f as &dyn Fn(&[u8; 20]) -> String);
        let (seq, file_line) = match e.ring_copy() {
            Some(copy) => {
                lodestar_log::note_written(Output::Live, copy.accounted());
                let (seq, _, off) = self.ring.push(copy, refs);
                // Purged as the ring purged its copy, if a kind turned off.
                let line = match off == Mask::NONE {
                    true => Some(e),
                    false => ring::purged(&e, off, refs),
                };
                let line = line
                    .filter(|_| file_on)
                    .map(|l| l.json(Some(&self.id(seq)), Some(&self.epoch)));
                (seq, line)
            }
            None => {
                lodestar_log::note_written(Output::Live, e.accounted());
                let (seq, kept, _) = self.ring.push(e, refs);
                // A line the ring purged as it came in (rendered under the
                // mask before a kind turned off) goes to the file as purged.
                let line = kept
                    .filter(|_| file_on)
                    .map(|k| k.json(Some(&self.id(seq)), Some(&self.epoch)));
                (seq, line)
            }
        };
        if let Some(line) = file_line {
            let notes = self.write_file(line, flags);
            self.note(notes);
        }
        seq
    }

    /// `<epoch>-<seq>`.
    pub fn id(&self, seq: u64) -> String {
        format!("{}-{seq}", self.epoch)
    }

    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    /// Writes the summaries of fold windows that have ended (all of them
    /// with `all`, before `app.stop`'s line).
    pub fn flush_folds(&self, all: bool) {
        let _inside = Inside::enter();
        let now = Instant::now();
        let mut console = Vec::new();
        lock(&self.folds[0]).flush(now, all, &mut console);
        let mut file = Vec::new();
        lock(&self.folds[1]).flush(now, all, &mut file);
        if !console.is_empty() {
            let json = read(&self.applied).console_json;
            for s in &console {
                self.write_console_entry(s, json);
            }
        }
        if !file.is_empty() {
            self.keep_summaries(file);
        }
    }

    /// Folding on or off (tests that count identical lines).
    #[cfg(test)]
    pub fn set_folding(&self, on: bool) {
        self.folding.store(on, Ordering::Relaxed);
    }

    /// The timer that flushes fold summaries within a second of their
    /// window's end.
    fn start_timer(&'static self) {
        let _ = std::thread::Builder::new()
            .name("log-fold".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(FOLD_TICK);
                    self.flush_folds(false);
                }
            });
    }

    /// The live mask now: the file's, less any kind whose deadline has
    /// passed though `follow` hasn't re-applied yet.
    pub fn mask_now(&self) -> Mask {
        let a = read(&self.applied);
        let now = unix_now();
        a.kinds
            .iter()
            .filter(|k| k.until > 0 && k.until <= now)
            .fold(a.file_mask, |m, k| m.without(k.kind))
    }

    pub(crate) fn ring(&self) -> &Ring {
        &self.ring
    }

    /// The folder the file opened in this run.
    pub fn opened_folder(&self) -> Option<std::path::PathBuf> {
        self.folder.get().cloned()
    }

    /// The configured folder (it applies at a restart).
    pub fn configured_folder(&self) -> std::path::PathBuf {
        read(&self.applied).file.dir.clone()
    }

    pub fn file_status(&self) -> FileStatus {
        match &*lock(&self.file) {
            FileState::Open(_) => FileStatus::On,
            FileState::Stopped => FileStatus::Stopped,
            FileState::Off => FileStatus::Off,
            FileState::Buffering { .. } => match read(&self.applied).file_enabled {
                true => FileStatus::On,
                false => FileStatus::Off,
            },
        }
    }

    /// When the current file last took a line (Unix millis; 0 none).
    pub fn last_file_line(&self) -> i64 {
        self.last_file_line.load(Ordering::Relaxed)
    }

    /// Deletes `names` in `dir`, and the current file with `current`:
    /// rotated first under the writer's lock, so it's never touched
    /// mid-write and no new line is lost (with no writer, moved aside
    /// under the same lock, so one opening later starts a new file). On
    /// the rotate worker when the file opened there this run, after the
    /// compressions queued before it, so a file being compressed can't
    /// come back as `.gz` after it's deleted; else here, under the same
    /// checks.
    pub(crate) fn delete_logs(
        &self,
        dir: &std::path::Path,
        mut names: Vec<names::LogName>,
        current: bool,
        all: bool,
    ) -> std::io::Result<file::Deleted> {
        let jobs = lock(&self.jobs).clone().filter(|j| j.dir == dir);
        // The writer's index when it has this folder, else read here.
        let index = match &jobs {
            Some(j) => j.index.clone(),
            None => Arc::new(Mutex::new(file::Index::load(dir))),
        };
        let _inside = Inside::enter();
        let mut skipped = Vec::new();
        // "All" never sweeps a file rotated after this request.
        let mut upto = names::LogName::parse(&format!(
            "lodestar-{}.jsonl",
            format::Stamp::now().compact()
        ));
        if current {
            let mut file = lock(&self.file);
            let aside = match &mut *file {
                FileState::Open(sink) if self.folder.get().map(|d| d.as_path()) == Some(dir) => {
                    sink.rotate_to_delete().map_err(|_| "failed")
                }
                // No writer in this folder: nothing is mid-write.
                _ => file::set_aside(dir, &index).and_then(|n| n.ok_or("none")),
            };
            match aside.map(|n| names::LogName::parse(&n)) {
                Ok(Some(rotated)) => {
                    upto = Some(rotated.clone());
                    names.push(rotated);
                }
                Ok(None) | Err("none") => {}
                Err(why) => skipped.push((file::CURRENT.to_string(), why)),
            }
        }
        let take = match all {
            true => file::Take::All { upto },
            false => file::Take::Details,
        };
        let mut done = match jobs.map(|j| j.delete(names.clone(), take.clone())) {
            Some(Ok(d)) => d,
            Some(Err(Some(e))) => return Err(e),
            // No worker for this folder: nothing compresses in it.
            Some(Err(None)) | None => file::delete(dir, &names, &take, &index),
        };
        done.skipped.extend(skipped);
        Ok(done)
    }
}

/// What a file written under `a` is marked with.
fn flags_of(a: &Applied) -> Flags {
    Flags {
        details: a.file_mask.has_details(),
        opt_in: a.file_mask.has(Kind::PeerActivity) || a.file_mask.has(Kind::FilterChange),
    }
}

fn write_note(e: WriteError) -> Option<Note> {
    match e {
        WriteError::DiskFull => Some(Note::DiskFull),
        WriteError::OverCap { first } => first.then_some(Note::OverCap),
        WriteError::Other(e) => Some(Note::FileError(e)),
    }
}

impl Sink for Runtime {
    fn record(&self, r: &Record<'_>) {
        let _inside = Inside::enter();
        let e = r.event;
        // Pending summaries go out before the stop line.
        if e.id == events::APP_STOP::EVENT.id {
            self.flush_folds(true);
        }
        let a = read(&self.applied);
        let stamp = Stamp::now();
        let reference = self.reference();
        let refs = reference
            .as_ref()
            .map(|f| f as &dyn Fn(&[u8; 20]) -> String);
        // The opt-in events never reach the console (Lodestar can't delete
        // what `docker logs` keeps), and required events pass every level.
        let console = (e.opt_in.is_none() && (e.required || e.level <= a.console_level))
            .then(|| Entry::typed(stamp, r, a.console_mask, refs));
        // Rendered once under the live mask (the file's): the ring keeps
        // it and the file line is written from it.
        let line = Entry::typed(stamp, r, a.file_mask, refs);
        let noting = NOTING.with(Cell::get);
        let file = a.file_enabled && (e.required || e.level <= a.file_level) && !noting;
        let (json, flags) = (a.console_json, flags_of(&a));
        drop(a);
        self.emit(console, json, line, file, flags, !e.required && !noting);
    }
}

/// The kinds whose file switch differs between two states.
fn switched(before: &Applied, after: &Applied) -> Vec<Switched> {
    Kind::ALL
        .into_iter()
        .filter_map(|kind| {
            let was = before.file_mask.has(kind);
            let is = after.file_mask.has(kind);
            (was != is).then(|| Switched {
                kind,
                on: is,
                until: after
                    .kinds
                    .iter()
                    .find(|k| k.kind == kind)
                    .map_or(0, |k| k.until),
            })
        })
        .collect()
}

/// Every level as `lodestar_log` has it, from `tracing`'s.
pub fn level_of(l: &tracing::Level) -> Level {
    match *l {
        tracing::Level::ERROR => Level::Error,
        tracing::Level::WARN => Level::Warn,
        tracing::Level::INFO => Level::Info,
        tracing::Level::DEBUG => Level::Debug,
        tracing::Level::TRACE => Level::Trace,
    }
}

#[cfg(test)]
mod source_tests;
#[cfg(test)]
pub(crate) mod tests;
