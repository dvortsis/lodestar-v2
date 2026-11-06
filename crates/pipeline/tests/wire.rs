//! The `Results` and `Counters` codecs (WK-6, architecture §19.2, §19.4,
//! §19.8, §3): the ≤ 3 KB median on a golden corpus, pinned version-1
//! bytes, round trips, and refusal of whatever a worker shouldn't send.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_cluster::Token;
use lodestar_filter::Action;
use lodestar_metainfo::{Info, InfoHash, InfoHashV2};
use lodestar_pipeline::enrich::{enrich_infos, file_infos};
use lodestar_pipeline::wire::results::{self, MAX_RECORDS, VERSION, flag};
use lodestar_pipeline::wire::{CountersBatch, Error, MAX_PAYLOAD, ResultRecord, ResultsBatch};
use lodestar_store::{Codec, Compressed, Counter, Prepared, Store, StoreConfig, flags};

const MAX_I64: u64 = i64::MAX as u64;

/// `TextEncoding` id of Windows-1252.
const WINDOWS_1252: u8 = 10;
use proptest::prelude::*;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/wire")
        .join(name)
}

// ---------------------------------------------------------------------
// The golden corpus.

struct Shape {
    piece_len: i64,
    name: String,
    /// `None`: a single-file torrent of this size.
    files: Option<Vec<(String, i64)>>,
    single: i64,
}

fn corpus() -> Vec<Shape> {
    let text = std::fs::read_to_string(fixture("corpus.txt")).unwrap();
    let mut out: Vec<Shape> = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (kind, rest) = line.split_once(' ').unwrap();
        let mut words = rest.splitn(3, ' ');
        let mut next = || words.next().unwrap().to_owned();
        match kind {
            "s" => {
                let piece: i64 = next().parse().unwrap();
                let size = next().parse().unwrap();
                out.push(Shape {
                    piece_len: piece << 10,
                    single: size,
                    name: next(),
                    files: None,
                });
            }
            "m" => {
                let (piece, name) = rest.split_once(' ').unwrap();
                out.push(Shape {
                    piece_len: piece.parse::<i64>().unwrap() << 10,
                    name: name.to_owned(),
                    files: Some(Vec::new()),
                    single: 0,
                });
            }
            "f" => {
                let (size, path) = rest.split_once(' ').unwrap();
                let files = out.last_mut().unwrap().files.as_mut().unwrap();
                files.push((path.to_owned(), size.parse().unwrap()));
            }
            "r" => {
                let count: usize = next().parse().unwrap();
                let size: i64 = next().parse().unwrap();
                let pattern = next();
                let width = count.to_string().len().max(2);
                let files = out.last_mut().unwrap().files.as_mut().unwrap();
                for n in 1..=count {
                    files.push((pattern.replace("{n}", &format!("{n:0width$}")), size));
                }
            }
            other => panic!("unknown corpus line kind {other:?}"),
        }
    }
    out
}

fn info_dict(s: &Shape) -> Vec<u8> {
    let mut d = BTreeMap::new();
    let total = match &s.files {
        None => {
            d.insert(b"length".to_vec(), OwnedValue::Int(s.single));
            s.single
        }
        Some(files) => {
            let list = files
                .iter()
                .map(|(path, size)| {
                    let mut f = BTreeMap::new();
                    f.insert(b"length".to_vec(), OwnedValue::Int(*size));
                    let segs = path.split('/').map(OwnedValue::from).collect();
                    f.insert(b"path".to_vec(), OwnedValue::List(segs));
                    OwnedValue::Dict(f)
                })
                .collect();
            d.insert(b"files".to_vec(), OwnedValue::List(list));
            files.iter().map(|f| f.1).sum()
        }
    };
    let pieces = (total + s.piece_len - 1) / s.piece_len;
    d.insert(b"name".to_vec(), OwnedValue::from(s.name.as_str()));
    d.insert(b"piece length".to_vec(), OwnedValue::Int(s.piece_len));
    d.insert(
        b"pieces".to_vec(),
        OwnedValue::Bytes(vec![0x5a; pieces as usize * 20]),
    );
    // An unknown key, as many real dicts have: it goes to `torrent_extra`.
    d.insert(b"source".to_vec(), OwnedValue::from("[invented]"));
    to_vec(&OwnedValue::Dict(d))
}

