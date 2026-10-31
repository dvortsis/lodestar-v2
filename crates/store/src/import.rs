//! Imports (IMP-1, `docs/decisions/2026-10-30-IMP-1.md`; plan §6–§7):
//! durable jobs bound to a source identity, and the commit of one
//! destination batch.
//!
//! - **One transaction per batch,** on the store's single writer: the
//!   records with their provenance and quality, filter hits, reindex
//!   entries, the job's counters and its cursor. The caller acknowledges
//!   the source only after [`Store::commit_import_batch`] returns `Ok`. A
//!   batch at or below the cursor is [`BatchCommitted::Replayed`] and
//!   writes nothing, so a crash between commit and acknowledgement
//!   replays without counting twice.
//! - **Every record is checked** against tombstones, quarantine and the
//!   floor's hash list inside the transaction, whatever the caller's
//!   filter decision says: a decision can only make a record stricter.
//! - **An import never** downgrades a verified row, clears a private
//!   restriction, resurrects a tombstone or counts as a DHT discovery: a
//!   stored verified torrent only gains provenance.
//! - **Pause, cancel and blocking are truthful:** a batch commits only
//!   while its job is running, checked in the batch's own transaction.
//!
//! Credentials are never stored: a job holds a sealed secret's name.
//! `options` and the cursor's `position` are the adapter's opaque bytes;
//! they are never printed ([`ImportJob`]'s `Debug` gives their length).

use std::fmt;

use lodestar_metainfo::{InfoHash, InfoHashV2};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

use crate::{
    Error, Filtered, Held, Prepared, Result, SourceId, Store, add_sources, begin_write, filter,
    find_any, flags, insert_new, rules, write_enrichment,
};

/// The version of this contract a job was created under.
pub const CONTRACT_VERSION: i64 = 1;
/// Longest adapter options blob.
pub const MAX_OPTIONS: usize = 4096;
/// Longest cursor position.
pub const MAX_POSITION: usize = 512;
/// Longest dataset id and schema signature.
pub const MAX_IDENTITY: usize = 64;
/// Longest candidate name, as the metainfo limit.
pub const MAX_NAME: usize = 4096;

/// Persisted enums: `code` is what's stored, append only.
macro_rules! codes {
    ($(#[$m:meta])* $name:ident { $($(#[$vm:meta])* $v:ident = $c:literal => $label:literal,)+ }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name { $($(#[$vm])* $v = $c,)+ }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$v,)+];

            pub fn code(self) -> i64 {
                self as i64
            }

            pub fn from_code(code: i64) -> Option<$name> {
                match code {
                    $($c => Some($name::$v),)+
                    _ => None,
                }
            }

            /// A fixed label, for logs, metrics and the API.
            pub fn label(self) -> &'static str {
                match self {
                    $($name::$v => $label,)+
                }
            }
        }
    };
}

codes! {
    /// Where a job is. Phases only move forward.
    Phase {
        Inspecting = 0 => "inspecting",
        Ready = 1 => "ready",
        /// Source batches are read and committed.
        Reading = 2 => "reading",
        /// The source is exhausted; the last batches drain.
        Committing = 3 => "committing",
        /// Everything is committed; waiting for the index to reach
        /// [`ImportJob::max_new_id`].
        Indexing = 4 => "indexing",
        Complete = 5 => "complete",
        /// Complete, with invalid records, dropped file rows or
        /// candidates to look at.
        CompleteWithIssues = 6 => "complete_with_issues",
    }
}

codes! {
    /// What's stopping a job, orthogonal to its phase.
    Control {
        /// Runnable.
        Running = 0 => "running",
        /// By you.
        Paused = 1 => "paused",
        /// With a [`Reason`]; resuming revalidates.
        Blocked = 2 => "blocked",
        Cancelled = 3 => "cancelled",
        Failed = 4 => "failed",
    }
}

codes! {
    /// Why a job is blocked or failed.
    Reason {
        NeedsCredentials = 0 => "needs_credentials",
        SourceChanged = 1 => "source_changed",
        SourceUnavailable = 2 => "source_unavailable",
        NoSpace = 3 => "no_space",
        /// It was running when the server stopped.
        Interrupted = 4 => "interrupted",
        StoreError = 5 => "store_error",
    }
}

codes! {
    /// How complete an imported row's file list is.
    Completeness {
        Unknown = 0 => "unknown",
        Partial = 1 => "partial",
        ReportedComplete = 2 => "reported_complete",
    }
}

codes! {
    /// Whether an imported row's metadata was checked against its hash.
    Verification {
        Unknown = 0 => "unknown",
        Unverified = 1 => "unverified",
        Verified = 2 => "verified",
        Conflicting = 3 => "conflicting",
    }
}

codes! {
    /// A record's declared privacy.
    Privacy {
        Unknown = 0 => "unknown",
        Public = 1 => "public",
        Private = 2 => "private",
    }
}

impl Privacy {
    /// The more restrictive of the two: private, then unknown, then
    /// public. So private is sticky.
    pub fn merge(self, other: Privacy) -> Privacy {
        let rank = |p: Privacy| match p {
            Privacy::Public => 0,
            Privacy::Unknown => 1,
            Privacy::Private => 2,
        };
        if rank(other) > rank(self) {
            other
        } else {
            self
        }
    }
}

/// Repair reason bits (`import_quality.repair`, `import_candidates.missing`).
/// Recorded only: nothing schedules them yet (V14, CR-3).
pub mod repair {
    pub const MISSING_NAME: i64 = 1 << 0;
    pub const MISSING_SIZE: i64 = 1 << 1;
    pub const MISSING_FILES: i64 = 1 << 2;
    pub const INCONSISTENT_FILES: i64 = 1 << 3;
    pub const ALL: i64 = MISSING_NAME | MISSING_SIZE | MISSING_FILES | INCONSISTENT_FILES;
}

/// A torrent's three independent states plus privacy.
///
/// Read through [`Store::import_quality`]: a row in `import_quality` wins;
/// without one, a torrent not flagged [`flags::IMPORTED`] was fetched from
/// the network (verified, complete, private only if its info dict says
/// so), and an imported one is a legacy import whose metadata is unknown,
/// except that a positive `info_size` shows a fetched and verified info
/// dict replaced it (the legacy importer always wrote 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quality {
    pub completeness: Completeness,
    pub verification: Verification,
    pub privacy: Privacy,
    /// [`repair`] bits.
    pub repair: i64,
}

/// A job's persisted counters. Parent records obey
/// [`ImportCounters::conserved`]; `files_dropped`, `partial` and
/// `source_bytes` are outside the sum.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportCounters {
    /// Parent records in committed batches.
    pub read: u64,
    pub new: u64,
    /// Already stored; provenance added.
    pub merged: u64,
    pub candidates: u64,
    /// Dropped by a filter rule, the floor or a tombstone.
    pub filtered: u64,
    pub quarantined: u64,
    pub invalid: u64,
    /// Malformed child rows the adapter dropped, whatever became of their
    /// parents.
    pub files_dropped: u64,
    /// New rows stored with a partial file list.
    pub partial: u64,
    /// Source bytes consumed by committed batches.
    pub source_bytes: u64,
}

impl ImportCounters {
    /// `read = new + merged + candidates + filtered + quarantined + invalid`.
    pub fn conserved(&self) -> bool {
        self.read
            == self.new
                + self.merged
                + self.candidates
                + self.filtered
                + self.quarantined
                + self.invalid
    }
}

