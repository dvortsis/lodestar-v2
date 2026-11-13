//! The worker's spool (architecture §19.6, workers plan WK-11), around
//! `lodestar_pipeline::spool`'s segments in `<data>/spool`.
//!
//! - **While no session is up,** every `Results` batch that wasn't
//!   accepted goes to the spool, sealed with the worker's own secrets key
//!   (owner wkq1), and leaves memory: what was in flight when the session
//!   went, and what the fetches under way and the prepare stage still
//!   finish. The pool opens no new connection meanwhile and the DHT keeps
//!   only its routing (WK-5 and WK-10c: the coordinator re-plans the
//!   per-IP shares without this worker at once). A full spool pauses the
//!   pool too, session or not, until a replay makes room; a batch that
//!   doesn't fit stays in memory and goes out on the next session.
//! - **On a new session** the spool is replayed in order, one segment at
//!   a time, under fresh batch ids (the coordinator matches `Accepted` by
//!   the outer id only, and its idempotency is by token, WK-9). A segment
//!   is removed securely once every record in it is accepted
//!   (`accepted: true`, whatever the commit decided); a refusal for now
//!   is sent again on the retry tick. Replay shares the session with live
//!   results and never holds up heartbeats, which the session driver
//!   sends on its own timer.
//! - **Replay twice = once:** records accepted from a segment are
//!   remembered by position, so a segment read again after a lost session
//!   sends only what wasn't accepted; what is resent anyway (an `Accepted`
//!   lost with the session) is answered as stale by the coordinator and
//!   counted nowhere twice.
//!
//! All file work runs on blocking threads.

use lodestar_cluster::ClusterError;
use lodestar_cluster::proto::ReconcileAnswer;
use lodestar_integrations::SecretStore;
use lodestar_pipeline::spool::{Caps, Kind, Record, Sealer, Spool, SpoolError, SpoolStats};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use super::Layout;

/// The folder inside the worker's data folder.
pub const SPOOL_DIR: &str = "spool";

/// What the runtime's spool is built from.
#[derive(Clone)]
pub struct SpoolConfig {
    pub dir: PathBuf,
    pub caps: Caps,
    pub sealer: Arc<dyn Sealer>,
}

impl std::fmt::Debug for SpoolConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpoolConfig")
            .field("dir", &self.dir)
            .field("caps", &self.caps)
            .finish_non_exhaustive()
    }
}

impl SpoolConfig {
    /// `<data>/spool`, sealed with `<data>/secrets.key`, at the default
    /// caps (1 GB, 24 h).
    /// The folder is created here, so one that can't be stops the worker
    /// with its message instead of leaving it without a spool.
    pub fn for_layout(layout: &Layout) -> Result<SpoolConfig, ClusterError> {
        let dir = layout.data.join(SPOOL_DIR);
        lodestar_pipeline::spool::create_private_dir(&dir)
            .map_err(|e| ClusterError::Io("create the spool folder", e.kind()))?;
        Ok(SpoolConfig {
            dir,
            caps: Caps::default(),
            sealer: Arc::new(layout.secrets()?),
        })
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The spool, shared by the runtime's tasks. It's opened on a blocking
/// thread by the session task as it starts ([`start`](Self::start)), never
/// on an async thread: opening lists the folder and removes leftovers.
#[derive(Debug)]
pub struct WorkerSpool {
    config: SpoolConfig,
    worker: u32,
    spool: Arc<Mutex<Option<Spool>>>,
    stats: std::sync::OnceLock<Arc<SpoolStats>>,
    full: AtomicBool,
    /// The spool couldn't be opened: the runtime runs without one.
    pub unavailable: AtomicBool,
    /// Write, read and removal failures (the disk, not the format).
    pub errors: AtomicU64,
    /// Lease-count drift seen after a replay: the difference between the
    /// worker's leases and the coordinator's, each time they differed.
    pub drift: AtomicU64,
    /// What the coordinator made of the lists sent when they differed
    /// (WK-11b): leases it gave back for re-grant, listed leases it didn't
    /// have on record, and lists it refused.
    pub returned: AtomicU64,
    pub unknown: AtomicU64,
    pub refused: AtomicU64,
    /// A replay drained since the last comparison.
    compare: AtomicBool,
}

/// One spooled record being replayed: its segment and frame position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Origin {
    pub seq: u64,
    pub index: usize,
}

/// A record read back for replay.
#[derive(Debug)]
pub struct Replayed {
    pub from: Origin,
    pub kind: Kind,
    pub leases: Vec<([u8; 20], u64)>,
    pub payload: Vec<u8>,
}

/// The segment being replayed: which of its frames are done (accepted, or
/// never to be sent: too old or damaged), by frame position.
#[derive(Debug, Default)]
pub struct Replay {
    current: Option<(u64, Vec<bool>)>,
    /// Frames accepted from segments read since (kept across sessions).
    accepted: HashMap<u64, Vec<bool>>,
}

impl Replay {
    /// Whether no segment is being replayed.
    pub fn is_idle(&self) -> bool {
        self.current.is_none()
    }