/// The record a worker would send for `raw`, prepared and enriched as the
/// prepare stage does it.
fn record(store: &Store, raw: &[u8], token: u64) -> ResultRecord {
    let info = Info::parse(raw).unwrap();
    let mut prepared = store.preparer().prepare(&info, Some(raw)).unwrap();
    let files = file_infos(&info.files, info.path_encoding);
    prepared.enrichment = Some(enrich_infos(&prepared.name, &files).0);
    ResultRecord {
        key: prepared.info_hash,
        token: Token(token),
        torrent: prepared,
        sources: 0b101,
        metadata_at: 1_793_000_000 + token as i64,
        action: None,
        rule_version: 42,
        fix: None,
        private_source: false,
        source_adult: false,
    }
}

/// Bytes per torrent on the wire: each record alone in a batch, header
/// included (frame compression would only make it smaller).
fn sizes(archive_raw: bool) -> Vec<usize> {
    let store = Store::open_in_memory(StoreConfig {
        archive_raw,
        ..StoreConfig::default()
    })
    .unwrap();
    let mut sizes: Vec<usize> = corpus()
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let raw = info_dict(s);
            let r = record(&store, &raw, i as u64 + 1);
            assert_eq!(r.torrent.raw.is_some(), archive_raw);
            let batch = ResultsBatch {
                batch: i as u64,
                records: vec![r],
            };
            let bytes = batch.encode().unwrap();
            assert_eq!(ResultsBatch::decode(&bytes).unwrap(), batch);
            // `pieces` never reaches the wire: no run of its bytes does.
            assert!(!bytes.windows(40).any(|w| w.iter().all(|&b| b == 0x5a)));
            assert!(!bytes.windows(6).any(|w| w == b"pieces"));
            bytes.len()
        })
        .collect();
    sizes.sort_unstable();
    sizes
}

fn pct(sorted: &[usize], p: usize) -> usize {
    sorted[(sorted.len() * p / 100).min(sorted.len() - 1)]
}

fn median(sorted: &[usize]) -> usize {
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]).div_ceil(2)
    }
}

/// §19.8: "Results ≤ 3 KB per torrent on the wire (median)", at defaults
/// (`archive_raw` off, plan Q8). The p95 and the median with
/// `archive_raw` on are reported, not gated.
#[test]
fn results_median_is_at_most_3_kb_on_the_golden_corpus() {
    let off = sizes(false);
    let on = sizes(true);
    assert!(off.len() >= 40, "the corpus shrank: {}", off.len());
    let report = format!(
        "results bytes per torrent over {} torrents: median {} B, p95 {} B, max {} B \
         (archive_raw off); median {} B, p95 {} B (archive_raw on)",
        off.len(),
        median(&off),
        pct(&off, 95),
        off[off.len() - 1],
        median(&on),
        pct(&on, 95),
    );
    println!("{report}");
    assert!(median(&off) <= 3 * 1024, "{report}");
}

// ---------------------------------------------------------------------
// Pinned version-1 bytes.

