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

// `lodestar worker` sets `runtime::Config::spool` from this; that line is in
// `cli/worker.rs`, which another slice owns (see the decisions file).
#[cfg_attr(not(test), allow(dead_code))]
impl SpoolConfig {
    /// `<data>/spool`, sealed with `<data>/secrets.key`, at the default
    /// caps (1 GB, 24 h).
    pub fn for_layout(layout: &Layout) -> Result<SpoolConfig, ClusterError> {
        Ok(SpoolConfig {
            dir: layout.data.join(SPOOL_DIR),
            caps: Caps::default(),
            sealer: Arc::new(layout.secrets()?),
        })
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The spool, shared by the runtime's tasks.
#[derive(Debug)]
pub struct WorkerSpool {
    spool: Arc<Mutex<Spool>>,
    stats: Arc<SpoolStats>,
    full: AtomicBool,
    /// Write and read failures (the disk, not the format).
    pub errors: AtomicU64,
    /// Lease-count drift seen after a replay: the difference between the
    /// worker's leases and the coordinator's, each time they differed.
    pub drift: AtomicU64,
    /// A replay drained since the last comparison.
    compare: AtomicBool,
}

/// One spooled record being replayed: its segment and position.
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

/// The segment being replayed: which of its records are accepted.
#[derive(Debug, Default)]
pub struct Replay {
    current: Option<(u64, Vec<bool>)>,
    /// Records accepted from segments read since (kept across sessions).
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

    /// Marks `from` accepted; `Some((seq, records))` once its whole
    /// segment is, for removal.
    pub fn accepted(&mut self, from: Origin) -> Option<(u64, u64)> {
        let (seq, done) = self.current.as_mut()?;
        if *seq != from.seq {
            return None;
        }
        if let Some(d) = done.get_mut(from.index) {
            *d = true;
        }
        if done.iter().all(|&d| d) {
            let n = done.len() as u64;
            let seq = *seq;
            self.current = None;
            self.accepted.remove(&seq);
            return Some((seq, n));
        }
        None
    }
}

impl WorkerSpool {
    /// Opens the spool for `worker` (blocking: lists the folder).
    pub fn open(config: &SpoolConfig, worker: u32) -> Result<WorkerSpool, SpoolError> {
        let spool = Spool::open(&config.dir, worker, config.caps, config.sealer.clone())?;
        let stats = spool.stats();
        Ok(WorkerSpool {
            spool: Arc::new(Mutex::new(spool)),
            stats,
            full: AtomicBool::new(false),
            errors: AtomicU64::new(0),
            drift: AtomicU64::new(0),
            compare: AtomicBool::new(false),
        })
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn stats(&self) -> &SpoolStats {
        &self.stats
    }

    /// Bytes on disk now (the heartbeat's `spool_bytes`).
    pub fn bytes(&self) -> u64 {
        self.stats.bytes.load(Ordering::Relaxed)
    }

    /// Whether the last write found the spool full (fetching pauses).
    pub fn is_full(&self) -> bool {
        self.full.load(Ordering::Acquire)
    }

    async fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Spool) -> T + Send + 'static,
    ) -> Option<T> {
        let spool = self.spool.clone();
        tokio::task::spawn_blocking(move || f(&mut lock(&spool)))
            .await
            .ok()
    }

    /// Writes one batch. `false` if it wasn't (full, or the disk failed):
    /// the batch stays in memory and is sent once a session is up.
    pub async fn put(
        &self,
        kind: Kind,
        batch: u64,
        leases: Vec<([u8; 20], u64)>,
        payload: Vec<u8>,
    ) -> bool {
        let record = Record {
            kind,
            batch,
            at: unix_now(),
            leases,
            payload,
        };
        let r = self.blocking(move |s| s.append(&record, record.at)).await;
        match r {
            Some(Ok(())) => true,
            Some(Err(SpoolError::Full)) => {
                self.full.store(true, Ordering::Release);
                false
            }
            _ => {
                self.errors.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    /// Syncs what was appended (the end of each round of writes).
    pub async fn sync(&self) {
        if !matches!(self.blocking(|s| s.sync()).await, Some(Ok(()))) {
            self.errors.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The next segment's records to replay, oldest first, starting it in
    /// `replay`; `None` when nothing is spooled. Segments wholly past the
    /// age cap, and segments with nothing left to send, are removed on
    /// the way. Damage is counted (`dropped_corrupt`) and named by segment
    /// only, never by its bytes.
    pub async fn next(&self, replay: &mut Replay) -> Vec<Replayed> {
        loop {
            let now = unix_now();
            let read = self
                .blocking(move |s| {
                    if s.segments().is_empty() {
                        s.close_active()?;
                    }
                    let Some(seq) = s.segments().first().copied() else {
                        return Ok(None);
                    };
                    if s.is_expired(seq, now) {
                        let seg = s.read(seq, now)?;
                        s.remove(seq, seg.records.len() as u64)?;
                        return Ok(Some((seq, Vec::new(), Vec::new())));
                    }
                    let seg = s.read(seq, now)?;
                    Ok::<_, SpoolError>(Some((seq, seg.records, seg.damage)))
                })
                .await;
            let (seq, records, _damage) = match read {
                Some(Ok(Some(r))) => r,
                Some(Ok(None)) => return Vec::new(),
                _ => {
                    self.errors.fetch_add(1, Ordering::Relaxed);
                    return Vec::new();
                }
            };
            let done = replay
                .accepted
                .remove(&seq)
                .filter(|d| d.len() == records.len())
                .unwrap_or_else(|| vec![false; records.len()]);
            let out: Vec<Replayed> = records
                .into_iter()
                .enumerate()
                .filter(|(i, _)| !done[*i])
                .map(|(index, r)| Replayed {
                    from: Origin { seq, index },
                    kind: r.kind,
                    leases: r.leases,
                    payload: r.payload,
                })
                .collect();
            if out.is_empty() {
                self.remove(seq, done.len() as u64).await;
                continue;
            }
            replay.current = Some((seq, done));
            return out;
        }
    }

    /// Removes a segment securely once all of it is accepted.
    pub async fn remove(&self, seq: u64, records: u64) {
        let r = self
            .blocking(move |s| {
                s.remove(seq, records)?;
                // Half empty again (so a spool near its cap doesn't flap).
                Ok::<_, SpoolError>(s.bytes() <= s.caps().max_bytes() / 2)
            })
            .await;
        match r {
            Some(Ok(room)) => {
                if room {
                    self.full.store(false, Ordering::Release);
                }
                if self.bytes() == 0 {
                    self.compare.store(true, Ordering::Release);
                }
            }
            _ => {
                self.errors.fetch_add(1, Ordering::Relaxed);
            }
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
        let bytes =
            std::fs::read(&path).map_err(|e| ClusterError::Io("read the spool", e.kind()))?;
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
        DEADLINE, MapFetch, W, assignment, config, filter_chunks, info, key_of, next_results,
        sealed, session_pair, until,
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
        sp.stats().records.load(Ordering::Relaxed)
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
        accept(&mut coord, &mut session, &mut rt, retried.batch, true).await;
        until("removed", || (sp.bytes() == 0, sp.bytes())).await;
        assert_eq!(c.committed.load(Ordering::Relaxed), 2, "each once");
        // Drained: the session compares lease counts once, and a
        // difference is counted (this runtime holds none).
        assert_eq!(rt.reconcile(), Some(0));
        assert_eq!(rt.reconcile(), None);
        rt.reconciled(0, 3);
        assert_eq!(sp.drift.load(Ordering::Relaxed), 3);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
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
        let spool = Spool::open(dir.path(), W.0, Caps::default(), sealer.clone()).unwrap();
        let segs = spool.segments();
        assert_eq!(segs.len(), 1);
        let bytes = std::fs::read(dir.path().join(format!("{:016x}.lsp", segs[0]))).unwrap();
        assert!(!has(&bytes), "the segment names the peer");
        let seg = spool.read(segs[0], unix_now()).unwrap();
        assert_eq!(seg.records.len(), 1);
        for r in &seg.records {
            assert!(!has(&r.encode().unwrap()), "a record names the peer");
            assert_eq!(r.leases, vec![(key_of(&raw).0, 9)]);
        }
    }

    #[test]
    fn a_reconcile_counts_the_drift() {
        let dir = tempfile::tempdir().unwrap();
        let config = SpoolConfig {
            dir: dir.path().to_path_buf(),
            caps: Caps::default(),
            sealer: Arc::new(SecretStore::from_key(&[4; 32])),
        };
        let sp = WorkerSpool::open(&config, 1).unwrap();
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
        let sp = WorkerSpool::open(&config, worker).unwrap();
        assert!(
            sp.put(Kind::Results, 1, vec![([1; 20], 2)], b"payload".to_vec())
                .await
        );
        drop(sp);
        let old = layout.secrets().unwrap().primary_key_id();

        assert!(crate::worker::rotate_key(&layout).unwrap());
        let secrets = layout.secrets().unwrap();
        assert_ne!(secrets.primary_key_id(), old);
        assert_eq!(secrets.key_count(), 1, "the old key is retired");
        assert!(!needs_reseal(&layout.data, worker, &secrets).unwrap());
        let spool = Spool::open(
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
