//! The spool's segment format and its folder (workers plan WK-11):
//! round trips, torn and damaged segments, the caps, removal and the
//! secrets-key reseal.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use lodestar_integrations::SecretStore;
use lodestar_pipeline::spool::{
    Caps, FRAME_HEAD, HEADER_LEN, Header, Kind, MAX_FUTURE_SECS, MAX_LEASES, Record,
    SEGMENT_SPAN_SECS, Sealer, Spool, SpoolError, Tail, decode_segment, encode_frame,
};
use proptest::prelude::*;

const NOW: i64 = 1_800_000_000;
const W: u32 = 9;

fn record(batch: u64, at: i64, size: usize) -> Record {
    Record {
        kind: Kind::Results,
        batch,
        at,
        leases: (0..3u8)
            .map(|i| ([i; 20], batch * 10 + u64::from(i)))
            .collect(),
        payload: (0..size).map(|i| (i as u64 * 31 + batch) as u8).collect(),
    }
}

fn secrets() -> Arc<SecretStore> {
    Arc::new(SecretStore::from_key(&[3; 32]))
}

fn spool(dir: &Path, caps: Caps) -> Spool {
    Spool::open(dir, W, caps, secrets()).unwrap()
}

fn only_segment(dir: &Path) -> std::path::PathBuf {
    let mut files: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    assert_eq!(files.len(), 1, "{files:?}");
    files.pop().unwrap()
}

fn all_records(s: &mut Spool, now: i64) -> Vec<Record> {
    s.segments()
        .into_iter()
        .flat_map(|seq| s.read(seq, now).unwrap().records)
        .collect()
}

fn arb_record() -> impl Strategy<Value = Record> {
    (
        prop_oneof![Just(Kind::Results), Just(Kind::Counters)],
        1u64..,
        any::<i64>(),
        proptest::collection::vec((any::<[u8; 20]>(), any::<u64>()), 0..40),
        proptest::collection::vec(any::<u8>(), 0..2000),
    )
        .prop_map(|(kind, batch, at, leases, payload)| Record {
            kind,
            batch,
            at,
            leases,
            payload,
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn a_record_round_trips(r in arb_record()) {
        let plain = r.encode().unwrap();
        prop_assert_eq!(Record::decode(&plain).unwrap(), r);
    }

    #[test]
    fn any_cut_of_a_record_is_refused(r in arb_record(), cut in any::<prop::sample::Index>()) {
        let plain = r.encode().unwrap();
        let cut = cut.index(plain.len());
        prop_assert!(Record::decode(&plain[..cut]).is_err());
    }

    /// A segment cut anywhere after its header gives back a prefix of
    /// what was written, whole records only, and says so.
    #[test]
    fn a_torn_segment_yields_only_whole_frames(
        bodies in proptest::collection::vec(proptest::collection::vec(any::<u8>(), 1..300), 1..6),
        cut in any::<prop::sample::Index>(),
    ) {
        let mut bytes = Header { worker: W, seq: 1, created: NOW }.encode().to_vec();
        let mut ends = Vec::new();
        for b in &bodies {
            bytes.extend(encode_frame(b).unwrap());
            ends.push(bytes.len());
        }
        let cut = HEADER_LEN + cut.index(bytes.len() - HEADER_LEN + 1);
        let d = decode_segment(&bytes[..cut]).unwrap();
        let whole = ends.iter().filter(|&&e| e <= cut).count();
        prop_assert_eq!(d.frames.len(), whole);
        for (f, b) in d.frames.iter().zip(&bodies) {
            prop_assert_eq!(*f, b.as_slice());
        }
        let clean = cut == HEADER_LEN || ends.contains(&cut);
        prop_assert_eq!(d.tail, if clean { Tail::Clean } else { Tail::Torn });
    }
}

#[test]
fn records_written_are_read_back_in_order_across_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = spool(dir.path(), Caps::default());
    let written: Vec<Record> = (1..=5).map(|b| record(b, NOW, 500)).collect();
    for r in &written {
        s.append(r, NOW).unwrap();
    }
    s.close_active().unwrap();
    drop(s);
    let mut s = spool(dir.path(), Caps::default());
    assert_eq!(all_records(&mut s, NOW), written);
    assert!(s.bytes() > 0);
}

#[test]
fn a_new_segment_starts_past_the_span_and_never_appends_to_an_old_one() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = spool(dir.path(), Caps::default());
    s.append(&record(1, NOW, 10), NOW).unwrap();
    s.append(&record(2, NOW, 10), NOW + SEGMENT_SPAN_SECS)
        .unwrap();
    s.close_active().unwrap();
    assert_eq!(s.segments(), vec![1, 2]);
    drop(s);
    // Reopened, appends go to a third segment.
    let mut s = spool(dir.path(), Caps::default());
    s.append(&record(3, NOW, 10), NOW).unwrap();
    s.close_active().unwrap();
    assert_eq!(s.segments(), vec![1, 2, 3]);
}

