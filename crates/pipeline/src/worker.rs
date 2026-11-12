//! The worker side of the boundary (architecture §19.4): what a worker
//! does between finding a key and handing a finished torrent to the
//! coordinator. M3 runs one built-in worker ([`WorkerId::LOCAL`]) in this
//! process; remote workers run the same stages behind the network, built
//! without a store by [`crate::stages::WorkerStages`].
//!
//! - Intake claims each new key from the coordinator (see `intake`) and
//!   keeps the lease's token here.
//! - The fetch pool reports its losses through [`WorkerFunnel`], which
//!   hands them to the coordinator under that token ([`Reports`]).
//! - The prepare stage parses each fetched info dict, builds the row and
//!   blob, enriches it (release parse, classification) and runs the
//!   content filter (§20.4) into [`Results`], so the writer only commits.

use std::borrow::Borrow;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_classify::{Classification, FileInfo};

use crate::source_adult::AdultSources;
use lodestar_cluster::{Token, WorkerId};
use lodestar_filter::{Engine, Facts, Handle, Item, Trail, Verdict};
use lodestar_metainfo::{Info, InfoHash};
use lodestar_peer_wire::FailureKind;
use lodestar_sources::{FetchedKey, Funnel, FunnelCounts, Loss, Stage};
use lodestar_store::{Prepared, Preparer, SourceId};
use tokio::time::{Instant, timeout_at};

use crate::gate::{Excluded, GateStats};
use crate::stages::{Inbox, Outbox, Reports};
use crate::unix_now;
use crate::writer::WriterStats;

/// The tokens of the leases this worker holds, and, when it tracks them,
/// of its results out under them (X-wk-own-lease-lost).
///
/// Once a key is fetched and prepared, [`PrepareStage`] moves its token
/// out of *held* ([`send_out`](Self::send_out)): the token now rides in the
/// `Results` record (and the spool) until the coordinator commits it. A
/// remote worker's leases track it there ([`tracking_out`]), under the same
/// lock: a claim the worker makes for the key meanwhile is answered with
/// that same lease, and [`insert`](Self::insert) says so
/// ([`Insert::Out`]), so it's never taken for a new lease.
///
/// An out entry is marked finished once its result is committed, released
/// or lost, and kept: tokens are never reissued, so a finished entry still
/// answers `Out`. How long it's kept depends on how it finished:
/// - **committed** ([`committed`](Self::committed)): for
///   [`FINISHED_GRACE`] only. `Accepted` means the batch is in the
///   coordinator's intake queue, not that it's written: the coordinator
///   keeps the lease (and answers the holder's claims with it) until its
///   writer commits, so the grace covers the writer's lag.
/// - **released or lost** ([`done`](Self::done)): until the horizon, as
///   while out. The coordinator keeps the lease until the loss report
///   lands, which can be minutes away (a session down, then back), and
///   answers the holder's claims with it meanwhile.
///
/// An entry still out goes at the horizon. At most [`MAX_OUT`] entries are
/// kept, finished ones going first. Either way a forgotten lease coming
/// back counts as new again, as it did before this was tracked, and a
/// worker restart forgets them all. The coordinator's built-in worker
/// doesn't track out ([`Default`]): its intake never asks.
///
/// [`tracking_out`]: Self::tracking_out
#[derive(Debug, Default)]
pub struct WorkerLeases {
    inner: Mutex<Leases>,
    /// How long an out entry is kept while its result is out; `None`: out
    /// isn't tracked.
    horizon: Option<Duration>,
    #[cfg(test)]
    gap: GapHook,
}

/// How long a committed out entry is kept: the coordinator's writer lag
/// it covers. The coordinator ends the lease only when its writer commits
/// the batch, behind up to 16 batches in intake and 64 in the writer's
/// queue; 180 s holds while the writer clears a batch every 2.25 s or
/// faster. A writer lagging longer still lets the holder's own lease come
/// back as new, and end as lost (X-wk-out-set-polish's decisions; the
/// follow-up X-wk-fetched-lease closes that at the coordinator).
pub(crate) const FINISHED_GRACE: Duration = Duration::from_secs(180);

/// Out entries kept at most: the worker pool's key cap (plan Q5), about
/// 10 MB at about 190 B an entry.
pub(crate) const MAX_OUT: usize = 50_000;

#[derive(Debug, Default)]
struct Leases {
    held: HashMap<InfoHash, Token>,
    out: HashMap<InfoHash, Out>,
    /// `out`'s entries by when they expire, for the prune.
    expiry: std::collections::BTreeMap<(std::time::Instant, u64), InfoHash>,
    /// `out`'s finished entries by when they expire, for the cap: a
    /// committed entry (a short grace) goes before a lost one.
    finished: std::collections::BTreeMap<(std::time::Instant, u64), InfoHash>,
    /// Out entries kept and not finished yet.
    pending: usize,
    next_seq: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Out {
    token: Token,
    expires: std::time::Instant,
    /// Tells two entries expiring at the same instant apart.
    seq: u64,
    finished: bool,
}

impl Leases {
    fn remove_out(&mut self, key: &InfoHash) -> Option<Out> {
        let o = self.out.remove(key)?;
        self.unindex(&o);
        Some(o)
    }

    fn unindex(&mut self, o: &Out) {
        self.expiry.remove(&(o.expires, o.seq));
        if o.finished {
            self.finished.remove(&(o.expires, o.seq));
        } else {
            self.pending -= 1;
        }
    }

