//! The import driver (IMP-4, `docs/decisions/2026-10-31-IMP-4.md`): runs
//! one job from its adapter through the filter into the store.
//!
//! - **The source must still be the job's:** its identity is checked
//!   against the job's binding before anything is read, and the adapter
//!   checks it again before every batch.
//! - **Every record goes through the full filter,** as at ingest: the
//!   floor and the CS-1 hash list, subscribed lists, the core, packs and
//!   your rules, on the record's name and files. The store then checks
//!   tombstones, quarantine and the floor's hashes again in the batch's
//!   own transaction, so a held or deleted hash is never resurrected.
//! - **One transaction per batch:** the accepted records, their filter
//!   hits and the cursor advance commit together
//!   ([`Store::commit_import_batch`]). A crash anywhere replays from the
//!   last committed cursor and counts nothing twice.
//! - **Stopping is truthful:** the stop flag is checked between batches,
//!   and the job is paused with exactly what was committed. A batch in
//!   flight is either committed whole or not at all.
//!
//! Imports aren't DHT discoveries: the rows carry the adapter's own
//! registered source and [`flags::IMPORTED`], never the crawler's.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lodestar_classify::{Classification, Correction, FileInfo, file_infos};
use lodestar_filter::reclassify::{adult_hold, enrich_corrected};
use lodestar_filter::{Action, Engine, Facts, Item, Sealer, Verdict};
use lodestar_store::import::{
    BatchCommitted, Control, Decision, ImportBatch, ImportCandidate, ImportCounters, ImportJob,
    ImportRecord, ImportTorrent, Phase, Reason,
};
use lodestar_store::{Error, Filtered, Held, Prepared, SHADOW, SourceId, Store, flags};

use crate::{bitmagnet, magnetico};

/// An opened source, of either adapter.
pub enum Adapter {
    Magnetico(magnetico::Source),
    Bitmagnet(bitmagnet::Source),
}

impl std::fmt::Debug for Adapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Adapter({})", self.name())
    }
}

/// Why a source couldn't be read: the adapter's fixed label, and what it
/// means for a running job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRefusal {
    pub label: &'static str,
    /// The reason a running job blocks with; `None` when it fails.
    pub job_reason: Option<Reason>,
    /// The adapter's message, which names the safe step. Fixed text
    /// only: no names, hashes or row content.
    pub message: String,
}

impl From<magnetico::Refusal> for SourceRefusal {
    fn from(r: magnetico::Refusal) -> Self {
        SourceRefusal {
            label: r.label(),
            job_reason: r.job_reason(),
            message: r.to_string(),
        }
    }
}

impl From<bitmagnet::Refusal> for SourceRefusal {
    fn from(r: bitmagnet::Refusal) -> Self {
        SourceRefusal {
            label: r.label(),
            job_reason: r.job_reason(),
            message: r.to_string(),
        }
    }
}

/// One parent record, as its adapter normalised it.
enum Rec {
    Magnetico(magnetico::Record),
    Bitmagnet(bitmagnet::Record),
}

/// One source batch, of either adapter.
struct SourceBatch {
    records: Vec<Rec>,
    position: Vec<u8>,
    source_bytes: u64,
    findings: Vec<(&'static str, u64)>,
}

impl Adapter {
    /// The stable adapter name: the job's adapter and the registered
    /// source's plugin id.
    pub fn name(&self) -> &'static str {
        match self {
            Adapter::Magnetico(_) => magnetico::ADAPTER,
            Adapter::Bitmagnet(_) => bitmagnet::ADAPTER,
        }
    }

    pub fn version(&self) -> i64 {
        match self {
            Adapter::Magnetico(_) => magnetico::ADAPTER_VERSION,
            Adapter::Bitmagnet(_) => bitmagnet::ADAPTER_VERSION,
        }
    }

    /// The source identity `(dataset, schema_sig)` a job binds to.
    pub fn identity(&self) -> ([u8; 32], [u8; 32]) {
        match self {
            Adapter::Magnetico(s) => (s.identity().dataset, s.identity().schema_sig),
            Adapter::Bitmagnet(s) => (s.identity().dataset, s.identity().schema_sig),
        }
    }