    /// A new session: the segment is read again from the start, skipping
    /// what was accepted.
    pub fn restart(&mut self) {
        if let Some((seq, done)) = self.current.take() {
            self.accepted.insert(seq, done);
        }
    }

    /// Marks `from` accepted; `Some(seq)` once its whole segment is done,
    /// for removal.
    pub fn accepted(&mut self, from: Origin) -> Option<u64> {
        let (seq, done) = self.current.as_mut()?;
        if *seq != from.seq {
            return None;
        }
        if let Some(d) = done.get_mut(from.index) {
            *d = true;
        }
        if done.iter().all(|&d| d) {
            let seq = *seq;
            self.current = None;
            self.accepted.remove(&seq);
            return Some(seq);
        }
        None
    }
}

/// What reading the next segment found.
enum Next {
    Empty,
    /// Wholly past the age cap, or unreadable: removed or set aside.
    Skip,
    Read(lodestar_pipeline::spool::Segment),
}

impl WorkerSpool {
    /// The spool for `worker`, not opened yet (no I/O here).
    pub fn new(config: SpoolConfig, worker: u32) -> WorkerSpool {
        WorkerSpool {
            config,
            worker,
            spool: Arc::new(Mutex::new(None)),
            stats: std::sync::OnceLock::new(),
            full: AtomicBool::new(false),
            unavailable: AtomicBool::new(false),
            errors: AtomicU64::new(0),
            drift: AtomicU64::new(0),
            returned: AtomicU64::new(0),
            unknown: AtomicU64::new(0),
            refused: AtomicU64::new(0),
            compare: AtomicBool::new(false),
        }
    }