fn golden_results() -> ResultsBatch {
    let v2 = InfoHashV2([0x22; 32]);
    let plain = ResultRecord {
        key: InfoHash([0x11; 20]),
        token: Token(300),
        torrent: Prepared {
            info_hash: InfoHash([0x11; 20]),
            info_hash_v2: None,
            name: "Invented.Name.2024.1080p".into(),
            name_raw: None,
            size: 1 << 31,
            piece_len: 1 << 21,
            piece_count: 1024,
            file_count: 1,
            info_size: 20_600,
            flags: flags::SINGLE_FILE,
            files: Compressed {
                codec: Codec::Raw,
                dict_id: None,
                data: b"tree".to_vec(),
            },
            extra: None,
            raw: None,
            enrichment: None,
        },
        sources: 1,
        metadata_at: 1_793_000_000,
        action: None,
        rule_version: 7,
        fix: None,
        private_source: false,
        source_adult: false,
    };
    let hybrid = ResultRecord {
        key: v2.truncated(),
        token: Token(u64::MAX),
        torrent: Prepared {
            info_hash: InfoHash([0x33; 20]),
            info_hash_v2: Some(v2),
            name: "Caf\u{e9}".into(),
            // Windows-1252, where 0xe9 is é.
            name_raw: Some((vec![b'C', b'a', b'f', 0xe9], WINDOWS_1252)),
            size: 5,
            piece_len: 16_384,
            piece_count: 1,
            file_count: 2,
            info_size: 120,
            flags: flags::PRIVATE | flags::V2 | flags::HYBRID,
            files: Compressed {
                codec: Codec::Zstd,
                dict_id: Some(4),
                data: vec![0x28, 0xb5, 0x2f, 0xfd],
            },
            extra: Some(b"d6:source3:abce".to_vec()),
            raw: Some(b"d4:name4:Cafee".to_vec()),
            enrichment: None,
        },
        sources: u64::MAX,
        metadata_at: -1,
        action: Some(Action::Quarantine),
        rule_version: 0,
        fix: Some(true),
        private_source: true,
        source_adult: true,
    };
    ResultsBatch {
        batch: 0x0102_0304,
        records: vec![plain, hybrid],
    }
}

fn golden_counters() -> CountersBatch {
    let c = |hour, metric, dim, key, value| Counter {
        hour,
        metric,
        dim,
        key,
        value,
    };
    CountersBatch {
        batch: 9,
        counters: vec![
            c(498_055, 4, 3, 0, 12),
            c(498_055, 14, 2, 0, 40),
            c(498_055, 14, 7, 3, 2),
            c(498_056, 9, 0, 5, 1),
        ],
    }
}

fn hex(b: &[u8]) -> String {
    b.chunks(32)
        .map(|c| c.iter().map(|x| format!("{x:02x}")).collect::<String>() + "\n")
        .collect()
}

