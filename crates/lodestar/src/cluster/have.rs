//! The workers' have-filter, on the coordinator (architecture §19.3, §7;
//! plan WK-3 and Q6).
//!
//! **A separate worker filter** (Q6, the runner's answer under owner
//! §2.46): not the coordinator's own, which `capacity_for` sizes at twice
//! the stored count, but one sized to the stored count plus 25 %, at 10
//! bits per key up to 30M stored and 8 above ([`worker_params`]). It's
//! built only once a remote worker is connected, so a single-machine
//! install never holds one (§19.8), and rebuilt from the store when it
//! fills.
//!
//! - **Inserts:** the coordinator's `Have` tells this of every insert
//!   ([`HaveObserver`]); until a filter is built that's one atomic load.
//!   Each insert goes into the filter (and into the one being rebuilt) and
//!   into the next delta.
//! - **Snapshots:** every connected worker gets a header, then the chunks,
//!   [`WINDOW`] ahead of its acknowledgements, read from the live filter as
//!   they go. Bits only ever get set, and every insert after the header is
//!   in a delta that follows it, so the worker ends with everything stored
//!   before its snapshot.
//! - **Deltas** go out on every [`sync`](WorkerFilter::sync), numbered per
//!   generation, to every worker whose header went out. One that can't be
//!   queued restarts that worker's snapshot, since it would leave a gap.
//! - **A rebuild** bumps the generation and restarts every snapshot.
//! - **A worker's [`RESYNC`] request** (at most one per [`RESYNC_MIN`]),
//!   or no progress for [`STALL`], restarts its snapshot too.
//!
//! Insert and rebuild notifications go through here, never `writer.rs`.
//! WK-10 wires it in: [`Have::observe`] at start once workers are allowed,
//! `sync` on every cluster tick, `ack` from the inbound sink, and
//! [`rebuild`](WorkerFilter::rebuild) on a blocking thread over the store
//! when `sync` asks for it.

// WK-10 wires it into the serve tick.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use lodestar_cluster::have::{HaveDelta, MAX_DELTA_KEYS, MAX_FILTER_BITS, RESYNC, chunk_range};
use lodestar_cluster::proto::{SnapshotAck, SnapshotChunk, SnapshotKind, ToWorker};
use lodestar_cluster::{ClusterServer, FleetSnapshot, SendError, WorkerId, WorkerStatus};
use lodestar_metainfo::InfoHash;
use lodestar_pipeline::{Bloom, HaveObserver, new_key};

/// Chunks in flight per worker: 2 MiB of its 8 MiB session queue.
pub const WINDOW: u32 = 4;
/// A snapshot with no acknowledgement for this long starts over.
pub const STALL: Duration = Duration::from_secs(60);
/// A worker's resync requests start a new snapshot at most this often;
/// one that comes sooner waits for it, so a worker can't make the
/// coordinator resend its filter in a loop.
pub const RESYNC_MIN: Duration = Duration::from_secs(5);
/// Keys waiting for the next delta; past it (no sync for a long time),
/// they're dropped and every worker gets a fresh snapshot instead.
pub const MAX_WAITING: usize = 1 << 20;
/// Q6: 10 bits per key up to this many stored, 8 above.
pub const WIDE_UP_TO: u64 = 30_000_000;
const MIN_CAPACITY: u64 = 1 << 20;

/// The worker filter's capacity, bits per key and hashes for `stored`
/// keys (Q6): 25 % headroom; 10 bits and 7 hashes (~1 % false positives)
/// up to 30M stored, 8 bits and 6 hashes (~2 %) above. The capacity stops
/// where the filter would pass the largest a worker accepts.
///
/// `stored` is the number of stored *swarm hashes* (`Have::len()`, what
/// `for_each_swarm_hash` yields), not torrents: a hybrid torrent counts
/// twice.
pub fn worker_params(stored: u64) -> (u64, u64, u8) {
    let capacity = stored.saturating_add(stored / 4).max(MIN_CAPACITY);
    let (bits, hashes) = if stored <= WIDE_UP_TO {
        (10, 7)
    } else {
        (8, 6)
    };
    (capacity.min(MAX_FILTER_BITS / bits), bits, hashes)
}

/// A complete worker filter.
struct Filter {
    bloom: Arc<Bloom>,
    /// Built at the largest size a worker accepts: past its capacity it
    /// stays as it is, since a rebuild couldn't make it larger.
    at_cap: bool,
    generation: u64,
    /// The next delta's sequence number.
    next_delta: u64,
}