/// One import job.
#[derive(Clone, PartialEq, Eq)]
pub struct ImportJob {
    pub id: i64,
    pub adapter: String,
    pub adapter_ver: i64,
    pub contract_ver: i64,
    pub dataset: Vec<u8>,
    pub schema_sig: Vec<u8>,
    pub phase: Phase,
    pub control: Control,
    pub reason: Option<Reason>,
    /// The adapter's options; never credentials.
    pub options: Vec<u8>,
    /// A sealed secret's name.
    pub credential: Option<String>,
    pub counters: ImportCounters,
    /// The last committed batch.
    pub seq: u64,
    /// Where the adapter resumes.
    pub position: Vec<u8>,
    /// The highest torrent id this job inserted.
    pub max_new_id: i64,
    /// This job's candidates still waiting (not stored, held or
    /// promoted since).
    pub open_candidates: u64,
    /// Bumped by every phase or control change, not by batches.
    pub revision: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub finished_at: Option<i64>,
}

impl ImportJob {
    /// Cancelled, failed or complete: nothing changes it any more.
    pub fn is_finished(&self) -> bool {
        matches!(self.control, Control::Cancelled | Control::Failed)
            || matches!(self.phase, Phase::Complete | Phase::CompleteWithIssues)
    }
}

impl fmt::Debug for ImportJob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImportJob")
            .field("id", &self.id)
            .field("adapter", &self.adapter)
            .field("adapter_ver", &self.adapter_ver)
            .field("phase", &self.phase)
            .field("control", &self.control)
            .field("reason", &self.reason)
            .field("options_len", &self.options.len())
            .field("credential", &self.credential)
            .field("counters", &self.counters)
            .field("seq", &self.seq)
            .field("position_len", &self.position.len())
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

/// A job to create. Its `Debug` gives the options' length only.
#[derive(Clone, Copy)]
pub struct NewImportJob<'a> {
    /// The stable adapter name: `[a-z0-9_]`, at most 32 bytes.
    pub adapter: &'a str,
    pub adapter_ver: i64,
    /// The dataset's identity (a snapshot or database id, not a name).
    pub dataset: &'a [u8],
    /// A digest of the source schema the adapter read.
    pub schema_sig: &'a [u8],
    pub options: &'a [u8],
    /// A sealed secret's name: `[a-z0-9_-]`, at most 64 bytes.
    pub credential: Option<&'a str>,
    pub at: i64,
}

impl fmt::Debug for NewImportJob<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NewImportJob")
            .field("adapter", &self.adapter)
            .field("adapter_ver", &self.adapter_ver)
            .field("options_len", &self.options.len())
            .field("credential", &self.credential)
            .finish_non_exhaustive()
    }
}

/// What the engine found when it checked a job before resuming it.
#[derive(Debug, Clone, Copy)]
pub struct Revalidation<'a> {
    /// The source identity the adapter sees now.
    pub dataset: &'a [u8],
    pub schema_sig: &'a [u8],
    /// The job's credential reference resolves (true when it has none).
    pub credential_ok: bool,
    pub space_ok: bool,
}

/// What a filter decided about one record: every hit (held, marked or
/// shadow), as ingest records them, and the `flags` marks of one it keeps.
/// The caller runs the filter engine on the record's name; the store adds
/// its own hash checks on top.
#[derive(Debug, Clone, Copy)]
pub struct Decision<'a> {
    pub hits: &'a [Filtered<'a>],
    /// [`flags::HIDDEN`], [`flags::DEMOTED`], [`flags::FLAGGED`].
    pub marks: i64,
}

impl Decision<'_> {
    /// How the record is held back, if a hit holds it: a tombstone beats
    /// quarantine.
    fn held(&self) -> Option<Held> {
        self.hits.iter().filter_map(|f| f.held).min()
    }
}

/// An imported torrent whose fields fill a canonical row.
#[derive(Debug, Clone, Copy)]
pub struct ImportTorrent<'a> {
    pub torrent: &'a Prepared,
    pub completeness: Completeness,
    /// Never [`Verification::Verified`]: promoting verified metadata
    /// from an import is IMP-06's.
    pub verification: Verification,
    pub privacy: Privacy,
    /// [`repair`] bits.
    pub repair: i64,
    /// The source's own date; `None` when unknown or a sentinel. It dates
    /// the row only when it's positive and not after the batch.
    pub source_at: Option<i64>,
    /// Malformed child rows dropped from this record by the adapter,
    /// counted whatever happens to the record.
    pub files_dropped: u32,
    pub decision: Decision<'a>,
}

/// A valid hash too incomplete for a canonical row. An empty name or a
/// zero size is stored as missing, never as a value.
#[derive(Debug, Clone, Copy)]
pub struct ImportCandidate<'a> {
    pub hash: InfoHash,
    pub hash_v2: Option<InfoHashV2>,
    pub name: Option<&'a [u8]>,
    pub size: Option<u64>,
    pub privacy: Privacy,
    pub source_at: Option<i64>,
    pub decision: Decision<'a>,
}

/// One parent record of a batch.
#[derive(Debug, Clone, Copy)]
pub enum ImportRecord<'a> {
    Torrent(ImportTorrent<'a>),
    Candidate(ImportCandidate<'a>),
    /// Rejected by the adapter with a fixed reason; counted only.
    Invalid,
}

/// One destination batch. Its `Debug` gives the position's length only.
#[derive(Clone, Copy)]
pub struct ImportBatch<'a> {
    pub job: i64,
    /// The cursor's `seq` plus one.
    pub seq: u64,
    /// Where the adapter resumes after this batch.
    pub position: &'a [u8],
    /// The adapter's registered source: its plugin id must be the job's
    /// adapter name.
    pub source: SourceId,
    pub source_bytes: u64,
    pub at: i64,
    /// The classification-rules generation the records were enriched
    /// under, as [`crate::WriteBatch::class_gen`].
    pub class_gen: Option<i64>,
    pub records: &'a [ImportRecord<'a>],
}

impl fmt::Debug for ImportBatch<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImportBatch")
            .field("job", &self.job)
            .field("seq", &self.seq)
            .field("position_len", &self.position.len())
            .field("records", &self.records.len())
            .finish_non_exhaustive()
    }
}

/// What happened to one record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordOutcome {
    New(i64),
    /// Already stored: provenance added.
    Merged(i64),
    Candidate,
    Filtered,
    Quarantined,
    Invalid,
}

/// What [`Store::commit_import_batch`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchCommitted {
    Committed {
        /// One per record, in order.
        outcomes: Vec<RecordOutcome>,
        /// The job's counters after the batch.
        counters: ImportCounters,
        /// The classification rules changed since enrichment: new rows
        /// are marked for enriching again.
        stale_class: bool,
    },
    /// Already committed (a replay): nothing was written.
    Replayed { counters: ImportCounters },
}

/// The steps of a batch, for the crash tests' fail points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    Records,
    Counters,
    Cursor,
    Commit,
    /// After the commit, before the caller hears of it.
    Acked,
}

#[cfg(test)]
thread_local! {
    pub(crate) static FAIL_AT: std::cell::Cell<Option<Step>> = const { std::cell::Cell::new(None) };
}

fn fail_point(step: Step) -> Result<()> {
    #[cfg(test)]
    if FAIL_AT.with(|f| f.get()) == Some(step) {
        return Err(Error::Invalid("fail point"));
    }
    let _ = step;
    Ok(())
}

const MARKS: i64 = flags::HIDDEN | flags::DEMOTED | flags::FLAGGED;

const JOB_SELECT: &str = "SELECT j.id, s.adapter, j.adapter_ver, j.contract_ver, s.dataset,
         s.schema_sig, j.phase, j.control, j.reason, j.options, j.credential,
         j.read, j.new, j.merged, j.candidates, j.filtered, j.quarantined, j.invalid,
         j.files_dropped, j.partial, j.source_bytes, c.seq, c.position, j.max_new_id,
         j.revision, j.created_at, j.updated_at, j.finished_at,
         (SELECT count(*) FROM import_candidates WHERE job = j.id)
     FROM import_jobs j JOIN import_sources s ON s.id = j.source
     JOIN import_cursors c ON c.job = j.id";

