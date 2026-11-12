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

/// The tokens of the leases this worker holds, and of the results on
/// their way out under them (X-wk-own-lease-lost).
///
/// Once a key is fetched and prepared, [`PrepareStage`] moves its token
/// from *held* to *out* in one step ([`send_out`](Self::send_out)): the
/// token now rides in the `Results` record (and the spool) until the
/// coordinator commits it. A claim the worker makes for the key meanwhile
/// is answered with that same lease; [`insert`](Self::insert) tells the
/// caller so, under the same lock, so it's never taken for a new lease.
/// The runtime clears an out entry when its result is committed, released
/// or lost ([`done`](Self::done)); entries older than a lease TTL are
/// pruned on insert, so a path that misses one can't grow the map.
#[derive(Debug)]
pub struct WorkerLeases {
    inner: Mutex<Leases>,
    ttl: Duration,
}

#[derive(Debug, Default)]
struct Leases {
    held: HashMap<InfoHash, Token>,
    out: HashMap<InfoHash, (Token, std::time::Instant)>,
    /// `out`'s entries in the order they went out, for the prune.
    order: std::collections::VecDeque<(std::time::Instant, InfoHash, Token)>,
}

impl Leases {
    fn prune(&mut self, now: std::time::Instant, ttl: Duration) {
        while let Some(&(at, key, token)) = self.order.front() {
            if now.saturating_duration_since(at) < ttl {
                break;
            }
            self.order.pop_front();
            if self.out.get(&key) == Some(&(token, at)) {
                self.out.remove(&key);
            }
        }
    }
}

/// What [`WorkerLeases::insert`] made of a grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Insert {
    /// A lease the worker didn't hold under this token: held now.
    New,
    /// Held already under this token: nothing changed.
    Held,
    /// Its result is on its way out under this token: nothing changed,
    /// and the key isn't held again.
    Out,
}

impl Default for WorkerLeases {
    fn default() -> WorkerLeases {
        WorkerLeases {
            inner: Mutex::default(),
            ttl: Duration::from_secs(crate::LEASE_TTL_SECS.unsigned_abs()),
        }
    }
}

impl WorkerLeases {
    fn lock(&self) -> std::sync::MutexGuard<'_, Leases> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Records a grant of `key` under `token`, unless it's the lease the
    /// worker holds or the one its result is on its way out under.
    pub fn insert(&self, key: InfoHash, token: Token) -> Insert {
        self.insert_at(key, token, std::time::Instant::now())
    }

    fn insert_at(&self, key: InfoHash, token: Token, now: std::time::Instant) -> Insert {
        let mut l = self.lock();
        l.prune(now, self.ttl);
        if l.out.get(&key).is_some_and(|&(t, _)| t == token) {
            return Insert::Out;
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

    /// The key's token, moved from held to out in one step: its result
    /// carries the lease from here.
    pub fn send_out(&self, key: &InfoHash) -> Option<Token> {
        self.send_out_at(key, std::time::Instant::now())
    }

    fn send_out_at(&self, key: &InfoHash, now: std::time::Instant) -> Option<Token> {
        let mut l = self.lock();
        let token = l.held.remove(key)?;
        l.out.insert(*key, (token, now));
        l.order.push_back((now, *key, token));
        Some(token)
    }

    /// The result under `token` was committed, released or lost.
    pub fn done(&self, key: &InfoHash, token: Token) {
        let mut l = self.lock();
        if l.out.get(key).is_some_and(|&(t, _)| t == token) {
            l.out.remove(key);
        }
    }

    /// Leases held.
    pub fn len(&self) -> usize {
        self.lock().held.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Results on their way out, still tracked.
    pub fn out_len(&self) -> usize {
        self.lock().out.len()
    }
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
                        let sent = out.send(r).await;
                        drop(item);
                        if sent.is_err() {
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

    #[test]
    fn a_grant_is_new_held_again_or_out() {
        let l = WorkerLeases::default();
        assert_eq!(l.insert(K, Token(7)), Insert::New);
        assert_eq!(l.insert(K, Token(7)), Insert::Held);
        assert_eq!(l.send_out(&K), Some(Token(7)));
        // Its result is out: the same lease coming back isn't held again.
        assert_eq!(l.insert(K, Token(7)), Insert::Out);
        assert_eq!((l.len(), l.out_len()), (0, 1));
        // A new lease is held as today; the old result stays tracked.
        assert_eq!(l.insert(K, Token(8)), Insert::New);
        assert_eq!((l.len(), l.out_len()), (1, 1));
        assert_eq!(l.take(&K), Some(Token(8)));
    }

    #[test]
    fn done_clears_only_its_own_result() {
        let l = WorkerLeases::default();
        l.insert(K, Token(7));
        l.send_out(&K);
        l.done(&K, Token(6));
        assert_eq!(l.out_len(), 1, "another token's result");
        // Committed, released or lost: all end the same way.
        l.done(&K, Token(7));
        assert_eq!(l.out_len(), 0);
        assert_eq!(l.insert(K, Token(7)), Insert::New);
        // A loss before the result goes out leaves nothing out.
        assert_eq!(l.take(&K), Some(Token(7)));
        assert_eq!(l.send_out(&K), None);
        assert_eq!((l.len(), l.out_len()), (0, 0));
    }

    #[test]
    fn out_entries_older_than_a_ttl_are_pruned() {
        let l = WorkerLeases::default();
        let t0 = std::time::Instant::now();
        l.insert_at(K, Token(7), t0);
        l.send_out_at(&K, t0);
        let just_before = t0 + l.ttl - Duration::from_millis(1);
        assert_eq!(l.insert_at(K, Token(7), just_before), Insert::Out);
        // Past the TTL (a spooled result replayed late): pruned, so its
        // own lease coming back counts as new, as before this fix.
        assert_eq!(l.insert_at(K, Token(7), t0 + l.ttl), Insert::New);
        assert_eq!(l.out_len(), 0);
        // An entry cleared and sent out again isn't pruned by the old
        // one's place in the order.
        let l = WorkerLeases::default();
        l.insert_at(K, Token(7), t0);
        l.send_out_at(&K, t0);
        l.done(&K, Token(7));
        let t1 = t0 + Duration::from_secs(1);
        l.insert_at(K, Token(7), t1);
        l.send_out_at(&K, t1);
        assert_eq!(
            l.insert_at(K, Token(7), t0 + l.ttl),
            Insert::Out,
            "still within its own TTL"
        );
    }

    /// The prepare stage moving a key out while its own lease comes back
    /// on another thread: whichever runs first, the token ends up held or
    /// out, never both and never neither.
    #[test]
    fn a_grant_racing_the_move_out_is_never_lost_or_held_twice() {
        for _ in 0..2_000 {
            let l = Arc::new(WorkerLeases::default());
            l.insert(K, Token(7));
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let mover = {
                let (l, barrier) = (l.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    l.send_out(&K)
                })
            };
            barrier.wait();
            let again = l.insert(K, Token(7));
            let moved = mover.join().unwrap();
            assert_eq!(moved, Some(Token(7)), "the result carries the lease");
            assert!(matches!(again, Insert::Held | Insert::Out), "{again:?}");
            assert_eq!((l.len(), l.out_len()), (0, 1), "held twice: {again:?}");
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