#[test]
fn a_torn_final_record_is_dropped_not_half_used() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = spool(dir.path(), Caps::default());
    for b in 1..=3 {
        s.append(&record(b, NOW, 400), NOW).unwrap();
    }
    s.close_active().unwrap();
    drop(s);
    let path = only_segment(dir.path());
    let len = fs::metadata(&path).unwrap().len();
    // Cut into the last frame's body, as a crash mid-write would.
    let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_len(len - 7).unwrap();
    drop(file);
    let mut s = spool(dir.path(), Caps::default());
    let got = all_records(&mut s, NOW);
    assert_eq!(got, vec![record(1, NOW, 400), record(2, NOW, 400)]);
    assert_eq!(
        s.stats()
            .dropped_torn
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    // A tail of zeros (a file grown before its data landed) is torn too.
    let mut bytes = fs::read(&path).unwrap();
    bytes.extend([0u8; 300]);
    fs::write(&path, &bytes).unwrap();
    let seg = s.read(1, NOW).unwrap();
    assert_eq!(seg.records.len(), 2);
    assert!(seg.damage.is_empty());
}

#[test]
fn damage_in_the_middle_stops_the_segment_and_is_reported_by_name_only() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = spool(dir.path(), Caps::default());
    for b in 1..=3 {
        s.append(&record(b, NOW, 400), NOW).unwrap();
    }
    s.close_active().unwrap();
    let path = only_segment(dir.path());
    let mut bytes = fs::read(&path).unwrap();
    let frame = (bytes.len() - HEADER_LEN) / 3;
    // A byte inside the second frame's body.
    bytes[HEADER_LEN + frame + FRAME_HEAD + 5] ^= 0x40;
    fs::write(&path, &bytes).unwrap();
    let seg = s.read(1, NOW).unwrap();
    assert_eq!(seg.records, vec![record(1, NOW, 400)]);
    assert_eq!(seg.damage.len(), 1);
    assert_eq!(seg.damage[0].segment, "0000000000000001.lsp");
    assert_eq!(seg.damage[0].part, "frame");
    assert_eq!(
        s.stats()
            .dropped_corrupt
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
}

#[test]
fn the_byte_cap_refuses_a_record_that_would_pass_it() {
    let dir = tempfile::tempdir().unwrap();
    let caps = Caps::unclamped(4_000, 3600);
    let mut s = spool(dir.path(), caps);
    let mut n = 0;
    loop {
        match s.append(&record(n + 1, NOW, 500), NOW) {
            Ok(()) => n += 1,
            Err(SpoolError::Full) => break,
            Err(e) => panic!("{e}"),
        }
        assert!(n < 100);
    }
    assert!(n >= 1);
    assert!(s.bytes() <= 4_000, "{}", s.bytes());
    s.close_active().unwrap();
    let on_disk: u64 = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().metadata().unwrap().len())
        .sum();
    assert_eq!(on_disk, s.bytes());
    assert!(!s.has_room(500));
    assert_eq!(s.stats().full.load(std::sync::atomic::Ordering::Relaxed), 1);
}