    /// Cancels the running read, and every later one.
    pub fn canceller(&self) -> Arc<AtomicBool> {
        match self {
            Adapter::Magnetico(s) => s.canceller(),
            Adapter::Bitmagnet(s) => s.canceller(),
        }
    }

    async fn read(
        &mut self,
        position: &[u8],
        now: i64,
    ) -> Result<Option<SourceBatch>, SourceRefusal> {
        Ok(match self {
            Adapter::Magnetico(s) => s.read_batch(position, now)?.map(|b| SourceBatch {
                findings: b.findings.iter().collect(),
                records: b.records.into_iter().map(Rec::Magnetico).collect(),
                position: b.position,
                source_bytes: b.source_bytes,
            }),
            Adapter::Bitmagnet(s) => s.read_batch(position, now).await?.map(|b| SourceBatch {
                findings: b.findings.iter().collect(),
                records: b.records.into_iter().map(Rec::Bitmagnet).collect(),
                position: b.position,
                source_bytes: b.source_bytes,
            }),
        })
    }
}

impl Rec {
    /// The record as an unverified info dict, when it's a torrent.
    fn info(&self) -> Option<lodestar_metainfo::Info> {
        match self {
            Rec::Magnetico(magnetico::Record::Torrent(t)) => Some(t.info()),
            Rec::Bitmagnet(bitmagnet::Record::Torrent(t)) => Some(t.info()),
            _ => None,
        }
    }

    /// The IMP-1 torrent record, by the adapter's own mapping.
    fn torrent<'a>(&self, p: &'a Prepared, d: Decision<'a>) -> Option<ImportTorrent<'a>> {
        match self {
            Rec::Magnetico(magnetico::Record::Torrent(t)) => Some(t.import(p, d)),
            Rec::Bitmagnet(bitmagnet::Record::Torrent(t)) => Some(t.import(p, d)),
            _ => None,
        }
    }

    /// The IMP-1 candidate record, by the adapter's own mapping.
    fn candidate<'a>(&'a self, d: Decision<'a>) -> Option<ImportCandidate<'a>> {
        match self {
            Rec::Magnetico(magnetico::Record::Candidate(c)) => Some(c.import(d)),
            Rec::Bitmagnet(bitmagnet::Record::Candidate(c)) => Some(c.import(d)),
            _ => None,
        }
    }
}

/// The filter a run judges records with.
#[derive(Clone, Copy)]
pub struct Filter<'a> {
    pub engine: &'a Engine,
    /// Seals the name of a record the filter quarantines. Without one a
    /// quarantine keeps no name.
    pub sealer: Option<&'a Sealer>,
    /// How long a quarantine lasts.
    pub quarantine_secs: i64,
    /// The classification-rules generation ([`Store::class_gen`]) read
    /// *before* the engine's rules were loaded, as the writer reads it: if
    /// the rules change during the run, the store marks the new rows for
    /// enriching again.
    pub class_gen: i64,
}

impl std::fmt::Debug for Filter<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Filter")
            .field("version", &self.engine.version)
            .field("sealer", &self.sealer.is_some())
            .finish_non_exhaustive()
    }
}

/// What a run may do besides reading.
#[derive(Clone, Copy)]
pub struct Options<'a> {
    /// The adapter's registered source ([`Store::register_source`] under
    /// the adapter's name).
    pub source: SourceId,
    pub filter: Filter<'a>,
    /// Set to stop between batches: the job is paused. Also cancel the
    /// adapter's running read ([`Adapter::canceller`]) to stop sooner.
    pub stop: &'a AtomicBool,
    /// Pause once this run has committed this many records (rounded up to
    /// a whole batch): a pilot of a big source.
    pub limit: Option<u64>,
    /// The clock, in Unix seconds.
    pub now: fn() -> i64,
    /// Called after each committed batch with the job's counters.
    pub progress: Option<&'a dyn Fn(&ImportCounters)>,
}

impl std::fmt::Debug for Options<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Options")
            .field("source", &self.source)
            .field("filter", &self.filter)
            .field("limit", &self.limit)
            .finish_non_exhaustive()
    }
}

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// The source is exhausted: the job waits for the index (`indexing`).
    Exhausted,
    /// Stopped by you, or at the limit: paused, resumable.
    Paused,
    /// Blocked with a reason; resuming checks it again.
    Blocked(Reason),
    /// The source can't be imported (an unsupported or refused source).
    Failed,
    /// The job wasn't running when the run started or a batch committed
    /// (paused, blocked or finished elsewhere). Nothing was lost.
    NotRunning(Control),
}

