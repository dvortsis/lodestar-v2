//! `relevance --data-dir` over a data directory built through the real
//! store and index APIs (M8-REL-1): hand-computed scores, a strictly
//! read-only open, the refusals, and `--held-out`'s aggregates-only
//! output.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;
use std::time::SystemTime;

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_search::{SearchIndex, index_dir};
use lodestar_store::rusqlite::Connection;
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig};
use tantivy::directory::{Directory, Lock, MmapDirectory};

const EPOCH: i64 = 1_767_225_600; // 2026-01-01T00:00:00Z

/// Invented names; `adult` is classified Adult and hidden by default.
const TORRENTS: &[(&str, &str)] = &[
    ("kestrel", "Kestrel.Harbor.2019.1080p.BluRay.x264-QWRTY"),
    ("marrow", "Marrow.Creek.S01.720p.WEB-DL.x264-QWRTY"),
    ("quillon", "Quillon.Fen.Documentary.2021.720p"),
    ("adult", "Marrow.Creek.XXX.Parody.2020.1080p.MP4-QWRTY.mp4"),
];
/// Judged, but in no store.
const MISSING_1: &str = "0123456789abcdef0123456789abcdef01234567";
const MISSING_2: &str = "fedcba9876543210fedcba9876543210fedcba98";
/// The queries, as typed: none may show up in `--held-out` output.
const QUERIES: &[&str] = &[
    "kestrel harbor",
    "marrow creek",
    "zephyr lantern",
    "orchid vale",
];

struct Fixture {
    _tmp: tempfile::TempDir,
    data: PathBuf,
    hashes: HashMap<&'static str, String>,
}

/// One data directory, shared by the tests that only read it; a test
/// that changes one copies it first.
fn fixture() -> &'static Fixture {
    static F: OnceLock<Fixture> = OnceLock::new();
    F.get_or_init(|| {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        std::fs::create_dir(&data).unwrap();
        let hashes = build(&data);
        Fixture {
            _tmp: tmp,
            data,
            hashes,
        }
    })
}

fn info_dict(name: &str, salt: u8) -> Vec<u8> {
    let mut d: BTreeMap<Vec<u8>, OwnedValue> = BTreeMap::new();
    d.insert(b"length".to_vec(), OwnedValue::Int(700 << 20));
    d.insert(
        b"name".to_vec(),
        OwnedValue::Bytes(name.as_bytes().to_vec()),
    );
    d.insert(b"piece length".to_vec(), OwnedValue::Int(1 << 20));
    let pieces = (0..700 * 20).map(|i| (i as u8) ^ salt).collect();
    d.insert(b"pieces".to_vec(), OwnedValue::Bytes(pieces));
    to_vec(&OwnedValue::Dict(d))
}

/// Stores, enriches and indexes [`TORRENTS`] in `data` as the server lays
/// it out (`lodestar.db`, `index/v…`, `index/.lock`), then closes it all.
fn build(data: &Path) -> HashMap<&'static str, String> {
    let raw: Vec<Vec<u8>> = TORRENTS
        .iter()
        .enumerate()
        .map(|(i, (_, name))| info_dict(name, i as u8))
        .collect();
    let infos: Vec<Info> = raw.iter().map(|r| Info::parse(r).unwrap()).collect();
    let hashes = TORRENTS
        .iter()
        .zip(&infos)
        .map(|((key, _), info)| (*key, hex(info.primary_hash().as_bytes())))
        .collect();
    let mut store = Store::open(data.join("lodestar.db"), StoreConfig::default()).unwrap();
    let batch: Vec<NewTorrent<'_>> = infos
        .iter()
        .enumerate()
        .map(|(i, info)| NewTorrent {
            info,
            raw_info: None,
            discovered_at: EPOCH + i as i64 * 3600,
            metadata_at: EPOCH + i as i64 * 3600 + 5,
            extra_flags: 0,
            source: SourceId::new(0).unwrap(),
        })
        .collect();
    store.insert(&batch).unwrap();
    let mut after = 0;
    while let Some((last, _)) =
        lodestar_pipeline::enrich::enrich_stored(&mut store, after, 1000).unwrap()
    {
        after = last;
    }
    let root = data.join("index");
    std::fs::create_dir_all(&root).unwrap();
    // The server's instance lock, taken and released as a stopped server
    // leaves it.
    drop(server_lock(&root).unwrap());
    let index = SearchIndex::open(&index_dir(&root)).unwrap();
    let mut indexer = index.writer(64 << 20).unwrap();
    while indexer.catch_up(&mut store, 10_000).unwrap() > 0 {}
    indexer.commit().unwrap();
    indexer.finish().unwrap();
    drop(index);
    drop(store);
    hashes
}

