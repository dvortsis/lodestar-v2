//! What the coordinator sends a remote worker to judge and prepare with
//! (architecture §19.4, §19.6, §20; plan WK-8 and Q1): the filter
//! snapshot and the file-tree dictionary, each versioned and chunked
//! (`lodestar_pipeline::wire::filter_snapshot`).
//!
//! - **Built only for remote workers.** [`WorkerSnapshots::due`] is false
//!   while no remote worker is connected, so a single-machine install
//!   never reads, builds or sends either (§19.8).
//! - **Sent on change:** a new engine version, a new day (the wire key) or
//!   a new active dictionary makes a rebuild due. A rebuilt snapshot whose
//!   bytes didn't change keeps its version, so nothing is resent.
//! - **Paced by acknowledgements**, [`WINDOW`] chunks ahead per worker; a
//!   worker that makes no progress for [`STALL`] starts over.
//! - **Wire keys** ([`wire_key`]): a worker seals what the commit would
//!   hold back under a key derived per day from the coordinator's own seal
//!   key (HMAC-SHA256) and its own UTC day, never the worker's clock, so
//!   the coordinator can open a spooled record after a restart. It accepts
//!   today's key and yesterday's ([`KEY_DAYS`]), enough for a 24 h spool
//!   (§19.6).
//!
//! WK-10 wires it in: `due` and `install` on the cluster tick (the store
//! reads on a blocking thread), `sync` on every tick, `ack` from the
//! inbound sink, and the versions into placement (`set_versions`); the
//! intake opens sealed records with [`WorkerSnapshots::keys`].

// WK-10 wires it into the serve tick.
#![allow(dead_code)]

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use hmac::{Hmac, KeyInit, Mac};
use lodestar_cluster::proto::{SnapshotAck, SnapshotKind, ToWorker};
use lodestar_cluster::{ClusterServer, FleetSnapshot, SendError, WorkerId, WorkerStatus};
use lodestar_filter::lists::KIND_IP;
use lodestar_pipeline::StoreFilter;
use lodestar_pipeline::wire::filter_snapshot::{
    ContentList, DictSnapshot, FilterSnapshot, WireKey, WireKeys, chunk, chunk_count, version_of,
};
use lodestar_store::{FloorSource, Store};
use sha2::Sha256;

/// Chunks in flight per worker and kind: 2 MiB each, so 4 MiB of its
/// 8 MiB session queue while the filter and the dictionary both travel.
pub const WINDOW: u32 = 4;
/// A transfer with no acknowledgement for this long starts over.
pub const STALL: Duration = Duration::from_secs(60);
/// Earlier days whose wire keys still open a sealed record: the day
/// before, since a spool holds a record for at most 24 h (§19.6).
pub const KEY_DAYS: u32 = 1;

const DAY: i64 = 86_400;

/// The wire-key id for Unix time `now`: its day.
pub fn day_of(now: i64) -> u32 {
    u32::try_from(now.div_euclid(DAY)).unwrap_or(0)
}

/// Day `day`'s wire key: HMAC-SHA256 of the day under the coordinator's
/// seal key, in its own domain. The seal key never leaves the coordinator.
pub fn wire_key(seal_key: &[u8; 32], day: u32) -> WireKey {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(seal_key).expect("any key length");
    mac.update(b"lodestar-wire-v1");
    mac.update(&[0]);
    mac.update(&day.to_le_bytes());
    WireKey::new(day, mac.finalize().into_bytes().into())
}

/// The keys a sealed record may be opened with at `now`: today's and the
/// [`KEY_DAYS`] before it.
pub fn wire_keys(seal_key: &[u8; 32], now: i64) -> Vec<WireKey> {
    keys_for_day(seal_key, day_of(now))
}

fn keys_for_day(seal_key: &[u8; 32], today: u32) -> Vec<WireKey> {
    (today.saturating_sub(KEY_DAYS)..=today)
        .map(|d| wire_key(seal_key, d))
        .collect()
}