/// One worker's snapshot transfer.
#[derive(Debug, Clone, Copy)]
struct Transfer {
    serial: u64,
    generation: u64,
    /// Whether the header went out (deltas follow it from then on).
    started: bool,
    /// Chunks sent and acknowledged, in order.
    sent: u32,
    acked: u32,
    total: u32,
    created: Instant,
    progress: Instant,
    /// A resync was asked for within [`RESYNC_MIN`] of `created`.
    resync: bool,
}

/// What an insert touches. Its own small lock, which the writer's insert
/// path takes and nothing holds while sending, so a commit never waits on
/// the cluster server.
#[derive(Default)]
struct Inserts {
    current: Option<Arc<Bloom>>,
    /// The filter a rebuild is filling; inserts go into both.
    building: Option<Arc<Bloom>>,
    waiting: Vec<InfoHash>,
    /// `waiting` passed its cap and was dropped.
    overflowed: bool,
}

/// The transfers. Lock order: `state`, then `inserts`.
#[derive(Default)]
struct State {
    current: Option<Filter>,
    rebuilding: bool,
    transfers: BTreeMap<WorkerId, Transfer>,
    /// Serials and generations are numbered from the coordinator's clock,
    /// so a restarted coordinator's are newer.
    serials: u64,
    generations: u64,
}

/// What a [`sync`](WorkerFilter::sync) found.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct HaveSync {
    /// A remote worker is connected and there's no filter yet, or it's
    /// full: call [`rebuild`](WorkerFilter::rebuild).
    pub rebuild: bool,
    /// Workers whose queue was full; they're retried on the next sync.
    pub full: Vec<WorkerId>,
    /// The filter is at the largest size a worker accepts and past its
    /// capacity: no rebuild is asked for, and false positives climb. The
    /// caller logs it once (WK-10).
    pub at_cap: bool,
}

/// The workers' have-filter.
pub struct WorkerFilter {
    /// Set once a filter is being built: until then an insert costs this
    /// one load.
    active: AtomicBool,
    /// Chunks in flight per worker ([`WINDOW`]) and the delta buffer's cap
    /// ([`MAX_WAITING`]); smaller in tests.
    window: u32,
    max_waiting: usize,
    inserts: Mutex<Inserts>,
    state: Mutex<State>,
}

impl std::fmt::Debug for WorkerFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `try_lock`: formatting it while a lock is held must not hang.
        let mut d = f.debug_struct("WorkerFilter");
        match self.state.try_lock() {
            Ok(st) => d
                .field("generation", &st.current.as_ref().map(|c| c.generation))
                .field("bloom", &st.current.as_ref().map(|c| &c.bloom))
                .field("rebuilding", &st.rebuilding)
                .field("transfers", &st.transfers.len()),
            Err(_) => d.field("state", &"locked"),
        };
        d.finish()
    }
}

impl HaveObserver for WorkerFilter {
    fn inserted(&self, hash: &InfoHash) {
        if !self.active.load(Ordering::Acquire) {
            return;
        }
        let mut ins = self.inserts();
        if let Some(b) = &ins.building {
            b.insert(hash);
        }
        let Some(c) = &ins.current else { return };
        c.insert(hash);
        if ins.waiting.len() >= self.max_waiting {
            ins.waiting = Vec::new();
            ins.overflowed = true;
        } else if !ins.overflowed {
            ins.waiting.push(*hash);
        }
    }
}

impl WorkerFilter {
    pub fn new() -> WorkerFilter {
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        WorkerFilter::starting_at(ms)
    }

    /// Generations and transfer serials count up from `start`.
    pub fn starting_at(start: u64) -> WorkerFilter {
        WorkerFilter {
            active: AtomicBool::new(false),
            window: WINDOW,
            max_waiting: MAX_WAITING,
            inserts: Mutex::default(),
            state: Mutex::new(State {
                generations: start,
                serials: start.max(1),
                ..State::default()
            }),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("worker filter lock")
    }

    fn inserts(&self) -> MutexGuard<'_, Inserts> {
        self.inserts.lock().expect("worker filter inserts lock")
    }

    /// The generation workers are sent; `None` before the first build.
    pub fn generation(&self) -> Option<u64> {
        self.state().current.as_ref().map(|c| c.generation)
    }

    pub fn memory_bytes(&self) -> u64 {
        let ins = self.inserts();
        ins.current.as_ref().map_or(0, |b| b.memory_bytes())
            + ins.building.as_ref().map_or(0, |b| b.memory_bytes())
    }