fn job_row(r: &Row<'_>) -> rusqlite::Result<ImportJob> {
    let bad = |i: usize| rusqlite::Error::IntegralValueOutOfRange(i, r.get(i).unwrap_or(-1));
    let n = |i: usize| -> rusqlite::Result<u64> { Ok(r.get::<_, i64>(i)?.max(0) as u64) };
    Ok(ImportJob {
        id: r.get(0)?,
        adapter: r.get(1)?,
        adapter_ver: r.get(2)?,
        contract_ver: r.get(3)?,
        dataset: r.get(4)?,
        schema_sig: r.get(5)?,
        phase: Phase::from_code(r.get(6)?).ok_or_else(|| bad(6))?,
        control: Control::from_code(r.get(7)?).ok_or_else(|| bad(7))?,
        reason: r.get::<_, Option<i64>>(8)?.and_then(Reason::from_code),
        options: r.get(9)?,
        credential: r.get(10)?,
        counters: ImportCounters {
            read: n(11)?,
            new: n(12)?,
            merged: n(13)?,
            candidates: n(14)?,
            filtered: n(15)?,
            quarantined: n(16)?,
            invalid: n(17)?,
            files_dropped: n(18)?,
            partial: n(19)?,
            source_bytes: n(20)?,
        },
        seq: n(21)?,
        position: r.get(22)?,
        max_new_id: r.get(23)?,
        revision: r.get(24)?,
        created_at: r.get(25)?,
        updated_at: r.get(26)?,
        finished_at: r.get(27)?,
        open_candidates: n(28)?,
    })
}

fn load(conn: &Connection, id: i64) -> Result<ImportJob> {
    conn.prepare_cached(&format!("{JOB_SELECT} WHERE j.id = ?1"))?
        .query_row([id], job_row)
        .optional()?
        .ok_or(Error::NotFound("import job"))
}

fn charset_ok(s: &str, max: usize, extra: &[u8]) -> bool {
    (1..=max).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || extra.contains(&b))
}

fn identity_ok(v: &[u8]) -> bool {
    (1..=MAX_IDENTITY).contains(&v.len())
}

/// A phase or control change, applied by [`Store::change_job`].
struct Change {
    phase: Phase,
    control: Control,
    reason: Option<Reason>,
}

impl Store {
    /// Creates a job at [`Phase::Inspecting`], with an empty cursor. At
    /// most one unfinished job may use a source identity.
    pub fn create_import_job(&mut self, job: &NewImportJob<'_>) -> Result<ImportJob> {
        if !charset_ok(job.adapter, 32, b"_") {
            return Err(Error::Invalid("import adapter name"));
        }
        if !identity_ok(job.dataset) || !identity_ok(job.schema_sig) {
            return Err(Error::Invalid("import source identity"));
        }
        if job.options.len() > MAX_OPTIONS {
            return Err(Error::Invalid("import options are too long"));
        }
        if job.credential.is_some_and(|c| !charset_ok(c, 64, b"_-")) {
            return Err(Error::Invalid(
                "a credential must be a sealed secret's name",
            ));
        }
        let tx = begin_write(&mut self.conn)?;
        tx.execute(
            "INSERT INTO import_sources (adapter, dataset, schema_sig) VALUES (?1, ?2, ?3)
             ON CONFLICT DO NOTHING",
            params![job.adapter, job.dataset, job.schema_sig],
        )?;
        let source: i64 = tx.query_row(
            "SELECT id FROM import_sources WHERE adapter = ?1 AND dataset = ?2 AND schema_sig = ?3",
            params![job.adapter, job.dataset, job.schema_sig],
            |r| r.get(0),
        )?;
        let open = tx
            .query_row(
                "SELECT 1 FROM import_jobs WHERE source = ?1 AND control < 3 AND phase < 5",
                [source],
                |_| Ok(()),
            )
            .optional()?;
        if open.is_some() {
            return Err(Error::Invalid(
                "an unfinished import job already uses this source",
            ));
        }
        tx.execute(
            "INSERT INTO import_jobs (source, adapter_ver, contract_ver, phase, control, options,
                 credential, created_at, updated_at)
             VALUES (?1, ?2, ?3, 0, 0, ?4, ?5, ?6, ?6)",
            params![
                source,
                job.adapter_ver,
                CONTRACT_VERSION,
                job.options,
                job.credential,
                job.at
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO import_cursors (job, seq, position, at) VALUES (?1, 0, x'', ?2)",
            params![id, job.at],
        )?;
        let created = load(&tx, id)?;
        tx.commit()?;
        Ok(created)
    }