/// What one run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub ended: Ended,
    /// The adapter's refusal label, when a refusal ended the run.
    pub refusal: Option<SourceRefusal>,
    /// Batches this run committed.
    pub batches: u64,
    /// Records this run committed.
    pub records: u64,
    /// The job's persisted counters after the run (all runs).
    pub counters: ImportCounters,
    /// This run's per-reason counts: the adapter's findings, plus
    /// `prepare_rejected` for torrents the store couldn't prepare
    /// (committed as invalid).
    pub findings: BTreeMap<&'static str, u64>,
}

/// A run that couldn't go on: a store error (the job is blocked with
/// `store_error` where that could still be written), or a job that can't
/// run at all.
#[derive(Debug)]
pub enum DriverError {
    Store(Error),
    /// The job isn't this adapter's, or is finished.
    Job(&'static str),
    /// A test's injected crash.
    #[cfg(test)]
    Crash(Fault),
}

impl std::fmt::Display for DriverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DriverError::Store(e) => e.fmt(f),
            DriverError::Job(m) => f.write_str(m),
            #[cfg(test)]
            DriverError::Crash(at) => write!(f, "crash at {at:?}"),
        }
    }
}

impl std::error::Error for DriverError {}

impl From<Error> for DriverError {
    fn from(e: Error) -> Self {
        DriverError::Store(e)
    }
}

/// Where a test crashes the driver.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// A batch was read; nothing judged.
    AfterRead,
    /// Half the batch's records are judged.
    MidBatch,
    /// Everything judged, just before the commit.
    BeforeCommit,
    /// Committed, before the driver moves its cursor.
    AfterCommit,
}

#[cfg(test)]
thread_local! {
    /// `(fault, batch)`: crash there in the run's nth batch (from 1).
    pub(crate) static CRASH: std::cell::Cell<Option<(Fault, u64)>> =
        const { std::cell::Cell::new(None) };
    /// Runs at every fault point: a change made elsewhere mid-run.
    #[allow(clippy::type_complexity)]
    pub(crate) static HOOK: std::cell::RefCell<Option<Box<dyn FnMut(Fault, u64)>>> =
        const { std::cell::RefCell::new(None) };
}

fn crash(at: Fault, batch: u64) -> Result<(), DriverError> {
    #[cfg(test)]
    HOOK.with(|h| {
        if let Some(f) = h.borrow_mut().as_mut() {
            f(at, batch);
        }
    });
    #[cfg(test)]
    if CRASH.with(|c| c.get()) == Some((at, batch)) {
        return Err(DriverError::Crash(at));
    }
    let _ = (at, batch);
    Ok(())
}

/// One filter decision, owned until the batch is built.
struct Hit {
    rule: String,
    action: u8,
    held: Option<Held>,
    sealed: Option<Vec<u8>>,
    evidence: Vec<u8>,
    adult_hold: bool,
}

/// A record judged: what the store needs besides the record itself.
#[derive(Default)]
struct Judged {
    hashes: Vec<[u8; 20]>,
    hits: Vec<Hit>,
    marks: i64,
}