/// The subscribed content lists as `lodestar_filter::lists::apply` applies
/// them: every enabled list that isn't an IP list and has data, a
/// floor-marked one with the hashes its publisher removed but you haven't
/// (owner §2.7).
pub fn read_lists(store: &Store) -> Result<Vec<ContentList>, lodestar_store::Error> {
    let mut out = Vec::new();
    for s in store.subscriptions()? {
        if !s.enabled || s.kind == KIND_IP {
            continue;
        }
        let Some(data) = store.subscription_data(s.id, false)? else {
            continue;
        };
        let mut hashes: Vec<[u8; 20]> = data
            .chunks_exact(20)
            .map(|c| c.try_into().expect("20 bytes"))
            .collect();
        if s.floor {
            let mut seen: HashSet<[u8; 20]> = hashes.iter().copied().collect();
            for h in store.pending_floor_of(&FloorSource::List(s.id))? {
                if seen.insert(h) {
                    hashes.push(h);
                }
            }
        }
        out.push(ContentList {
            id: s.id as u32,
            floor: s.floor,
            action: s.action,
            hashes,
        });
    }
    Ok(out)
}

/// The snapshot of what the commit judges with now, sealed records to go
/// under today's wire key. Blocking: run it on a read connection, off the
/// async threads.
pub fn read_snapshot(
    store: &Store,
    seal_key: &[u8; 32],
    now: i64,
) -> Result<FilterSnapshot, lodestar_store::Error> {
    let mut filter = StoreFilter::read(store)?;
    // The commit refuses held keys; the worker never needs them.
    filter.state.held = Vec::new();
    Ok(FilterSnapshot {
        key: wire_key(seal_key, day_of(now)),
        filter,
        lists: read_lists(store)?,
    })
}

/// The active file-tree dictionary, if one has been trained.
pub fn read_dict(store: &Store) -> Result<Option<DictSnapshot>, lodestar_store::Error> {
    let Some(id) = store.active_file_dict() else {
        return Ok(None);
    };
    let bytes: Vec<u8> =
        store
            .connection()
            .query_row("SELECT data FROM zstd_dicts WHERE id = ?1", [id], |r| {
                r.get(0)
            })?;
    Ok(Some(DictSnapshot { id, bytes }))
}

/// One worker's transfer of the current payload.
#[derive(Debug, Clone, Copy)]
struct Transfer {
    version: u64,
    sent: u32,
    acked: u32,
    total: u32,
    progress: Instant,
}

/// One kind's current payload and its transfers.
struct Sender {
    kind: SnapshotKind,
    current: Option<(u64, Arc<Vec<u8>>)>,
    transfers: BTreeMap<WorkerId, Transfer>,
}

/// The version, its length and the workers; never the bytes, which for the
/// filter hold the wire key and every rule.
impl std::fmt::Debug for Sender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sender")
            .field("kind", &self.kind)
            .field("version", &self.version())
            .field("bytes", &self.current.as_ref().map(|c| c.1.len()))
            .field("transfers", &self.transfers)
            .finish()
    }
}

impl Sender {
    fn new(kind: SnapshotKind) -> Sender {
        Sender {
            kind,
            current: None,
            transfers: BTreeMap::new(),
        }
    }

    fn version(&self) -> u64 {
        self.current.as_ref().map_or(0, |c| c.0)
    }

    /// A new payload restarts every transfer; the same version changes
    /// nothing.
    fn set(&mut self, version: u64, bytes: Vec<u8>) {
        if self.version() == version {
            return;
        }
        self.current = Some((version, Arc::new(bytes)));
        self.transfers.clear();
    }

    /// Sends what each worker's window allows; returns false for a worker
    /// whose queue was full.
    fn pump(
        &mut self,
        w: WorkerId,
        now: Instant,
        send: &mut impl FnMut(WorkerId, ToWorker) -> Result<(), SendError>,
    ) -> bool {
        let Some((version, bytes)) = self.current.clone() else {
            return true;
        };
        let t = self.transfers.entry(w).or_insert(Transfer {
            version,
            sent: 0,
            acked: 0,
            total: chunk_count(bytes.len()),
            progress: now,
        });
        while t.sent < t.total && t.sent < t.acked + WINDOW {
            let c = chunk(self.kind, version, &bytes, t.sent);
            if send(w, ToWorker::Snapshot(c)).is_err() {
                return false;
            }
            t.sent += 1;
        }
        true
    }