    /// Builds the filter for `stored` swarm hashes (`Have::len()`, not
    /// torrents: a hybrid counts twice) from the store, swaps it in at a
    /// new generation and restarts every worker's snapshot. `scan` calls
    /// its argument for every stored swarm hash (`for_each_swarm_hash` on
    /// its own read connection). It blocks: run it on a blocking thread.
    ///
    /// Keys inserted while the scan runs go into the new filter as well,
    /// so a key the scan's read misses is still in it. On an error the old
    /// filter stays. `Ok(false)` if another rebuild is running.
    pub fn rebuild<E>(
        &self,
        stored: u64,
        scan: impl FnOnce(&mut dyn FnMut(InfoHash)) -> Result<(), E>,
    ) -> Result<bool, E> {
        {
            let mut st = self.state();
            if st.rebuilding {
                return Ok(false);
            }
            st.rebuilding = true;
        }
        // Never smaller than what the filter in use already holds.
        let held = self.inserts().current.as_ref().map_or(0, |b| b.len());
        let (capacity, bits, hashes) = worker_params(stored.max(held));
        let at_cap = capacity == MAX_FILTER_BITS / bits;
        let bloom = Arc::new(Bloom::with_params(capacity, bits, hashes, new_key()));
        self.inserts().building = Some(bloom.clone());
        // From here every insert reaches the new filter, before the scan
        // takes its read. The scan holds no lock while it reads the store.
        // Release/Acquire is enough: the `building` slot itself is behind
        // the inserts mutex, and a key the scan could miss was committed
        // after the scan's read began, which (through SQLite's own locks)
        // is after this store, so its insert sees `active`.
        self.active.store(true, Ordering::Release);
        let result = scan(&mut |h| bloom.insert(&h));
        let mut st = self.state();
        let mut ins = self.inserts();
        ins.building = None;
        st.rebuilding = false;
        if result.is_ok() {
            // Every worker starts over at the new generation, so what was
            // waiting for a delta of the old one is in the new filter and
            // goes with it.
            ins.current = Some(bloom.clone());
            ins.waiting.clear();
            ins.overflowed = false;
            st.generations += 1;
            st.current = Some(Filter {
                bloom,
                at_cap,
                generation: st.generations,
                next_delta: 0,
            });
            st.transfers.clear();
        }
        result.map(|()| true)
    }

    /// Restarts snapshots, sends headers, chunks and deltas to the
    /// connected remote workers in `fleet`, and says whether a rebuild is
    /// due.
    pub fn sync(&self, fleet: &FleetSnapshot, server: &ClusterServer, now: Instant) -> HaveSync {
        self.sync_with(&fleet.workers, now, |w, m| server.send(w, m))
    }

    /// A worker's acknowledgement (from the inbound sink): sends the next
    /// chunks, or a fresh snapshot for a [`RESYNC`] request.
    pub fn ack(&self, worker: WorkerId, ack: SnapshotAck, server: &ClusterServer, now: Instant) {
        self.ack_with(worker, ack, now, |w, m| server.send(w, m));
    }

    fn sync_with(
        &self,
        workers: &[WorkerStatus],
        now: Instant,
        mut send: impl FnMut(WorkerId, ToWorker) -> Result<(), SendError>,
    ) -> HaveSync {
        let remote: Vec<WorkerId> = workers
            .iter()
            .filter(|w| w.connected && w.worker != WorkerId::LOCAL)
            .map(|w| w.worker)
            .collect();
        let mut st = self.state();
        let full_at_cap = st
            .current
            .as_ref()
            .is_some_and(|c| c.at_cap && c.bloom.is_full());
        let rebuild = !remote.is_empty()
            && !st.rebuilding
            && st
                .current
                .as_ref()
                .is_none_or(|c| !c.at_cap && c.bloom.is_full());
        st.transfers.retain(|w, _| remote.contains(w));
        let Some(generation) = st.current.as_ref().map(|c| c.generation) else {
            return HaveSync {
                rebuild,
                full: Vec::new(),
                at_cap: false,
            };
        };
        // Taken before any header goes out below: a key inserted later is
        // in the bits those workers are sent, or in the next delta.
        let (waiting, overflowed) = {
            let mut ins = self.inserts();
            (
                std::mem::take(&mut ins.waiting),
                std::mem::take(&mut ins.overflowed),
            )
        };
        if overflowed {
            st.transfers.clear();
        }
        for w in &remote {
            let restart = st.transfers.get(w).is_none_or(|t| {
                (t.acked < t.total && now.duration_since(t.progress) > STALL)
                    || (t.resync && now.duration_since(t.created) >= RESYNC_MIN)
            });
            if restart {
                let t = new_transfer(&mut st, generation, now);
                st.transfers.insert(*w, t);
            }
        }
        let mut full = Vec::new();
        for w in &remote {
            if !pump(&mut st, *w, self.window, now, &mut send) {
                full.push(*w);
            }
        }
        // Deltas, to every worker whose header went out.
        let started: Vec<WorkerId> = st
            .transfers
            .iter()
            .filter(|(_, t)| t.started)
            .map(|(w, _)| *w)
            .collect();
        if started.is_empty() {
            return HaveSync {
                rebuild,
                full,
                at_cap: full_at_cap,
            };
        }
        for keys in waiting.chunks(MAX_DELTA_KEYS) {
            let c = st.current.as_mut().expect("current");
            let d = HaveDelta {
                generation,
                seq: c.next_delta,
                keys: keys.iter().map(|k| k.0).collect(),
            };
            c.next_delta = c.next_delta.wrapping_add(1);
            let msg = ToWorker::HaveDelta(d.encode().expect("a bounded, non-empty batch"));
            for w in &started {
                if st.transfers.get(w).is_some_and(|t| t.started) && send(*w, msg.clone()).is_err()
                {
                    // It would miss this delta: a fresh snapshot instead.
                    let t = new_transfer(&mut st, generation, now);
                    st.transfers.insert(*w, t);
                    if !full.contains(w) {
                        full.push(*w);
                    }
                }
            }
        }
        full.sort();
        HaveSync {
            rebuild,
            full,
            at_cap: full_at_cap,
        }
    }