#[test]
fn records_older_than_the_age_cap_are_dropped_and_counted() {
    let dir = tempfile::tempdir().unwrap();
    let caps = Caps::default();
    let mut s = spool(dir.path(), caps);
    s.append(&record(1, NOW - caps.max_age_secs() - 1, 10), NOW)
        .unwrap();
    s.append(&record(2, NOW - 5, 10), NOW).unwrap();
    s.close_active().unwrap();
    assert_eq!(all_records(&mut s, NOW), vec![record(2, NOW - 5, 10)]);
    assert_eq!(
        s.stats()
            .dropped_old
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    // A whole segment past the cap is known by its header alone.
    assert!(!s.is_expired(1, NOW));
    assert!(s.is_expired(1, NOW + caps.max_age_secs() + SEGMENT_SPAN_SECS + 1));
}

#[test]
fn removal_overwrites_then_unlinks() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = spool(dir.path(), Caps::default());
    s.append(&record(1, NOW, 100), NOW).unwrap();
    s.close_active().unwrap();
    let path = only_segment(dir.path());
    // A handle opened before sees the file's bytes after the unlink: zeros.
    // (Unix; Windows can't remove a file that's open without delete
    // sharing. A second hard link would be left intact, by design.)
    #[cfg(unix)]
    let mut peek = fs::File::open(&path).unwrap();
    s.remove(1).unwrap();
    assert!(!path.exists());
    #[cfg(unix)]
    {
        use std::io::Read;
        let mut bytes = Vec::new();
        peek.read_to_end(&mut bytes).unwrap();
        assert!(!bytes.is_empty());
        assert!(bytes.iter().all(|&b| b == 0));
    }
    assert_eq!(s.bytes(), 0);
    assert!(s.is_empty());
}

#[test]
fn leftovers_and_foreign_segments_are_removed_at_open() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Spool::open(dir.path(), W + 1, Caps::default(), secrets()).unwrap();
    s.append(&record(1, NOW, 10), NOW).unwrap();
    s.close_active().unwrap();
    drop(s);
    fs::write(dir.path().join("0000000000000007.tmp"), b"half").unwrap();
    let s = spool(dir.path(), Caps::default());
    assert!(s.is_empty());
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn a_record_sealed_for_another_segment_doesnt_open() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = spool(dir.path(), Caps::default());
    s.append(&record(1, NOW, 10), NOW).unwrap();
    s.append(&record(2, NOW, 10), NOW + SEGMENT_SPAN_SECS)
        .unwrap();
    s.close_active().unwrap();
    // Move segment 2's frame into segment 1.
    let one = dir.path().join("0000000000000001.lsp");
    let two = fs::read(dir.path().join("0000000000000002.lsp")).unwrap();
    let mut bytes = fs::read(&one).unwrap();
    bytes.extend_from_slice(&two[HEADER_LEN..]);
    fs::write(&one, &bytes).unwrap();
    let seg = s.read(1, NOW).unwrap();
    assert_eq!(seg.records, vec![record(1, NOW, 10)]);
    assert_eq!(seg.damage[0].part, "seal");
}

#[test]
fn nothing_on_disk_reads_in_clear() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = spool(dir.path(), Caps::default());
    let mut r = record(1, NOW, 0);
    r.payload = b"a-name-that-must-stay-sealed".repeat(4);
    r.leases = vec![([0xAB; 20], 77)];
    s.append(&r, NOW).unwrap();
    s.close_active().unwrap();
    let bytes = fs::read(only_segment(dir.path())).unwrap();
    let find = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
    assert!(!find(b"a-name-that-must"));
    assert!(!find(&[0xAB; 20]));
}