    fn sync_with(
        &mut self,
        remote: &[WorkerId],
        now: Instant,
        send: &mut impl FnMut(WorkerId, ToWorker) -> Result<(), SendError>,
    ) -> Vec<WorkerId> {
        self.transfers.retain(|w, _| remote.contains(w));
        // A stalled transfer starts over from chunk 0.
        self.transfers
            .retain(|_, t| t.acked == t.total || now.duration_since(t.progress) <= STALL);
        let mut full = Vec::new();
        for w in remote {
            if !self.pump(*w, now, send) {
                full.push(*w);
            }
        }
        full
    }

    fn ack_with(
        &mut self,
        worker: WorkerId,
        ack: SnapshotAck,
        now: Instant,
        send: &mut impl FnMut(WorkerId, ToWorker) -> Result<(), SendError>,
    ) {
        let Some(t) = self.transfers.get_mut(&worker) else {
            return;
        };
        // Only the next unacknowledged chunk of this transfer counts.
        if ack.kind != self.kind || ack.version != t.version || ack.seq != t.acked {
            return;
        }
        if t.acked < t.sent {
            t.acked += 1;
            t.progress = now;
        }
        self.pump(worker, now, send);
    }
}

/// What [`WorkerSnapshots::install`] built from, to tell when it's due.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Built {
    engine: u64,
    day: u32,
    dict: Option<i64>,
}

/// The coordinator's snapshots for remote workers (see the module docs).
#[derive(Debug)]
pub struct WorkerSnapshots {
    filter: Sender,
    dict: Sender,
    built: Option<Built>,
    keys: Arc<WireKeys>,
}

impl Default for WorkerSnapshots {
    fn default() -> Self {
        WorkerSnapshots::new()
    }
}

impl WorkerSnapshots {
    pub fn new() -> WorkerSnapshots {
        WorkerSnapshots {
            filter: Sender::new(SnapshotKind::Filter),
            dict: Sender::new(SnapshotKind::Dictionary),
            built: None,
            keys: Arc::new(WireKeys::default()),
        }
    }

    /// The keys the intake opens sealed records with.
    pub fn keys(&self) -> Arc<WireKeys> {
        self.keys.clone()
    }

    /// The versions placement announces in `Assign` (0: none sent yet).
    pub fn versions(&self) -> (u64, u64) {
        (self.dict.version(), self.filter.version())
    }

    /// Whether to read and [`install`](Self::install) the snapshots now: a
    /// remote worker is connected, and the filter engine's version, the
    /// day or the active dictionary changed since the last build.
    pub fn due(&self, workers: &[WorkerStatus], engine: u64, now: i64, dict: Option<i64>) -> bool {
        !remote(workers).is_empty()
            && self.built
                != Some(Built {
                    engine,
                    day: day_of(now),
                    dict,
                })
    }

    /// Takes what was read: the filter snapshot under engine version
    /// `engine`, and the dictionary. The day it's built for is its wire
    /// key's (the day it was read), so one read at 23:59:59Z and installed
    /// after midnight is due again at once. Sets the wire keys for that day.
    /// Returns the (dictionary, filter) versions.
    ///
    /// **All or nothing:** both payloads are encoded before anything
    /// changes. If either fails (a store past the caps), workers keep the
    /// snapshots they have, the failed build is remembered so it isn't
    /// read again until something changes, and the error is returned this
    /// once, for the caller to log (it names a cap, never a rule).
    pub fn install(
        &mut self,
        engine: u64,
        seal_key: &[u8; 32],
        snapshot: &FilterSnapshot,
        dict: Option<&DictSnapshot>,
    ) -> Result<(u64, u64), lodestar_pipeline::wire::Error> {
        let day = snapshot.key.id;
        self.built = Some(Built {
            engine,
            day,
            dict: dict.map(|d| d.id),
        });
        if snapshot.key != wire_key(seal_key, day) {
            return Err(lodestar_pipeline::wire::Error::Invalid("wire key"));
        }
        let filter = snapshot.encode()?;
        let encoded = dict
            .map(|d| d.encode().map(|b| (d.version(), b)))
            .transpose()?;
        self.filter.set(version_of(&filter), filter);
        match encoded {
            Some((version, bytes)) => self.dict.set(version, bytes),
            // No active dictionary any more: stop announcing one.
            None => self.dict = Sender::new(SnapshotKind::Dictionary),
        }
        self.keys.set(keys_for_day(seal_key, day));
        Ok(self.versions())
    }