/// The lock a serving Lodestar holds on its index root
/// (`lodestar_search::rebuild`'s `lock_root`).
fn server_lock(root: &Path) -> tantivy::Result<tantivy::directory::DirectoryLock> {
    Ok(MmapDirectory::open(root)?.acquire_lock(&Lock {
        filepath: PathBuf::from(".lock"),
        is_blocking: false,
    })?)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A judgments folder: q1 finds kestrel (3) but not quillon (2); q2
/// finds marrow (2), never the hidden adult copy (x), and not a missing
/// torrent (3); q3 grades only a missing torrent and q4 forbids only one,
/// so both are skipped.
fn judgments(f: &Fixture) -> tempfile::TempDir {
    let h = |k: &str| f.hashes[k].clone();
    let text = format!(
        "# invented\n\
         ? {}\n= the film\n3 {}\n2 {}\n\n\
         ? {}\n= the series\n2 {}\nx {}\n3 {}\n\n\
         ? {}\n= nothing stored\n3 {}\n\n\
         ? {}\n= nothing to check\nx {}\n",
        QUERIES[0],
        h("kestrel").to_uppercase(),
        h("quillon"),
        QUERIES[1],
        h("marrow"),
        h("adult"),
        MISSING_1,
        QUERIES[2],
        MISSING_2,
        QUERIES[3],
        MISSING_2,
    );
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("judgments.txt"), text).unwrap();
    dir
}

fn relevance(args: &[&std::ffi::OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_relevance"))
        .args(args)
        .output()
        .unwrap()
}

fn run_on(data: &Path, set: &Path, extra: &[&str]) -> Output {
    let mut args: Vec<&std::ffi::OsStr> = vec![
        "--data-dir".as_ref(),
        data.as_os_str(),
        "--dir".as_ref(),
        set.as_os_str(),
    ];
    args.extend(extra.iter().map(|s| std::ffi::OsStr::new(*s)));
    relevance(&args)
}

fn text(o: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

/// A figure's printed value, e.g. `figure(out, "NDCG@10")`.
fn figure(out: &str, name: &str) -> String {
    out.lines()
        .find_map(|l| l.trim().strip_prefix(name))
        .unwrap_or_else(|| panic!("no {name} in {out}"))
        .trim()
        .to_string()
}

#[test]
fn scores_match_a_hand_computation() {
    let f = fixture();
    let set = judgments(f);
    let o = run_on(&f.data, set.path(), &["--all"]);
    let (out, err) = text(&o);
    assert!(o.status.success(), "{out}\n{err}");
    // q1: kestrel (3) at rank 1; ideal 3 then quillon's 2.
    let q1 = 7.0 / (7.0 + 3.0 / 3f64.log2());
    // q2: the missing torrent is left out, so marrow (2) at rank 1 is ideal.
    let ndcg = (q1 + 1.0) / 2.0;
    assert_eq!(figure(&out, "NDCG@10"), format!("{ndcg:.4}"), "{out}");
    assert_eq!(figure(&out, "MRR"), "1.0000", "{out}");
    // Recall: q1 1 of 2, q2 1 of 1.
    assert_eq!(figure(&out, "recall@50"), "0.7500", "{out}");
    assert_eq!(figure(&out, "forbidden"), "0 queries", "{out}");
    assert_eq!(
        figure(&out, "missing"),
        "2 judged torrents not in this store",
        "{out}"
    );
    assert!(figure(&out, "skipped").starts_with("2 queries"), "{out}");
    assert!(
        out.contains("4 torrents (index v") && out.contains("2 queries judged"),
        "{out}"
    );
    // Results are listed by hash, lowercased.
    assert!(out.contains(&format!("{}:3", f.hashes["kestrel"])), "{out}");
}

/// Every path under `dir`, and each file's length and modification time.
/// A directory is recorded by its presence only: a file created, removed
/// or renamed in it shows in the paths. Its own mtime is left out because
/// Windows can report a stale one from the parent's listing for a moment
/// after its contents change. Files are read with `fs::metadata`, not the
/// listing, for the same reason.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Option<(u64, SystemTime)>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let path = e.unwrap().path();
            let m = std::fs::metadata(&path).unwrap();
            if m.is_dir() {
                stack.push(path.clone());
                out.insert(path, None);
            } else {
                out.insert(path, Some((m.len(), m.modified().unwrap())));
            }
        }
    }
    out
}