#[test]
fn a_reseal_moves_every_record_to_the_new_key() {
    let dir = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let mut store = SecretStore::open_or_create(data.path()).unwrap();
    let mut s = Spool::open(
        dir.path(),
        W,
        Caps::default(),
        Arc::new(store_copy(data.path())),
    )
    .unwrap();
    for b in 1..=3 {
        s.append(&record(b, NOW, 200), NOW).unwrap();
    }
    s.close_active().unwrap();
    drop(s);
    store.rotate().unwrap();
    let rotated = Arc::new(store_copy(data.path()));
    let mut s = Spool::open(dir.path(), W, Caps::default(), rotated.clone()).unwrap();
    assert_eq!(s.reseal().unwrap(), 1);
    let seg = fs::read(only_segment(dir.path())).unwrap();
    let d = decode_segment(&seg).unwrap();
    assert!(d.frames.iter().all(|f| !rotated.is_stale(f)));
    // Nothing left to do on a second run.
    assert_eq!(s.reseal().unwrap(), 0);
    store.retire_old_keys().unwrap();
    let mut s = Spool::open(
        dir.path(),
        W,
        Caps::default(),
        Arc::new(store_copy(data.path())),
    )
    .unwrap();
    assert_eq!(
        all_records(&mut s, NOW),
        (1..=3).map(|b| record(b, NOW, 200)).collect::<Vec<_>>()
    );
    assert_eq!(s.bytes(), seg.len() as u64);
}

fn store_copy(data: &Path) -> SecretStore {
    SecretStore::open_or_create(data).unwrap()
}

#[test]
fn leases_over_the_cap_are_refused_at_encode() {
    let mut r = record(1, NOW, 0);
    r.leases = vec![([0; 20], 1); MAX_LEASES + 1];
    assert!(r.encode().is_err());
    r.leases.pop();
    assert!(r.encode().is_ok());
    r.batch = 0;
    assert!(r.encode().is_err());
}

/// The security review's L1: a link planted in the folder is unlinked,
/// never followed, so its target isn't zeroed; nor is a temp-file link
/// written through.
#[cfg(unix)]
#[test]
fn planted_links_are_never_followed() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let victim = outside.path().join("victim");
    fs::write(&victim, b"precious").unwrap();
    let spool_dir = dir.path().join("spool");
    fs::create_dir_all(&spool_dir).unwrap();
    std::os::unix::fs::symlink(&victim, spool_dir.join("0000000000000001.tmp")).unwrap();
    std::os::unix::fs::symlink(&victim, spool_dir.join("0000000000000002.lsp")).unwrap();
    let mut s = spool(&spool_dir, Caps::default());
    assert_eq!(fs::read(&victim).unwrap(), b"precious");
    assert!(fs::symlink_metadata(spool_dir.join("0000000000000001.tmp")).is_err());
    assert!(fs::symlink_metadata(spool_dir.join("0000000000000002.lsp")).is_err());
    // A link planted where the next temp file will be is removed, not
    // written through.
    std::os::unix::fs::symlink(&victim, spool_dir.join("0000000000000003.tmp")).unwrap();
    s.append(&record(1, NOW, 10), NOW).unwrap();
    s.close_active().unwrap();
    assert_eq!(fs::read(&victim).unwrap(), b"precious");
    assert_eq!(s.segments(), vec![3]);
    // Removal itself never follows one either.
    let seg = spool_dir.join("0000000000000003.lsp");
    fs::remove_file(&seg).unwrap();
    std::os::unix::fs::symlink(&victim, &seg).unwrap();
    s.remove(3).unwrap();
    assert_eq!(fs::read(&victim).unwrap(), b"precious");
}

/// A file with a second hard link (a temp name left linked to its
/// segment, or a planted link to another file) is unlinked, never zeroed:
/// zeroing it would zero the file it shares.
#[cfg(unix)]
#[test]
fn a_hard_linked_file_is_unlinked_not_zeroed() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = spool(dir.path(), Caps::default());
    for b in 1..=2 {
        s.append(&record(b, NOW, 100), NOW).unwrap();
    }
    s.close_active().unwrap();
    drop(s);
    let seg = dir.path().join("0000000000000001.lsp");
    fs::hard_link(&seg, dir.path().join("0000000000000001.tmp")).unwrap();
    // And a planted link to a file outside the folder.
    let outside = tempfile::tempdir().unwrap();
    let victim = outside.path().join("victim");
    fs::write(&victim, b"precious").unwrap();
    fs::hard_link(&victim, dir.path().join("0000000000000002.tmp")).unwrap();
    let mut s = spool(dir.path(), Caps::default());
    assert!(!dir.path().join("0000000000000001.tmp").exists());
    assert!(!dir.path().join("0000000000000002.tmp").exists());
    assert_eq!(fs::read(&victim).unwrap(), b"precious");
    assert_eq!(
        all_records(&mut s, NOW),
        vec![record(1, NOW, 100), record(2, NOW, 100)]
    );
    // Removing a segment that has another link leaves the other intact.
    let keep = outside.path().join("keep");
    fs::hard_link(&seg, &keep).unwrap();
    let before = fs::read(&keep).unwrap();
    s.remove(1).unwrap();
    assert!(!seg.exists());
    assert_eq!(fs::read(&keep).unwrap(), before);
}