    /// Forgets `worker`'s transfers, so its next sync starts both snapshots
    /// from chunk 0. Call it when a worker's session starts (WK-10): a
    /// worker that restarted, or couldn't decode what it got, before the
    /// fleet saw it disconnect would otherwise never be sent them again.
    pub fn forget(&mut self, worker: WorkerId) {
        self.filter.transfers.remove(&worker);
        self.dict.transfers.remove(&worker);
    }

    /// Starts and continues transfers to the connected remote workers;
    /// returns those whose queue was full (retried on the next sync).
    pub fn sync(
        &mut self,
        fleet: &FleetSnapshot,
        server: &ClusterServer,
        now: Instant,
    ) -> Vec<WorkerId> {
        self.sync_with(&fleet.workers, now, &mut |w, m| server.send(w, m))
    }

    /// A worker's acknowledgement (from the inbound sink).
    pub fn ack(
        &mut self,
        worker: WorkerId,
        ack: SnapshotAck,
        server: &ClusterServer,
        now: Instant,
    ) {
        self.ack_with(worker, ack, now, &mut |w, m| server.send(w, m));
    }

    fn sync_with(
        &mut self,
        workers: &[WorkerStatus],
        now: Instant,
        send: &mut impl FnMut(WorkerId, ToWorker) -> Result<(), SendError>,
    ) -> Vec<WorkerId> {
        let remote = remote(workers);
        let mut full = self.dict.sync_with(&remote, now, send);
        for w in self.filter.sync_with(&remote, now, send) {
            if !full.contains(&w) {
                full.push(w);
            }
        }
        full.sort();
        full
    }

    fn ack_with(
        &mut self,
        worker: WorkerId,
        ack: SnapshotAck,
        now: Instant,
        send: &mut impl FnMut(WorkerId, ToWorker) -> Result<(), SendError>,
    ) {
        if worker == WorkerId::LOCAL {
            return;
        }
        match ack.kind {
            SnapshotKind::Filter => self.filter.ack_with(worker, ack, now, send),
            SnapshotKind::Dictionary => self.dict.ack_with(worker, ack, now, send),
            SnapshotKind::HaveFilter => {}
        }
    }
}