/// Runs the job until its source is exhausted, it's stopped, or a refusal
/// or store error ends it. The job must be running (`resume` it first).
pub async fn run(
    store: &mut Store,
    job_id: i64,
    adapter: &mut Adapter,
    opts: &Options<'_>,
) -> Result<Report, DriverError> {
    let mut job = load(store, job_id)?;
    if job.adapter != adapter.name() {
        return Err(DriverError::Job("the import job is another adapter's"));
    }
    if job.is_finished() {
        return Err(DriverError::Job("the import job has finished"));
    }
    let mut report = Report {
        ended: Ended::Exhausted,
        refusal: None,
        batches: 0,
        records: 0,
        counters: job.counters,
        findings: BTreeMap::new(),
    };
    if job.control != Control::Running {
        report.ended = Ended::NotRunning(job.control);
        return Ok(report);
    }
    // The source must be the one the job (and so its cursor) is bound to.
    let (dataset, schema_sig) = adapter.identity();
    if job.dataset != dataset || job.schema_sig != schema_sig {
        job = store.block_import_job(job.id, job.revision, Reason::SourceChanged, (opts.now)())?;
        report.ended = Ended::Blocked(Reason::SourceChanged);
        report.counters = job.counters;
        return Ok(report);
    }
    for phase in [Phase::Ready, Phase::Reading] {
        if job.phase.code() < phase.code() {
            job = store.set_import_phase(job.id, job.revision, phase, (opts.now)())?;
        }
    }
    let mut preparer = store.preparer();
    let mut seq = job.seq;
    let mut position = job.position.clone();
    loop {
        if job.phase.code() > Phase::Committing.code() {
            break;
        }
        if opts.stop.load(Ordering::Relaxed) || opts.limit.is_some_and(|l| report.records >= l) {
            return pause(store, job.id, report, opts);
        }
        let now = (opts.now)();
        let read = adapter.read(&position, now).await;
        let batch = match read {
            Ok(Some(b)) => b,
            Ok(None) => {
                // Exhausted: everything is committed, so the job waits for
                // the index.
                for phase in [Phase::Committing, Phase::Indexing] {
                    if load(store, job.id)?.phase.code() < phase.code() {
                        match change(store, job.id, |s, j| {
                            s.set_import_phase(j.id, j.revision, phase, now)
                        })? {
                            Ok(j) => job = j,
                            Err(c) => return not_running(store, job.id, report, c),
                        }
                    }
                }
                report.counters = job.counters;
                return Ok(report);
            }
            Err(r) if r.label == "cancelled" => return pause(store, job.id, report, opts),
            Err(r) => {
                let changed = match r.job_reason {
                    Some(reason) => {
                        report.ended = Ended::Blocked(reason);
                        change(store, job.id, |s, j| {
                            s.block_import_job(j.id, j.revision, reason, now)
                        })?
                    }
                    None => {
                        report.ended = Ended::Failed;
                        change(store, job.id, |s, j| {
                            s.fail_import_job(j.id, j.revision, Reason::SourceUnavailable, now)
                        })?
                    }
                };
                report.refusal = Some(r);
                return match changed {
                    Ok(j) => {
                        report.counters = j.counters;
                        Ok(report)
                    }
                    Err(c) => not_running(store, job.id, report, c),
                };
            }
        };
        let n = report.batches + 1;
        crash(Fault::AfterRead, n)?;
        let mut prepared: Vec<Option<Prepared>> = Vec::with_capacity(batch.records.len());
        let mut judged: Vec<Judged> = Vec::with_capacity(batch.records.len());
        let mut prepare_rejected = 0;
        for (i, rec) in batch.records.iter().enumerate() {
            if i == batch.records.len() / 2 {
                crash(Fault::MidBatch, n)?;
            }
            let (p, j) = match rec.info() {
                Some(info) => match preparer.prepare(&info, None) {
                    Ok(mut p) => {
                        let j = judge_torrent(&opts.filter, &mut p, &info, rec, now);
                        (Some(p), j)
                    }
                    // A record the store can't prepare is committed as
                    // invalid (IMP-2).
                    Err(_) => {
                        prepare_rejected += 1;
                        (None, Judged::default())
                    }
                },
                None => (None, judge_candidate(&opts.filter, rec, now)),
            };
            prepared.push(p);
            judged.push(j);
        }
        let quarantine_until = now.saturating_add(opts.filter.quarantine_secs);
        let filtered: Vec<Vec<Filtered<'_>>> = judged
            .iter()
            .map(|j| {
                j.hits
                    .iter()
                    .map(|h| Filtered {
                        hashes: &j.hashes,
                        rule: &h.rule,
                        action: h.action,
                        held: h.held,
                        sealed: h.sealed.as_deref(),
                        evidence: &h.evidence,
                        at: now,
                        expires_at: quarantine_until,
                        adult_hold: h.adult_hold,
                    })
                    .collect()
            })
            .collect();
        let records: Vec<ImportRecord<'_>> = batch
            .records
            .iter()
            .zip(&prepared)
            .zip(judged.iter().zip(&filtered))
            .map(|((rec, p), (j, hits))| {
                let d = Decision {
                    hits,
                    marks: j.marks,
                };
                match p {
                    Some(p) => rec.torrent(p, d).map(ImportRecord::Torrent),
                    None => rec.candidate(d).map(ImportRecord::Candidate),
                }
                .unwrap_or(ImportRecord::Invalid)
            })
            .collect();
        crash(Fault::BeforeCommit, n)?;
        let committed = store.commit_import_batch(&ImportBatch {
            job: job.id,
            seq: seq + 1,
            position: &batch.position,
            source: opts.source,
            source_bytes: batch.source_bytes,
            at: now,
            class_gen: Some(opts.filter.class_gen),
            records: &records,
        });
        match committed {
            Ok(BatchCommitted::Committed { counters, .. }) => {
                crash(Fault::AfterCommit, n)?;
                seq += 1;
                position = batch.position;
                report.batches += 1;
                report.records += records.len() as u64;
                report.counters = counters;
                if let Some(p) = opts.progress {
                    p(&counters);
                }
                for (label, count) in &batch.findings {
                    *report.findings.entry(label).or_default() += count;
                }
                if prepare_rejected > 0 {
                    *report.findings.entry("prepare_rejected").or_default() += prepare_rejected;
                }
            }
            // Already committed: carry on from what the store holds.
            Ok(BatchCommitted::Replayed { .. }) => {
                let j = load(store, job.id)?;
                (seq, position) = (j.seq, j.position);
            }
            Err(Error::ImportNotRunning) => {
                let j = load(store, job.id)?;
                report.ended = Ended::NotRunning(j.control);
                report.counters = j.counters;
                return Ok(report);
            }
            Err(e) => {
                // Best effort: a store that can't commit may not take the
                // block either; the restart's interrupt covers that.
                if let Ok(j) = load(store, job.id) {
                    let _ = store.block_import_job(j.id, j.revision, Reason::StoreError, now);
                }
                return Err(e.into());
            }
        }
        job = load(store, job.id)?;
    }
    report.counters = job.counters;
    Ok(report)
}

