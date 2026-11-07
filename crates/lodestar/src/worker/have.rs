//! The worker's copy of the have-filter (architecture §19.3, §7; plan WK-3
//! and Q6).
//!
//! It answers "might this hash be stored?" for what the worker's DHT finds,
//! so rediscoveries aren't claimed. The coordinator sends a snapshot (a
//! header, then chunks in order) and deltas (batches of inserts); this
//! assembles them and swaps the finished filter in.
//!
//! - **Never a wrong "no".** Until a snapshot is complete there is no
//!   filter, and the answer is "maybe": the claim goes to the coordinator,
//!   which answers from the store. A delta is ORed into the filter being
//!   assembled as well as into the one in use, so nothing stored before or
//!   during a snapshot is missed.
//! - **The old filter stays until the new one is complete,** unless holding
//!   both would take more than [`IN_PLACE_ABOVE`]: then the old one goes
//!   first, and the worker answers "maybe" until the new one is complete.
//! - **Nothing half-built is used.** A chunk out of order (one missing or
//!   repeated) drops the assembly. A gap in the deltas, or a new session,
//!   means something was lost; the answer is a [`RESYNC`] request, and the
//!   coordinator sends a fresh snapshot.
//!
//! WK-10 wires it into the worker runtime: it hands over `HaveSnapshot`,
//! `Snapshot` chunks of the have-filter kind and `HaveDelta`, sends back
//! the acknowledgements returned, calls [`WorkerHave::reconnected`] on
//! every `Welcome` and [`WorkerHave::poll`] on every tick. All of these
//! run from the session's one message loop, in order: a chunk handled
//! while `header` is still allocating its filter would be dropped.

// WK-10 wires it into the runtime.
#![allow(dead_code)]

use std::sync::RwLock;
use std::time::{Duration, Instant};

use lodestar_cluster::have::{Assembly, HaveDelta, HaveError, HaveHeader, RESYNC, filter_bytes};
use lodestar_cluster::proto::{SnapshotAck, SnapshotChunk};
use lodestar_metainfo::InfoHash;
use lodestar_pipeline::Bloom;

/// Above this, a worker swaps its filter in place (plan Q6): 64 MiB
/// (mebibytes, 2^26 bytes; Q6 says "64 MB") for the old and new filters
/// together.
pub const IN_PLACE_ABOVE: u64 = 64 << 20;
/// A resync request with no header after this long is sent again: the
/// first may have been lost with a session (`poll`).
pub const RESYNC_RETRY: Duration = Duration::from_secs(30);

/// A complete filter and where its deltas are.
#[derive(Debug)]
struct Held {
    bloom: Bloom,
    generation: u64,
    /// The next delta sequence number expected.
    next_delta: u64,
}

#[derive(Debug)]
struct Pending {
    /// `None` refills the filter in use: a snapshot of the same filter
    /// (a resync) ORs into it, with no second copy and no "maybe" window.
    bloom: Option<Bloom>,
    assembly: Assembly,
    next_delta: u64,
}

#[derive(Debug, Default)]
struct State {
    current: Option<Held>,
    pending: Option<Pending>,
    /// A resync was asked for and no new header has come since: don't ask
    /// again for every message that follows.
    resync_asked: bool,
    /// When `poll` first saw `resync_asked`; the retry clock.
    asked_at: Option<Instant>,
    refused: u64,
}

/// What a delta did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// Into the filter in use, the one assembling, or both.
    Inserted,
    /// Of a generation this worker isn't holding or assembling, or
    /// already in the snapshot: ignored.
    Stale,
    /// A delta is missing: a resync is due.
    Gap,
}

/// The worker's have-filter.
#[derive(Debug)]
pub struct WorkerHave {
    state: RwLock<State>,
    /// [`IN_PLACE_ABOVE`]; smaller in tests.
    in_place_above: u64,
}

impl Default for WorkerHave {
    fn default() -> Self {
        WorkerHave::new()
    }
}

impl WorkerHave {
    pub fn new() -> WorkerHave {
        WorkerHave::in_place_above(IN_PLACE_ABOVE)
    }