fn remote(workers: &[WorkerStatus]) -> Vec<WorkerId> {
    workers
        .iter()
        .filter(|w| w.connected && w.worker != WorkerId::LOCAL)
        .map(|w| w.worker)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_classify::ContentType;
    use lodestar_cluster::proto::Stats;
    use lodestar_cluster::proto::{MAX_CHUNK, SnapshotChunk};
    use lodestar_filter::lists::SharedIps;
    use lodestar_filter::{Action, CoreRule, Item, RuleRef};
    use lodestar_store::{NewSubscription, Refresh, StoreConfig};

    const SEAL: [u8; 32] = [5; 32];
    const NOW: i64 = 20_400 * DAY + 3_600;

    fn status(id: u32, connected: bool) -> WorkerStatus {
        WorkerStatus {
            worker: WorkerId(id),
            connected,
            draining: false,
            offset_ms: 0,
            skewed: false,
            leases: 0,
            last_seen_ms: 0,
            stats: Stats::default(),
            external_ips: vec![],
        }
    }

    #[test]
    fn wire_keys_follow_the_coordinators_day_and_seal_key() {
        let a = wire_key(&SEAL, 20_400);
        assert_eq!(a, wire_key(&SEAL, 20_400), "derived, not stored");
        assert_ne!(a, wire_key(&SEAL, 20_401));
        assert_ne!(a, wire_key(&[6; 32], 20_400));
        let ids: Vec<u32> = wire_keys(&SEAL, NOW).iter().map(|k| k.id).collect();
        assert_eq!(ids, [20_399, 20_400], "today and the day before");
        assert_eq!(day_of(-1), 0);
        assert!(!format!("{a:?}").contains("5, 5"));
    }

    /// Spooled records depend on the derivation staying the same across
    /// upgrades: HMAC-SHA256 under the seal key of the label, a zero byte
    /// and the day (little-endian u32), computed independently.
    #[test]
    fn the_wire_key_derivation_is_pinned() {
        let expected = [
            0x13, 0x56, 0x93, 0x9c, 0x6f, 0x03, 0xcb, 0xb3, 0x22, 0x0a, 0x49, 0xaa, 0x2e, 0xea,
            0x6d, 0x05, 0x64, 0x44, 0x80, 0x7c, 0x16, 0xec, 0x1c, 0x95, 0x79, 0x6a, 0x41, 0x85,
            0x04, 0x97, 0x32, 0x8b,
        ];
        assert_eq!(wire_key(&SEAL, 20_400), WireKey::new(20_400, expected));
    }

    /// A snapshot read before midnight and installed after it is built
    /// for its own day, so the new day's snapshot is due at once; one
    /// whose key isn't the seal key's for its day is refused.
    #[test]
    fn install_takes_its_day_from_the_snapshots_key() {
        let store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let last_second = 20_401 * DAY - 1;
        let snap = read_snapshot(&store, &SEAL, last_second).unwrap();
        let mut s = WorkerSnapshots::new();
        s.install(1, &SEAL, &snap, None).unwrap();
        let w = [status(3, true)];
        assert!(!s.due(&w, 1, last_second, None));
        assert!(s.due(&w, 1, last_second + 1, None), "the next day is due");
        assert_eq!(format!("{:?}", s.keys()), "WireKeys([20399, 20400])");
        let mut other = snap.clone();
        other.key = wire_key(&[6; 32], 20_400);
        assert!(s.install(1, &SEAL, &other, None).is_err());
    }

    /// A build that can't be encoded changes nothing workers have, and
    /// isn't due again until something changes.
    #[test]
    fn a_failed_install_keeps_what_workers_have() {
        let store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let snap = read_snapshot(&store, &SEAL, NOW).unwrap();
        let mut s = WorkerSnapshots::new();
        let dict = DictSnapshot {
            id: 7,
            bytes: vec![1, 2, 3],
        };
        let before = s.install(1, &SEAL, &snap, Some(&dict)).unwrap();
        let mut bad = snap.clone();
        bad.filter.state.cs_sigs = vec![0; lodestar_pipeline::wire::filter_snapshot::MAX_ROWS + 1];
        let bad_dict = DictSnapshot {
            id: 8,
            bytes: vec![],
        };
        assert!(s.install(2, &SEAL, &bad, Some(&bad_dict)).is_err());
        assert_eq!(s.versions(), before, "both payloads kept");
        let w = [status(3, true)];
        assert!(
            !s.due(&w, 2, NOW, Some(8)),
            "not read again for the same state"
        );
        assert!(s.due(&w, 3, NOW, Some(8)), "a real change is");
        // A dictionary that can't be encoded fails it too, filter unchanged.
        let zero = DictSnapshot {
            id: 0,
            bytes: vec![],
        };
        assert!(s.install(4, &SEAL, &snap, Some(&zero)).is_err());
        assert_eq!(s.versions(), before);
    }

    fn subscribe(store: &mut Store, url: &str, floor: bool, action: Action, hashes: &[[u8; 20]]) {
        let id = store
            .add_subscription(&NewSubscription {
                url,
                name: url,
                kind: 0,
                format: None,
                action: action.code(),
                floor,
                interval_secs: 3600,
            })
            .unwrap();
        let data = hashes.concat();
        store
            .record_refresh(
                id,
                1,
                &Refresh::Updated {
                    data: &data,
                    entries: hashes.len() as i64,
                    etag: None,
                    last_modified: None,
                },
            )
            .unwrap();
    }

    /// The worker's engine, built from the snapshot that went over the
    /// wire, decides as the server's own, built the server's way (the
    /// lists through `lists::apply`, the rest through `load_filter`).
    #[test]
    fn the_snapshot_holds_exactly_what_the_commit_judges_with() {
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        store.add_cs_hashes(&[[1; 20]], 0, 1).unwrap();
        store.add_cs_sig(77, 1).unwrap();
        store.set_core_switch("CS-5a", false).unwrap();
        store
            .add_rule("zebraquark", Action::Quarantine.code(), None, None)
            .unwrap();
        store
            .add_rule("cam", Action::Hide.code(), None, Some("year:<1990"))
            .unwrap();
        store.add_class_rule("studiox", true).unwrap();
        subscribe(
            &mut store,
            "https://a.example/floor",
            true,
            Action::Drop,
            &[[2; 20]],
        );
        subscribe(
            &mut store,
            "https://a.example/hide",
            false,
            Action::Hide,
            &[[3; 20]],
        );
        let server = lodestar_pipeline::default_filter();
        lodestar_filter::lists::apply(&store, &server, &SharedIps::default()).unwrap();
        let notes = lodestar_pipeline::load_filter(&server, &StoreFilter::read(&store).unwrap());
        assert_eq!(notes, Vec::<String>::new());

        let snap = read_snapshot(&store, &SEAL, NOW).unwrap();
        assert_eq!(snap.lists.len(), 2);
        let wire = snap.encode().unwrap();
        let (worker, notes) = FilterSnapshot::decode(&wire).unwrap().engine();
        assert!(notes.is_empty());
        let server = server.current();
        for h in [[1; 20], [2; 20], [3; 20], [4; 20]] {
            assert_eq!(
                worker.blocked_hash(&[h]),
                server.blocked_hash(&[h]),
                "{h:?}"
            );
        }
        assert_eq!(
            worker.blocked_hash(&[[2; 20]]).map(|d| d.rule),
            Some(RuleRef::Core(CoreRule::Cs1b))
        );
        assert!(!worker.core.switches.is_on(CoreRule::Cs5a));
        for (name, hashes) in [
            ("zebraquark one", [[9; 20]]),
            ("a cam rip", [[9; 20]]),
            ("studiox scene", [[9; 20]]),
            ("listed", [[3; 20]]),
            ("plain", [[9; 20]]),
        ] {
            let item = Item {
                hashes: &hashes,
                name,
                files: &[],
                content_type: ContentType::Other,
            };
            let (a, b) = (
                worker.judge(&item, None, NOW),
                server.judge(&item, None, NOW),
            );
            assert_eq!(a, b, "{name}");
        }
        let item = Item {
            hashes: &[[9; 20]],
            name: "zebraquark one",
            files: &[],
            content_type: ContentType::Other,
        };
        assert_eq!(
            worker.judge(&item, None, NOW).decision.map(|d| d.action),
            Some(Action::Quarantine)
        );
        assert_eq!(
            worker.class_correction(
                &Item {
                    name: "studiox scene",
                    ..item
                },
                None
            ),
            Some(true)
        );
        // The same store gives the same bytes, so nothing is resent.
        let again = read_snapshot(&store, &SEAL, NOW).unwrap().encode().unwrap();
        assert_eq!(version_of(&again), version_of(&wire));
        // The held set never leaves the coordinator.
        assert!(snap.filter.state.held.is_empty());
    }

    #[test]
    fn the_dictionary_is_read_from_the_store() {
        let store = Store::open_in_memory(StoreConfig::default()).unwrap();
        assert_eq!(read_dict(&store).unwrap(), None);
        // A store whose active dictionary is set, as training leaves it.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db.sqlite");
        let store = Store::open(&path, StoreConfig::default()).unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO zstd_dicts (id, kind, created_at, samples, data)
                 VALUES (7, 'file_tree', 1, 400, x'0102030405')",
                [],
            )
            .unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO settings (key, value) VALUES ('file_dict', 7)",
                [],
            )
            .unwrap();
        drop(store);
        let store = Store::open(&path, StoreConfig::default()).unwrap();
        let d = read_dict(&store).unwrap().unwrap();
        assert_eq!((d.id, d.bytes.as_slice()), (7, &[1u8, 2, 3, 4, 5][..]));
        let mut s = WorkerSnapshots::new();
        let snap = read_snapshot(&store, &SEAL, NOW).unwrap();
        assert_eq!(s.install(1, &SEAL, &snap, Some(&d)).unwrap().0, 7);
        // It goes out like the filter, and stops being announced once
        // there's no active dictionary.
        let mut sent = Sent::new();
        s.sync_with(
            &[status(3, true)],
            Instant::now(),
            &mut sink(&mut sent, None),
        );
        assert!(
            sent.iter()
                .any(|(_, c)| c.kind == SnapshotKind::Dictionary && c.version == 7)
        );
        s.install(2, &SEAL, &snap, None).unwrap();
        assert_eq!(s.versions().0, 0);
    }

    /// Neither side's `Debug` shows the snapshot's bytes: the wire key and
    /// the rules.
    #[test]
    fn debug_never_shows_the_key_or_a_rule() {
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        store
            .add_rule("zebraquark", Action::Quarantine.code(), None, None)
            .unwrap();
        let snap = read_snapshot(&store, &SEAL, NOW).unwrap();
        let mut s = WorkerSnapshots::new();
        s.install(1, &SEAL, &snap, None).unwrap();
        let mut sent = Sent::new();
        s.sync_with(
            &[status(3, true)],
            Instant::now(),
            &mut sink(&mut sent, None),
        );
        let mut inc = lodestar_pipeline::wire::filter_snapshot::Incoming::new(SnapshotKind::Filter);
        let mut part = sent[0].1.clone();
        part.total = 2;
        part.bytes.resize(MAX_CHUNK, 0);
        inc.take(part).unwrap();
        let key = wire_key(&SEAL, day_of(NOW));
        let text = format!(
            "{s:?} {inc:?} {key:?} {:?}",
            ToWorker::Snapshot(sent[0].1.clone())
        );
        assert!(!text.contains("zebraquark"), "{text}");
        // The key's bytes, as Debug would print a byte list.
        let bytes = &snap.encode().unwrap()[4..36];
        let list = format!("{:?}", &bytes[..4]);
        assert!(!text.contains(list.trim_matches(['[', ']'])), "{text}");
    }

    type Sent = Vec<(WorkerId, SnapshotChunk)>;

    fn sink(
        sent: &mut Sent,
        full: Option<WorkerId>,
    ) -> impl FnMut(WorkerId, ToWorker) -> Result<(), SendError> + '_ {
        move |w, m| {
            if Some(w) == full {
                return Err(SendError::Full);
            }
            let ToWorker::Snapshot(c) = m else {
                panic!("not a snapshot");
            };
            sent.push((w, c));
            Ok(())
        }
    }

    fn install(s: &mut WorkerSnapshots, rows: usize) -> u64 {
        let mut snap = FilterSnapshot::decode(
            &FilterSnapshot {
                key: wire_key(&SEAL, day_of(NOW)),
                filter: StoreFilter::read(&Store::open_in_memory(StoreConfig::default()).unwrap())
                    .unwrap(),
                lists: Vec::new(),
            }
            .encode()
            .unwrap(),
        )
        .unwrap();
        snap.lists = vec![ContentList {
            id: 1,
            floor: false,
            action: Action::Drop.code(),
            hashes: (0..rows as u32)
                .map(|i| {
                    let mut h = [0u8; 20];
                    h[..4].copy_from_slice(&i.to_le_bytes());
                    h
                })
                .collect(),
        }];
        s.install(1, &SEAL, &snap, None).unwrap().1
    }

    /// Nothing is built or sent while no remote worker is connected.
    #[test]
    fn a_single_machine_builds_and_sends_nothing() {
        let mut s = WorkerSnapshots::new();
        let none = [status(0, true), status(3, false)];
        assert!(!s.due(&none, 1, NOW, None));
        assert!(s.due(&[status(3, true)], 1, NOW, None));
        let mut sent = Sent::new();
        assert!(
            s.sync_with(&none, Instant::now(), &mut sink(&mut sent, None))
                .is_empty()
        );
        assert!(sent.is_empty());
        assert_eq!(s.versions(), (0, 0));
        assert_eq!(format!("{:?}", s.keys()), "WireKeys([])");
    }

    /// Chunks go out a window ahead of the acknowledgements; a stale or
    /// repeated ack moves nothing; a stall starts over; a full queue is
    /// reported and retried.
    #[test]
    fn transfers_are_paced_by_acknowledgements() {
        let mut s = WorkerSnapshots::new();
        // About 6.3 chunks of listed hashes.
        let version = install(&mut s, 6 * MAX_CHUNK / 20 + 3_000);
        assert_eq!(s.versions(), (0, version));
        assert!(
            !s.due(&[status(3, true)], 1, NOW, None),
            "built for this engine and day"
        );
        assert!(s.due(&[status(3, true)], 2, NOW, None), "a new engine");
        assert!(s.due(&[status(3, true)], 1, NOW + DAY, None), "a new day");
        assert_eq!(format!("{:?}", s.keys()), "WireKeys([20399, 20400])");
        let workers = [status(3, true)];
        let t0 = Instant::now();
        let mut sent = Sent::new();
        assert!(
            s.sync_with(&workers, t0, &mut sink(&mut sent, None))
                .is_empty()
        );
        assert_eq!(sent.len(), WINDOW as usize);
        assert!(
            sent.iter()
                .all(|(_, c)| c.version == version && c.total == 7)
        );
        // Syncing again sends nothing new.
        s.sync_with(&workers, t0, &mut sink(&mut sent, None));
        assert_eq!(sent.len(), WINDOW as usize);
        let ack = |seq| SnapshotAck {
            kind: SnapshotKind::Filter,
            version,
            seq,
        };
        // A stale ack, another kind, another version: nothing.
        for a in [
            ack(1),
            SnapshotAck {
                kind: SnapshotKind::Dictionary,
                ..ack(0)
            },
            SnapshotAck {
                version: 9,
                ..ack(0)
            },
        ] {
            s.ack_with(WorkerId(3), a, t0, &mut sink(&mut sent, None));
        }
        assert_eq!(sent.len(), WINDOW as usize);
        s.ack_with(WorkerId(3), ack(0), t0, &mut sink(&mut sent, None));
        assert_eq!(sent.len(), WINDOW as usize + 1);
        assert_eq!(sent.last().unwrap().1.seq, WINDOW);
        s.ack_with(WorkerId(3), ack(0), t0, &mut sink(&mut sent, None));
        assert_eq!(
            sent.len(),
            WINDOW as usize + 1,
            "a repeated ack moves nothing"
        );
        // No progress for longer than STALL: the transfer starts over.
        sent.clear();
        s.sync_with(
            &workers,
            t0 + STALL + Duration::from_secs(1),
            &mut sink(&mut sent, None),
        );
        assert_eq!(sent.first().map(|(_, c)| c.seq), Some(0));
        // A full queue is reported.
        let two = [status(3, true), status(4, true)];
        let full = s.sync_with(&two, t0, &mut sink(&mut sent, Some(WorkerId(4))));
        assert_eq!(full, vec![WorkerId(4)]);
        // A worker whose session starts again gets both from chunk 0.
        s.forget(WorkerId(3));
        sent.clear();
        s.sync_with(&workers, t0, &mut sink(&mut sent, None));
        assert_eq!(sent.first().map(|(_, c)| c.seq), Some(0));
        // The built-in worker is never sent anything, nor its acks taken.
        s.ack_with(WorkerId::LOCAL, ack(0), t0, &mut sink(&mut sent, None));
        // The same content keeps its version and its transfers.
        let before = sent.len();
        assert_eq!(install(&mut s, 6 * MAX_CHUNK / 20 + 3_000), version);
        s.sync_with(&workers, t0, &mut sink(&mut sent, None));
        assert_eq!(sent.len(), before, "nothing resent for the same bytes");
    }

    /// The snapshot's size at the store's defaults, and with large rule
    /// sets (reported in `docs/decisions/2026-11-07-WK-8.md`).
    #[test]
    fn snapshot_sizes() {
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let empty = read_snapshot(&store, &SEAL, NOW).unwrap().encode().unwrap();
        for i in 0..10_000 {
            store
                .add_rule(
                    &format!("name:\"release group {i}\" size:>1GB"),
                    Action::Hide.code(),
                    None,
                    None,
                )
                .unwrap();
        }
        let rules = read_snapshot(&store, &SEAL, NOW).unwrap().encode().unwrap();
        let hashes: Vec<[u8; 20]> = (0..100_000u32)
            .map(|i| {
                let mut h = [0u8; 20];
                h[..4].copy_from_slice(&i.to_le_bytes());
                h
            })
            .collect();
        store.add_cs_hashes(&hashes, 1, 1).unwrap();
        let both = read_snapshot(&store, &SEAL, NOW).unwrap().encode().unwrap();
        eprintln!(
            "snapshot bytes: defaults {}, 10k rules {}, + 100k CS-1 entries {} ({} chunks)",
            empty.len(),
            rules.len(),
            both.len(),
            chunk_count(both.len())
        );
        assert!(empty.len() < 1024, "{}", empty.len());
        assert!(rules.len() < 1 << 20, "{}", rules.len());
        assert!(both.len() < 4 << 20, "{}", both.len());
    }
}