fn load(store: &Store, id: i64) -> Result<ImportJob, DriverError> {
    store
        .import_job(id)?
        .ok_or(DriverError::Job("no such import job"))
}

fn pause(
    store: &mut Store,
    id: i64,
    mut report: Report,
    opts: &Options<'_>,
) -> Result<Report, DriverError> {
    let now = (opts.now)();
    match change(store, id, |s, j| s.pause_import_job(j.id, j.revision, now))? {
        Ok(j) => {
            report.ended = Ended::Paused;
            report.counters = j.counters;
            Ok(report)
        }
        Err(c) => not_running(store, id, report, c),
    }
}

/// One job change the driver makes. When the store refuses it because the
/// job was paused, cancelled or finished elsewhere meanwhile, that's
/// `Ok(Err(control))`: a truthful [`Ended::NotRunning`], not a fault.
fn change(
    store: &mut Store,
    id: i64,
    f: impl FnOnce(&mut Store, &ImportJob) -> lodestar_store::Result<ImportJob>,
) -> Result<Result<ImportJob, Control>, DriverError> {
    let j = load(store, id)?;
    match f(store, &j) {
        Ok(j) => Ok(Ok(j)),
        Err(e @ Error::Invalid(_)) => {
            let now = load(store, id)?;
            if now.control != Control::Running || now.is_finished() {
                Ok(Err(now.control))
            } else {
                Err(e.into())
            }
        }
        Err(e) => Err(e.into()),
    }
}

fn not_running(
    store: &Store,
    id: i64,
    mut report: Report,
    control: Control,
) -> Result<Report, DriverError> {
    report.ended = Ended::NotRunning(control);
    report.counters = load(store, id)?.counters;
    Ok(report)
}

/// A torrent record through enrichment and the filter, as ingest judges a
/// fetched one (`pipeline::worker::prepare_one`): the hash lists first,
/// then the name and files.
fn judge_torrent(
    filter: &Filter<'_>,
    p: &mut Prepared,
    info: &lodestar_metainfo::Info,
    rec: &Rec,
    now: i64,
) -> Judged {
    let hashes: Vec<[u8; 20]> = p.swarm_hashes().map(|h| h.0).collect();
    let files = file_infos(&info.files, info.path_encoding);
    let (enriched, class) = enrich_corrected(&p.name, &files, Correction::default());
    p.enrichment = Some(enriched);
    let source_at = rec
        .torrent(
            p,
            Decision {
                hits: &[],
                marks: 0,
            },
        )
        .and_then(|t| t.source_at);
    let facts = Facts {
        class: &class,
        size: p.size,
        piece_len: p.piece_len,
        flags: p.flags,
        discovered_at: source_at.filter(|at| (1..=now).contains(at)).unwrap_or(now),
        now,
    };
    let (verdict, fix) = verdict(filter, &hashes, &p.name, &files, &class, &facts, now);
    // Your classification rule changes what's stored (§20.9).
    if fix.is_some() {
        let c = Correction {
            adult: fix,
            not_junk: false,
        };
        p.enrichment = Some(enrich_corrected(&p.name, &files, c).0);
    }
    judged(filter, hashes, &p.name, verdict, fix)
}