    fn ack_with(
        &self,
        worker: WorkerId,
        ack: SnapshotAck,
        now: Instant,
        mut send: impl FnMut(WorkerId, ToWorker) -> Result<(), SendError>,
    ) {
        if ack.kind != SnapshotKind::HaveFilter || worker == WorkerId::LOCAL {
            return;
        }
        let mut st = self.state();
        let Some(generation) = st.current.as_ref().map(|c| c.generation) else {
            return;
        };
        if ack == RESYNC {
            if let Some(t) = st.transfers.get_mut(&worker)
                && now.duration_since(t.created) < RESYNC_MIN
            {
                // Too soon after the last start: the sync after
                // `RESYNC_MIN` restarts it.
                t.resync = true;
                return;
            }
            let t = new_transfer(&mut st, generation, now);
            st.transfers.insert(worker, t);
        } else {
            let Some(t) = st.transfers.get_mut(&worker) else {
                return;
            };
            // Only the next acknowledgement of this transfer counts; one of
            // an earlier transfer is ignored.
            if ack.version != t.serial || ack.seq != t.acked || t.acked >= t.sent {
                return;
            }
            t.acked += 1;
            t.progress = now;
        }
        pump(&mut st, worker, self.window, now, &mut send);
    }
}

impl Default for WorkerFilter {
    fn default() -> Self {
        WorkerFilter::new()
    }
}

fn new_transfer(st: &mut State, generation: u64, now: Instant) -> Transfer {
    st.serials = st.serials.wrapping_add(1).max(1);
    let total = st
        .current
        .as_ref()
        .map_or(0, |c| c.bloom.header(0, 1, 0).chunks);
    Transfer {
        serial: st.serials,
        generation,
        started: false,
        sent: 0,
        acked: 0,
        total,
        created: now,
        progress: now,
        resync: false,
    }
}