/// A clock set back rolls the open segment, so the records stamped by the
/// new clock aren't dropped with an older header that now reads as from
/// the future.
#[test]
fn a_clock_set_back_starts_a_new_segment() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = spool(dir.path(), Caps::default());
    s.append(&record(1, NOW, 10), NOW).unwrap();
    let back = NOW - 10 * MAX_FUTURE_SECS;
    s.append(&record(2, back, 10), back).unwrap();
    s.close_active().unwrap();
    assert_eq!(s.segments(), vec![1, 2]);
    assert!(
        s.is_expired(1, back),
        "the first now reads as from the future"
    );
    assert!(!s.is_expired(2, back));
    let seg = s.read(2, back).unwrap();
    assert_eq!(seg.records, vec![record(2, back, 10)]);
}

/// L2: the folder is 0700 and its files 0600, whatever the umask.
#[cfg(unix)]
#[test]
fn the_folder_and_its_files_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let spool_dir = dir.path().join("spool");
    fs::create_dir_all(&spool_dir).unwrap();
    fs::set_permissions(&spool_dir, fs::Permissions::from_mode(0o755)).unwrap();
    let mut s = spool(&spool_dir, Caps::default());
    s.append(&record(1, NOW, 10), NOW).unwrap();
    s.close_active().unwrap();
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&spool_dir), 0o700);
    assert_eq!(mode(&only_segment(&spool_dir)), 0o600);
}

/// L3: only the spool's own names number it, the number never wraps, and
/// a new segment never replaces a file of its name.
#[test]
fn the_sequence_never_wraps_or_overwrites() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("ffffffffffffffff.x"), b"not ours").unwrap();
    let mut s = spool(dir.path(), Caps::default());
    s.append(&record(1, NOW, 10), NOW).unwrap();
    s.close_active().unwrap();
    assert_eq!(s.segments(), vec![1], "a foreign name doesn't number it");
    drop(s);

    // A segment at the last number: the next is refused, not wrapped.
    let dir = tempfile::tempdir().unwrap();
    let last = Header {
        worker: W,
        seq: u64::MAX,
        created: NOW,
    };
    fs::write(dir.path().join("ffffffffffffffff.lsp"), last.encode()).unwrap();
    let mut s = spool(dir.path(), Caps::default());
    assert!(s.append(&record(1, NOW, 10), NOW).is_err());
    assert_eq!(s.segments(), vec![u64::MAX]);
    drop(s);

    // A file already at the next name is never replaced.
    let dir = tempfile::tempdir().unwrap();
    let mut s = spool(dir.path(), Caps::default());
    fs::write(dir.path().join("0000000000000001.lsp"), b"someone else's").unwrap();
    assert!(s.append(&record(1, NOW, 10), NOW).is_err());
    assert_eq!(
        fs::read(dir.path().join("0000000000000001.lsp")).unwrap(),
        b"someone else's"
    );
}

/// N1: a length damaged mid-segment to point past the end reads as
/// damage, not a torn write; a genuinely torn last frame still reads torn.
#[test]
fn a_length_past_the_end_mid_segment_is_damage() {
    let mut bytes = Header {
        worker: W,
        seq: 1,
        created: NOW,
    }
    .encode()
    .to_vec();
    let first = bytes.len();
    for b in [&[1u8; 50][..], &[2u8; 60][..], &[3u8; 70][..]] {
        bytes.extend(encode_frame(b).unwrap());
    }
    let mut damaged = bytes.clone();
    damaged[first..first + 4].copy_from_slice(&100_000u32.to_le_bytes());
    assert_eq!(decode_segment(&damaged).unwrap().tail, Tail::Corrupt);
    let torn = &bytes[..bytes.len() - 5];
    let d = decode_segment(torn).unwrap();
    assert_eq!((d.tail, d.frames.len()), (Tail::Torn, 2));
}