    /// Swaps in place when the old and new filters together pass `bytes`.
    pub fn in_place_above(bytes: u64) -> WorkerHave {
        WorkerHave {
            state: RwLock::default(),
            in_place_above: bytes,
        }
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, State> {
        self.state.read().expect("worker have lock")
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, State> {
        self.state.write().expect("worker have lock")
    }

    /// `false` only if the hash is surely not stored. With no complete
    /// filter, always `true`.
    pub fn might_be_stored(&self, hash: &InfoHash) -> bool {
        match &self.read().current {
            Some(h) => h.bloom.contains(hash),
            None => true,
        }
    }

    /// Whether a complete filter is in use.
    pub fn is_ready(&self) -> bool {
        self.read().current.is_some()
    }

    /// The generation in use.
    pub fn generation(&self) -> Option<u64> {
        self.read().current.as_ref().map(|h| h.generation)
    }

    /// Bytes held: the filter in use and the one assembling.
    pub fn memory_bytes(&self) -> u64 {
        let st = self.read();
        st.current.as_ref().map_or(0, |h| h.bloom.memory_bytes())
            + st.pending
                .as_ref()
                .and_then(|p| p.bloom.as_ref())
                .map_or(0, Bloom::memory_bytes)
    }

    /// Messages refused (malformed, out of order, gaps).
    pub fn refused(&self) -> u64 {
        self.read().refused
    }

    /// A new session: anything queued to the old one may be lost. Drops an
    /// assembly and asks for a fresh snapshot; the filter in use stays.
    pub fn reconnected(&self) -> SnapshotAck {
        let mut st = self.write();
        st.pending = None;
        st.resync_asked = true;
        RESYNC
    }

    /// Call on every tick with the current time. While a resync is asked
    /// for and no header has come, the request is sent again every
    /// [`RESYNC_RETRY`]: the first may have been lost with its session.
    pub fn poll(&self, now: Instant) -> Option<SnapshotAck> {
        let mut st = self.write();
        if !st.resync_asked {
            st.asked_at = None;
            return None;
        }
        match st.asked_at {
            None => {
                st.asked_at = Some(now);
                None
            }
            Some(at) if now.duration_since(at) >= RESYNC_RETRY => {
                st.asked_at = Some(now);
                Some(RESYNC)
            }
            Some(_) => None,
        }
    }

    /// A `HaveSnapshot` header: starts a new assembly, replacing any other.
    pub fn header(&self, bytes: &[u8]) -> Result<(), HaveError> {
        let h = match HaveHeader::decode(bytes) {
            Ok(h) => h,
            Err(e) => {
                self.write().refused += 1;
                return Err(e);
            }
        };
        let pending = |bloom| Pending {
            bloom,
            assembly: Assembly::new(h),
            next_delta: h.next_delta,
        };
        {
            let mut st = self.write();
            st.pending = None;
            st.resync_asked = false;
            st.asked_at = None;
            if st
                .current
                .as_ref()
                .is_some_and(|c| c.generation == h.generation && c.bloom.matches(&h))
            {
                st.pending = Some(pending(None));
                return Ok(());
            }
            let new = filter_bytes(h.bits);
            let old = st.current.as_ref().map_or(0, |c| c.bloom.memory_bytes());
            if old + new > self.in_place_above {
                st.current = None;
            }
        }
        // Allocated with no lock held, so lookups don't wait on it.
        let bloom = Bloom::for_header(&h);
        self.write().pending = Some(pending(Some(bloom)));
        Ok(())
    }

    /// A have-filter `Snapshot` chunk. Returns the acknowledgement to send:
    /// the chunk's own, or a [`RESYNC`] request when it was out of order
    /// (the assembly is dropped). A chunk of an earlier transfer is
    /// ignored.
    pub fn chunk(&self, c: &SnapshotChunk) -> Option<SnapshotAck> {
        let mut guard = self.write();
        let st = &mut *guard;
        let p = st.pending.as_mut()?;
        if !p.assembly.owns(c) {
            return None;
        }
        match p.assembly.accept(c) {
            Ok(placed) => {
                let (offset, ack, last) = (placed.offset, placed.ack, placed.last);
                match (&p.bloom, &st.current) {
                    (Some(b), _) => b.write_bytes(offset, placed.bytes),
                    (None, Some(c)) => c.bloom.write_bytes(offset, placed.bytes),
                    // A refill whose filter went away: can't happen, since
                    // only a header drops the filter in use.
                    (None, None) => {
                        st.pending = None;
                        return None;
                    }
                }
                if last {
                    let p = st.pending.take().expect("pending");
                    let generation = p.assembly.header().generation;
                    match p.bloom {
                        Some(bloom) => {
                            st.current = Some(Held {
                                generation,
                                bloom,
                                next_delta: p.next_delta,
                            });
                        }
                        None => {
                            if let Some(c) = st.current.as_mut() {
                                c.next_delta = p.next_delta;
                            }
                        }
                    }
                }
                Some(ack)
            }
            Err(_) => {
                st.refused += 1;
                st.pending = None;
                ask_resync(st)
            }
        }
    }

    /// A `HaveDelta`. Returns what it did, and a [`RESYNC`] request to send
    /// when a delta was missed.
    pub fn delta(&self, bytes: &[u8]) -> (Result<Applied, HaveError>, Option<SnapshotAck>) {
        let d = match HaveDelta::decode(bytes) {
            Ok(d) => d,
            Err(e) => {
                let mut st = self.write();
                st.refused += 1;
                return (Err(e), None);
            }
        };
        let mut st = self.write();
        match apply(&mut st, &d) {
            Some(applied) => (Ok(applied), None),
            None => {
                st.refused += 1;
                // The assembly can't complete correctly without it.
                if st
                    .pending
                    .as_ref()
                    .is_some_and(|p| p.assembly.header().generation == d.generation)
                {
                    st.pending = None;
                }
                (Ok(Applied::Gap), ask_resync(&mut st))
            }
        }
    }
}

/// Asks for a resync once per incident.
fn ask_resync(st: &mut State) -> Option<SnapshotAck> {
    if st.resync_asked {
        return None;
    }
    st.resync_asked = true;
    Some(RESYNC)
}

/// Applies a delta in sequence. `None` is a gap.
fn apply(st: &mut State, d: &HaveDelta) -> Option<Applied> {
    use std::cmp::Ordering::{Equal, Greater, Less};
    let insert = |b: &Bloom| {
        for k in &d.keys {
            b.insert(&InfoHash(*k));
        }
    };
    let State {
        current, pending, ..
    } = st;
    // The assembly's deltas go into the filter in use too: it keeps
    // answering until the new one is complete.
    if let Some(p) = pending
        && p.assembly.header().generation == d.generation
    {
        return match d.seq.cmp(&p.next_delta) {
            Less => Some(Applied::Stale),
            Greater => None,
            Equal => {
                if let Some(b) = &p.bloom {
                    insert(b);
                }
                if let Some(c) = current {
                    insert(&c.bloom);
                }
                p.next_delta = p.next_delta.wrapping_add(1);
                Some(Applied::Inserted)
            }
        };
    }
    if let Some(c) = current
        && c.generation == d.generation
    {
        return match d.seq.cmp(&c.next_delta) {
            Less => Some(Applied::Stale),
            Greater => None,
            Equal => {
                insert(&c.bloom);
                c.next_delta = c.next_delta.wrapping_add(1);
                Some(Applied::Inserted)
            }
        };
    }
    Some(Applied::Stale)
}

#[cfg(test)]
mod tests {
    use lodestar_cluster::have::{HaveKey, RESYNC, chunk_range};
    use lodestar_cluster::proto::SnapshotKind;

    use super::*;

    fn key(i: u64) -> InfoHash {
        let mut h = [0u8; 20];
        h[..8].copy_from_slice(&i.to_le_bytes());
        h[8..16].copy_from_slice(&(i ^ 0x5555).to_be_bytes());
        InfoHash(h)
    }

    /// The coordinator's filter of `n` keys, 3 chunks long.
    fn source(n: u64, seed: u8) -> Bloom {
        let b = Bloom::with_params(1 << 20, 10, 7, HaveKey::from_bytes([seed; 16]));
        for i in 0..n {
            b.insert(&key(i));
        }
        b
    }

    fn chunks(b: &Bloom, serial: u64) -> Vec<SnapshotChunk> {
        let total = b.header(1, serial, 0).chunks;
        (0..total)
            .map(|seq| SnapshotChunk {
                kind: SnapshotKind::HaveFilter,
                version: serial,
                seq,
                total,
                bytes: b.read_bytes(chunk_range(b.bit_len(), seq)),
            })
            .collect()
    }

    fn delta(generation: u64, seq: u64, keys: std::ops::Range<u64>) -> Vec<u8> {
        HaveDelta {
            generation,
            seq,
            keys: keys.map(|i| key(i).0).collect(),
        }
        .encode()
        .unwrap()
    }

    /// Sends a whole snapshot of `b` at `generation`.
    fn load(w: &WorkerHave, b: &Bloom, generation: u64, serial: u64) {
        w.header(&b.header(generation, serial, 0).encode()).unwrap();
        for c in chunks(b, serial) {
            assert_eq!(w.chunk(&c).unwrap().seq, c.seq);
        }
        assert_eq!(w.generation(), Some(generation));
    }

    #[test]
    fn says_maybe_until_a_snapshot_is_complete() {
        let w = WorkerHave::new();
        assert!(w.might_be_stored(&key(1_000_000)));
        let b = source(100, 1);
        w.header(&b.header(1, 5, 0).encode()).unwrap();
        let cs = chunks(&b, 5);
        assert_eq!(cs.len(), 3);
        w.chunk(&cs[0]);
        w.chunk(&cs[1]);
        assert!(!w.is_ready());
        assert!(w.might_be_stored(&key(1_000_000)));
        w.chunk(&cs[2]);
        assert!(w.is_ready());
        assert!((0..100).all(|i| w.might_be_stored(&key(i))));
        assert!(!w.might_be_stored(&key(1_000_000)));
    }

    #[test]
    fn out_of_order_and_missing_chunks_are_refused() {
        let b = source(100, 2);
        let cs = chunks(&b, 7);
        for order in [[0usize, 2, 1], [1, 0, 2], [0, 0, 1], [0, 1, 1]] {
            let w = WorkerHave::new();
            w.header(&b.header(1, 7, 0).encode()).unwrap();
            let acks: Vec<_> = order.iter().map(|i| w.chunk(&cs[*i])).collect();
            assert!(!w.is_ready(), "{order:?}");
            assert!(acks.contains(&Some(RESYNC)), "{order:?}");
            assert_eq!(w.refused(), 1);
            // The assembly is gone: the rest of the transfer is ignored.
            assert_eq!(w.chunk(&cs[2]), None);
            assert!(!w.is_ready());
        }
        // A wrong length or total is refused the same way.
        let w = WorkerHave::new();
        w.header(&b.header(1, 7, 0).encode()).unwrap();
        let mut short = cs[0].clone();
        short.bytes.pop();
        assert_eq!(w.chunk(&short), Some(RESYNC));
        let w = WorkerHave::new();
        w.header(&b.header(1, 7, 0).encode()).unwrap();
        let mut total = cs[0].clone();
        total.total = 4;
        assert_eq!(w.chunk(&total), Some(RESYNC));
        // Another transfer's chunks, or none started, aren't an error.
        let w = WorkerHave::new();
        assert_eq!(w.chunk(&cs[0]), None);
        w.header(&b.header(1, 8, 0).encode()).unwrap();
        assert_eq!(w.chunk(&cs[0]), None);
        assert_eq!(w.refused(), 0);
    }

    #[test]
    fn the_old_filter_answers_until_the_new_one_is_complete() {
        let w = WorkerHave::new();
        let old = source(100, 3);
        load(&w, &old, 1, 1);
        let new = source(200, 4);
        w.header(&new.header(2, 2, 0).encode()).unwrap();
        let cs = chunks(&new, 2);
        w.chunk(&cs[0]);
        assert_eq!(w.generation(), Some(1));
        assert!((0..100).all(|i| w.might_be_stored(&key(i))));
        // A delta of the new generation reaches both.
        assert_eq!(
            w.delta(&delta(2, 0, 500..510)),
            (Ok(Applied::Inserted), None)
        );
        assert!((500..510).all(|i| w.might_be_stored(&key(i))));
        assert_eq!(w.memory_bytes(), 2 * old.memory_bytes());
        w.chunk(&cs[1]);
        w.chunk(&cs[2]);
        assert_eq!(w.generation(), Some(2));
        assert!((0..200).chain(500..510).all(|i| w.might_be_stored(&key(i))));
        assert_eq!(w.memory_bytes(), new.memory_bytes());
    }

    #[test]
    fn above_the_limit_the_swap_is_in_place() {
        let old = source(100, 5);
        let w = WorkerHave::in_place_above(2 * old.memory_bytes() - 1);
        load(&w, &old, 1, 1);
        let new = source(150, 6);
        w.header(&new.header(2, 2, 0).encode()).unwrap();
        // The old one is gone at once: "maybe" for everything meanwhile.
        assert!(!w.is_ready());
        assert!(w.might_be_stored(&key(1_000_000)));
        assert_eq!(w.memory_bytes(), new.memory_bytes());
        for c in chunks(&new, 2) {
            w.chunk(&c);
        }
        assert_eq!(w.generation(), Some(2));
        assert!((0..150).all(|i| w.might_be_stored(&key(i))));
    }

    #[test]
    fn deltas_in_sequence_or_a_resync() {
        let w = WorkerHave::new();
        let b = source(10, 7);
        load(&w, &b, 3, 1);
        assert_eq!(
            w.delta(&delta(3, 0, 100..105)),
            (Ok(Applied::Inserted), None)
        );
        // Again, or an old generation: ignored.
        assert_eq!(w.delta(&delta(3, 0, 200..205)), (Ok(Applied::Stale), None));
        assert_eq!(w.delta(&delta(2, 1, 200..205)), (Ok(Applied::Stale), None));
        assert!((200..205).all(|i| !w.might_be_stored(&key(i))));
        // Seq 1 went missing: ask once, keep answering.
        assert_eq!(
            w.delta(&delta(3, 2, 300..305)),
            (Ok(Applied::Gap), Some(RESYNC))
        );
        assert_eq!(w.delta(&delta(3, 3, 300..305)), (Ok(Applied::Gap), None));
        assert!(w.is_ready());
        assert!((100..105).all(|i| w.might_be_stored(&key(i))));
        // Malformed bytes are refused and counted.
        assert_eq!(w.delta(&[9, 9]).0, Err(HaveError::Version));
        assert!(w.header(&[1; 70]).is_err());
        assert!(w.is_ready());
        // A new header clears the request, so the next gap asks again.
        load(&w, &b, 4, 2);
        assert_eq!(
            w.delta(&delta(4, 1, 0..1)),
            (Ok(Applied::Gap), Some(RESYNC))
        );
    }

    #[test]
    fn a_gap_while_assembling_drops_the_assembly() {
        let w = WorkerHave::new();
        let b = source(10, 8);
        w.header(&b.header(1, 1, 5).encode()).unwrap();
        let cs = chunks(&b, 1);
        w.chunk(&cs[0]);
        assert_eq!(w.delta(&delta(1, 4, 0..1)), (Ok(Applied::Stale), None));
        assert_eq!(
            w.delta(&delta(1, 6, 0..1)),
            (Ok(Applied::Gap), Some(RESYNC))
        );
        assert_eq!(w.chunk(&cs[1]), None);
        assert!(!w.is_ready());
    }

    #[test]
    fn a_new_session_asks_for_a_snapshot_and_keeps_the_filter() {
        let w = WorkerHave::new();
        let b = source(10, 9);
        load(&w, &b, 1, 1);
        w.header(&b.header(2, 2, 0).encode()).unwrap();
        assert_eq!(w.reconnected(), RESYNC);
        assert_eq!(w.chunk(&chunks(&b, 2)[0]), None);
        assert_eq!(w.generation(), Some(1));
        assert!((0..10).all(|i| w.might_be_stored(&key(i))));
    }

    #[test]
    fn debug_never_shows_the_key() {
        let w = WorkerHave::new();
        let b = source(1, 0xC3);
        load(&w, &b, 1, 1);
        w.header(&b.header(2, 2, 0).encode()).unwrap();
        let shown = format!("{w:?}");
        assert!(shown.contains("HaveKey(..)"), "{shown}");
        for needle in ["c3", "C3", "195"] {
            assert!(!shown.contains(needle), "{needle} in {shown}");
        }
    }

    #[test]
    fn a_resync_of_the_same_filter_refills_it_with_no_maybe_window() {
        // Even with in-place swaps always on.
        let w = WorkerHave::in_place_above(0);
        let b = source(100, 0x11);
        load(&w, &b, 1, 1);
        for i in 100..120 {
            b.insert(&key(i));
        }
        let mem = w.memory_bytes();
        w.header(&b.header(1, 2, 0).encode()).unwrap();
        assert!(w.is_ready());
        assert_eq!(w.memory_bytes(), mem);
        assert!(!w.might_be_stored(&key(1_000_000)));
        let cs = chunks(&b, 2);
        w.chunk(&cs[0]);
        assert_eq!(
            w.delta(&delta(1, 0, 200..210)),
            (Ok(Applied::Inserted), None)
        );
        w.chunk(&cs[1]);
        w.chunk(&cs[2]);
        assert_eq!(w.generation(), Some(1));
        assert!((0..120).chain(200..210).all(|i| w.might_be_stored(&key(i))));
        // Its deltas carry on from the header's sequence number.
        assert_eq!(
            w.delta(&delta(1, 1, 300..301)),
            (Ok(Applied::Inserted), None)
        );
        // Another generation or key is a new filter.
        w.header(&b.header(2, 3, 0).encode()).unwrap();
        assert!(!w.is_ready());
    }

    #[test]
    fn sequence_numbers_at_the_end_of_their_range_wrap() {
        let w = WorkerHave::new();
        let b = source(1, 0x12);
        w.header(&b.header(1, 1, u64::MAX).encode()).unwrap();
        for c in chunks(&b, 1) {
            w.chunk(&c);
        }
        assert_eq!(
            w.delta(&delta(1, u64::MAX, 5..6)),
            (Ok(Applied::Inserted), None)
        );
        assert_eq!(w.delta(&delta(1, 0, 6..7)), (Ok(Applied::Inserted), None));
    }

    #[test]
    fn a_lost_resync_request_is_sent_again() {
        let w = WorkerHave::new();
        let b = source(10, 0x13);
        load(&w, &b, 1, 1);
        let t0 = Instant::now();
        assert_eq!(w.poll(t0), None);
        // A gap asks once; that request is lost with the session.
        assert_eq!(w.delta(&delta(1, 5, 0..1)).1, Some(RESYNC));
        assert_eq!(w.poll(t0), None);
        assert_eq!(w.poll(t0 + RESYNC_RETRY / 2), None);
        assert_eq!(w.poll(t0 + RESYNC_RETRY), Some(RESYNC));
        assert_eq!(w.poll(t0 + RESYNC_RETRY + RESYNC_RETRY / 2), None);
        assert_eq!(w.poll(t0 + 2 * RESYNC_RETRY), Some(RESYNC));
        // The header after the re-ask ends it.
        load(&w, &b, 1, 2);
        assert_eq!(w.poll(t0 + 10 * RESYNC_RETRY), None);
        assert_eq!(w.poll(t0 + 20 * RESYNC_RETRY), None);
        assert!((0..10).all(|i| w.might_be_stored(&key(i))));
    }
}