/// Sends a worker's header if it hasn't gone, then chunks up to the
/// window. `false` if its queue was full (retried on the next sync).
fn pump(
    st: &mut State,
    worker: WorkerId,
    window: u32,
    now: Instant,
    send: &mut impl FnMut(WorkerId, ToWorker) -> Result<(), SendError>,
) -> bool {
    let (Some(c), Some(t)) = (st.current.as_ref(), st.transfers.get_mut(&worker)) else {
        return true;
    };
    if !t.started {
        let h = c.bloom.header(t.generation, t.serial, c.next_delta);
        // `worker_params` keeps it in range; never send one a worker
        // would refuse.
        debug_assert!(h.check().is_ok());
        if h.check().is_err() {
            return true;
        }
        match send(worker, ToWorker::HaveSnapshot(h.encode())) {
            Ok(()) => {
                t.started = true;
                t.progress = now;
            }
            Err(SendError::Full) => return false,
            Err(_) => return true,
        }
    }
    while t.sent < t.total && t.sent < t.acked + window {
        let bytes = c.bloom.read_bytes(chunk_range(c.bloom.bit_len(), t.sent));
        let chunk = SnapshotChunk {
            kind: SnapshotKind::HaveFilter,
            version: t.serial,
            seq: t.sent,
            total: t.total,
            bytes,
        };
        match send(worker, ToWorker::Snapshot(chunk)) {
            Ok(()) => t.sent += 1,
            Err(SendError::Full) => return false,
            Err(_) => return true,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::sync::Arc;

    use lodestar_cluster::have::HaveHeader;
    use lodestar_cluster::proto::Stats;
    use lodestar_pipeline::Have;
    use proptest::prelude::*;

    use super::*;
    use crate::worker::have::WorkerHave;

    fn status(id: u32) -> WorkerStatus {
        WorkerStatus {
            worker: WorkerId(id),
            connected: true,
            session: None,
            draining: false,
            paused: false,
            offset_ms: 0,
            skewed: false,
            leases: 0,
            last_seen_ms: 0,
            stats: Stats::default(),
            external_ips: vec![],
        }
    }

    fn key(i: u64) -> InfoHash {
        let mut h = [0u8; 20];
        h[..8].copy_from_slice(&i.to_le_bytes());
        h[8..16].copy_from_slice(&i.wrapping_mul(0x9E37_79B9_7F4A_7C15).to_le_bytes());
        InfoHash(h)
    }

    /// A coordinator (its `Have`, the store, the worker filter) and
    /// workers behind lossy, reorder-free queues, as sessions are.
    struct Net {
        have: Arc<Have>,
        filter: Arc<WorkerFilter>,
        stored: Vec<InfoHash>,
        workers: Vec<WorkerStatus>,
        sides: HashMap<WorkerId, WorkerHave>,
        queues: HashMap<WorkerId, VecDeque<ToWorker>>,
        full: HashMap<WorkerId, bool>,
        /// Keys stored when each transfer's header went out.
        stored_at: HashMap<u64, usize>,
        /// The transfer each worker's filter came from.
        held: HashMap<WorkerId, u64>,
        now: Instant,
    }

    impl Net {
        fn new(workers: u32) -> Net {
            Net::with_window(workers, WINDOW)
        }

        /// A window of 1 makes chunks and deltas interleave.
        fn with_window(workers: u32, window: u32) -> Net {
            Net::with(workers, window, false, MAX_WAITING)
        }

        /// `in_place`: workers always drop their filter at a new header.
        fn with(workers: u32, window: u32, in_place: bool, max_waiting: usize) -> Net {
            let have = Arc::new(Have::new(1024));
            let mut filter = WorkerFilter::starting_at(100);
            filter.window = window;
            filter.max_waiting = max_waiting;
            let filter = Arc::new(filter);
            assert!(have.observe(filter.clone()));
            let ids: Vec<WorkerId> = (1..=workers).map(WorkerId).collect();
            Net {
                have,
                filter,
                stored: Vec::new(),
                workers: ids.iter().map(|w| status(w.0)).collect(),
                sides: ids
                    .iter()
                    .map(|w| {
                        let side = if in_place {
                            WorkerHave::in_place_above(0)
                        } else {
                            WorkerHave::new()
                        };
                        (*w, side)
                    })
                    .collect(),
                queues: ids.iter().map(|w| (*w, VecDeque::new())).collect(),
                full: ids.iter().map(|w| (*w, false)).collect(),
                stored_at: HashMap::new(),
                held: HashMap::new(),
                now: Instant::now(),
            }
        }

        fn store(&mut self, n: u64) {
            for _ in 0..n {
                let k = key(self.stored.len() as u64);
                self.stored.push(k);
                self.have.insert(&k);
            }
        }

        fn sender(
            queues: &mut HashMap<WorkerId, VecDeque<ToWorker>>,
            full: &HashMap<WorkerId, bool>,
            stored_at: &mut HashMap<u64, usize>,
            stored: usize,
        ) -> impl FnMut(WorkerId, ToWorker) -> Result<(), SendError> {
            move |w, m| {
                if full[&w] {
                    return Err(SendError::Full);
                }
                if let ToWorker::HaveSnapshot(b) = &m {
                    stored_at.insert(HaveHeader::decode(b).unwrap().serial, stored);
                }
                queues.get_mut(&w).unwrap().push_back(m);
                Ok(())
            }
        }

        fn sync(&mut self) -> HaveSync {
            let send = Self::sender(
                &mut self.queues,
                &self.full,
                &mut self.stored_at,
                self.stored.len(),
            );
            self.filter.sync_with(&self.workers, self.now, send)
        }

        fn rebuild(&mut self) {
            let stored = self.stored.clone();
            let built = self
                .filter
                .rebuild(stored.len() as u64, |insert| {
                    stored.iter().for_each(|k| insert(*k));
                    Ok::<(), ()>(())
                })
                .unwrap();
            assert!(built);
        }

        fn ack(&mut self, w: WorkerId, ack: SnapshotAck) {
            let send = Self::sender(
                &mut self.queues,
                &self.full,
                &mut self.stored_at,
                self.stored.len(),
            );
            self.filter.ack_with(w, ack, self.now, send);
        }

        /// Hands up to `n` queued messages to worker `w`.
        fn deliver(&mut self, w: WorkerId, n: usize) {
            for _ in 0..n {
                let Some(m) = self.queues.get_mut(&w).unwrap().pop_front() else {
                    return;
                };
                let side = &self.sides[&w];
                let ack = match m {
                    ToWorker::HaveSnapshot(b) => {
                        side.header(&b).unwrap();
                        None
                    }
                    ToWorker::Snapshot(c) => {
                        let ack = side.chunk(&c);
                        if ack.is_some_and(|a| a.version == c.version && c.seq + 1 == c.total) {
                            self.held.insert(w, c.version);
                        }
                        ack
                    }
                    ToWorker::HaveDelta(b) => side.delta(&b).1,
                    other => panic!("unexpected {other:?}"),
                };
                if let Some(a) = ack {
                    self.ack(w, a);
                }
            }
        }

        /// The session drops: what was queued is lost, and the worker asks
        /// for a fresh snapshot on the next `Welcome`.
        fn reconnect(&mut self, w: WorkerId) {
            self.queues.get_mut(&w).unwrap().clear();
            let a = self.sides[&w].reconnected();
            self.ack(w, a);
        }

        /// The gate: no worker says "not stored" for a key stored before
        /// the snapshot it holds.
        fn check(&self) {
            for (w, serial) in &self.held {
                let side = &self.sides[w];
                let before = self.stored_at[serial];
                if let Some(k) = self.stored[..before]
                    .iter()
                    .find(|k| !side.might_be_stored(k))
                {
                    panic!("{w:?} says {k:?} isn't stored (transfer {serial})");
                }
            }
        }

        /// Syncs and delivers until every worker is ready and a sync sends
        /// nothing. Time moves on, so deferred resyncs and stalls happen.
        fn settle(&mut self) {
            for _ in 0..1000 {
                self.now += RESYNC_MIN;
                if self.sync().rebuild {
                    self.rebuild();
                    continue;
                }
                let ready = self.sides.values().all(WorkerHave::is_ready);
                if ready && self.queues.values().all(VecDeque::is_empty) {
                    return;
                }
                let ws: Vec<WorkerId> = self.queues.keys().copied().collect();
                for w in &ws {
                    self.deliver(*w, usize::MAX);
                }
            }
            panic!("never settled");
        }
    }

    #[test]
    fn single_machine_builds_nothing() {
        let mut net = Net::new(0);
        net.store(1000);
        let s = net.sync();
        assert_eq!(s, HaveSync::default());
        assert_eq!(net.filter.memory_bytes(), 0);
        assert_eq!(net.filter.generation(), None);
        assert!(!net.filter.active.load(Ordering::Relaxed));
        assert!(net.filter.inserts().waiting.is_empty());
    }

    #[test]
    fn a_worker_gets_everything_stored() {
        let mut net = Net::new(2);
        net.store(500);
        assert!(net.sync().rebuild);
        net.rebuild();
        assert_eq!(net.filter.generation(), Some(101));
        net.store(300);
        net.settle();
        net.store(200);
        net.settle();
        for side in net.sides.values() {
            assert!(side.is_ready());
            assert_eq!(side.generation(), Some(101));
            assert!(net.stored.iter().all(|k| side.might_be_stored(k)));
            // A real "no" for keys never stored, at about 1%.
            let no = (1_000_000..1_010_000)
                .filter(|i| !side.might_be_stored(&key(*i)))
                .count();
            assert!(no > 9_800, "{no}");
        }
        // The worker filter is Q6's size: 2^20 keys at 10 bits.
        assert_eq!(net.filter.memory_bytes(), (1 << 20) * 10 / 8);
    }

    #[test]
    fn a_full_filter_is_rebuilt_and_sent_again() {
        let mut net = Net::new(1);
        net.store(10);
        net.settle();
        let first = net.filter.generation().unwrap();
        // Fill past capacity (2^20 at the minimum size).
        net.store((1 << 20) + (1 << 17));
        assert!(net.sync().rebuild);
        net.settle();
        let second = net.filter.generation().unwrap();
        assert!(second > first);
        let side = &net.sides[&WorkerId(1)];
        assert_eq!(side.generation(), Some(second));
        assert!(net.stored.iter().all(|k| side.might_be_stored(k)));
    }

    #[test]
    fn a_full_queue_restarts_the_snapshot_and_a_stall_too() {
        let mut net = Net::new(1);
        let w = WorkerId(1);
        net.store(10);
        net.sync();
        net.rebuild();
        net.full.insert(w, true);
        assert_eq!(net.sync().full, vec![w]);
        net.full.insert(w, false);
        net.sync();
        // Header and the first chunks, then nothing is acknowledged.
        let serial = net.filter.state().transfers[&w].serial;
        net.queues.get_mut(&w).unwrap().clear();
        net.now += STALL + Duration::from_secs(1);
        net.sync();
        assert!(net.filter.state().transfers[&w].serial > serial);
        net.store(5);
        net.settle();
        assert!(net.stored.iter().all(|k| net.sides[&w].might_be_stored(k)));
    }

    #[test]
    fn stale_acknowledgements_are_ignored() {
        let mut net = Net::new(1);
        let w = WorkerId(1);
        net.store(1);
        net.sync();
        net.rebuild();
        net.sync();
        let t = net.filter.state().transfers[&w];
        assert_eq!((t.sent, t.acked), (t.total.min(WINDOW), 0));
        net.ack(
            w,
            SnapshotAck {
                kind: SnapshotKind::HaveFilter,
                version: t.serial + 7,
                seq: 0,
            },
        );
        net.ack(
            w,
            SnapshotAck {
                kind: SnapshotKind::HaveFilter,
                version: t.serial,
                seq: 1,
            },
        );
        let after = net.filter.state().transfers[&w];
        assert_eq!((after.serial, after.acked), (t.serial, 0));
    }

    #[test]
    fn keys_stored_during_the_scan_are_kept() {
        let mut net = Net::new(1);
        net.store(100);
        net.sync();
        // The scan reads a snapshot of the store taken before these
        // commits; the writer inserts them while it runs.
        let read: Vec<InfoHash> = net.stored.clone();
        let later: Vec<InfoHash> = (100..150).map(key).collect();
        let have = &net.have;
        net.filter
            .rebuild(read.len() as u64, |insert| {
                for (i, k) in read.iter().enumerate() {
                    if i == 50 {
                        later.iter().for_each(|k| have.insert(k));
                    }
                    insert(*k);
                }
                Ok::<(), ()>(())
            })
            .unwrap();
        net.stored.extend(later);
        net.settle();
        let side = &net.sides[&WorkerId(1)];
        assert!(net.stored.iter().all(|k| side.might_be_stored(k)));
    }

    #[test]
    fn keys_stored_on_another_thread_during_a_rebuild_are_kept() {
        let mut net = Net::new(1);
        net.store(10);
        net.settle();
        let filter = net.filter.clone();
        // The store: the writer commits a key, then inserts it, as
        // `writer.rs` does; the scan reads what's committed when it runs.
        let store = Arc::new(std::sync::Mutex::new(net.stored.clone()));
        let writer = {
            let have = net.have.clone();
            let store = store.clone();
            std::thread::spawn(move || {
                for i in 10..20_000 {
                    store.lock().unwrap().push(key(i));
                    have.insert(&key(i));
                }
            })
        };
        filter
            .rebuild(10, |insert| {
                let read = store.lock().unwrap().clone();
                read.into_iter().for_each(&mut *insert);
                Ok::<(), ()>(())
            })
            .unwrap();
        writer.join().unwrap();
        net.stored.extend((10..20_000).map(key));
        net.settle();
        let side = &net.sides[&WorkerId(1)];
        assert!(net.stored.iter().all(|k| side.might_be_stored(k)));
    }

    #[test]
    fn a_failed_rebuild_keeps_the_old_filter() {
        let mut net = Net::new(1);
        net.store(10);
        net.settle();
        let generation = net.filter.generation();
        let r = net.filter.rebuild(10, |insert| {
            insert(key(0));
            Err("store")
        });
        assert_eq!(r, Err("store"));
        assert_eq!(net.filter.generation(), generation);
        assert!(!net.filter.state().rebuilding);
        assert!(net.filter.inserts().building.is_none());
        net.store(5);
        net.settle();
        assert!(
            net.stored
                .iter()
                .all(|k| net.sides[&WorkerId(1)].might_be_stored(k))
        );
        // And a second one at once is refused while one runs.
        net.filter.state().rebuilding = true;
        assert_eq!(net.filter.rebuild(1, |_| Ok::<(), ()>(())), Ok(false));
    }

    #[test]
    fn too_many_waiting_keys_restart_every_snapshot() {
        let mut net = Net::with(2, WINDOW, false, 100);
        net.store(10);
        net.settle();
        let serials: Vec<u64> = net
            .filter
            .state()
            .transfers
            .values()
            .map(|t| t.serial)
            .collect();
        net.store(150);
        assert!(net.filter.inserts().overflowed);
        assert!(net.filter.inserts().waiting.is_empty());
        net.sync();
        let after: Vec<u64> = net
            .filter
            .state()
            .transfers
            .values()
            .map(|t| t.serial)
            .collect();
        assert!(after.iter().zip(&serials).all(|(a, b)| a > b));
        net.settle();
        for side in net.sides.values() {
            assert!(net.stored.iter().all(|k| side.might_be_stored(k)));
        }
    }

    #[test]
    fn resync_requests_are_rate_limited() {
        let mut net = Net::new(1);
        let w = WorkerId(1);
        net.store(10);
        net.settle();
        let serial = net.filter.state().transfers[&w].serial;
        net.queues.get_mut(&w).unwrap().clear();
        // Long after the last start: at once.
        net.ack(w, RESYNC);
        let next = net.filter.state().transfers[&w].serial;
        assert!(next > serial);
        let queued = net.queues[&w].len();
        // Again straight away: deferred, nothing more sent.
        for _ in 0..10 {
            net.ack(w, RESYNC);
        }
        assert_eq!(net.filter.state().transfers[&w].serial, next);
        assert_eq!(net.queues[&w].len(), queued);
        net.sync();
        assert_eq!(net.filter.state().transfers[&w].serial, next);
        net.now += RESYNC_MIN;
        net.sync();
        assert!(net.filter.state().transfers[&w].serial > next);
    }

    #[test]
    fn a_filter_at_the_cap_is_never_rebuilt_in_a_loop() {
        let mut net = Net::new(1);
        net.store(10);
        net.settle();
        // Pretend the filter in use is already the largest a worker takes
        // and past its capacity.
        {
            let mut st = net.filter.state();
            st.current.as_mut().unwrap().at_cap = true;
        }
        net.store((1 << 20) + (1 << 17));
        let s = net.sync();
        assert!(!s.rebuild);
        assert!(s.at_cap);
        // Workers still finish their snapshot.
        net.settle();
        assert!(net.sides[&WorkerId(1)].is_ready());
        // A stored count past the cap gives a capped filter, flagged.
        let huge = MAX_FILTER_BITS; // keys
        let (cap, bits, _) = worker_params(huge);
        assert_eq!(cap * bits, MAX_FILTER_BITS);
    }

    #[test]
    fn a_rebuild_is_never_sized_below_what_the_filter_holds() {
        let mut net = Net::new(1);
        net.store(10);
        net.settle();
        net.store(2_000_000);
        // A caller that passes torrents, not swarm hashes, too few.
        net.filter
            .rebuild(5, |insert| {
                net.stored.iter().for_each(|k| insert(*k));
                Ok::<(), ()>(())
            })
            .unwrap();
        let held = net.filter.inserts().current.as_ref().unwrap().capacity();
        assert!(held >= 2_000_000, "{held}");
    }

    #[test]
    fn worker_params_follow_q6_and_stay_in_range() {
        assert_eq!(worker_params(0), (1 << 20, 10, 7));
        assert_eq!(worker_params(10_000_000), (12_500_000, 10, 7));
        assert_eq!(worker_params(30_000_000), (37_500_000, 10, 7));
        assert_eq!(worker_params(50_000_000), (62_500_000, 8, 6));
        let (cap, bits, _) = worker_params(u64::MAX / 2);
        assert!(cap * bits <= MAX_FILTER_BITS);
    }

    #[derive(Debug, Clone)]
    enum Op {
        Store(u64),
        Sync,
        Deliver(u32, usize),
        Full(u32, bool),
        Reconnect(u32),
        Rebuild,
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            3 => (1u64..400).prop_map(Op::Store),
            3 => Just(Op::Sync),
            4 => (1u32..=2, 1usize..4).prop_map(|(w, n)| Op::Deliver(w, n)),
            1 => (1u32..=2, any::<bool>()).prop_map(|(w, f)| Op::Full(w, f)),
            1 => (1u32..=2).prop_map(Op::Reconnect),
            1 => Just(Op::Rebuild),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(24))]

        /// Whatever arrives in whatever order a session allows (queues
        /// full, sessions lost, rebuilds mid-transfer), a worker never says
        /// "not stored" for a key stored before the snapshot it holds, and
        /// once things settle it has every key.
        #[test]
        fn never_a_wrong_no(
            ops in proptest::collection::vec(op(), 1..60),
            window in 1u32..=WINDOW,
            in_place in any::<bool>(),
        ) {
            let mut net = Net::with(2, window, in_place, MAX_WAITING);
            for op in ops {
                match op {
                    Op::Store(n) => net.store(n),
                    Op::Sync => {
                        if net.sync().rebuild {
                            net.rebuild();
                        }
                    }
                    Op::Deliver(w, n) => net.deliver(WorkerId(w), n),
                    Op::Full(w, f) => {
                        net.full.insert(WorkerId(w), f);
                    }
                    Op::Reconnect(w) => net.reconnect(WorkerId(w)),
                    Op::Rebuild => {
                        if net.filter.generation().is_some() {
                            net.rebuild();
                        }
                    }
                }
                net.check();
            }
            for f in net.full.values_mut() {
                *f = false;
            }
            net.settle();
            net.check();
            for side in net.sides.values() {
                prop_assert!(side.is_ready());
                prop_assert!(net.stored.iter().all(|k| side.might_be_stored(k)));
            }
        }
    }
}