/// I4: a record stamped further ahead than a clock may be off (the clock
/// was set back) is dropped as past its cap, not kept for longer.
#[test]
fn a_record_from_the_future_is_dropped_as_expired() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = spool(dir.path(), Caps::default());
    s.append(&record(1, NOW + 2 * MAX_FUTURE_SECS, 10), NOW)
        .unwrap();
    s.append(&record(2, NOW + MAX_FUTURE_SECS / 2, 10), NOW)
        .unwrap();
    s.close_active().unwrap();
    assert_eq!(
        all_records(&mut s, NOW),
        vec![record(2, NOW + MAX_FUTURE_SECS / 2, 10)]
    );
    assert_eq!(
        s.stats()
            .dropped_old
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    // A segment created in the future is past its cap by its header.
    assert!(s.is_expired(1, NOW - 2 * MAX_FUTURE_SECS));
}

/// N6: a leftover that can't be removed is skipped and counted; the spool
/// still opens.
#[test]
fn a_leftover_that_cant_be_removed_doesnt_stop_the_spool() {
    let dir = tempfile::tempdir().unwrap();
    let stuck = dir.path().join("0000000000000005.tmp");
    fs::create_dir(&stuck).unwrap();
    fs::write(stuck.join("inside"), b"x").unwrap();
    let mut s = spool(dir.path(), Caps::default());
    assert_eq!(
        s.stats()
            .leftovers
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    s.append(&record(1, NOW, 10), NOW).unwrap();
    s.close_active().unwrap();
    assert_eq!(s.segments(), vec![6], "numbered past the leftover");
}

/// The coordinator's side of a replay (WK-9's intake), with spooled
/// payloads read back from disk: replayed twice counts once, and a worker
/// that is quarantined, revoked or purged has its replay refused.
mod replay {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use lodestar_cluster::proto::Payload;
    use lodestar_cluster::{BatchAnswer, Claim, Claimed, InboundSink, StoredKeys, Token, WorkerId};
    use lodestar_metainfo::{Info, InfoHash};
    use lodestar_pipeline::remote::{Clocks, RemoteSink, SinkParts, Standing};
    use lodestar_pipeline::wire::filter_snapshot::{SealedRecord, WireKey, WireKeys};
    use lodestar_pipeline::wire::{ResultRecord, ResultsBatch};
    use lodestar_pipeline::{Pipeline, PipelineConfig};
    use lodestar_sources::{PoolConfig, Stage};
    use lodestar_store::{Prepared, SourceId, Store, StoreConfig};

    use super::*;

    const WAIT: Duration = Duration::from_secs(15);
    const WORKER: WorkerId = WorkerId(W);

    async fn until(what: &str, mut f: impl FnMut() -> bool) {
        tokio::time::timeout(WAIT, async {
            while !f() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
    }

    fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    fn info(name: &str) -> Vec<u8> {
        format!(
            "d6:lengthi1073741824e4:name{}:{name}12:piece lengthi1048576e6:pieces20:{}e",
            name.len(),
            "\0".repeat(20)
        )
        .into_bytes()
    }

    fn prepared(store: &Store, raw: &[u8]) -> Prepared {
        let info = Info::parse(raw).unwrap();
        let mut p = store.preparer().prepare(&info, Some(raw)).unwrap();
        let files = lodestar_classify::file_infos(&info.files, info.path_encoding);
        p.enrichment = Some(lodestar_pipeline::enrich::enrich_infos(&p.name, &files).0);
        p
    }

    fn wire_key() -> WireKey {
        WireKey::new(1, [7; 32])
    }

    /// One worker-sealed `Results` batch, as the runtime encodes it.
    fn encoded(batch: u64, torrent: Prepared, token: Token) -> Vec<u8> {
        let rec = ResultRecord {
            key: torrent.info_hash,
            token,
            torrent,
            sources: 0,
            metadata_at: now(),
            action: None,
            rule_version: 0,
            fix: None,
            private_source: false,
            source_adult: false,
        };
        let sealed = SealedRecord::seal(&rec, &wire_key(), WORKER).unwrap();
        ResultsBatch {
            batch,
            records: Vec::new(),
        }
        .encode_with(&[sealed])
        .unwrap()
    }

    #[derive(Default)]
    struct Fake {
        refused: Mutex<Vec<WorkerId>>,
        purged: Mutex<HashMap<WorkerId, i64>>,
    }
    impl Standing for Fake {
        fn refuses_late(&self, w: WorkerId) -> bool {
            self.refused.lock().unwrap().contains(&w)
        }
        fn last_purge(&self, w: WorkerId) -> Option<i64> {
            self.purged.lock().unwrap().get(&w).copied()
        }
    }

    struct NoOffset;
    impl Clocks for NoOffset {
        fn offset_ms(&self, _: WorkerId) -> i64 {
            0
        }
    }

    struct Reader(Mutex<Store>);
    impl StoredKeys for Reader {
        fn stored(&self, keys: &[InfoHash]) -> Vec<bool> {
            let r = self.0.lock().unwrap();
            keys.iter().map(|k| r.find(k).unwrap().is_some()).collect()
        }
    }

    struct Rig {
        dir: tempfile::TempDir,
        pipeline: Pipeline,
        worker_store: Store,
        standing: Arc<Fake>,
        sink: Arc<RemoteSink>,
    }

    impl Rig {
        async fn new() -> Rig {
            let dir = tempfile::tempdir().unwrap();
            let db = dir.path().join("lodestar.db");
            let store = Store::open(&db, StoreConfig::default()).unwrap();
            let config = PipelineConfig {
                remote_workers: true,
                pool: PoolConfig {
                    lookup_wait: Duration::from_secs(600),
                    ..PoolConfig::default()
                },
                flush_every: Duration::from_millis(100),
                batch_wait: Duration::from_millis(20),
                ..PipelineConfig::default()
            };
            let pipeline = Pipeline::start(config, store, vec![]).await.unwrap();
            let standing = Arc::new(Fake::default());
            let keys = Arc::new(WireKeys::default());
            keys.set(vec![wire_key()]);
            let reader = || Store::open_reader(&db, StoreConfig::default()).unwrap();
            let sink = pipeline.monitor().remote().sink(SinkParts {
                preparer: Mutex::new(reader().preparer()),
                standing: standing.clone(),
                keys,
                stored: Arc::new(Reader(Mutex::new(reader()))),
                clocks: Arc::new(NoOffset),
                hints_allow_private: false,
            });
            let worker_store =
                Store::open(dir.path().join("worker.db"), StoreConfig::default()).unwrap();
            Rig {
                dir,
                pipeline,
                worker_store,
                standing,
                sink,
            }
        }

        fn lease(&self, key: InfoHash) -> Token {
            let c = Claim {
                key,
                source: SourceId::new(5).unwrap(),
                strategy: 3,
                at: now() - 30,
            };
            let authority = self.pipeline.monitor().lease_authority().clone();
            match authority.claim_batch(WORKER, &[c], now(), None, None)[0] {
                BatchAnswer::Claimed {
                    claimed: Claimed::Granted(t),
                    ..
                } => t,
                ref other => panic!("{other:?}"),
            }
        }

        fn stored(&self, key: &InfoHash) -> bool {
            Store::open_reader(self.dir.path().join("lodestar.db"), StoreConfig::default())
                .unwrap()
                .find(key)
                .unwrap()
                .is_some()
        }

        fn stat(
            &self,
            f: impl Fn(&lodestar_pipeline::remote::RemoteStats) -> &std::sync::atomic::AtomicU64,
        ) -> u64 {
            f(&self.pipeline.monitor().remote().stats).load(Ordering::Acquire)
        }

        async fn wait_records(&self, n: u64) {
            until("the intake read the records", || {
                self.stat(|s| &s.records) >= n
            })
            .await;
        }

        /// Spools one leased torrent and reads it back as a replay would.
        fn spool_one(&self, name: &str) -> (InfoHash, Record) {
            let t = prepared(&self.worker_store, &info(name));
            let key = t.info_hash;
            let token = self.lease(key);
            let dir = self.dir.path().join("spool");
            let mut s = Spool::open(&dir, W, Caps::default(), secrets()).unwrap();
            let r = Record {
                kind: Kind::Results,
                batch: 1,
                at: now(),
                leases: vec![(key.0, token.0)],
                payload: encoded(1, t, token),
            };
            s.append(&r, now()).unwrap();
            s.close_active().unwrap();
            let seq = *s.segments().last().unwrap();
            let back = s.read(seq, now()).unwrap().records.pop().unwrap();
            assert_eq!(back, r);
            (key, back)
        }

        fn replay(&self, batch: u64, r: &Record) -> bool {
            self.sink.results(
                WORKER,
                Payload {
                    batch,
                    bytes: r.payload.clone(),
                },
            )
        }
    }

    /// Replayed twice (an `Accepted` lost with the session): stored once,
    /// harvested once, and the funnel reconciles.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_replay_twice_counts_once() {
        let rig = Rig::new().await;
        let (key, r) = rig.spool_one("Replayed.Twice.2024.mkv");
        assert!(rig.replay(100, &r));
        until("stored", || rig.stored(&key)).await;
        rig.wait_records(1).await;
        assert!(rig.replay(101, &r), "a replay is answered accepted");
        rig.wait_records(2).await;
        assert_eq!(rig.stat(|s| &s.committed), 1);
        assert_eq!(rig.stat(|s| &s.stale), 1);
        let authority = rig.pipeline.monitor().lease_authority().clone();
        until("the lease ended", || !authority.in_flight(&key)).await;
        let hour = now().div_euclid(3600);
        let store = rig.pipeline.shutdown().await.unwrap();
        let funnel = store.cohorts(hour - 1, hour + 1).unwrap();
        let count = |stage: Stage| -> i64 {
            funnel
                .iter()
                .filter(|c| c.stage == stage as u8)
                .map(|c| c.count)
                .sum()
        };
        assert_eq!(count(Stage::Harvested), 1);
        assert_eq!(count(Stage::Stored), 1);
        assert_eq!(count(Stage::Lost), 0);
    }

    /// A quarantined, revoked or purged worker's replay is refused,
    /// whatever the epoch: here a current-epoch record, still under the
    /// worker's own lease. (Revocation also refuses the session itself.)
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_refused_workers_replay_is_refused_whatever_the_epoch() {
        let rig = Rig::new().await;
        let (key, r) = rig.spool_one("Quarantined.2024.mkv");
        rig.standing.refused.lock().unwrap().push(WORKER);
        assert!(rig.replay(100, &r));
        rig.wait_records(1).await;
        assert_eq!(rig.stat(|s| &s.committed), 0);
        assert_eq!(rig.stat(|s| &s.late_refused), 1);
        assert!(!rig.stored(&key));
        // Its lease went back to wait for a re-grant, not lost.
        let authority = rig.pipeline.monitor().lease_authority().clone();
        assert!(authority.get(&key).is_none(), "no longer the worker's");
        assert!(authority.in_flight(&key), "waiting for a re-grant");
        rig.standing.refused.lock().unwrap().clear();

        // Leased before the worker's last purge: refused too.
        let (key, r) = rig.spool_one("Purged.2024.mkv");
        rig.standing
            .purged
            .lock()
            .unwrap()
            .insert(WORKER, now() + 10);
        assert!(rig.replay(101, &r));
        rig.wait_records(2).await;
        assert_eq!(rig.stat(|s| &s.purged), 1);
        assert_eq!(rig.stat(|s| &s.committed), 0);
        assert!(!rig.stored(&key));
        rig.pipeline.shutdown().await.unwrap();
    }
}