    pub fn import_job(&self, id: i64) -> Result<Option<ImportJob>> {
        match load(&self.conn, id) {
            Ok(job) => Ok(Some(job)),
            Err(Error::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Up to `limit` jobs, newest first, with ids below `before` (for
    /// paging; `None` from the newest).
    pub fn import_jobs(&self, before: Option<i64>, limit: usize) -> Result<Vec<ImportJob>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "{JOB_SELECT} WHERE j.id < ?1 ORDER BY j.id DESC LIMIT ?2"
        ))?;
        let rows = stmt.query_map(params![before.unwrap_or(i64::MAX), limit as i64], job_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// One phase or control change against `revision`, in one transaction.
    fn change_job(
        &mut self,
        id: i64,
        revision: i64,
        at: i64,
        f: impl FnOnce(&ImportJob) -> Result<Option<Change>>,
    ) -> Result<ImportJob> {
        let tx = begin_write(&mut self.conn)?;
        let job = load(&tx, id)?;
        if job.revision != revision {
            return Err(Error::Invalid("the import job changed since it was read"));
        }
        if job.is_finished() {
            return Err(Error::Invalid("the import job has finished"));
        }
        let Some(c) = f(&job)? else {
            return Ok(job);
        };
        let finished = matches!(c.control, Control::Cancelled | Control::Failed)
            || matches!(c.phase, Phase::Complete | Phase::CompleteWithIssues);
        tx.execute(
            "UPDATE import_jobs SET phase = ?2, control = ?3, reason = ?4,
                 revision = revision + 1, updated_at = ?5,
                 finished_at = CASE WHEN ?6 THEN ?5 ELSE finished_at END
             WHERE id = ?1",
            params![
                id,
                c.phase.code(),
                c.control.code(),
                c.reason.map(Reason::code),
                at,
                finished
            ],
        )?;
        let changed = load(&tx, id)?;
        tx.commit()?;
        Ok(changed)
    }

    /// Moves a running job forward. [`Phase::Complete`] is only reached
    /// from [`Phase::Indexing`], and is stored as
    /// [`Phase::CompleteWithIssues`] when it has invalid records, dropped
    /// file rows or candidates still waiting.
    pub fn set_import_phase(
        &mut self,
        id: i64,
        revision: i64,
        phase: Phase,
        at: i64,
    ) -> Result<ImportJob> {
        self.change_job(id, revision, at, |job| {
            if job.control != Control::Running {
                return Err(Error::Invalid("the import job isn't running"));
            }
            let phase = match phase {
                Phase::CompleteWithIssues => {
                    return Err(Error::Invalid("the store decides whether there are issues"));
                }
                Phase::Complete if job.phase != Phase::Indexing => {
                    return Err(Error::Invalid("an import completes only after indexing"));
                }
                Phase::Complete => {
                    let c = &job.counters;
                    if c.invalid > 0 || c.files_dropped > 0 || job.open_candidates > 0 {
                        Phase::CompleteWithIssues
                    } else {
                        Phase::Complete
                    }
                }
                p if p.code() <= job.phase.code() => {
                    return Err(Error::Invalid("import phases only move forward"));
                }
                p => p,
            };
            Ok(Some(Change {
                phase,
                control: Control::Running,
                reason: None,
            }))
        })
    }

    /// Pauses a running or blocked job. It stays paused across restarts.
    pub fn pause_import_job(&mut self, id: i64, revision: i64, at: i64) -> Result<ImportJob> {
        self.change_job(id, revision, at, |job| {
            Ok((job.control != Control::Paused).then_some(Change {
                phase: job.phase,
                control: Control::Paused,
                reason: None,
            }))
        })
    }

    /// Resumes a paused or blocked job after checking it again: a changed
    /// source, a missing credential or too little space blocks it with
    /// that reason instead. The cursor is kept either way.
    pub fn resume_import_job(
        &mut self,
        id: i64,
        revision: i64,
        check: &Revalidation<'_>,
        at: i64,
    ) -> Result<ImportJob> {
        self.change_job(id, revision, at, |job| {
            if !matches!(job.control, Control::Paused | Control::Blocked) {
                return Err(Error::Invalid("the import job isn't paused or blocked"));
            }
            let reason = if check.dataset != job.dataset || check.schema_sig != job.schema_sig {
                Some(Reason::SourceChanged)
            } else if !check.credential_ok {
                Some(Reason::NeedsCredentials)
            } else if !check.space_ok {
                Some(Reason::NoSpace)
            } else {
                None
            };
            Ok(Some(Change {
                phase: job.phase,
                control: if reason.is_some() {
                    Control::Blocked
                } else {
                    Control::Running
                },
                reason,
            }))
        })
    }

    /// Blocks a running job with a reason (the source went away, space ran
    /// low).
    pub fn block_import_job(
        &mut self,
        id: i64,
        revision: i64,
        reason: Reason,
        at: i64,
    ) -> Result<ImportJob> {
        self.change_job(id, revision, at, |job| {
            if job.control != Control::Running {
                return Err(Error::Invalid("the import job isn't running"));
            }
            Ok(Some(Change {
                phase: job.phase,
                control: Control::Blocked,
                reason: Some(reason),
            }))
        })
    }

    /// Cancels an unfinished job. What it committed stays.
    pub fn cancel_import_job(&mut self, id: i64, revision: i64, at: i64) -> Result<ImportJob> {
        self.change_job(id, revision, at, |job| {
            Ok(Some(Change {
                phase: job.phase,
                control: Control::Cancelled,
                reason: None,
            }))
        })
    }

    /// Fails an unfinished job. What it committed stays.
    pub fn fail_import_job(
        &mut self,
        id: i64,
        revision: i64,
        reason: Reason,
        at: i64,
    ) -> Result<ImportJob> {
        self.change_job(id, revision, at, |job| {
            Ok(Some(Change {
                phase: job.phase,
                control: Control::Failed,
                reason: Some(reason),
            }))
        })
    }

    /// At start: every job that was running and reads its source
    /// (inspecting to committing) is blocked as [`Reason::Interrupted`],
    /// so it resumes only through [`Store::resume_import_job`]'s checks.
    /// Paused jobs stay paused; indexing jobs read nothing and go on.
    /// Returns how many were blocked.
    pub fn interrupt_import_jobs(&mut self, at: i64) -> Result<usize> {
        let tx = begin_write(&mut self.conn)?;
        let n = tx.execute(
            "UPDATE import_jobs SET control = ?1, reason = ?2, revision = revision + 1,
                 updated_at = ?3
             WHERE control = ?4 AND phase <= ?5",
            params![
                Control::Blocked.code(),
                Reason::Interrupted.code(),
                at,
                Control::Running.code(),
                Phase::Committing.code()
            ],
        )?;
        tx.commit()?;
        Ok(n)
    }

    /// Commits one destination batch; see the module docs. The caller
    /// acknowledges the source only after this returns `Ok`.
    pub fn commit_import_batch(&mut self, batch: &ImportBatch<'_>) -> Result<BatchCommitted> {
        if batch.position.len() > MAX_POSITION {
            return Err(Error::Invalid("import cursor position is too long"));
        }
        if batch.seq == 0 {
            return Err(Error::Invalid("import batches are numbered from 1"));
        }
        let Store { conn, codec, .. } = self;
        let tx = begin_write(conn)?;
        let job = load(&tx, batch.job)?;
        if batch.seq <= job.seq {
            // The last batch again must resume where it left off: else the
            // adapter's numbering has drifted, and acknowledging would
            // lose a batch.
            if batch.seq == job.seq && batch.position != job.position {
                return Err(Error::Invalid("a replayed batch doesn't match the cursor"));
            }
            return Ok(BatchCommitted::Replayed {
                counters: job.counters,
            });
        }
        if batch.seq != job.seq + 1 {
            return Err(Error::Invalid("import batch out of sequence"));
        }
        if job.control != Control::Running
            || !matches!(job.phase, Phase::Reading | Phase::Committing)
        {
            return Err(Error::ImportNotRunning);
        }
        let plugin: Option<String> = tx
            .prepare_cached("SELECT plugin FROM sources WHERE id = ?1")?
            .query_row([batch.source.get()], |r| r.get(0))
            .optional()?;
        if plugin.as_deref() != Some(job.adapter.as_str()) {
            return Err(Error::Invalid("the batch's source isn't the job's adapter"));
        }
        let mut c = ImportCounters {
            read: batch.records.len() as u64,
            source_bytes: batch.source_bytes,
            ..ImportCounters::default()
        };
        let mut outcomes = Vec::with_capacity(batch.records.len());
        let mut new_ids = Vec::new();
        for record in batch.records {
            let outcome = match record {
                ImportRecord::Torrent(t) => {
                    if t.torrent
                        .files
                        .dict_id
                        .is_some_and(|id| !codec.has_dict(id))
                    {
                        return Err(Error::Corrupt("prepared blob names an unknown dictionary"));
                    }
                    c.files_dropped += u64::from(t.files_dropped);
                    let o = commit_torrent(&tx, batch, t)?;
                    if let RecordOutcome::New(id) = o {
                        new_ids.push(id);
                        if t.completeness == Completeness::Partial {
                            c.partial += 1;
                        }
                    }
                    o
                }
                ImportRecord::Candidate(cand) => commit_candidate(&tx, batch, cand)?,
                ImportRecord::Invalid => RecordOutcome::Invalid,
            };
            match outcome {
                RecordOutcome::New(_) => c.new += 1,
                RecordOutcome::Merged(_) => c.merged += 1,
                RecordOutcome::Candidate => c.candidates += 1,
                RecordOutcome::Filtered => c.filtered += 1,
                RecordOutcome::Quarantined => c.quarantined += 1,
                RecordOutcome::Invalid => c.invalid += 1,
            }
            outcomes.push(outcome);
        }
        let stale_class = match batch.class_gen {
            Some(g) if rules::class_gen(&tx)? != g => {
                let mut stmt = tx.prepare_cached(
                    "UPDATE torrent_parsed SET parser_ver = 0 WHERE torrent_id = ?1",
                )?;
                for id in &new_ids {
                    stmt.execute([id])?;
                }
                true
            }
            _ => false,
        };
        fail_point(Step::Records)?;
        let i = |v: u64| i64::try_from(v).unwrap_or(i64::MAX);
        tx.prepare_cached(
            "UPDATE import_jobs SET read = read + ?2, new = new + ?3, merged = merged + ?4,
                 candidates = candidates + ?5, filtered = filtered + ?6,
                 quarantined = quarantined + ?7, invalid = invalid + ?8,
                 files_dropped = files_dropped + ?9, partial = partial + ?10,
                 source_bytes = source_bytes + ?11, max_new_id = max(max_new_id, ?12),
                 updated_at = ?13
             WHERE id = ?1",
        )?
        .execute(params![
            batch.job,
            i(c.read),
            i(c.new),
            i(c.merged),
            i(c.candidates),
            i(c.filtered),
            i(c.quarantined),
            i(c.invalid),
            i(c.files_dropped),
            i(c.partial),
            i(c.source_bytes),
            new_ids.iter().copied().max().unwrap_or(0),
            batch.at,
        ])?;
        fail_point(Step::Counters)?;
        tx.prepare_cached(
            "UPDATE import_cursors SET seq = ?2, position = ?3, at = ?4 WHERE job = ?1",
        )?
        .execute(params![batch.job, i(batch.seq), batch.position, batch.at])?;
        fail_point(Step::Cursor)?;
        let counters = load(&tx, batch.job)?.counters;
        fail_point(Step::Commit)?;
        tx.commit()?;
        fail_point(Step::Acked)?;
        Ok(BatchCommitted::Committed {
            outcomes,
            counters,
            stale_class,
        })
    }

    /// A stored torrent's [`Quality`], by the rule [`Quality`] states;
    /// `None` when it isn't stored.
    pub fn import_quality(&self, id: i64) -> Result<Option<Quality>> {
        quality(&self.conn, id)
    }

    /// Up to `limit` torrents a job touched, by id after `after`: each
    /// with whether the job inserted it (`true`) or only added
    /// provenance.
    pub fn import_job_members(
        &self,
        job: i64,
        after: i64,
        limit: usize,
    ) -> Result<Vec<(i64, bool)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT torrent_id, created FROM import_membership
             WHERE job = ?1 AND torrent_id > ?2 ORDER BY job, torrent_id LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![job, after, limit as i64], |r| {
            Ok((r.get(0)?, r.get::<_, i64>(1)? != 0))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Takes back up to `limit` of a job's records, in one transaction
    /// (IMP-4, master plan F17: rolling back a pilot). Call it again until
    /// [`Removed::done`].
    ///
    /// - A torrent the job inserted is deleted, and logged so the indexer
    ///   drops it, unless another job touched it too or a network fetch
    ///   has verified it since: then only this job's provenance goes.
    /// - A torrent the job merged into loses this job's provenance only.
    ///   Its source bit and a privacy the import made stricter stay:
    ///   other jobs of the adapter may share the bit, and a restriction is
    ///   never loosened.
    /// - The job's waiting candidates are deleted.
    /// - Filter hits, tombstones and quarantine the job wrote stay: they
    ///   are safety records, not imported data.
    ///
    /// The job's counters stay as its history. A job still reading its
    /// source is refused: cancel or pause it first.
    pub fn remove_import_job_records(&mut self, job: i64, limit: usize) -> Result<Removed> {
        let tx = begin_write(&mut self.conn)?;
        let j = load(&tx, job)?;
        if j.control == Control::Running && j.phase.code() <= Phase::Committing.code() {
            return Err(Error::Invalid(
                "the import job is running: pause or cancel it first",
            ));
        }
        let members: Vec<(i64, bool)> = {
            let mut stmt = tx.prepare_cached(
                "SELECT torrent_id, created FROM import_membership
                 WHERE job = ?1 ORDER BY job, torrent_id LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![job, limit as i64], |r| {
                Ok((r.get(0)?, r.get::<_, i64>(1)? != 0))
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let mut out = Removed::default();
        {
            let mut shared = tx.prepare_cached(
                "SELECT 1 FROM import_membership WHERE torrent_id = ?1 AND job != ?2",
            )?;
            let mut forget = tx.prepare_cached(
                "DELETE FROM import_membership WHERE torrent_id = ?1 AND job = ?2",
            )?;
            // Cached: the delete carries Insights' trigger.
            let mut delete = tx.prepare_cached("DELETE FROM torrents WHERE id = ?1")?;
            let mut log = tx.prepare_cached("INSERT INTO reindex_log (torrent_id) VALUES (?1)")?;
            for &(id, created) in &members {
                if !created {
                    forget.execute(params![id, job])?;
                    out.provenance += 1;
                } else if shared.exists(params![id, job])? {
                    forget.execute(params![id, job])?;
                    out.kept_shared += 1;
                } else if quality(&tx, id)?
                    .is_some_and(|q| q.verification == Verification::Verified)
                {
                    forget.execute(params![id, job])?;
                    out.kept_verified += 1;
                } else {
                    // Membership and quality cascade with the row.
                    delete.execute([id])?;
                    log.execute([id])?;
                    out.torrents += 1;
                }
            }
        }
        let room = limit.saturating_sub(members.len());
        out.candidates = tx
            .prepare_cached(
                "DELETE FROM import_candidates WHERE id IN
                     (SELECT id FROM import_candidates WHERE job = ?1 LIMIT ?2)",
            )?
            .execute(params![job, room as i64])? as u64;
        out.done = members.len() < limit && (out.candidates as usize) < room;
        tx.commit()?;
        Ok(out)
    }
}

/// What one [`Store::remove_import_job_records`] call did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Removed {
    /// Torrents the job inserted, deleted.
    pub torrents: u64,
    /// Torrents the job inserted that another job touched too: kept.
    pub kept_shared: u64,
    /// Torrents the job inserted that a network fetch verified since:
    /// kept.
    pub kept_verified: u64,
    /// Stored torrents the job only merged into: provenance removed.
    pub provenance: u64,
    /// Waiting candidates deleted.
    pub candidates: u64,
    /// Nothing of the job's is left to remove.
    pub done: bool,
}

impl Removed {
    /// Adds another call's counts; `done` is the latest call's.
    pub fn add(&mut self, other: Removed) {
        self.torrents += other.torrents;
        self.kept_shared += other.kept_shared;
        self.kept_verified += other.kept_verified;
        self.provenance += other.provenance;
        self.candidates += other.candidates;
        self.done = other.done;
    }
}

/// See [`Quality`] for the rule.
pub(crate) fn quality(conn: &Connection, id: i64) -> Result<Option<Quality>> {
    Ok(conn
        .prepare_cached(
            "SELECT t.flags, t.info_size, q.completeness, q.verification, q.privacy, q.repair
             FROM torrents t LEFT JOIN import_quality q ON q.torrent_id = t.id
             WHERE t.id = ?1",
        )?
        .query_row([id], |r| {
            let (flags, info_size): (i64, i64) = (r.get(0)?, r.get(1)?);
            if let Some(c) = r.get::<_, Option<i64>>(2)? {
                return Ok(Quality {
                    completeness: Completeness::from_code(c).unwrap_or(Completeness::Unknown),
                    verification: Verification::from_code(r.get(3)?)
                        .unwrap_or(Verification::Unknown),
                    privacy: Privacy::from_code(r.get(4)?).unwrap_or(Privacy::Unknown),
                    repair: r.get(5)?,
                });
            }
            let private = flags & flags::PRIVATE != 0;
            let verified = flags & flags::IMPORTED == 0 || info_size > 0;
            Ok(if verified {
                Quality {
                    completeness: Completeness::ReportedComplete,
                    verification: Verification::Verified,
                    privacy: if private {
                        Privacy::Private
                    } else {
                        Privacy::Public
                    },
                    repair: 0,
                }
            } else {
                Quality {
                    completeness: if flags & flags::FILES_PARTIAL != 0 {
                        Completeness::Partial
                    } else {
                        Completeness::Unknown
                    },
                    verification: Verification::Unknown,
                    privacy: if private {
                        Privacy::Private
                    } else {
                        Privacy::Unknown
                    },
                    repair: 0,
                }
            })
        })
        .optional()?)
}

/// Writes a torrent's quality row, replacing any. `source_at` and
/// `imported_at` are kept when the row exists.
fn write_quality(
    conn: &Connection,
    id: i64,
    q: &Quality,
    source_at: Option<i64>,
    imported_at: Option<i64>,
) -> Result<()> {
    conn.prepare_cached(
        "INSERT INTO import_quality (torrent_id, completeness, verification, privacy, repair,
             source_at, imported_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT (torrent_id) DO UPDATE SET completeness = excluded.completeness,
             verification = excluded.verification, privacy = excluded.privacy,
             repair = excluded.repair",
    )?
    .execute(params![
        id,
        q.completeness.code(),
        q.verification.code(),
        q.privacy.code(),
        q.repair & repair::ALL,
        source_at,
        imported_at
    ])?;
    Ok(())
}

/// Whether a swarm hash is already held: a tombstone or the floor's hash
/// list (filtered), or quarantine.
fn store_hold(tx: &Transaction<'_>, hashes: &[InfoHash]) -> Result<Option<Held>> {
    let mut dropped = tx.prepare_cached(
        "SELECT 1 FROM tombstones WHERE hash = ?1 UNION ALL SELECT 1 FROM cs_hashes WHERE hash = ?1",
    )?;
    let mut quarantined = tx.prepare_cached("SELECT 1 FROM quarantine WHERE hash = ?1")?;
    let mut held = None;
    for h in hashes {
        if dropped.exists([&h.0[..]])? {
            return Ok(Some(Held::Tombstone));
        }
        if quarantined.exists([&h.0[..]])? {
            held = Some(Held::Quarantine);
        }
    }
    Ok(held)
}

fn held_outcome(held: Held) -> RecordOutcome {
    match held {
        Held::Tombstone => RecordOutcome::Filtered,
        Held::Quarantine => RecordOutcome::Quarantined,
    }
}

/// A stored torrent seen in an import: it gains the adapter's source bit
/// and the job's membership. A verified row changes nothing else; any
/// other may become private, never public.
fn merge(tx: &Transaction<'_>, id: i64, batch: &ImportBatch<'_>, privacy: Privacy) -> Result<()> {
    add_sources(tx, id, batch.source.bit() as u64)?;
    tx.prepare_cached(
        "INSERT INTO import_membership (torrent_id, job, created) VALUES (?1, ?2, 0)
         ON CONFLICT DO NOTHING",
    )?
    .execute(params![id, batch.job])?;
    let Some(q) = quality(tx, id)? else {
        return Ok(());
    };
    if q.verification == Verification::Verified {
        return Ok(());
    }
    let now = q.privacy.merge(privacy);
    if now == q.privacy {
        return Ok(());
    }
    write_quality(tx, id, &Quality { privacy: now, ..q }, None, None)?;
    if now == Privacy::Private {
        let marked = tx
            .prepare_cached(
                "UPDATE torrents SET flags = flags | ?2 WHERE id = ?1 AND flags & ?2 = 0",
            )?
            .execute(params![id, flags::PRIVATE])?;
        if marked > 0 {
            tx.prepare_cached("INSERT INTO reindex_log (torrent_id) VALUES (?1)")?
                .execute([id])?;
        }
    }
    Ok(())
}

/// The privacy a candidate under one of these hashes declared, if any.
fn candidate_privacy(tx: &Transaction<'_>, hashes: &[InfoHash]) -> Result<Option<Privacy>> {
    let mut stmt = tx.prepare_cached(
        "SELECT privacy FROM import_candidates WHERE hash = ?1
         UNION ALL
         SELECT privacy FROM import_candidates
         WHERE hash_v2 IS NOT NULL AND substr(hash_v2, 1, 20) = ?1",
    )?;
    let mut out: Option<Privacy> = None;
    for h in hashes {
        let mut rows = stmt.query([&h.0[..]])?;
        while let Some(r) = rows.next()? {
            let p = Privacy::from_code(r.get(0)?).unwrap_or(Privacy::Unknown);
            out = Some(out.map_or(p, |o| o.merge(p)));
        }
    }
    Ok(out)
}

fn commit_torrent(
    tx: &Transaction<'_>,
    batch: &ImportBatch<'_>,
    t: &ImportTorrent<'_>,
) -> Result<RecordOutcome> {
    if t.verification == Verification::Verified {
        return Err(Error::Invalid("an import can't claim verified metadata"));
    }
    let p = t.torrent;
    let hashes: Vec<InfoHash> = p.swarm_hashes().collect();
    if let Some(held) = store_hold(tx, &hashes)? {
        return Ok(held_outcome(held));
    }
    if let Some(id) = find_any(tx, hashes.iter().copied())? {
        // The stored row's own filter state stands (§20.9 refiles stored
        // torrents); a held decision only keeps this record out.
        if let Some(held) = t.decision.held() {
            return Ok(held_outcome(held));
        }
        merge(tx, id, batch, t.privacy)?;
        return Ok(RecordOutcome::Merged(id));
    }
    filter::write(tx, t.decision.hits)?;
    if let Some(held) = t.decision.held() {
        return Ok(held_outcome(held));
    }
    let mut privacy = t.privacy;
    if p.flags & flags::PRIVATE != 0 {
        privacy = privacy.merge(Privacy::Private);
    }
    if let Some(c) = candidate_privacy(tx, &hashes)? {
        privacy = privacy.merge(c);
    }
    let flags = p.flags
        | flags::IMPORTED
        | (t.decision.marks & MARKS)
        | if t.completeness == Completeness::Partial {
            flags::FILES_PARTIAL
        } else {
            0
        }
        | if privacy == Privacy::Private {
            flags::PRIVATE
        } else {
            0
        };
    // A source date in the future or not positive is a sentinel: the
    // row is dated by the import instead (`source_at` keeps it as given).
    let discovered_at = t
        .source_at
        .filter(|at| (1..=batch.at).contains(at))
        .unwrap_or(batch.at);
    let id = insert_new(tx, p, flags, discovered_at, batch.at, batch.source)?;
    if let Some(e) = &p.enrichment {
        write_enrichment(tx, id, e)?;
    }
    tx.prepare_cached(
        "INSERT INTO import_quality (torrent_id, completeness, verification, privacy, repair,
             source_at, imported_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?
    .execute(params![
        id,
        t.completeness.code(),
        t.verification.code(),
        privacy.code(),
        t.repair & repair::ALL,
        t.source_at,
        batch.at
    ])?;
    tx.prepare_cached(
        "INSERT INTO import_membership (torrent_id, job, created) VALUES (?1, ?2, 1)",
    )?
    .execute(params![id, batch.job])?;
    Ok(RecordOutcome::New(id))
}

fn commit_candidate(
    tx: &Transaction<'_>,
    batch: &ImportBatch<'_>,
    cand: &ImportCandidate<'_>,
) -> Result<RecordOutcome> {
    let mut hashes = vec![cand.hash];
    if let Some(v2) = cand
        .hash_v2
        .map(|h| h.truncated())
        .filter(|t| *t != cand.hash)
    {
        hashes.push(v2);
    }
    if let Some(held) = store_hold(tx, &hashes)? {
        return Ok(held_outcome(held));
    }
    if let Some(id) = find_any(tx, hashes.iter().copied())? {
        if let Some(held) = cand.decision.held() {
            return Ok(held_outcome(held));
        }
        merge(tx, id, batch, cand.privacy)?;
        return Ok(RecordOutcome::Merged(id));
    }
    if let Some(held) = cand.decision.held() {
        filter::write(tx, cand.decision.hits)?;
        return Ok(held_outcome(held));
    }
    // Never an empty name or a zero size: those are missing.
    let name = cand.name.filter(|n| (1..=MAX_NAME).contains(&n.len()));
    let size = cand
        .size
        .filter(|s| *s > 0)
        .and_then(|s| i64::try_from(s).ok());
    // One candidate per torrent identity, as in `torrents`. A record
    // under the swarm hashes of several stored candidates (a hybrid's v1
    // and v2 views stored apart) folds them into the oldest, so setting
    // its v2 hash can never collide with another row's.
    let mut ids = std::collections::BTreeSet::new();
    {
        let mut stmt = tx.prepare_cached(
            "SELECT id FROM import_candidates WHERE hash = ?1
             UNION ALL
             SELECT id FROM import_candidates
             WHERE hash_v2 IS NOT NULL AND substr(hash_v2, 1, 20) = ?1",
        )?;
        for h in &hashes {
            for id in stmt.query_map([&h.0[..]], |r| r.get::<_, i64>(0))? {
                ids.insert(id?);
            }
        }
    }
    let mut rows = Vec::with_capacity(ids.len());
    {
        let mut read = tx.prepare_cached(
            "SELECT hash, hash_v2, name, size, privacy, source_at FROM import_candidates
             WHERE id = ?1",
        )?;
        for id in &ids {
            rows.push(read.query_row([id], |r| {
                Ok((
                    r.get::<_, Vec<u8>>(0)?,
                    Folded {
                        hash_v2: r.get(1)?,
                        name: r.get(2)?,
                        size: r.get(3)?,
                        privacy: Privacy::from_code(r.get(4)?).unwrap_or(Privacy::Unknown),
                        source_at: r.get(5)?,
                    },
                ))
            })?);
        }
    }
    // The identity the record and the rows describe together: at most
    // one v2 hash, and at most one v1 (a hash that isn't the truncated
    // v2). Anything else is inconsistent source data: the record is
    // refused, and nothing is folded or dropped (runner, IMP-1 round 3).
    let record_v2 = cand.hash_v2.map(|h| h.0.to_vec());
    let mut v2s: Vec<&Vec<u8>> = record_v2
        .iter()
        .chain(rows.iter().filter_map(|(_, f)| f.hash_v2.as_ref()))
        .collect();
    v2s.sort();
    v2s.dedup();
    if v2s.len() > 1 {
        return Ok(RecordOutcome::Invalid);
    }
    let truncated = v2s.first().map(|v| &v[..20]);
    let mut v1s: Vec<&[u8]> = std::iter::once(&cand.hash.0[..])
        .chain(rows.iter().map(|(h, _)| &h[..]))
        .filter(|h| Some(*h) != truncated)
        .collect();
    v1s.sort();
    v1s.dedup();
    if v1s.len() > 1 {
        return Ok(RecordOutcome::Invalid);
    }
    // Keyed as `torrents` key a hybrid: by its v1 hash when one is known,
    // so a hold on either hash finds it. Every row holding that hash is
    // among those folded, so it can't collide.
    let key = v1s.first().map(|h| h.to_vec());
    filter::write(tx, cand.decision.hits)?;
    let row = match ids.first().copied() {
        Some(keep) => {
            // The oldest row's values win, then the others' in id order,
            // then the record's.
            let mut merged = Folded {
                hash_v2: record_v2,
                name: name.map(<[u8]>::to_vec),
                size,
                privacy: cand.privacy,
                source_at: cand.source_at,
            };
            for (_, row) in rows.into_iter().rev() {
                merged = row.over(merged);
            }
            let mut delete = tx.prepare_cached("DELETE FROM import_candidates WHERE id = ?1")?;
            for id in ids.iter().skip(1) {
                delete.execute([id])?;
            }
            tx.prepare_cached(
                "UPDATE import_candidates SET hash = coalesce(?7, hash), hash_v2 = ?2, name = ?3,
                     size = ?4, privacy = ?5, source_at = ?6
                 WHERE id = ?1",
            )?
            .execute(params![
                keep,
                merged.hash_v2,
                merged.name,
                merged.size,
                merged.privacy.code(),
                merged.source_at,
                key,
            ])?;
            keep
        }
        None => {
            insert_candidate(tx, batch, cand, name, size)?;
            tx.last_insert_rowid()
        }
    };
    // What's missing follows what's stored; a candidate never holds files.
    tx.prepare_cached(
        "UPDATE import_candidates SET missing = ?2
             | CASE WHEN name IS NULL THEN ?3 ELSE 0 END
             | CASE WHEN size IS NULL THEN ?4 ELSE 0 END
         WHERE id = ?1",
    )?
    .execute(params![
        row,
        repair::MISSING_FILES,
        repair::MISSING_NAME,
        repair::MISSING_SIZE
    ])?;
    Ok(RecordOutcome::Candidate)
}

/// A candidate's fields while several are folded into one.
struct Folded {
    hash_v2: Option<Vec<u8>>,
    name: Option<Vec<u8>>,
    size: Option<i64>,
    privacy: Privacy,
    source_at: Option<i64>,
}

impl Folded {
    /// `self`'s values where it has them, else `under`'s; privacy the
    /// more restrictive.
    fn over(self, under: Folded) -> Folded {
        Folded {
            hash_v2: self.hash_v2.or(under.hash_v2),
            name: self.name.or(under.name),
            size: self.size.or(under.size),
            privacy: self.privacy.merge(under.privacy),
            source_at: self.source_at.or(under.source_at),
        }
    }
}

fn insert_candidate(
    tx: &Transaction<'_>,
    batch: &ImportBatch<'_>,
    cand: &ImportCandidate<'_>,
    name: Option<&[u8]>,
    size: Option<i64>,
) -> Result<()> {
    tx.prepare_cached(
        "INSERT INTO import_candidates
             (hash, hash_v2, job, name, size, missing, privacy, source_at, at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?9, ?6, ?7, ?8)",
    )?
    .execute(params![
        &cand.hash.0[..],
        cand.hash_v2.as_ref().map(|h| &h.0[..]),
        batch.job,
        name,
        size,
        cand.privacy.code(),
        cand.source_at,
        batch.at,
        repair::MISSING_FILES
            | if name.is_none() {
                repair::MISSING_NAME
            } else {
                0
            }
            | if size.is_none() {
                repair::MISSING_SIZE
            } else {
                0
            },
    ])?;
    Ok(())
}

/// Whether a torrent's privacy is private, by [`quality`]'s rule (sticky
/// across a verified upgrade).
pub(crate) fn sticky_private(conn: &Connection, id: i64) -> Result<bool> {
    Ok(quality(conn, id)?.is_some_and(|q| q.privacy == Privacy::Private))
}

/// A partial import whose info dict was fetched and verified: its quality
/// is written out as verified and complete, so it never depends on the
/// read rule again, with no repair reasons left. It's private when it was
/// or the info dict says so (`private`), else public.
pub(crate) fn verified_upgrade(conn: &Connection, id: i64, private: bool) -> Result<()> {
    let q = Quality {
        completeness: Completeness::ReportedComplete,
        verification: Verification::Verified,
        privacy: if private {
            Privacy::Private
        } else {
            Privacy::Public
        },
        repair: 0,
    };
    write_quality(conn, id, &q, None, None)
}

#[cfg(test)]
mod tests {
    use lodestar_metainfo::InfoHash;

    use super::*;
    use crate::StoreConfig;

    const T: i64 = 1_700_000_000;
    const PASS: Decision<'static> = Decision {
        hits: &[],
        marks: 0,
    };

    fn prepared(store: &Store, name: &str) -> Prepared {
        let raw = format!(
            "d6:lengthi5e4:name{}:{name}12:piece lengthi16384e6:pieces20:",
            name.len()
        );
        let mut raw = raw.into_bytes();
        raw.extend([0; 20]);
        raw.push(b'e');
        let info = lodestar_metainfo::Info::parse(&raw).unwrap();
        store.preparer().prepare(&info, None).unwrap()
    }

    fn reading(store: &mut Store) -> ImportJob {
        let job = store
            .create_import_job(&NewImportJob {
                adapter: "magnetico",
                adapter_ver: 1,
                dataset: b"ds",
                schema_sig: b"sig",
                options: b"",
                credential: None,
                at: T,
            })
            .unwrap();
        let job = store
            .set_import_phase(job.id, job.revision, Phase::Reading, T)
            .unwrap();
        assert_eq!(job.contract_ver, CONTRACT_VERSION);
        job
    }

    fn state(store: &Store, id: i64) -> (ImportJob, i64, i64, i64) {
        let n = |sql: &str| store.conn.query_row(sql, [], |r| r.get(0)).unwrap();
        (
            store.import_job(id).unwrap().unwrap(),
            n("SELECT count(*) FROM torrents"),
            n("SELECT count(*) FROM import_candidates"),
            n("SELECT count(*) FROM import_membership"),
        )
    }

    /// A crash at each step of the transaction leaves nothing behind, and
    /// the replay commits once; a crash after the commit, before the
    /// caller hears of it, replays as `Replayed` with the same counters.
    #[test]
    fn a_crash_at_every_step_replays_exactly_once() {
        for step in [
            Step::Records,
            Step::Counters,
            Step::Cursor,
            Step::Commit,
            Step::Acked,
        ] {
            let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
            let src = store.register_source("magnetico", "1", T).unwrap();
            let job = reading(&mut store);
            let a = prepared(&store, "Alpha");
            let records = [
                ImportRecord::Torrent(ImportTorrent {
                    torrent: &a,
                    completeness: Completeness::Partial,
                    verification: Verification::Unknown,
                    privacy: Privacy::Unknown,
                    repair: repair::MISSING_FILES,
                    source_at: None,
                    files_dropped: 2,
                    decision: PASS,
                }),
                ImportRecord::Candidate(ImportCandidate {
                    hash: InfoHash([3; 20]),
                    hash_v2: None,
                    name: None,
                    size: Some(9),
                    privacy: Privacy::Public,
                    source_at: None,
                    decision: PASS,
                }),
                ImportRecord::Invalid,
            ];
            let batch = ImportBatch {
                job: job.id,
                seq: 1,
                position: b"after-1",
                source: src,
                source_bytes: 64,
                at: T,
                class_gen: None,
                records: &records,
            };
            let untouched = state(&store, job.id);

            FAIL_AT.with(|f| f.set(Some(step)));
            let crashed = store.commit_import_batch(&batch);
            FAIL_AT.with(|f| f.set(None));
            assert!(crashed.is_err(), "{step:?}");

            let first = store.commit_import_batch(&batch).unwrap();
            let after = state(&store, job.id);
            if step == Step::Acked {
                assert!(matches!(first, BatchCommitted::Replayed { .. }), "{step:?}");
            } else {
                assert!(
                    matches!(first, BatchCommitted::Committed { .. }),
                    "{step:?}"
                );
                assert_eq!((untouched.1, untouched.2, untouched.3), (0, 0, 0));
            }
            let c = after.0.counters;
            assert_eq!(
                c,
                ImportCounters {
                    read: 3,
                    new: 1,
                    merged: 0,
                    candidates: 1,
                    filtered: 0,
                    quarantined: 0,
                    invalid: 1,
                    files_dropped: 2,
                    partial: 1,
                    source_bytes: 64,
                },
                "{step:?}"
            );
            assert!(c.conserved());
            assert_eq!((after.0.seq, &after.0.position[..]), (1, &b"after-1"[..]));
            assert_eq!((after.1, after.2, after.3), (1, 1, 1), "{step:?}");
            assert_eq!(
                store.commit_import_batch(&batch).unwrap(),
                BatchCommitted::Replayed { counters: c }
            );
            assert_eq!(state(&store, job.id), after, "{step:?}");
        }
    }

    #[test]
    fn an_import_cannot_claim_verified_metadata() {
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let src = store.register_source("magnetico", "1", T).unwrap();
        let job = reading(&mut store);
        let a = prepared(&store, "Alpha");
        let records = [ImportRecord::Torrent(ImportTorrent {
            torrent: &a,
            completeness: Completeness::ReportedComplete,
            verification: Verification::Verified,
            privacy: Privacy::Public,
            repair: 0,
            source_at: None,
            files_dropped: 0,
            decision: PASS,
        })];
        let batch = ImportBatch {
            job: job.id,
            seq: 1,
            position: b"",
            source: src,
            source_bytes: 0,
            at: T,
            class_gen: None,
            records: &records,
        };
        assert!(store.commit_import_batch(&batch).is_err());
        assert_eq!(state(&store, job.id).1, 0);
    }

    /// A candidate under a pure v2 torrent's truncated hash goes when the
    /// torrent is stored, by any path, and isn't journalled.
    #[test]
    fn a_stored_torrent_replaces_its_candidate() {
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let src = store.register_source("magnetico", "1", T).unwrap();
        let job = reading(&mut store);
        let candidates: Vec<ImportRecord<'_>> = [[1u8; 20], [2; 20]]
            .into_iter()
            .map(|h| {
                ImportRecord::Candidate(ImportCandidate {
                    hash: InfoHash(h),
                    hash_v2: None,
                    name: Some(b"c"),
                    size: None,
                    privacy: Privacy::Unknown,
                    source_at: None,
                    decision: PASS,
                })
            })
            .collect();
        store
            .commit_import_batch(&ImportBatch {
                job: job.id,
                seq: 1,
                position: b"",
                source: src,
                source_bytes: 0,
                at: T,
                class_gen: None,
                records: &candidates,
            })
            .unwrap();
        let mut v2 = [0u8; 32];
        v2[..20].copy_from_slice(&[2; 20]);
        for (id, hash, v2) in [(1, [1u8; 20], None), (2, [7; 20], Some(v2))] {
            store
                .conn
                .execute(
                    "INSERT INTO torrents (id, info_hash, info_hash_v2, name, size, piece_len,
                         piece_count, file_count, info_size, flags, discovered_at, metadata_at,
                         last_seen_at)
                     VALUES (?1, ?2, ?3, 'x', 1, 1, 1, 1, 1, 0, 0, 0, 0)",
                    params![id, &hash[..], v2.as_ref().map(|v| &v[..])],
                )
                .unwrap();
        }
        let n = |sql: &str| -> i64 { store.conn.query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(n("SELECT count(*) FROM import_candidates"), 0);
        assert_eq!(n("SELECT count(*) FROM snapshot_deletions"), 0);
    }

    #[test]
    fn privacy_merges_to_the_more_restrictive() {
        use Privacy::*;
        for (a, b, want) in [
            (Public, Public, Public),
            (Public, Unknown, Unknown),
            (Unknown, Private, Private),
            (Private, Public, Private),
            (Private, Unknown, Private),
        ] {
            assert_eq!(a.merge(b), want);
            assert_eq!(b.merge(a), want);
        }
    }

    #[test]
    fn codes_round_trip() {
        for p in Phase::ALL {
            assert_eq!(Phase::from_code(p.code()), Some(*p));
        }
        for c in Control::ALL {
            assert_eq!(Control::from_code(c.code()), Some(*c));
        }
        for r in Reason::ALL {
            assert_eq!(Reason::from_code(r.code()), Some(*r));
        }
        assert_eq!(Phase::from_code(7), None);
        assert_eq!(Reason::NeedsCredentials.label(), "needs_credentials");
    }
}