#[test]
fn opening_changes_nothing_and_the_baseline_gates() {
    let f = fixture();
    let set = judgments(f);
    let before = snapshot(&f.data);
    let o = run_on(&f.data, set.path(), &["--write-baseline"]);
    assert!(o.status.success(), "{:?}", text(&o));
    let o = run_on(&f.data, set.path(), &["--gate"]);
    let (out, err) = text(&o);
    assert!(o.status.success(), "{out}\n{err}");
    assert!(out.contains("PASS"), "{out}");
    assert_eq!(snapshot(&f.data), before);
    let baseline = std::fs::read_to_string(set.path().join("baseline.txt")).unwrap();
    assert!(baseline.contains("store_torrents 4\n"), "{baseline}");
    assert!(baseline.contains("\nindex v"), "{baseline}");

    // Changed judgments fail the gate.
    let j = set.path().join("judgments.txt");
    let changed = std::fs::read_to_string(&j).unwrap() + "# edited\n";
    std::fs::write(&j, changed).unwrap();
    let o = run_on(&f.data, set.path(), &["--gate"]);
    assert!(!o.status.success());
    assert!(text(&o).0.contains("judgments changed"));
}

/// A copy of the fixture's data directory, for a test that changes it.
fn copy_of(f: &Fixture) -> tempfile::TempDir {
    fn copy(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap() {
            let e = e.unwrap();
            let dest = to.join(e.file_name());
            if e.file_type().unwrap().is_dir() {
                copy(&e.path(), &dest);
            } else {
                std::fs::copy(e.path(), dest).unwrap();
            }
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    copy(&f.data, tmp.path());
    tmp
}

fn refused(o: &Output, needle: &str) {
    let (out, err) = text(o);
    assert!(!o.status.success(), "{out}\n{err}");
    assert!(err.contains(needle), "{needle:?} not in {err}");
}

#[test]
fn refused_while_a_server_holds_it() {
    let f = fixture();
    let set = judgments(f);
    // A copy, so the other tests' runs over the fixture aren't refused.
    let data = copy_of(f);
    let root = data.path().join("index");
    let held = server_lock(&root).unwrap();
    refused(&run_on(data.path(), set.path(), &[]), "in use");
    drop(held);
    assert!(run_on(data.path(), set.path(), &[]).status.success());

    let index = SearchIndex::open(&index_dir(&root)).unwrap();
    let writer = index.writer(64 << 20).unwrap();
    refused(&run_on(data.path(), set.path(), &[]), "in use");
    writer.finish().unwrap();
    drop(index);
    assert!(run_on(data.path(), set.path(), &[]).status.success());
}

#[test]
fn another_schema_or_an_open_log_is_refused() {
    let f = fixture();
    let set = judgments(f);

    let newer = copy_of(f);
    let db = newer.path().join("lodestar.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute(
        "INSERT INTO schema_migrations (version, applied_at) \
         SELECT max(version) + 1, 0 FROM schema_migrations",
        [],
    )
    .unwrap();
    drop(conn);
    refused(&run_on(newer.path(), set.path(), &[]), "never migrates");

    let older = copy_of(f);
    let root = older.path().join("index");
    let current = index_dir(&root);
    std::fs::rename(&current, root.join("v1-t1")).unwrap();
    refused(&run_on(older.path(), set.path(), &[]), "found v1-t1");

    // A server that hasn't written yet has an empty log; a lone -shm is
    // a store that's open or wasn't closed cleanly.
    for (side, body) in [
        ("lodestar.db-wal", &b"not folded in"[..]),
        ("lodestar.db-wal", b""),
        ("lodestar.db-shm", b""),
    ] {
        let logged = copy_of(f);
        std::fs::write(logged.path().join(side), body).unwrap();
        refused(&run_on(logged.path(), set.path(), &[]), "closed cleanly");
    }

    // An index copied without its meta lock: a reader would create it.
    let bare = copy_of(f);
    std::fs::remove_file(index_dir(&bare.path().join("index")).join(".tantivy-meta.lock")).unwrap();
    let before = snapshot(bare.path());
    refused(&run_on(bare.path(), set.path(), &[]), ".tantivy-meta.lock");
    assert_eq!(snapshot(bare.path()), before);

    refused(
        &run_on(&f.data.join("nope"), set.path(), &[]),
        "not a directory",
    );
}

#[test]
fn held_out_prints_aggregates_only() {
    let f = fixture();
    let set = judgments(f);
    let o = run_on(&f.data, set.path(), &["--held-out"]);
    let (out, err) = text(&o);
    assert!(o.status.success(), "{out}\n{err}");
    assert_eq!(figure(&out, "queries"), "2 scored", "{out}");
    assert_eq!(figure(&out, "recall@50"), "0.7500", "{out}");
    assert_eq!(
        figure(&out, "missing"),
        "2 judged torrents not in this store",
        "{out}"
    );
    let all = format!("{out}{err}").to_lowercase();
    let mut secrets: Vec<String> = f.hashes.values().cloned().collect();
    secrets.extend([MISSING_1.into(), MISSING_2.into()]);
    for (_, name) in TORRENTS {
        secrets.push(name.to_lowercase());
        secrets.extend(
            name.split('.')
                .filter(|w| w.len() > 3)
                .map(str::to_lowercase),
        );
    }
    for q in QUERIES {
        secrets.push(q.to_string());
        secrets.extend(q.split(' ').map(str::to_string));
    }
    secrets.extend(["the film".into(), "the series".into()]);
    for s in &secrets {
        assert!(!all.contains(s.as_str()), "{s:?} leaked:\n{out}\n{err}");
    }
    // Nothing was written next to the judgments.
    assert!(!set.path().join("baseline.txt").exists());
}

#[test]
fn held_out_errors_name_only_the_line() {
    let f = fixture();
    let set = tempfile::tempdir().unwrap();
    std::fs::write(
        set.path().join("judgments.txt"),
        "? secret query words\n= secret why\n3 secretnothexhash\n",
    )
    .unwrap();
    let o = run_on(&f.data, set.path(), &["--held-out"]);
    let (out, err) = text(&o);
    assert!(!o.status.success());
    assert_eq!(
        err.trim(),
        "error: judgments.txt:3: expected a 40-hex-digit info hash"
    );
    assert!(!format!("{out}{err}").contains("secret"));

    // A query that doesn't parse: its error would quote it.
    let bad_query = format!("? kestrel sise:1\n= why\n3 {}\n", f.hashes["kestrel"]);
    std::fs::write(set.path().join("judgments.txt"), bad_query).unwrap();
    let o = run_on(&f.data, set.path(), &["--held-out"]);
    let (out, err) = text(&o);
    assert!(!o.status.success());
    assert_eq!(err.trim(), "error: judgments.txt:1: the query failed");
    assert!(!format!("{out}{err}").contains("sise"));

    // Larger than the cap: refused before it's parsed.
    let big = "# padding\n".repeat((16 << 20) / 10 + 1);
    std::fs::write(set.path().join("judgments.txt"), big).unwrap();
    let o = run_on(&f.data, set.path(), &["--held-out"]);
    assert!(text(&o).1.contains("larger than 16 MiB"), "{:?}", text(&o));

    for extra in ["--all", "--gate", "--write-baseline"] {
        let o = run_on(&f.data, set.path(), &["--held-out", extra]);
        assert_eq!(o.status.code(), Some(2), "{extra}");
    }
    let o = run_on(&f.data, set.path(), &["--held-out", "--worst", "3"]);
    assert_eq!(o.status.code(), Some(2));
    // --held-out and --data-dir each need their partner.
    let o = relevance(&["--held-out".as_ref()]);
    assert_eq!(o.status.code(), Some(2));
    let o = relevance(&["--data-dir".as_ref(), f.data.as_os_str()]);
    assert_eq!(o.status.code(), Some(2));
}