    /// Opens the folder on a blocking thread. `false` if it couldn't be
    /// opened: counted, and the runtime keeps batches in memory as before.
    pub async fn start(&self) -> bool {
        let (config, worker) = (self.config.clone(), self.worker);
        let opened = tokio::task::spawn_blocking(move || {
            Spool::open(&config.dir, worker, config.caps, config.sealer)
        })
        .await;
        match opened {
            Ok(Ok(spool)) => {
                let _ = self.stats.set(spool.stats());
                *lock(&self.spool) = Some(spool);
                true
            }
            _ => {
                self.unavailable.store(true, Ordering::Release);
                self.errors.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    /// The spool's counters (`None` until it's open).
    pub fn stats(&self) -> Option<&SpoolStats> {
        self.stats.get().map(|s| &**s)
    }

    /// Bytes on disk now (the heartbeat's `spool_bytes`).
    pub fn bytes(&self) -> u64 {
        self.stats().map_or(0, |s| s.bytes.load(Ordering::Relaxed))
    }

    /// Whether results may still wait on disk for a replay: the spool
    /// isn't open yet, or holds bytes. A spool that couldn't be opened
    /// holds nothing to replay. A new session's lease list waits until
    /// this is false (CLUSTER-01).
    pub fn replay_pending(&self) -> bool {
        !self.unavailable.load(Ordering::Acquire)
            && self
                .stats()
                .is_none_or(|s| s.bytes.load(Ordering::Relaxed) > 0)
    }

    /// Whether the last write found the spool full (fetching pauses).
    pub fn is_full(&self) -> bool {
        self.full.load(Ordering::Acquire)
    }

    /// Runs `f` on the open spool on a blocking thread; `None` if it isn't
    /// open (or the thread failed).
    async fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Spool) -> T + Send + 'static,
    ) -> Option<T> {
        let spool = self.spool.clone();
        tokio::task::spawn_blocking(move || lock(&spool).as_mut().map(f))
            .await
            .ok()
            .flatten()
    }

    /// Writes one batch, made at `at` (its age runs from then). `false` if
    /// it wasn't (full, no spool, or the disk
    /// failed): the batch stays in memory and is sent once a session is up.
    pub async fn put(
        &self,
        kind: Kind,
        batch: u64,
        at: i64,
        leases: Vec<([u8; 20], u64)>,
        payload: Vec<u8>,
    ) -> bool {
        let record = Record {
            kind,
            batch,
            at,
            leases,
            payload,
        };
        match self.blocking(move |s| s.append(&record, unix_now())).await {
            Some(Ok(())) => true,
            Some(Err(SpoolError::Full)) => {
                self.full.store(true, Ordering::Release);
                false
            }
            None if self.unavailable.load(Ordering::Acquire) => false,
            _ => {
                self.errors.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    /// Syncs what was appended (the end of each round of writes).
    pub async fn sync(&self) {
        if matches!(self.blocking(|s| s.sync()).await, Some(Err(_))) {
            self.errors.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The next segment's records to replay, oldest first, starting it in
    /// `replay`; empty when nothing is spooled. On the way, a segment
    /// wholly past the age cap or with nothing left to send is removed,
    /// and one that can't be read is set aside until the next start, so it
    /// never holds up the rest. Damage is counted (`dropped_corrupt`).
    /// After it, the caller re-checks the pool's pause (a removal can
    /// clear "full").
    pub async fn next(&self, replay: &mut Replay) -> Vec<Replayed> {
        loop {
            let now = unix_now();
            let read = self
                .blocking(move |s| {
                    if s.segments().is_empty() {
                        s.close_active()?;
                    }
                    let Some(seq) = s.segments().first().copied() else {
                        return Ok(Next::Empty);
                    };
                    if s.is_expired(seq, now) {
                        // Every record in it is past the cap: dropped and
                        // counted without opening any.
                        if s.drop_expired(seq).is_err() {
                            s.forget(seq);
                        }
                        return Ok(Next::Skip);
                    }
                    match s.read(seq, now) {
                        Ok(seg) => Ok(Next::Read(seg)),
                        // Gone, or unreadable now: set aside.
                        Err(_) => {
                            s.forget(seq);
                            Err(SpoolError::Io("read", std::io::ErrorKind::Other))
                        }
                    }
                })
                .await;
            let seg = match read {
                Some(Ok(Next::Read(seg))) => seg,
                Some(Ok(Next::Skip)) => {
                    self.after_removal().await;
                    continue;
                }
                Some(Err(_)) => {
                    self.errors.fetch_add(1, Ordering::Relaxed);
                    self.after_removal().await;
                    continue;
                }
                Some(Ok(Next::Empty)) | None => return Vec::new(),
            };
            let seq = seg.seq;
            // Done: accepted on an earlier read, or never to be sent (too
            // old, damaged), by frame position.
            let mut done = replay
                .accepted
                .remove(&seq)
                .filter(|d| d.len() == seg.frames)
                .unwrap_or_else(|| vec![false; seg.frames]);
            let mut live = vec![false; seg.frames];
            for &i in &seg.index {
                live[i] = true;
            }
            for (d, l) in done.iter_mut().zip(&live) {
                *d |= !l;
            }
            let out: Vec<Replayed> = seg
                .records
                .into_iter()
                .zip(seg.index)
                .filter(|(_, i)| !done[*i])
                .map(|(r, index)| Replayed {
                    from: Origin { seq, index },
                    kind: r.kind,
                    leases: r.leases,
                    payload: r.payload,
                })
                .collect();
            if out.is_empty() {
                self.remove(seq).await;
                continue;
            }
            replay.current = Some((seq, done));
            return out;
        }
    }

    /// Removes a segment securely (all of it accepted, or past the age
    /// cap). One that can't be removed now is set aside until the next
    /// start, so replay goes on.
    pub async fn remove(&self, seq: u64) {
        let r = self
            .blocking(move |s| {
                let r = s.remove(seq);
                if r.is_err() {
                    s.forget(seq);
                }
                r
            })
            .await;
        if !matches!(r, Some(Ok(()))) {
            self.errors.fetch_add(1, Ordering::Relaxed);
        }
        self.after_removal().await;
    }

    /// After any segment leaves: "full" clears once the spool is half
    /// empty (so one near its cap doesn't flap), and an empty spool asks
    /// for the lease-count comparison.
    async fn after_removal(&self) {
        let room = self
            .blocking(|s| s.bytes() <= s.caps().max_bytes() / 2)
            .await
            .unwrap_or(true);
        if room {
            self.full.store(false, Ordering::Release);
        }
        if self.bytes() == 0 {
            self.compare.store(true, Ordering::Release);
        }
    }

    /// Whether a replay has drained since the last call: the session then
    /// compares lease counts once.
    pub fn take_reconcile(&self) -> bool {
        self.compare.swap(false, Ordering::AcqRel)
    }

    /// Counts the difference between the worker's lease count and the
    /// coordinator's, once a replay is done; 0 when they agree.
    pub fn reconcile(&self, worker: u32, coordinator: u32) -> u32 {
        let d = worker.abs_diff(coordinator);
        if d > 0 {
            self.drift.fetch_add(u64::from(d), Ordering::Relaxed);
        }
        d
    }

    /// Records the coordinator's answer to the list of leases held.
    pub fn listed(&self, answer: ReconcileAnswer) {
        match answer {
            ReconcileAnswer::Done {
                returned, unknown, ..
            } => {
                self.returned
                    .fetch_add(u64::from(returned), Ordering::Relaxed);
                self.unknown
                    .fetch_add(u64::from(unknown), Ordering::Relaxed);
            }
            ReconcileAnswer::Refused => {
                self.refused.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// `--rotate-key`'s step for the spool, between the rotation and the
/// retire (WK-2's note): every segment is re-sealed under the new primary
/// key. Returns the segments rewritten (0 with no spool or no join).
pub fn reseal(layout: &Layout, worker: Option<u32>) -> Result<usize, ClusterError> {
    let dir = layout.data.join(SPOOL_DIR);
    let Some(worker) = worker else {
        return Ok(0);
    };
    if !dir.exists() {
        return Ok(0);
    }
    let secrets: Arc<dyn Sealer> = Arc::new(SecretStore::open_or_create(&layout.data)?);
    let fail = |_| ClusterError::Corrupt("the re-sealed spool");
    let mut spool = Spool::open(&dir, worker, Caps::default(), secrets).map_err(fail)?;
    spool.reseal().map_err(fail)
}

/// Whether the spool folder in `data` holds anything sealed under a key
/// other than `secrets`' primary (a check before the retire).
pub fn needs_reseal(data: &Path, worker: u32, secrets: &SecretStore) -> Result<bool, ClusterError> {
    let dir = data.join(SPOOL_DIR);
    if !dir.exists() {
        return Ok(false);
    }
    for entry in
        std::fs::read_dir(&dir).map_err(|e| ClusterError::Io("read the spool", e.kind()))?
    {
        let path = entry
            .map_err(|e| ClusterError::Io("read the spool", e.kind()))?
            .path();
        if path.extension().is_none_or(|e| e != "lsp") {
            continue;
        }
        // Capped as the spool's own reads are: a segment never exceeds it.
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(
            &mut std::io::Read::take(
                std::fs::File::open(&path)
                    .map_err(|e| ClusterError::Io("read the spool", e.kind()))?,
                lodestar_pipeline::spool::SEGMENT_BYTES
                    + 2 * lodestar_pipeline::spool::MAX_FRAME as u64,
            ),
            &mut bytes,
        )
        .map_err(|e| ClusterError::Io("read the spool", e.kind()))?;
        let Ok(d) = lodestar_pipeline::spool::decode_segment(&bytes) else {
            continue;
        };
        if d.header.worker == worker && d.frames.iter().any(|f| Sealer::is_stale(secrets, f)) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    //! The spool inside the runtime: batches go to disk with no session,
    //! are replayed on the next, once, and leave the disk once accepted;
    //! a full spool pauses fetching; nothing on disk names a peer.

    use std::sync::Arc;

    use lodestar_cluster::proto::ToWorker;
    use lodestar_cluster::{Token, WorkerEvent};
    use lodestar_pipeline::spool::Caps;
    use lodestar_pipeline::wire::ResultsBatch;

    use super::*;
    use crate::worker::runtime::tests::{
        DEADLINE, MapFetch, W, assign_dht, assignment, config, filter_chunks, info, key_of,
        next_results, sealed, session_pair, until,
    };
    use crate::worker::runtime::{Config, Runtime};
    use crate::worker::session::Work;

    fn spooled(dir: &Path, caps: Caps) -> Config {
        Config {
            spool: Some(SpoolConfig {
                dir: dir.to_path_buf(),
                caps,
                sealer: Arc::new(SecretStore::from_key(&[4; 32])),
            }),
            ..config(Arc::new(MapFetch::default()))
        }
    }

    fn records(sp: &WorkerSpool) -> u64 {
        sp.stats().map_or(0, |s| s.appended.load(Ordering::Relaxed))
    }

    async fn accept(
        coord: &mut lodestar_cluster::CoordinatorLink,
        session: &mut lodestar_cluster::WorkerSession,
        rt: &mut Runtime,
        batch: u64,
        accepted: bool,
    ) {
        coord
            .send(ToWorker::Accepted { batch, accepted })
            .await
            .unwrap();
        let e = tokio::time::timeout(DEADLINE, session.next_event())
            .await
            .unwrap()
            .unwrap();
        rt.event(e);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn batches_spooled_offline_are_replayed_once_then_removed() {
        let dir = tempfile::tempdir().unwrap();
        let mut rt = Runtime::start(spooled(dir.path(), Caps::default()));
        let sp = rt.spool().expect("a spool");
        let out = rt.test_outbound();
        // No session: two batches go to disk, one at a time.
        out.send(sealed(1)).await.unwrap();
        until("spooled", || (records(&sp) == 1, records(&sp))).await;
        out.send(sealed(2)).await.unwrap();
        until("spooled", || (records(&sp) == 2, records(&sp))).await;
        assert!(sp.bytes() > 0);

        // A session: replayed in order, each with its tokens.
        let (mut coord, mut session) = session_pair().await;
        rt.connected(session.handle());
        let first = next_results(&mut coord).await;
        let second = next_results(&mut coord).await;
        let token = |p: &lodestar_cluster::proto::Payload| {
            let (_, sealed) = ResultsBatch::decode_with(&p.bytes).unwrap();
            sealed[0].token
        };
        assert_eq!((token(&first), token(&second)), (Token(2), Token(3)));

        // The first is accepted; the session is lost before the second.
        accept(&mut coord, &mut session, &mut rt, first.batch, true).await;
        let c = rt.counts();
        until("committed", || {
            let n = c.committed.load(Ordering::Relaxed);
            (n == 1, n)
        })
        .await;
        rt.disconnected();
        drop((coord, session));

        // Replayed again: only what wasn't accepted.
        let (mut coord, mut session) = session_pair().await;
        rt.connected(session.handle());
        let again = next_results(&mut coord).await;
        assert_eq!(token(&again), Token(3));
        // Refused for now: sent again on the retry tick.
        accept(&mut coord, &mut session, &mut rt, again.batch, false).await;
        let retried = next_results(&mut coord).await;
        assert_eq!(retried.batch, again.batch);
        // Refused for now isn't accepted: the segment is still on disk.
        assert!(sp.bytes() > 0, "kept after a refusal");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        accept(&mut coord, &mut session, &mut rt, retried.batch, true).await;
        until("removed", || (sp.bytes() == 0, sp.bytes())).await;
        assert_eq!(c.committed.load(Ordering::Relaxed), 2, "each once");
        // Drained: the session compares lease counts once, and a
        // difference is counted (this runtime holds none). The flag is
        // set just after the bytes reach 0, so wait on the flag itself.
        let d = until("drained", || {
            let r = rt.reconcile();
            (r.is_some(), r)
        })
        .await;
        assert_eq!(d, Some(0));
        assert_eq!(rt.reconcile(), None);
        rt.reconciled(0, 3);
        assert_eq!(sp.drift.load(Ordering::Relaxed), 3);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        rt.shutdown().await;
    }

    /// CLUSTER-05: a local stop in the middle of a batch (sent, not yet
    /// accepted, and then the session lost) leaves it on disk by the time
    /// the stop returns, so the process can exit at once. CLUSTER-01: the
    /// new session's lease list waits while it's there to replay, and goes
    /// out once the replay has drained.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stop_mid_batch_spools_it_and_the_list_waits_for_the_replay() {
        let dir = tempfile::tempdir().unwrap();
        let mut rt = Runtime::start(spooled(dir.path(), Caps::default()));
        let sp = rt.spool().unwrap();
        let (mut coord, session) = session_pair().await;
        rt.connected(session.handle());
        // Kept: the session task ends with the queue.
        let out = rt.test_outbound();
        out.send(sealed(4)).await.unwrap();
        let sent = next_results(&mut coord).await;
        assert_eq!(records(&sp), 0, "a session up: nothing spooled");
        rt.disconnected();
        drop((coord, session));
        rt.stop().await;
        // No wait: durable once the stop returns.
        assert_eq!(records(&sp), 1);
        assert!(rt.holding().is_none(), "a replay is pending");
        drop((rt, sp, out));

        // The next start replays it; the list goes out after.
        let mut rt = Runtime::start(spooled(dir.path(), Caps::default()));
        let sp = rt.spool().unwrap();
        assert!(rt.holding().is_none(), "not open, or not replayed yet");
        let (mut coord, mut session) = session_pair().await;
        rt.connected(session.handle());
        let again = next_results(&mut coord).await;
        assert_eq!(again.bytes, sent.bytes);
        accept(&mut coord, &mut session, &mut rt, again.batch, true).await;
        until("removed", || (sp.bytes() == 0, sp.bytes())).await;
        let list = until("listed", || {
            let l = rt.holding();
            (l.is_some(), l)
        })
        .await;
        // Its lease stays listed after the commit, for the grace.
        assert_eq!(list.map(|l| l.len()), Some(1));
        rt.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_spool_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let rt = Runtime::start(spooled(dir.path(), Caps::default()));
        let sp = rt.spool().unwrap();
        rt.test_outbound().send(sealed(7)).await.unwrap();
        until("spooled", || (records(&sp) == 1, records(&sp))).await;
        rt.shutdown().await;
        drop(sp);

        let mut rt = Runtime::start(spooled(dir.path(), Caps::default()));
        let (mut coord, mut session) = session_pair().await;
        rt.connected(session.handle());
        let p = next_results(&mut coord).await;
        let (_, s) = ResultsBatch::decode_with(&p.bytes).unwrap();
        assert_eq!(s[0].token, Token(8));
        accept(&mut coord, &mut session, &mut rt, p.batch, true).await;
        let sp = rt.spool().unwrap();
        until("removed", || (sp.bytes() == 0, sp.bytes())).await;
        rt.shutdown().await;
    }

    /// Offline, the pool opens no new connection (WK-10c), and a full
    /// spool keeps it paused with a session back, until a replay makes
    /// room; a batch that doesn't fit stays in memory meanwhile.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_full_spool_pauses_fetching_until_a_replay_makes_room() {
        let dir = tempfile::tempdir().unwrap();
        let caps = Caps::unclamped(3_000, 3600);
        let mut rt = Runtime::start(spooled(dir.path(), caps));
        let (_coord, session) = session_pair().await;
        rt.connected(session.handle());
        rt.event(WorkerEvent::Assign(assignment(1, 0, 1)));
        assert!(!rt.test_pool_paused(), "a session: fetching");
        rt.disconnected();
        assert!(rt.test_pool_paused(), "offline: paused");
        let out = rt.test_outbound();
        let sp = rt.spool().unwrap();
        // One batch at a time, until one doesn't fit.
        let mut n = 0u64;
        while !sp.is_full() {
            n += 1;
            assert!(n < 200, "never full");
            out.send(sealed(n as usize)).await.unwrap();
            until("spooled or full", || {
                (records(&sp) == n || sp.is_full(), records(&sp))
            })
            .await;
        }
        assert!(sp.bytes() <= 3_000);
        let spooled = records(&sp);
        // A session back: still paused while full. Everything is replayed,
        // the one kept in memory too, and accepted; then it resumes.
        let (mut coord, mut session) = session_pair().await;
        rt.connected(session.handle());
        rt.event(WorkerEvent::Assign(assignment(2, 0, 1)));
        assert!(rt.test_pool_paused(), "full: paused");
        for _ in 0..=spooled {
            let p = next_results(&mut coord).await;
            accept(&mut coord, &mut session, &mut rt, p.batch, true).await;
        }
        let c = rt.counts();
        until("all committed", || {
            let k = c.committed.load(Ordering::Relaxed);
            (k == n, k)
        })
        .await;
        until("room again", || (!rt.test_pool_paused(), sp.bytes())).await;
        assert_eq!(sp.bytes(), 0);
        rt.shutdown().await;
    }

    /// A full spool that ages out entirely while offline: on the next
    /// session every segment is dropped by its age, "full" clears and the
    /// pool resumes (the self-review's B2).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_full_spool_that_ages_out_resumes_fetching() {
        let dir = tempfile::tempdir().unwrap();
        let caps = Caps::unclamped(3_000, 3600);
        let mut rt = Runtime::start(spooled(dir.path(), caps));
        let (_coord, session) = session_pair().await;
        rt.connected(session.handle());
        rt.event(WorkerEvent::Assign(assignment(1, 0, 1)));
        rt.disconnected();
        let out = rt.test_outbound();
        let sp = rt.spool().unwrap();
        let mut n = 0u64;
        while !sp.is_full() {
            n += 1;
            assert!(n < 200, "never full");
            out.send(sealed(n as usize)).await.unwrap();
            until("spooled or full", || {
                (records(&sp) == n || sp.is_full(), records(&sp))
            })
            .await;
        }
        rt.shutdown().await;
        drop(sp);
        // Two days on: every segment is wholly past the age cap.
        for e in std::fs::read_dir(dir.path()).unwrap() {
            let path = e.unwrap().path();
            let mut bytes = std::fs::read(&path).unwrap();
            let mut h = lodestar_pipeline::spool::Header::decode(&bytes).unwrap();
            h.created -= 2 * 86_400;
            bytes[..lodestar_pipeline::spool::HEADER_LEN].copy_from_slice(&h.encode());
            std::fs::write(&path, &bytes).unwrap();
        }
        let mut rt = Runtime::start(spooled(dir.path(), caps));
        let sp = rt.spool().unwrap();
        until("opened", || (sp.stats().is_some(), ())).await;
        // Full, as a live worker would be at the reconnect.
        sp.full.store(true, Ordering::Release);
        assert!(rt.test_pool_paused());
        let (_coord, session) = session_pair().await;
        rt.connected(session.handle());
        rt.event(WorkerEvent::Assign(assignment(2, 0, 1)));
        until("drained by age", || (sp.bytes() == 0, sp.bytes())).await;
        until("resumed", || (!rt.test_pool_paused(), sp.is_full())).await;
        let st = sp.stats().unwrap();
        assert!(st.dropped_old.load(Ordering::Relaxed) >= 1);
        assert_eq!(st.records.load(Ordering::Relaxed), 0);
        rt.shutdown().await;
    }

    /// A full spool with a session up (a reconnect at the cap): nothing new
    /// is claimed and the DHT keeps only its routing, until a replay makes
    /// room (the runner's review, B1).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_full_spool_with_a_session_up_claims_nothing_and_pauses_the_dht() {
        use lodestar_cluster::assign::{Intensity, ShardSpec};
        let dir = tempfile::tempdir().unwrap();
        let mut rt = Runtime::start(spooled(dir.path(), Caps::default()));
        let (_coord, session) = session_pair().await;
        rt.connected(session.handle());
        let shard = ShardSpec::new(64..128, 0xff).unwrap();
        rt.event(WorkerEvent::Assign(assign_dht(
            1,
            Some(shard),
            Intensity::Balanced,
        )));
        let (_, chunks) = filter_chunks().await;
        for c in chunks {
            rt.event(WorkerEvent::Snapshot(c));
        }
        until("started", || (rt.is_started(), ())).await;
        let sp = rt.spool().unwrap();
        until("opened", || (sp.stats().is_some(), ())).await;
        let key = lodestar_metainfo::InfoHash([7; 20]);
        assert!(rt.test_dht_runs(), "a session: the DHT samples");
        assert!(rt.test_would_claim(key));
        assert!(!rt.test_pool_paused());

        sp.full.store(true, Ordering::Release);
        assert!(!rt.test_dht_runs(), "full: routing only");
        let held = rt.counts().held_back.load(Ordering::Relaxed);
        assert!(!rt.test_would_claim(key), "full: nothing claimed");
        assert_eq!(rt.counts().held_back.load(Ordering::Relaxed), held + 1);
        assert!(rt.test_pool_paused());

        // Room again: all three resume.
        sp.full.store(false, Ordering::Release);
        assert!(rt.test_dht_runs());
        assert!(rt.test_would_claim(key));
        assert!(!rt.test_pool_paused());
        rt.shutdown().await;
    }

    /// The DHT resumes by itself once a replay frees room: a real full
    /// spool, a real replay, and the DHT's state read as it stands, with
    /// nothing re-checked by the test (the runner's delta review).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_dht_resumes_when_a_replay_frees_room() {
        use lodestar_cluster::assign::{Intensity, ShardSpec};
        let dir = tempfile::tempdir().unwrap();
        let mut rt = Runtime::start(spooled(dir.path(), Caps::default()));
        let (_coord, session) = session_pair().await;
        rt.connected(session.handle());
        let shard = ShardSpec::new(64..128, 0xff).unwrap();
        rt.event(WorkerEvent::Assign(assign_dht(
            1,
            Some(shard),
            Intensity::Balanced,
        )));
        let (_, chunks) = filter_chunks().await;
        for c in chunks {
            rt.event(WorkerEvent::Snapshot(c));
        }
        until("started", || (rt.is_started(), ())).await;
        until("the DHT runs", || (rt.test_dht_told_to_run(), ())).await;
        // Offline: a spooled batch (written as the session task would),
        // and the spool full, as a reconnect at the cap finds it.
        rt.disconnected();
        drop((_coord, session));
        let sp = rt.spool().unwrap();
        until("opened", || (sp.stats().is_some(), ())).await;
        assert!(
            sp.put(
                Kind::Results,
                77,
                unix_now(),
                vec![([5; 20], 6)],
                b"a spooled batch".to_vec()
            )
            .await
        );
        sp.full.store(true, Ordering::Release);
        // Back, still full: the DHT stays at routing until room is made.
        let (mut coord, mut session) = session_pair().await;
        rt.connected(session.handle());
        rt.event(WorkerEvent::Assign(assign_dht(
            2,
            Some(shard),
            Intensity::Balanced,
        )));
        assert!(!rt.test_dht_told_to_run(), "full: routing only");
        // The replay: the batch is accepted and its segment removed, which
        // frees the room.
        let p = next_results(&mut coord).await;
        assert_eq!(p.bytes, b"a spooled batch");
        accept(&mut coord, &mut session, &mut rt, p.batch, true).await;
        until("the DHT runs again by itself", || {
            (rt.test_dht_told_to_run(), sp.bytes())
        })
        .await;
        rt.shutdown().await;
    }

    /// A record's age is the time `put` is given (when its batch was made),
    /// not when it was written: an old batch reads back as past its cap.
    #[tokio::test]
    async fn put_keeps_the_batchs_own_time() {
        let dir = tempfile::tempdir().unwrap();
        let config = SpoolConfig {
            dir: dir.path().to_path_buf(),
            caps: Caps::default(),
            sealer: Arc::new(SecretStore::from_key(&[4; 32])),
        };
        let sp = WorkerSpool::new(config, 1);
        assert!(sp.start().await);
        let old = unix_now() - lodestar_pipeline::spool::MAX_AGE_SECS - 10;
        assert!(
            sp.put(Kind::Results, 1, old, vec![([1; 20], 2)], b"old".to_vec())
                .await
        );
        let fresh = unix_now();
        assert!(
            sp.put(Kind::Results, 2, fresh, vec![([2; 20], 3)], b"new".to_vec())
                .await
        );
        let mut replay = Replay::default();
        let got = sp.next(&mut replay).await;
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].payload, b"new");
        assert_eq!(sp.stats().unwrap().dropped_old.load(Ordering::Relaxed), 1);
    }

    /// A torrent fetched from a known peer and spooled: no form of the
    /// peer's address is in the segment, sealed or opened.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_peer_address_reaches_the_spool() {
        let dir = tempfile::tempdir().unwrap();
        let fetch = Arc::new(MapFetch::default());
        let raw = info("Spooled from a known peer");
        lock(&fetch.0).insert(key_of(&raw), raw.clone());
        let mut rt = Runtime::start(Config {
            spool: spooled(dir.path(), Caps::default()).spool,
            ..config(fetch)
        });
        let (coord, session) = session_pair().await;
        rt.connected(session.handle());
        rt.event(WorkerEvent::Assign(assignment(1, 0, 1)));
        let (_, chunks) = filter_chunks().await;
        for c in chunks {
            rt.event(WorkerEvent::Snapshot(c));
        }
        until("started", || (rt.is_started(), ())).await;
        // The coordinator reads nothing, so the batch is still unaccepted
        // when the session goes, and is spooled.
        let peer: std::net::SocketAddr = "198.51.100.77:51413".parse().unwrap();
        rt.test_grant(key_of(&raw), Token(9), vec![peer]);
        let c = rt.counts();
        until("sent", || {
            let n = c.sent.load(Ordering::Relaxed);
            (n == 1, n)
        })
        .await;
        rt.disconnected();
        drop((coord, session));
        let sp = rt.spool().unwrap();
        until("spooled", || (records(&sp) == 1, records(&sp))).await;
        rt.shutdown().await;

        let ip = [198u8, 51, 100, 77];
        let port = 51413u16;
        let needles: Vec<Vec<u8>> = vec![
            ip.to_vec(),
            b"198.51.100.77".to_vec(),
            port.to_be_bytes().iter().chain(&ip).copied().collect(),
            ip.iter().chain(&port.to_be_bytes()).copied().collect(),
            ip.iter().chain(&port.to_le_bytes()).copied().collect(),
        ];
        let has = |hay: &[u8]| {
            needles
                .iter()
                .any(|n| hay.windows(n.len()).any(|w| w == n.as_slice()))
        };
        let sealer: Arc<dyn Sealer> = Arc::new(SecretStore::from_key(&[4; 32]));
        let mut spool = Spool::open(dir.path(), W.0, Caps::default(), sealer.clone()).unwrap();
        let segs = spool.segments();
        assert_eq!(segs.len(), 1);
        let bytes = std::fs::read(dir.path().join(format!("{:016x}.lsp", segs[0]))).unwrap();
        assert!(!has(&bytes), "the segment names the peer");
        let seg = spool.read(segs[0], unix_now()).unwrap();
        assert_eq!(seg.records.len(), 1);
        // Inside each payload too: its records opened with the wire key,
        // as the coordinator opens them, and re-encoded in clear.
        let keys = lodestar_pipeline::wire::filter_snapshot::WireKeys::default();
        keys.set(crate::cluster::filter::wire_keys(&[4; 32], unix_now()));
        for r in &seg.records {
            assert!(!has(&r.encode().unwrap()), "a record names the peer");
            assert_eq!(r.leases, vec![(key_of(&raw).0, 9)]);
            let (_, sealed) = ResultsBatch::decode_with(&r.payload).unwrap();
            assert_eq!(sealed.len(), 1);
            for s in &sealed {
                let opened = keys.open(s, W).expect("the record opens");
                let clear = ResultsBatch {
                    batch: 1,
                    records: vec![opened],
                }
                .encode()
                .unwrap();
                assert!(!has(&clear), "an opened record names the peer");
            }
        }
    }

    #[tokio::test]
    async fn a_reconcile_counts_the_drift() {
        let dir = tempfile::tempdir().unwrap();
        let config = SpoolConfig {
            dir: dir.path().to_path_buf(),
            caps: Caps::default(),
            sealer: Arc::new(SecretStore::from_key(&[4; 32])),
        };
        let sp = WorkerSpool::new(config, 1);
        assert!(sp.start().await);
        assert_eq!(sp.reconcile(5, 5), 0);
        assert_eq!(sp.reconcile(3, 5), 2);
        assert_eq!(sp.reconcile(6, 5), 1);
        assert_eq!(sp.drift.load(Ordering::Relaxed), 3);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rotate_key_reseals_the_spool_before_the_retire() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        let clock = lodestar_cluster::ManualClock::starting_now();
        let creds = crate::worker::tests::credentials(&clock);
        let worker = creds.worker().0;
        layout.save(&creds).unwrap();
        let mut config = SpoolConfig::for_layout(&layout).unwrap();
        config.caps = Caps::default();
        let sp = WorkerSpool::new(config, worker);
        assert!(sp.start().await);
        assert!(
            sp.put(
                Kind::Results,
                1,
                unix_now(),
                vec![([1; 20], 2)],
                b"payload".to_vec()
            )
            .await
        );
        drop(sp);
        let old = layout.secrets().unwrap().primary_key_id();

        assert!(crate::worker::rotate_key(&layout).unwrap());
        let secrets = layout.secrets().unwrap();
        assert_ne!(secrets.primary_key_id(), old);
        assert_eq!(secrets.key_count(), 1, "the old key is retired");
        assert!(!needs_reseal(&layout.data, worker, &secrets).unwrap());
        let mut spool = Spool::open(
            &layout.data.join(SPOOL_DIR),
            worker,
            Caps::default(),
            Arc::new(secrets),
        )
        .unwrap();
        let seg = spool.read(spool.segments()[0], unix_now()).unwrap();
        assert_eq!(seg.records.len(), 1);
        assert_eq!(seg.records[0].payload, b"payload");
    }
}