    /// Drops what has expired, then, past the cap, finished entries due to
    /// expire first, then the entry due to expire first.
    fn prune(&mut self, now: std::time::Instant) {
        while let Some((&(at, _), _)) = self.expiry.first_key_value() {
            if at > now {
                break;
            }
            let ((_, seq), key) = self.expiry.pop_first().expect("a first entry");
            self.drop_indexed(key, seq);
        }
        while self.out.len() > MAX_OUT {
            let key = match self.finished.first_key_value() {
                Some((_, &key)) => key,
                None => *self.expiry.first_key_value().expect("indexed").1,
            };
            self.remove_out(&key);
        }
        debug_assert_eq!(self.out.len(), self.expiry.len());
        // A peak's capacity isn't kept for ever.
        if self.out.capacity() > 1024 && self.out.capacity() > 4 * self.out.len() {
            self.out.shrink_to(2 * self.out.len());
        }
    }

    /// The index entry `seq` for `key` is gone: drops `key`'s entry if
    /// it's that one (it always is, but progress mustn't rest on that).
    fn drop_indexed(&mut self, key: InfoHash, seq: u64) {
        if self.out.get(&key).is_some_and(|o| o.seq == seq) {
            let o = self.out.remove(&key).expect("present");
            if o.finished {
                self.finished.remove(&(o.expires, o.seq));
            } else {
                self.pending -= 1;
            }
        }
    }

    fn track(&mut self, key: InfoHash, token: Token, expires: std::time::Instant, finished: bool) {
        let seq = self.next_seq;
        self.next_seq += 1;
        let o = Out {
            token,
            expires,
            seq,
            finished,
        };
        if let Some(old) = self.out.insert(key, o) {
            self.unindex(&old);
        }
        if finished {
            self.finished.insert((expires, seq), key);
        } else {
            self.pending += 1;
        }
        self.expiry.insert((expires, seq), key);
    }
}

/// What [`WorkerLeases::insert`] made of a grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Insert {
    /// A lease the worker didn't hold under this token: held now.
    New,
    /// Held already under this token: nothing changed.
    Held,
    /// Its result went out under this token: nothing changed, and the key
    /// isn't held again.
    Out,
}

/// A test's probe, run at the move out with the lock held.
#[cfg(test)]
type Probe = Box<dyn Fn(&Mutex<Leases>) + Send>;

/// Runs a test's probe at the move out, while the lock is held (tests).
#[cfg(test)]
#[derive(Default)]
struct GapHook(Mutex<Option<Probe>>);

#[cfg(test)]
impl std::fmt::Debug for GapHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GapHook")
    }
}