fn unhex(s: &str) -> Vec<u8> {
    let s: String = s.split_whitespace().collect();
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

/// The checked-in bytes decode to the same batch and re-encode to the same
/// bytes: a format change shows here and needs a new version.
/// `LODESTAR_BLESS=1` rewrites the files (only with a version bump).
fn check_golden(file: &str, bytes: &[u8]) -> Vec<u8> {
    let path = fixture(file);
    if std::env::var_os("LODESTAR_BLESS").is_some() {
        std::fs::write(&path, hex(bytes)).unwrap();
    }
    let pinned = unhex(&std::fs::read_to_string(&path).unwrap());
    assert_eq!(hex(bytes), hex(&pinned), "{file} changed");
    pinned
}

#[test]
fn results_version_1_bytes_are_pinned() {
    let batch = golden_results();
    let pinned = check_golden("results_v1.hex", &batch.encode().unwrap());
    assert_eq!(pinned[0], VERSION);
    assert_eq!(ResultsBatch::decode(&pinned).unwrap(), batch);
}

#[test]
fn counters_version_1_bytes_are_pinned() {
    let batch = golden_counters();
    let pinned = check_golden("counters_v1.hex", &batch.encode().unwrap());
    assert_eq!(CountersBatch::decode(&pinned).unwrap(), batch);
}

// ---------------------------------------------------------------------
// Refusals.

#[test]
fn unknown_versions_are_refused() {
    let mut r = golden_results().encode().unwrap();
    let mut c = golden_counters().encode().unwrap();
    for v in [0, 2, 0xff] {
        r[0] = v;
        c[0] = v;
        assert_eq!(ResultsBatch::decode(&r), Err(Error::Version(v)));
        assert_eq!(CountersBatch::decode(&c), Err(Error::Version(v)));
    }
}

#[test]
fn every_truncation_and_any_trailing_byte_is_refused() {
    let results = |b: &[u8]| ResultsBatch::decode(b).map(drop);
    let counters = |b: &[u8]| CountersBatch::decode(b).map(drop);
    type Decode<'a> = &'a dyn Fn(&[u8]) -> Result<(), Error>;
    let cases: [(Vec<u8>, Decode<'_>); 2] = [
        (golden_results().encode().unwrap(), &results),
        (golden_counters().encode().unwrap(), &counters),
    ];
    for (bytes, decode) in cases {
        for cut in 0..bytes.len() {
            assert!(decode(&bytes[..cut]).is_err(), "cut at {cut}");
        }
        let mut long = bytes.clone();
        long.push(0);
        assert_eq!(decode(&long), Err(Error::Trailing));
    }
}

#[test]
fn a_raw_dict_or_extra_holding_pieces_is_refused_both_ways() {
    let mut batch = golden_results();
    batch.records[1].torrent.raw = Some(b"d4:name1:a6:pieces20:aaaaaaaaaaaaaaaaaaaae".to_vec());
    assert_eq!(batch.encode(), Err(Error::Invalid("raw info")));
    let mut batch = golden_results();
    batch.records[1].torrent.extra = Some(b"d6:pieces0:e".to_vec());
    assert_eq!(batch.encode(), Err(Error::Invalid("extra")));
    // Nor an empty set of unknown keys, which `Preparer` leaves out.
    let mut batch = golden_results();
    batch.records[1].torrent.extra = Some(b"de".to_vec());
    assert_eq!(batch.encode(), Err(Error::Invalid("extra")));
    // Nor does a decoder take one written by hand: swap the raw dict in
    // the pinned bytes for one of the same length that holds `pieces`.
    let good = golden_results().encode().unwrap();
    let at = good
        .windows(14)
        .position(|w| w == b"d4:name4:Cafee")
        .unwrap();
    let mut bad = good.clone();
    bad[at..at + 14].copy_from_slice(b"d6:pieces2:xxe");
    assert!(ResultsBatch::decode(&bad).is_err());
    bad[at..at + 14].copy_from_slice(b"d6:piecesi10ee");
    assert!(ResultsBatch::decode(&bad).is_err());
}

#[test]
fn unknown_flag_bits_and_bad_codes_are_refused() {
    let good = golden_results().encode().unwrap();
    // The first record's flags varint follows version (1), batch id (4
    // bytes) and count (1).
    let at = 6;
    assert_eq!(good[at], 0);
    // Bit 12 and up are unknown in version 1: a two-byte varint.
    let mut bad = good.clone();
    bad.splice(at..=at, [0x80, 0x20]);
    assert_eq!(ResultsBatch::decode(&bad), Err(Error::Invalid("flags")));
    // FIX_ADULT without FIX.
    let mut bad = good.clone();
    let f = flag::FIX_ADULT;
    bad.splice(at..=at, [(f as u8) | 0x80, (f >> 7) as u8]);
    assert_eq!(ResultsBatch::decode(&bad), Err(Error::Invalid("flags")));

    // An unknown action, codec and name encoding, each on its own.
    let mut b = golden_results();
    b.records[1].torrent.name_raw = Some((vec![1], 250));
    assert_eq!(b.encode(), Err(Error::Invalid("name encoding")));
    // The raw name must decode to `name`, and is only kept when it isn't
    // plain UTF-8.
    for raw in [
        (b"Cafe".to_vec(), WINDOWS_1252),
        // Under UTF-8 the same byte decodes to U+FFFD, not to `name`.
        (vec![b'C', b'a', b'f', 0xe9], 0),
        ("Caf\u{e9}".as_bytes().to_vec(), 0),
    ] {
        let mut b = golden_results();
        b.records[1].torrent.name_raw = Some(raw);
        assert_eq!(b.encode(), Err(Error::Invalid("raw name")));
    }
    // Encodings metainfo never picks for a name.
    let id = |name: &str| {
        (0..=u8::MAX)
            .find(|&i| {
                lodestar_metainfo::TextEncoding::from_id(i)
                    .is_some_and(|e| e.encoding().name() == name)
            })
            .unwrap()
    };
    for name in ["UTF-16BE", "UTF-16LE", "x-user-defined", "replacement"] {
        let mut b = golden_results();
        b.records[1].torrent.name_raw = Some((vec![b'x'], id(name)));
        assert_eq!(b.encode(), Err(Error::Invalid("name encoding")), "{name}");
    }
    let mut b = golden_results();
    b.records[0].torrent.files.dict_id = Some(1);
    assert_eq!(b.encode(), Err(Error::Invalid("dictionary")));
}

/// A batch of one record whose bytes the test writes field by field, so
/// the decoder's own refusals are reached (the encoder refuses first).
struct Raw {
    flags: u64,
    v2: Option<[u8; 32]>,
    action: Option<u8>,
    name: Vec<u8>,
    size: u64,
    torrent_flags: i64,
    codec: u8,
    dict_id: Option<i64>,
}

impl Default for Raw {
    fn default() -> Raw {
        Raw {
            flags: 0,
            v2: None,
            action: None,
            name: b"n".to_vec(),
            size: 1,
            torrent_flags: 0,
            codec: 0,
            dict_id: None,
        }
    }
}

fn uvar(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn ivar(out: &mut Vec<u8>, v: i64) {
    uvar(out, ((v << 1) ^ (v >> 63)) as u64);
}

impl Raw {
    fn record(&self, out: &mut Vec<u8>, key: u8) {
        uvar(out, self.flags);
        out.extend_from_slice(&[key; 20]);
        if let Some(v2) = self.v2 {
            out.extend_from_slice(&v2);
        }
        for _ in 0..4 {
            uvar(out, 1); // token, sources, metadata_at (zigzag 1 = -1), rule
        }
        if let Some(a) = self.action {
            out.push(a);
        }
        uvar(out, self.name.len() as u64);
        out.extend_from_slice(&self.name);
        for v in [self.size, 1, 1, 1, 1] {
            uvar(out, v);
        }
        ivar(out, self.torrent_flags);
        out.push(self.codec);
        if let Some(id) = self.dict_id {
            ivar(out, id);
        }
        uvar(out, 0);
    }

    fn batch(&self, records: usize) -> Vec<u8> {
        let mut out = vec![VERSION, 0];
        uvar(&mut out, records as u64);
        for _ in 0..records {
            self.record(&mut out, 0x11);
        }
        out
    }

    fn decode(&self) -> Result<ResultsBatch, Error> {
        ResultsBatch::decode(&self.batch(1))
    }
}

#[test]
fn bad_fields_are_refused_by_the_decoder() {
    let inv = |w| Err(Error::Invalid(w));
    Raw::default().decode().unwrap();
    let v2 = Some([0x22; 32]);
    let v2_flags = flags::V2 | flags::HYBRID;
    // The hand-written layout is the encoder's.
    let back = Raw {
        flags: flag::V2 | flag::KEY_IS_V2,
        v2,
        torrent_flags: v2_flags,
        ..Raw::default()
    }
    .decode()
    .unwrap();
    assert_eq!(back.records[0].key, InfoHashV2([0x22; 32]).truncated());
    assert_eq!(
        back.encode().unwrap(),
        Raw {
            flags: flag::V2 | flag::KEY_IS_V2,
            v2,
            torrent_flags: v2_flags,
            ..Raw::default()
        }
        .batch(1)
    );

    let cases: Vec<(Raw, Result<ResultsBatch, Error>)> = vec![
        (
            Raw {
                name: vec![0xff, 0xfe],
                ..Raw::default()
            },
            inv("name"),
        ),
        (
            Raw {
                flags: flag::ACTION,
                action: Some(5),
                ..Raw::default()
            },
            inv("action"),
        ),
        (
            Raw {
                codec: 2,
                ..Raw::default()
            },
            inv("codec"),
        ),
        (
            Raw {
                flags: flag::DICT_ID,
                dict_id: Some(1),
                ..Raw::default()
            },
            inv("dictionary"),
        ),
        (
            Raw {
                flags: flag::DICT_ID,
                codec: 1,
                dict_id: Some(-1),
                ..Raw::default()
            },
            inv("dictionary"),
        ),
        // The key is the v2 hash, but there's none.
        (
            Raw {
                flags: flag::KEY_IS_V2,
                ..Raw::default()
            },
            inv("key"),
        ),
        // A pure v2 torrent: its truncated v2 hash is `info_hash`, so the
        // flag would name the same key twice over.
        (
            Raw {
                flags: flag::V2 | flag::KEY_IS_V2,
                v2: Some([0x11; 32]),
                torrent_flags: flags::V2,
                ..Raw::default()
            },
            inv("key"),
        ),
        // SQLite integers.
        (
            Raw {
                size: u64::MAX,
                ..Raw::default()
            },
            inv("integer"),
        ),
        (
            Raw {
                size: i64::MAX as u64 + 1,
                ..Raw::default()
            },
            inv("integer"),
        ),
        // Flags only the coordinator sets, and structural ones that lie.
        (
            Raw {
                torrent_flags: flags::HIDDEN,
                ..Raw::default()
            },
            inv("torrent flags"),
        ),
        (
            Raw {
                torrent_flags: flags::IMPORTED,
                ..Raw::default()
            },
            inv("torrent flags"),
        ),
        (
            Raw {
                torrent_flags: flags::PRIVATE_SOURCE,
                ..Raw::default()
            },
            inv("torrent flags"),
        ),
        (
            Raw {
                torrent_flags: -1,
                ..Raw::default()
            },
            inv("torrent flags"),
        ),
        (
            Raw {
                torrent_flags: flags::V2,
                ..Raw::default()
            },
            inv("torrent flags"),
        ),
        (
            Raw {
                flags: flag::V2,
                v2,
                ..Raw::default()
            },
            inv("torrent flags"),
        ),
        (
            Raw {
                flags: flag::V2,
                v2,
                torrent_flags: flags::V2,
                ..Raw::default()
            },
            inv("torrent flags"),
        ),
    ];
    for (raw, want) in cases {
        assert_eq!(raw.decode(), want, "flags {:#x}", raw.flags);
    }

    // The same key twice, and more records than the cap.
    assert_eq!(
        ResultsBatch::decode(&Raw::default().batch(2)),
        inv("duplicate key")
    );
    let mut over = vec![VERSION, 0];
    uvar(&mut over, MAX_RECORDS as u64 + 1);
    assert_eq!(ResultsBatch::decode(&over), Err(Error::TooLarge("records")));
}

#[test]
fn caps_hold_on_both_sides() {
    let one = golden_results().records.remove(0);
    let many = ResultsBatch {
        batch: 0,
        records: (0..=MAX_RECORDS)
            .map(|i| {
                let mut r = one.clone();
                let mut h = [0u8; 20];
                h[..8].copy_from_slice(&(i as u64).to_le_bytes());
                r.key = InfoHash(h);
                r.torrent.info_hash = InfoHash(h);
                r
            })
            .collect(),
    };
    assert_eq!(many.encode(), Err(Error::TooLarge("records")));
    // A batch whose records together pass the payload cap is an error to
    // split, never a frame the coordinator would refuse.
    let mut big = one.clone();
    big.torrent.files.data = vec![7; MAX_PAYLOAD / 2];
    let mut big2 = big.clone();
    big2.key = InfoHash([0x44; 20]);
    big2.torrent.info_hash = big2.key;
    let b = ResultsBatch {
        batch: 0,
        records: vec![big, big2],
    };
    assert_eq!(b.encode(), Err(Error::TooLarge("payload")));
    let huge = vec![1u8; MAX_PAYLOAD + 1];
    assert_eq!(ResultsBatch::decode(&huge), Err(Error::TooLarge("payload")));
    assert_eq!(
        CountersBatch::decode(&huge),
        Err(Error::TooLarge("payload"))
    );
    // A count far past what the bytes could hold allocates nothing.
    assert!(ResultsBatch::decode(&[VERSION, 0, 0x80, 0x08]).is_err());
    let mut name = one.clone();
    name.torrent.name = "x".repeat(results::MAX_NAME + 1);
    let b = ResultsBatch {
        batch: 0,
        records: vec![name],
    };
    assert_eq!(b.encode(), Err(Error::TooLarge("name")));
}

#[test]
fn enrichment_must_be_this_builds_and_agree_with_its_hot_columns() {
    let store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let raw = info_dict(&corpus()[0]);
    let good = record(&store, &raw, 1);
    let encode = |r: &ResultRecord| {
        ResultsBatch {
            batch: 0,
            records: vec![r.clone()],
        }
        .encode()
    };
    encode(&good).unwrap();
    let tweak = |f: &dyn Fn(&mut lodestar_store::Enriched)| {
        let mut r = good.clone();
        f(r.torrent.enrichment.as_mut().unwrap());
        encode(&r)
    };
    // Another build's enrichment is kept as sent (the store enriches it
    // again), but its hot columns must still be in range.
    tweak(&|e| {
        e.version += 1;
        e.data = b"a format this build can't read".to_vec();
    })
    .unwrap();
    assert!(tweak(&|e| e.version = 0).is_err());
    assert!(
        tweak(&|e| {
            e.version += 1;
            e.content_type = 250;
        })
        .is_err()
    );
    assert!(
        tweak(&|e| {
            e.version += 1;
            e.junk_score = 101;
        })
        .is_err()
    );
    assert!(tweak(&|e| e.content_type = 250).is_err());
    assert!(tweak(&|e| e.junk_score = 101).is_err());
    assert!(tweak(&|e| e.junk_score = (e.junk_score + 1) % 100).is_err());
    assert!(tweak(&|e| e.comp.clear()).is_err());
    assert!(tweak(&|e| e.data = b"not a record".to_vec()).is_err());
}

// ---------------------------------------------------------------------
// Properties.

fn arb_record() -> impl Strategy<Value = ResultRecord> {
    let names = prop::collection::vec("[ -~]{1,24}", 1..6);
    (
        prop::collection::vec(0x20u8..=0xff, 0..40),
        names,
        any::<[u8; 20]>(),
        prop::option::of(any::<[u8; 32]>()),
        any::<bool>(),
        (any::<u64>(), any::<u64>(), any::<i64>(), any::<u64>()),
        (prop::option::of(0u8..5), prop::option::of(any::<bool>())),
        (any::<bool>(), any::<bool>(), any::<bool>(), any::<i64>()),
        prop::collection::vec(any::<u8>(), 0..64),
    )
        .prop_map(
            |(
                raw_name,
                files,
                v1,
                v2,
                key_v2,
                (token, sources, at, rule),
                (action, fix),
                (private, adult, enrich, tflags),
                blob,
            )| {
                // A plain UTF-8 name, or one kept raw beside its decoding.
                // UTF-8 (lossy, so invalid bytes are kept raw), GBK,
                // Shift_JIS and Windows-1252, as metainfo picks them.
                let id = [0u8, 1, 4, WINDOWS_1252][raw_name.len() % 4];
                let enc = lodestar_metainfo::TextEncoding::from_id(id).unwrap();
                let name = enc.decode(&raw_name).into_owned();
                let plain = id == 0 && name.as_bytes() == raw_name.as_slice();
                let name_raw = (!plain).then(|| (raw_name.clone(), id));
                let files: Vec<_> = files
                    .into_iter()
                    .enumerate()
                    .map(|(i, p)| lodestar_classify::FileInfo::new(vec![p], i as u64 * 1000))
                    .collect();
                let v2 = v2.map(InfoHashV2);
                let key = match v2 {
                    Some(h) if key_v2 && h.truncated().0 != v1 => h.truncated(),
                    _ => InfoHash(v1),
                };
                ResultRecord {
                    key,
                    token: Token(token),
                    torrent: Prepared {
                        info_hash: InfoHash(v1),
                        info_hash_v2: v2,
                        name: name.clone(),
                        name_raw: name_raw.clone(),
                        size: sources & MAX_I64,
                        piece_len: token & MAX_I64,
                        piece_count: rule & MAX_I64,
                        file_count: files.len() as u64,
                        info_size: blob.len() as u64,
                        flags: tflags & (flags::PRIVATE | flags::PADDING | flags::SINGLE_FILE)
                            | if v2.is_some() { flags::V2 } else { 0 }
                            | if v2.is_some_and(|h| h.truncated().0 != v1) {
                                flags::HYBRID
                            } else {
                                0
                            },
                        files: Compressed {
                            codec: if blob.len() % 2 == 0 {
                                Codec::Raw
                            } else {
                                Codec::Zstd
                            },
                            dict_id: (blob.len() % 4 == 1).then_some(at & i64::MAX),
                            data: blob,
                        },
                        extra: private.then(|| b"d1:xi1ee".to_vec()),
                        raw: adult.then(|| b"d4:name1:ae".to_vec()),
                        enrichment: enrich.then(|| enrich_infos(&name, &files).0),
                    },
                    sources,
                    metadata_at: at,
                    action: action.map(|c| Action::from_code(c).unwrap()),
                    rule_version: rule,
                    fix,
                    private_source: private,
                    source_adult: adult,
                }
            },
        )
}

fn arb_counter() -> impl Strategy<Value = Counter> {
    (
        0i64..600_000,
        1u32..=lodestar_pipeline::metric::LAST_CODE,
        0u32..8,
        -3i64..3,
        1i64..1000,
    )
        .prop_map(|(hour, metric, dim, key, value)| Counter {
            hour,
            // The one gauge can't be summed, so it isn't on the wire.
            metric: if metric == lodestar_pipeline::metric::SEARCHABLE {
                lodestar_pipeline::metric::STORED
            } else {
                metric
            },
            dim,
            key,
            value,
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn results_round_trip(batch in any::<u64>(), records in prop::collection::vec(arb_record(), 0..6)) {
        let mut seen = std::collections::HashSet::new();
        let records: Vec<_> = records.into_iter().filter(|r| seen.insert(r.key)).collect();
        let b = ResultsBatch { batch, records };
        let bytes = b.encode().unwrap();
        prop_assert_eq!(ResultsBatch::decode(&bytes).unwrap(), b);
    }

    /// Damage anywhere is refused or decodes to a batch that re-encodes to
    /// exactly the damaged bytes: one canonical form, nothing half-used.
    #[test]
    fn damaged_results_are_refused_or_canonical(
        r in arb_record(),
        at in any::<prop::sample::Index>(),
        byte in any::<u8>(),
    ) {
        let mut bytes = ResultsBatch { batch: 1, records: vec![r] }.encode().unwrap();
        let i = at.index(bytes.len());
        bytes[i] = byte;
        if let Ok(b) = ResultsBatch::decode(&bytes) {
            prop_assert_eq!(b.encode().unwrap(), bytes);
        }
    }

    /// Counters keep every key's sum, in canonical order.
    #[test]
    fn counters_keep_every_sum(batch in any::<u64>(), counters in prop::collection::vec(arb_counter(), 0..64)) {
        let mut sums: HashMap<(i64, u32, u32, i64), i64> = HashMap::new();
        for c in &counters {
            *sums.entry((c.hour, c.metric, c.dim, c.key)).or_default() += c.value;
        }
        sums.retain(|_, v| *v != 0);
        let bytes = CountersBatch { batch, counters }.encode().unwrap();
        let back = CountersBatch::decode(&bytes).unwrap();
        prop_assert_eq!(back.batch, batch);
        let got: HashMap<_, _> = back
            .counters
            .iter()
            .map(|c| ((c.hour, c.metric, c.dim, c.key), c.value))
            .collect();
        prop_assert_eq!(got, sums);
        prop_assert_eq!(back.encode().unwrap(), bytes);
    }

    #[test]
    fn arbitrary_bytes_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        if let Ok(b) = ResultsBatch::decode(&bytes) {
            prop_assert_eq!(b.encode().unwrap(), bytes.clone());
        }
        if let Ok(b) = CountersBatch::decode(&bytes) {
            prop_assert_eq!(b.encode().unwrap(), bytes);
        }
    }
}