/// A candidate through the filter: the hash lists, and its name when it
/// has one. It has no files.
fn judge_candidate(filter: &Filter<'_>, rec: &Rec, now: i64) -> Judged {
    let Some(c) = rec.candidate(Decision {
        hits: &[],
        marks: 0,
    }) else {
        return Judged::default();
    };
    let hashes = vec![c.hash.0];
    let name = c.name.map(String::from_utf8_lossy).unwrap_or_default();
    let (_, class) = enrich_corrected(&name, &[], Correction::default());
    let facts = Facts {
        class: &class,
        size: c.size.unwrap_or(0),
        piece_len: 0,
        flags: 0,
        discovered_at: c
            .source_at
            .filter(|at| (1..=now).contains(at))
            .unwrap_or(now),
        now,
    };
    let (verdict, fix) = verdict(filter, &hashes, &name, &[], &class, &facts, now);
    judged(filter, hashes, &name, verdict, fix)
}

/// The engine's verdict: a hash on the floor's or a dropping list first,
/// then the full judgement with your classification rules.
fn verdict(
    filter: &Filter<'_>,
    hashes: &[[u8; 20]],
    name: &str,
    files: &[FileInfo],
    class: &Classification,
    facts: &Facts<'_>,
    now: i64,
) -> (Verdict, Option<bool>) {
    if let Some(d) = filter.engine.blocked_hash(hashes) {
        let v = Verdict {
            decision: Some(d),
            shadow: Vec::new(),
        };
        return (v, None);
    }
    let item = Item {
        hashes,
        name,
        files,
        content_type: class.content_type,
    };
    let (v, says) = filter.engine.judge_classified(&item, Some(facts), now);
    let fix = says
        .filter(|adult| *adult != (class.content_type == lodestar_classify::ContentType::Adult));
    (v, fix)
}

/// The hits to record, as the writer records ingest's (one per decision,
/// held, marked or shadow), and the marks of a record that's kept.
fn judged(
    filter: &Filter<'_>,
    hashes: Vec<[u8; 20]>,
    name: &str,
    verdict: Verdict,
    fix: Option<bool>,
) -> Judged {
    let held = match verdict.decision.as_ref().map(|d| d.action) {
        Some(Action::Drop) => Some(Held::Tombstone),
        Some(Action::Quarantine) => Some(Held::Quarantine),
        _ => None,
    };
    let marks = match verdict.decision.as_ref().map(|d| d.action) {
        Some(Action::Hide) => flags::HIDDEN,
        Some(Action::Demote) => flags::DEMOTED,
        Some(Action::Flag) => flags::FLAGGED,
        _ => 0,
    };
    let sealed = (held == Some(Held::Quarantine))
        .then(|| filter.sealer.and_then(|s| s.seal(name).ok()))
        .flatten();
    let mut hits = Vec::new();
    if let Some(d) = &verdict.decision {
        hits.push(Hit {
            rule: d.rule.id(),
            action: d.action.code(),
            held,
            sealed: sealed.clone(),
            evidence: d.evidence_codes(),
            // Held by the Adult-only core only because your rule made it
            // adult: a tombstone when the quarantine ends (§20.9).
            adult_hold: fix == Some(true) && adult_hold(d),
        });
    }
    for d in &verdict.shadow {
        hits.push(Hit {
            rule: d.rule.id(),
            action: d.action.code() | SHADOW,
            held: None,
            sealed: None,
            evidence: d.evidence_codes(),
            adult_hold: false,
        });
    }
    Judged {
        hashes,
        hits,
        marks,
    }
}

#[cfg(test)]
mod tests;