impl WorkerLeases {
    /// A remote worker's leases: a result out is tracked for `horizon`
    /// after it went out.
    pub fn tracking_out(horizon: Duration) -> WorkerLeases {
        WorkerLeases {
            horizon: Some(horizon),
            ..WorkerLeases::default()
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Leases> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Records a grant of `key` under `token`, unless it's the lease the
    /// worker holds or the one a result of its went out under.
    pub fn insert(&self, key: InfoHash, token: Token) -> Insert {
        self.insert_at(key, token, std::time::Instant::now())
    }

    fn insert_at(&self, key: InfoHash, token: Token, now: std::time::Instant) -> Insert {
        let mut l = self.lock();
        if self.horizon.is_some() {
            l.prune(now);
            if l.out.get(&key).is_some_and(|o| o.token == token) {
                return Insert::Out;
            }
        }
        match l.held.insert(key, token) {
            Some(t) if t == token => Insert::Held,
            _ => Insert::New,
        }
    }

    /// The key's token, removed: the lease ends with this report.
    pub fn take(&self, key: &InfoHash) -> Option<Token> {
        self.lock().held.remove(key)
    }

    /// The key's token, removed from held: its result carries the lease
    /// from here. Tracked as out, under the same lock, when out is tracked.
    pub fn send_out(&self, key: &InfoHash) -> Option<Token> {
        self.send_out_at(key, std::time::Instant::now())
    }

    fn send_out_at(&self, key: &InfoHash, now: std::time::Instant) -> Option<Token> {
        let mut l = self.lock();
        let token = l.held.remove(key)?;
        #[cfg(test)]
        if let Some(probe) = &*self.gap.0.lock().unwrap() {
            probe(&self.inner);
        }
        if let Some(horizon) = self.horizon {
            l.track(*key, token, later(now, horizon), false);
            l.prune(now);
        }
        Some(token)
    }

    /// The result under `token` was released or lost: its entry stays,
    /// finished, until the horizon (the coordinator keeps the lease until
    /// the loss report lands).
    pub fn done(&self, key: &InfoHash, token: Token) {
        self.finish(key, token, None);
    }

    /// The result under `token` was committed: its entry stays, finished,
    /// for [`FINISHED_GRACE`].
    pub fn committed(&self, key: &InfoHash, token: Token) {
        self.committed_at(key, token, std::time::Instant::now());
    }

    fn committed_at(&self, key: &InfoHash, token: Token, now: std::time::Instant) {
        self.finish(key, token, Some(later(now, FINISHED_GRACE)));
    }

    fn finish(&self, key: &InfoHash, token: Token, until: Option<std::time::Instant>) {
        let mut l = self.lock();
        let Some(o) = l.out.get(key).copied() else {
            return;
        };
        if o.token == token && !o.finished {
            let expires = until.map_or(o.expires, |u| o.expires.min(u));
            l.track(*key, token, expires, true);
        }
    }

    /// Leases held.
    pub fn len(&self) -> usize {
        self.lock().held.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Out entries kept whose result's commit, release or loss hasn't been
    /// seen (one past the horizon or the cap is no longer kept, so no
    /// longer counted).
    pub fn pending_len(&self) -> usize {
        self.lock().pending
    }

    /// Out entries kept, finished or not.
    #[cfg(test)]
    pub(crate) fn out_len(&self) -> usize {
        self.lock().out.len()
    }
}

/// `now + by`, or far in the future rather than a panic under the lock.
fn later(now: std::time::Instant, by: Duration) -> std::time::Instant {
    // Past what an `Instant` can hold: a century, or less where the
    // platform's clock can't hold that, and never a panic.
    now.checked_add(by)
        .or_else(|| now.checked_add(Duration::from_secs(100 * 365 * 24 * 3600)))
        .unwrap_or(now)
}

/// The pool's funnel on a worker: progress stages count directly, the
/// harvest was counted at grant, and losses go to the coordinator under
/// the lease's token.
pub(crate) struct WorkerFunnel {
    pub counts: Arc<FunnelCounts>,
    pub reports: Arc<dyn Reports>,
    pub leases: Arc<WorkerLeases>,
    pub gate: Arc<GateStats>,
}

impl Funnel for WorkerFunnel {
    fn record(&self, cohort: i64, source: SourceId, stage: Stage, loss: Option<Loss>) {
        if crate::in_funnel(cohort) {
            self.counts.record(cohort, source, stage, loss);
        }
    }

    fn admitted(&self, _key: &InfoHash, _cohort: i64, _source: SourceId) {}

    fn lost(
        &self,
        key: &InfoHash,
        _cohort: i64,
        _source: SourceId,
        loss: Loss,
        cause: Option<FailureKind>,
    ) {
        match self.leases.take(key) {
            Some(token) => {
                // Counted where the lease ends, once per key: the pool
                // gave it up on data that didn't hash to it. A protocol
                // violation is an `Invalid` loss too, but not this.
                if self.reports.lost(key, token, loss)
                    && loss == Loss::Invalid
                    && cause == Some(FailureKind::HashMismatch)
                {
                    self.gate.exclude(Excluded::FailedVerification, 1);
                }
            }
            // Every key enters the pool under a lease; one without is a
            // bug, and counting it would unbalance the funnel.
            None => lodestar_log::log_event!(
                crate::log_events::LEASE_MISSING,
                key = lodestar_log::Torrent(&key.0),
                stage = "lost"
            ),
        }
    }

    /// Intake granted the sighting a new lease (a reply would have been
    /// held), and the pool fetched the key lately but didn't store it.
    /// Without this nothing ends the lease, and the lease tables grow by
    /// one entry per such sighting (R-mem-triage).
    ///
    /// The lease ends as `Invalid` whatever the first fetch's outcome was.
    /// Prepare rejecting the dictionary is the common case, but a key also
    /// gets here after the writer refused it for a stopped source
    /// (`Evicted`), after a Drop rule at commit (`Blocked`, which puts it
    /// in neither the held set nor the blocklist), or through the writer's
    /// retry channel, which skips the have-filter for a key committed in
    /// the meantime. Insights' loss-reason share therefore overcounts
    /// `invalid` slightly; that's accepted, since the lease has to end and
    /// tracking the first loss isn't worth it. Each such sighting within
    /// the pool's recent window is its own harvest and loss, possibly more
    /// than once per key.
    fn duplicate(&self, key: &InfoHash, _cohort: i64, _source: SourceId) {
        if let Some(token) = self.leases.take(key) {
            self.reports.lost(key, token, Loss::Invalid);
        }
    }
}

/// A prepared torrent on its way to the coordinator (the `Results`
/// record, §19.4). Provenance beyond what the worker saw comes from the
/// coordinator's lease.
#[derive(Debug)]
pub struct Results {
    pub key: InfoHash,
    pub token: Token,
    pub worker: WorkerId,
    pub torrent: Prepared,
    /// Sources the worker saw for the key (peer finders included).
    pub sources: u64,
    pub metadata_at: i64,
    /// What the filter needs to judge again at commit.
    pub files: Vec<FileInfo>,
    pub class: Classification,
    /// The worker's filter verdict, and the rule-set version it used.
    pub verdict: Verdict,
    pub version: u64,
    /// What the verdict's judgment found above the pack layer, for the
    /// writer's second judgment with SP-3 or SP-4 matching
    /// ([`lodestar_filter::Engine::judge_at_commit_reusing`]).
    pub trail: Trail,
    /// The classification rule applied to the stored enrichment (§20.9):
    /// Is adult or Not adult, where it differs from detection.
    pub fix: Option<bool>,
    /// Only private sources reported the key.
    pub private_source: bool,
    /// An always-adult source found it (X-source-adult): stored adult and
    /// judged as adult too, whatever the classification rules say.
    pub source_adult: bool,
}

pub(crate) struct PrepareStage {
    /// Who the results come from.
    pub worker: WorkerId,
    pub preparer: Preparer,
    pub filter: Arc<Handle>,
    pub reports: Arc<dyn Reports>,
    pub leases: Arc<WorkerLeases>,
    pub stats: Arc<WriterStats>,
    pub gate: Arc<GateStats>,
    /// The sources whose every find is adult.
    pub adult: AdultSources,
    pub batch_max: usize,
    pub batch_wait: Duration,
}

impl PrepareStage {
    /// Runs until the pool's output closes. Waiting on `out` passes the
    /// writer's (or the link's) backpressure on to the pool. An item from
    /// a byte-capped queue keeps its bytes until its result is sent.
    pub async fn run<I, O>(mut self, mut fetched: I, out: O)
    where
        I: Inbox,
        I::Item: Borrow<FetchedKey>,
        O: Outbox<Results>,
    {
        while let Some(first) = fetched.recv().await {
            let batch = collect(first, &mut fetched, self.batch_max, self.batch_wait).await;
            let preparer = self.preparer;
            let engine = self.filter.current();
            let adult = self.adult;
            let (preparer, done) = tokio::task::spawn_blocking(move || {
                let mut preparer = preparer;
                let done: Vec<_> = batch
                    .into_iter()
                    .map(|item| {
                        let fk: &FetchedKey = item.borrow();
                        let source_adult = adult.any(fk.sources);
                        let r = prepare_one(&mut preparer, &engine, fk, source_adult);
                        (item, r)
                    })
                    .collect();
                (preparer, done)
            })
            .await
            .expect("prepare task");
            self.preparer = preparer;
            for (item, result) in done {
                let fk: &FetchedKey = item.borrow();
                // A result carries its lease out (in one step, so the
                // worker's own claim meanwhile is known for what it is);
                // a loss ends it here.
                let token = if result.is_ok() {
                    self.leases.send_out(&fk.key)
                } else {
                    self.leases.take(&fk.key)
                };
                let Some(token) = token else {
                    lodestar_log::log_event!(
                        crate::log_events::LEASE_MISSING,
                        key = lodestar_log::Torrent(&fk.key.0),
                        stage = "fetched"
                    );
                    continue;
                };
                match result {
                    Ok(done) => {
                        let r = Results {
                            key: fk.key,
                            token,
                            worker: self.worker,
                            torrent: done.torrent,
                            sources: fk.sources,
                            metadata_at: unix_now(),
                            files: done.files,
                            class: done.class,
                            verdict: done.verdict,
                            version: done.version,
                            trail: done.trail,
                            fix: done.fix,
                            private_source: fk.private_source,
                            source_adult: done.source_adult,
                        };
                        let (key, token) = (r.key, r.token);
                        let sent = out.send(r).await;
                        drop(item);
                        if sent.is_err() {
                            // Never sent: nothing will finish it.
                            self.leases.done(&key, token);
                            return;
                        }
                    }
                    Err(loss) => {
                        let (counter, excluded) = match loss {
                            Loss::Blocked => (&self.stats.blocked, Excluded::Filtered),
                            _ => (&self.stats.invalid, Excluded::InvalidMetainfo),
                        };
                        counter.fetch_add(1, Ordering::Relaxed);
                        if self.reports.lost(&fk.key, token, loss) {
                            self.gate.exclude(excluded, 1);
                        }
                    }
                }
            }
        }
    }
}

/// A prepared torrent with what the filter made of it.
struct Done {
    torrent: Prepared,
    files: Vec<FileInfo>,
    class: Classification,
    verdict: Verdict,
    version: u64,
    trail: Trail,
    fix: Option<bool>,
    source_adult: bool,
}

/// Parses, prepares, enriches and judges one fetched info dict. A key in
/// the floor's hash list is refused before parsing; every other verdict
/// goes to the writer, which records it and re-checks it (§19.4).
/// `source_adult`: an always-adult source found it, so it's stored adult
/// and judged as adult too ([`crate::source_adult`]).
fn prepare_one(
    preparer: &mut Preparer,
    engine: &Engine,
    fk: &FetchedKey,
    source_adult: bool,
) -> Result<Done, Loss> {
    if engine.blocked_hash(&[fk.key.0]).is_some() {
        return Err(Loss::Blocked);
    }
    // Hashes to the key but isn't usable metainfo: invalid.
    let info = Info::parse(&fk.fetched.info).map_err(|_| Loss::Invalid)?;
    let mut prepared = preparer
        .prepare(&info, Some(&fk.fetched.info))
        .map_err(|e| {
            lodestar_log::log_event!(
                crate::log_events::PREPARE_FAILED,
                key = lodestar_log::Torrent(&fk.key.0),
                error = lodestar_log::Error::new(&e)
            );
            Loss::Invalid
        })?;
    // Enrichment runs here, on the worker side (§19.4).
    let files = crate::enrich::file_infos(&info.files, info.path_encoding);
    let (enriched, class) = crate::enrich::enrich_infos(&prepared.name, &files);
    prepared.enrichment = Some(enriched);
    let hashes: Vec<[u8; 20]> = prepared.swarm_hashes().map(|h| h.0).collect();
    let now = unix_now();
    let facts = Facts {
        class: &class,
        size: prepared.size,
        piece_len: prepared.piece_len,
        flags: prepared.flags,
        discovered_at: now,
        now,
    };
    let item = Item {
        hashes: &hashes,
        name: &prepared.name,
        files: &files,
        content_type: class.content_type,
    };
    // Your classification rules change what's stored; an Is adult one
    // also makes the torrent meet the Adult-only core (§20.9). So does an
    // always-adult source, which no Not adult rule undoes.
    let mut trail = Trail::default();
    let (verdict, fix) =
        crate::source_adult::judge_kept(engine, &item, &facts, source_adult, now, Some(&mut trail));
    if fix.is_some() {
        prepared.enrichment = Some(crate::enrich::enrich_with_rule(&prepared.name, &files, fix));
    }
    Ok(Done {
        torrent: prepared,
        files,
        class,
        verdict,
        version: engine.version,
        trail,
        fix,
        source_adult,
    })
}

/// Gathers more items for the same batch, briefly: at most `max`, for at
/// most `wait` after the first. Not cancel-safe: dropped while it waits,
/// the items gathered so far are lost.
pub(crate) async fn collect<I: Inbox>(
    first: I::Item,
    rx: &mut I,
    max: usize,
    wait: Duration,
) -> Vec<I::Item> {
    let mut batch = vec![first];
    let deadline = Instant::now() + wait;
    while batch.len() < max {
        match rx.try_recv() {
            Ok(x) => batch.push(x),
            Err(true) => break,
            Err(false) => match timeout_at(deadline, rx.recv()).await {
                Ok(Some(x)) => batch.push(x),
                Ok(None) | Err(_) => break,
            },
        }
    }
    batch
}

#[cfg(test)]
mod tests {
    use super::*;

    const K: InfoHash = InfoHash([1; 20]);
    const H: Duration = Duration::from_secs(1200);

    fn tracking() -> WorkerLeases {
        WorkerLeases::tracking_out(H)
    }

    #[test]
    fn a_grant_is_new_held_again_or_out() {
        let l = tracking();
        assert_eq!(l.insert(K, Token(7)), Insert::New);
        assert_eq!(l.insert(K, Token(7)), Insert::Held);
        assert_eq!(l.send_out(&K), Some(Token(7)));
        // Its result is out: the same lease coming back isn't held again.
        assert_eq!(l.insert(K, Token(7)), Insert::Out);
        assert_eq!((l.len(), l.pending_len()), (0, 1));
        // A new lease is held as before; the old result stays tracked.
        assert_eq!(l.insert(K, Token(8)), Insert::New);
        assert_eq!((l.len(), l.pending_len()), (1, 1));
        assert_eq!(l.take(&K), Some(Token(8)));
    }

    /// The coordinator's built-in worker: nothing is tracked out, and the
    /// answers are as before this fix.
    #[test]
    fn untracked_leases_behave_as_before() {
        let l = WorkerLeases::default();
        assert_eq!(l.insert(K, Token(7)), Insert::New);
        assert_eq!(l.send_out(&K), Some(Token(7)));
        assert_eq!(l.pending_len(), 0);
        assert_eq!(l.insert(K, Token(7)), Insert::New);
    }

    /// Committed, released or lost: finished, but still known, so the
    /// same lease coming back after its commit is still not new.
    #[test]
    fn done_finishes_only_its_own_result_and_keeps_it_known() {
        let l = tracking();
        l.insert(K, Token(7));
        l.send_out(&K);
        l.done(&K, Token(6));
        assert_eq!(l.pending_len(), 1, "another token's result");
        l.done(&K, Token(7));
        l.done(&K, Token(7));
        assert_eq!(l.pending_len(), 0, "finished once");
        assert_eq!(l.insert(K, Token(7)), Insert::Out, "after the commit");
        assert_eq!(l.len(), 0);
        // A loss before the result goes out leaves nothing out.
        assert_eq!(l.insert(K, Token(9)), Insert::New);
        assert_eq!(l.take(&K), Some(Token(9)));
        assert_eq!(l.send_out(&K), None);
        assert_eq!((l.len(), l.pending_len()), (0, 0));
    }

    #[test]
    fn out_entries_older_than_the_horizon_are_pruned() {
        let l = tracking();
        let t0 = std::time::Instant::now();
        l.insert_at(K, Token(7), t0);
        l.send_out_at(&K, t0);
        let just_before = t0 + H - Duration::from_millis(1);
        assert_eq!(l.insert_at(K, Token(7), just_before), Insert::Out);
        // Past the horizon, still out: forgotten, so its own lease coming
        // back counts as new, as before this was tracked.
        assert_eq!(
            l.insert_at(K, Token(7), t0 + H + Duration::from_millis(1)),
            Insert::New
        );
        assert_eq!((l.out_len(), l.pending_len()), (0, 0));
    }

    /// Committed, an entry is kept for the grace only, from its commit.
    #[test]
    fn committed_entries_go_after_the_grace() {
        let l = tracking();
        let t0 = std::time::Instant::now();
        l.insert_at(K, Token(7), t0);
        l.send_out_at(&K, t0);
        let t1 = t0 + Duration::from_secs(5);
        l.committed_at(&K, Token(7), t1);
        let k2 = InfoHash([2; 20]);
        let just_before = t1 + FINISHED_GRACE - Duration::from_millis(1);
        assert_eq!(l.insert_at(k2, Token(1), just_before), Insert::New);
        assert_eq!(l.insert_at(K, Token(7), just_before), Insert::Out);
        // Expired at its expiry instant.
        assert_eq!(l.insert_at(K, Token(7), t1 + FINISHED_GRACE), Insert::New);
        assert_eq!(l.out_len(), 0);
        // An entry committed and sent out again keeps its new expiry.
        let l = tracking();
        l.insert_at(K, Token(7), t0);
        l.send_out_at(&K, t0);
        l.committed_at(&K, Token(7), t0);
        l.insert_at(K, Token(8), t0);
        l.send_out_at(&K, t0);
        assert_eq!((l.out_len(), l.pending_len()), (1, 1));
        let past_grace = t0 + FINISHED_GRACE + Duration::from_secs(1);
        assert_eq!(l.insert_at(K, Token(8), past_grace), Insert::Out);
    }

    /// Released or lost, an entry is kept until the horizon: the
    /// coordinator holds the lease until the loss report lands, and may
    /// answer the holder's claims with it meanwhile.
    #[test]
    fn a_lost_entry_is_kept_until_the_horizon() {
        let l = tracking();
        let t0 = std::time::Instant::now();
        l.insert_at(K, Token(7), t0);
        l.send_out_at(&K, t0);
        l.done(&K, Token(7));
        assert_eq!(l.pending_len(), 0);
        let well_past_grace = t0 + FINISHED_GRACE + Duration::from_secs(1);
        assert_eq!(l.insert_at(K, Token(7), well_past_grace), Insert::Out);
        assert_eq!(l.insert_at(K, Token(7), t0 + H), Insert::New);
    }

    /// A writer lagging less than the grace (the coordinator still holds
    /// the lease, and answers the holder's claim with it) never makes that
    /// lease look new, so nothing ends it; past the grace it does, the
    /// window this grace leaves (on the injected clock).
    #[test]
    fn a_writer_lagging_less_than_the_grace_ends_no_lease() {
        let l = tracking();
        let t0 = std::time::Instant::now();
        assert_eq!(l.insert_at(K, Token(7), t0), Insert::New);
        l.send_out_at(&K, t0);
        // `Accepted`: in the coordinator's intake, not yet written.
        l.committed_at(&K, Token(7), t0);
        for lag in [1, 60, 120, 179] {
            let at = t0 + Duration::from_secs(lag);
            assert_eq!(l.insert_at(K, Token(7), at), Insert::Out, "{lag} s");
        }
        assert_eq!(l.len(), 0, "never held again, so never fed to the pool");
        assert_eq!(l.take(&K), None, "and nothing for a loss to end");
        assert_eq!(l.insert_at(K, Token(7), t0 + FINISHED_GRACE), Insert::New);
    }

    /// Past the cap, finished entries go first, then the entry due to
    /// expire first; and the map gives back a peak's capacity.
    #[test]
    fn out_entries_are_capped_and_the_map_shrinks() {
        let l = tracking();
        let t0 = std::time::Instant::now();
        let key = |n: usize| {
            let mut k = [0u8; 20];
            k[..8].copy_from_slice(&(n as u64).to_le_bytes());
            InfoHash(k)
        };
        let send = |n: usize| {
            l.insert_at(key(n), Token(n as u64), t0);
            l.send_out_at(&key(n), t0 + Duration::from_millis(n as u64));
        };
        for n in 0..MAX_OUT {
            send(n);
        }
        // Key 10 finished (lost): it goes before key 0, which is still out.
        l.done(&key(10), Token(10));
        send(MAX_OUT);
        assert_eq!(l.out_len(), MAX_OUT, "a hard cap");
        assert_eq!(l.insert_at(key(10), Token(10), t0), Insert::New);
        assert_eq!(l.insert_at(key(0), Token(0), t0), Insert::Out);
        // Then the earliest to expire.
        send(MAX_OUT + 1);
        assert_eq!(l.insert_at(key(0), Token(0), t0), Insert::New);
        assert_eq!(l.out_len(), MAX_OUT);
        // Among finished entries, the one due to expire first: key 30,
        // committed (a short grace), goes before key 20, lost earlier but
        // kept to the horizon.
        l.done(&key(20), Token(20));
        l.committed_at(&key(30), Token(30), t0);
        send(MAX_OUT + 2);
        assert_eq!(l.insert_at(key(30), Token(30), t0), Insert::New);
        assert_eq!(l.insert_at(key(20), Token(20), t0), Insert::Out);
        assert_eq!(l.out_len(), MAX_OUT);
        // Most committed: past the grace, the map gives back its capacity
        // but keeps room for what's left.
        for n in 1..=MAX_OUT + 1 {
            if n % 50 != 0 {
                l.committed_at(&key(n), Token(n as u64), t0);
            }
        }
        l.insert_at(InfoHash([9; 20]), Token(1), t0 + FINISHED_GRACE);
        let (len, cap) = {
            let g = l.lock();
            (g.out.len(), g.out.capacity())
        };
        assert!(len > 900 && len < 1_100, "{len} left");
        assert!(cap <= 4 * len, "capacity kept: {cap} for {len}");
    }

    /// The horizon's TTL (the runtime reads the protocol's default) is the
    /// one the coordinator's lease authority enforces.
    #[test]
    fn the_protocols_lease_ttl_is_the_one_enforced() {
        assert_eq!(
            lodestar_cluster::ServerConfig::default().lease_ttl_secs,
            crate::LEASE_TTL_SECS
        );
    }

    /// At the instant held no longer has the token, the lock is still held,
    /// so no `insert` can run between the two halves of the move out. A
    /// change that lets the lock go at that point fails this.
    #[test]
    fn the_move_out_happens_under_one_lock() {
        let l = tracking();
        l.insert(K, Token(7));
        let seen = Arc::new(Mutex::new(None));
        let probe_seen = seen.clone();
        *l.gap.0.lock().unwrap() = Some(Box::new(move |inner: &Mutex<Leases>| {
            let locked = std::thread::scope(|sc| {
                sc.spawn(|| matches!(inner.try_lock(), Err(std::sync::TryLockError::WouldBlock)))
                    .join()
                    .unwrap()
            });
            *probe_seen.lock().unwrap() = Some(locked);
        }));
        assert_eq!(l.send_out(&K), Some(Token(7)));
        assert_eq!(
            *seen.lock().unwrap(),
            Some(true),
            "the lock was let go mid-move (None: the probe never ran)"
        );
        assert_eq!(l.insert(K, Token(7)), Insert::Out);
    }

    /// The prepare stage moving a key out (then, in half the rounds, its
    /// commit) racing the same lease coming back on another thread. Both
    /// start from one spinning flag, and the side given a head start
    /// alternates, so both orders come up: the grant lands first (`Held`)
    /// or after the move (`Out`). Either way the key is never held again
    /// once its result went out. Rounds go on until both orders were seen
    /// (at least 2,000), the head start doubling while one is missing, up
    /// to a deadline.
    #[test]
    fn a_grant_racing_the_move_out_or_the_commit_is_never_held_twice() {
        use std::sync::atomic::AtomicBool;
        let start = std::time::Instant::now();
        let mut spins = 1_000u32;
        let (mut held, mut out) = (0u32, 0u32);
        let mut round = 0u32;
        while round < 2_000 || held == 0 || out == 0 {
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "one order only after {round} rounds: held {held}, out {out}"
            );
            if round > 0 && round.is_multiple_of(200) && (held == 0 || out == 0) {
                spins = spins.saturating_mul(2);
            }
            let commit = round % 4 >= 2;
            let mover_first = round.is_multiple_of(2);
            let l = Arc::new(tracking());
            l.insert(K, Token(7));
            let go = Arc::new(AtomicBool::new(false));
            let wait = move |go: &AtomicBool, late: bool| {
                while !go.load(Ordering::Acquire) {
                    std::hint::spin_loop();
                }
                if late {
                    for _ in 0..spins {
                        std::hint::spin_loop();
                    }
                }
            };
            let mover = {
                let (l, go) = (l.clone(), go.clone());
                std::thread::spawn(move || {
                    wait(&go, !mover_first);
                    let moved = l.send_out(&K);
                    if commit {
                        l.committed(&K, Token(7));
                    }
                    moved
                })
            };
            let granter = {
                let (l, go) = (l.clone(), go.clone());
                std::thread::spawn(move || {
                    wait(&go, mover_first);
                    l.insert(K, Token(7))
                })
            };
            go.store(true, Ordering::Release);
            let again = granter.join().unwrap();
            assert_eq!(mover.join().unwrap(), Some(Token(7)));
            match again {
                Insert::Held => held += 1,
                Insert::Out => out += 1,
                Insert::New => panic!("held twice in round {round}"),
            }
            assert_eq!(l.len(), 0, "held twice in round {round}");
            assert_eq!(l.pending_len(), usize::from(!commit));
            assert_eq!(l.insert(K, Token(7)), Insert::Out);
            round += 1;
        }
    }

    use lodestar_bencode::{OwnedValue, to_vec};
    use lodestar_cluster::{Claim, Claimed};
    use lodestar_peer_wire::Fetched;
    use lodestar_store::{Store, StoreConfig};
    use tokio::sync::mpsc;

    use crate::coordinator::Coordinator;
    use crate::gate::Origin;

    fn info(name: &str) -> Vec<u8> {
        to_vec(&OwnedValue::Dict(
            [
                (b"length".to_vec(), OwnedValue::Int(1 << 20)),
                (b"name".to_vec(), OwnedValue::from(name)),
                (b"piece length".to_vec(), OwnedValue::Int(1 << 18)),
                (b"pieces".to_vec(), OwnedValue::Bytes(vec![0; 80])),
            ]
            .into_iter()
            .collect(),
        ))
    }

    /// The prepare stage's terminal losses reach the gate once each: the
    /// floor's hash list as filtered, unusable metainfo as invalid, and a
    /// usable one goes on to the writer uncounted.
    #[tokio::test]
    async fn prepare_losses_count_as_filtered_and_invalid_metainfo() {
        let funnel = Arc::new(FunnelCounts::default());
        let coordinator = Arc::new(Coordinator::new(1, 600, funnel.clone()));
        let leases = Arc::new(WorkerLeases::default());
        let gate = Arc::new(GateStats::new(0));
        let filter = Arc::new(crate::default_filter());
        let source = SourceId::new(1).unwrap();
        // A dictionary that hashes to its key but isn't metainfo.
        let raws = [info("Floor"), b"de".to_vec(), info("Fine")];
        let keys: Vec<InfoHash> = raws
            .iter()
            .map(|r| InfoHash(lodestar_peer_wire::testing::Seed::v1(r.clone()).info_hash))
            .collect();
        filter.update(|core| {
            core.cs
                .add_hash(keys[0].0, lodestar_filter::Origin::Imported)
        });
        for k in &keys {
            let claim = Claim {
                key: *k,
                source,
                strategy: 0,
                at: 0,
            };
            let Claimed::Granted(token) = coordinator.claim(WorkerId::LOCAL, claim, 0) else {
                panic!("granted")
            };
            leases.insert(*k, token);
        }
        let store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let stage = PrepareStage {
            worker: WorkerId::LOCAL,
            preparer: store.preparer(),
            filter,
            reports: coordinator.clone(),
            leases,
            stats: Arc::default(),
            gate: gate.clone(),
            adult: AdultSources::default(),
            batch_max: 8,
            batch_wait: Duration::from_millis(1),
        };
        let (in_tx, in_rx) = mpsc::channel(8);
        let (out_tx, mut out_rx) = mpsc::channel(8);
        for (key, raw) in keys.iter().zip(raws) {
            in_tx
                .send(FetchedKey {
                    key: *key,
                    fetched: Fetched {
                        info: raw,
                        v2: false,
                        encrypted: false,
                        pex: Vec::new(),
                        client: None,
                    },
                    source,
                    sources: 1 << source.get(),
                    cohort: 0,
                    strategy: 0,
                    discovered_at: 0,
                    private_source: false,
                })
                .await
                .unwrap();
        }
        drop(in_tx);
        stage.run(in_rx, out_tx).await;
        let r = out_rx.recv().await.expect("the usable one");
        assert_eq!(r.key, keys[2]);
        assert_eq!(gate.excluded(Excluded::Filtered), 1);
        assert_eq!(gate.excluded(Excluded::InvalidMetainfo), 1);
        assert_eq!(gate.excluded(Excluded::FailedVerification), 0);
        assert_eq!(gate.new_verified(Origin::Network), 0);
    }

    /// A fetch an always-adult source found is stored adult, as an Is
    /// adult correction makes it, and says so to the writer; the same
    /// torrent from another source is detection's.
    #[test]
    fn a_fetch_an_always_adult_source_found_is_prepared_adult() {
        use lodestar_classify::{ContentType, Record};
        let store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let mut preparer = store.preparer();
        let engine = crate::default_filter().current();
        let raw = info("Garden.Tour.2022.1080p.mkv");
        let key = InfoHash(lodestar_peer_wire::testing::Seed::v1(raw.clone()).info_hash);
        let source = SourceId::new(2).unwrap();
        let fk = FetchedKey {
            key,
            fetched: Fetched {
                info: raw,
                v2: false,
                encrypted: false,
                pex: Vec::new(),
                client: None,
            },
            source,
            sources: 1 << source.get(),
            cohort: 0,
            strategy: 0,
            discovered_at: 0,
            private_source: false,
        };
        let stored = |d: &Done| {
            let e = d.torrent.enrichment.as_ref().unwrap();
            let r = Record::decode(&e.data).unwrap();
            (ContentType::from_code(e.content_type), r.content_type)
        };
        let plain = prepare_one(&mut preparer, &engine, &fk, false).unwrap();
        assert_ne!(plain.class.content_type, ContentType::Adult);
        assert_ne!(stored(&plain).0, Some(ContentType::Adult));
        assert!(!plain.source_adult && plain.fix.is_none());
        let adult = prepare_one(&mut preparer, &engine, &fk, true).unwrap();
        assert_eq!(
            stored(&adult),
            (Some(ContentType::Adult), ContentType::Adult)
        );
        assert!(adult.source_adult);
        assert_eq!(adult.fix, Some(true));
        // Detection's own reading is kept for filtering beside it.
        assert_eq!(adult.class.content_type, plain.class.content_type);
    }

    /// The pool's terminal loss to data that didn't hash to the key counts
    /// once, under the current lease only. A protocol violation is the
    /// same funnel loss but not a failed verification, and other losses
    /// don't count.
    #[test]
    fn a_verification_loss_counts_once_under_its_lease() {
        let funnel = Arc::new(FunnelCounts::default());
        let coordinator = Arc::new(Coordinator::new(1, 600, funnel.clone()));
        let leases = Arc::new(WorkerLeases::default());
        let gate = Arc::new(GateStats::new(0));
        let wf = WorkerFunnel {
            counts: funnel.clone(),
            reports: coordinator.clone(),
            leases: leases.clone(),
            gate: gate.clone(),
        };
        let source = SourceId::new(1).unwrap();
        let cases = [
            (Loss::Invalid, Some(FailureKind::HashMismatch)),
            (Loss::Invalid, Some(FailureKind::Invalid)),
            (Loss::NoPeers, None),
            // Not a cause the pool passes, but the gate doesn't rely on it.
            (Loss::Evicted, Some(FailureKind::HashMismatch)),
        ];
        for (i, (loss, cause)) in cases.into_iter().enumerate() {
            let key = InfoHash([i as u8 + 1; 20]);
            let claim = Claim {
                key,
                source,
                strategy: 0,
                at: 0,
            };
            let Claimed::Granted(token) = coordinator.claim(WorkerId::LOCAL, claim, 0) else {
                panic!("granted")
            };
            leases.insert(key, token);
            wf.lost(&key, 0, source, loss, cause);
            // A second report has no lease left.
            wf.lost(&key, 0, source, loss, cause);
            // Only the hash mismatch, the first case, counts.
            assert_eq!(
                gate.excluded(Excluded::FailedVerification),
                1,
                "after {cause:?}"
            );
        }
        // Both invalid keys are still the funnel's `invalid` losses.
        assert_eq!(funnel.total(Stage::Lost, Some(Loss::Invalid)), 2);
        assert_eq!(funnel.total(Stage::Lost, Some(Loss::NoPeers)), 1);
        assert_eq!(funnel.total(Stage::Lost, Some(Loss::Evicted)), 1);
    }
}
