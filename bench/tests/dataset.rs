//! The 10M bench mode at a small scale (M8-PERF-1): the persisted
//! dataset is deterministic, refused on a key mismatch and never written
//! by a run, and both bins' `--dataset` runs report every §3 row in the
//! documented JSON. 10M itself can't run in CI; the mode is the same code.

use std::path::{Path, PathBuf};
use std::process::Command;

use lodestar_bench::dataset::{self, Key, Refused, Row, Status};
use lodestar_bench::synth::Generator;
use serde_json::Value;

/// Small, but past the store's 256-sample floor, so a dictionary trains.
const N: u64 = 1_200;

fn build(dir: &Path, key: Key) -> dataset::Dataset {
    dataset::open_or_build(dir, key, &mut |_| {}).expect("build")
}

#[test]
fn the_same_key_builds_the_same_bytes() {
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let key = Key::new(7, N);
    let (da, db) = (build(a.path(), key), build(b.path(), key));
    assert!(da.built_now && db.built_now);
    same_content(&da, &db, "two builds of one key");
    same_bytes(&da.db(), &db.db(), "two builds of one key");
    assert_eq!(da.totals.files, db.totals.files);
    assert!(da.totals.files > N, "about 30 files a torrent");
    for d in [&da, &db] {
        let index =
            lodestar_search::SearchIndex::open(&lodestar_search::index_dir(&d.index_root()))
                .unwrap();
        assert_eq!(index.num_docs(), N);
    }
    // A changed row is named, by table, row and column.
    let copy = tempfile::tempdir().unwrap();
    let changed = copy.path().join("changed.db");
    std::fs::copy(da.db(), &changed).unwrap();
    lodestar_store::rusqlite::Connection::open(&changed)
        .unwrap()
        .execute(
            "UPDATE swarm SET leechers = leechers + 1 WHERE torrent_id = 5",
            [],
        )
        .unwrap();
    let why = dataset::explain_difference(&da.db(), &changed)
        .unwrap()
        .expect("they differ");
    assert!(why.starts_with("swarm (") && why.contains("(5, "), "{why}");
    assert_eq!(
        dataset::explain_difference(&da.db(), &db.db()).unwrap(),
        None
    );
    // Reused, not rebuilt.
    let again = build(a.path(), key);
    assert!(!again.built_now);
    assert_eq!(
        dataset::digest(&again.db()).unwrap(),
        dataset::digest(&da.db()).unwrap()
    );
}

/// Fails, naming where they differ, unless two datasets hold the same
/// content.
fn same_content(a: &dataset::Dataset, b: &dataset::Dataset, what: &str) {
    if dataset::digest(&a.db()).unwrap() != dataset::digest(&b.db()).unwrap() {
        let why = dataset::explain_difference(&a.db(), &b.db()).unwrap();
        panic!(
            "{what}: the content differs: {why:?}; file trees: {}",
            file_trees(&a.db(), &b.db())
        );
    }
}

/// A `torrent_files` row: torrent, codec, dictionary id, blob.
type Blob = (i64, i64, Option<i64>, Vec<u8>);

/// A store's file-tree blobs in torrent order, and a codec holding its
/// dictionaries.
fn blobs(p: &Path) -> (Vec<Blob>, lodestar_store::BlobCodec) {
    use lodestar_store::rusqlite::{Connection, OpenFlags};
    let c = Connection::open_with_flags(p, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let blobs = c
        .prepare("SELECT torrent_id, codec, dict_id, data FROM torrent_files ORDER BY 1")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut codec = lodestar_store::BlobCodec::new(0);
    let mut dicts = c.prepare("SELECT id, data FROM zstd_dicts").unwrap();
    for d in dicts.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap() {
        let (id, data) = d.unwrap();
        codec.add_dict(id, data);
    }
    (blobs, codec)
}

/// The tree a blob holds, `None` when it won't decode.
fn tree(codec: &mut lodestar_store::BlobCodec, (_, kind, dict, data): &Blob) -> Option<Vec<u8>> {
    let kind = lodestar_store::Codec::from_id(*kind)?;
    codec.decompress(kind, *dict, data).ok()
}

/// Whether two stores' file-tree blobs differ, and only in how they
/// encode the same trees: the same torrents, and every blob that differs
/// has the same codec and dictionary id and decodes, with its own store's
/// dictionaries, to the same tree.
fn only_encodings_differ(a: &Path, b: &Path) -> bool {
    let ((ba, mut ca), (bb, mut cb)) = (blobs(a), blobs(b));
    if ba.len() != bb.len() {
        return false;
    }
    let mut differ = false;
    for (x, y) in ba.iter().zip(&bb) {
        if x == y {
            continue;
        }
        differ = true;
        if (x.0, x.1, x.2) != (y.0, y.1, y.2) {
            return false;
        }
        match (tree(&mut ca, x), tree(&mut cb, y)) {
            (Some(t), Some(u)) if t == u => {}
            _ => return false,
        }
    }
    differ
}

/// Whether two stores' file-tree blobs that differ hold the same trees:
/// each is decompressed with its own store's dictionary.
fn file_trees(a: &Path, b: &Path) -> String {
    let ((ba, mut ca), (bb, mut cb)) = (blobs(a), blobs(b));
    let (mut blobs, mut trees) = (0, 0);
    let mut first = None;
    for (x, y) in ba.iter().zip(&bb) {
        if x == y {
            continue;
        }
        blobs += 1;
        let same = (x.0, x.1, x.2) == (y.0, y.1, y.2) && {
            let t = tree(&mut ca, x);
            t.is_some() && t == tree(&mut cb, y)
        };
        if !same {
            trees += 1;
        }
        first.get_or_insert((x.0, x.3.len(), y.3.len(), same));
    }
    format!(
        "{blobs} blobs differ, {trees} of them in the tree itself; first: {first:?} \
         (torrent, sizes, same tree)"
    )
}

/// The store's bytes but for SQLite's header fields that count
/// checkpoints (the file change counter, bytes 24..28, and
/// version-valid-for, 92..96): when a checkpoint runs depends on timing.
fn masked(db: &Path) -> Vec<u8> {
    let mut b = std::fs::read(db).unwrap();
    b[24..28].fill(0);
    b[92..96].fill(0);
    b
}

/// A copy of the store with every file-tree blob replaced by the tree it
/// holds, rebuilt from its content (`VACUUM INTO`), as [`masked`] bytes.
fn trees_only(db: &Path) -> Vec<u8> {
    use lodestar_store::rusqlite::{Connection, params};
    let dir = tempfile::tempdir().unwrap();
    let copy = dir.path().join("copy.db");
    std::fs::copy(db, &copy).unwrap();
    let (rows, mut codec) = blobs(&copy);
    let c = Connection::open(&copy).unwrap();
    for row in rows {
        let tree = tree(&mut codec, &row)
            .unwrap_or_else(|| panic!("torrent {}'s file tree won't decode", row.0));
        c.execute(
            "UPDATE torrent_files SET data = ?2 WHERE torrent_id = ?1",
            params![row.0, tree],
        )
        .unwrap();
    }
    let out = dir.path().join("trees.db");
    c.execute("VACUUM INTO ?1", [out.to_str().unwrap()])
        .unwrap();
    masked(&out)
}

/// Fails unless two stores are the same bytes, but for the checkpoint
/// counters ([`masked`]) and how their file-tree blobs are encoded. zstd
/// has encoded one tree two ways on macOS arm64
/// (`docs/decisions/2026-11-14-X-flake-dataset-macos.md`). Only when
/// file-tree blobs differ, and every one that does encodes the same tree
/// ([`only_encodings_differ`]), are both stores compared rebuilt with
/// their blobs replaced by the trees they hold ([`trees_only`]), since a
/// blob of another length moves the pages after it. Any other difference,
/// page layout included, fails on the bytes.
fn same_bytes(a: &Path, b: &Path, what: &str) {
    let (x, y) = (masked(a), masked(b));
    if x == y {
        return;
    }
    assert!(
        only_encodings_differ(a, b),
        "{what}: the store's bytes differ ({} and {} bytes), and not only in how \
         file trees are encoded; file trees: {}",
        x.len(),
        y.len(),
        file_trees(a, b)
    );
    assert!(
        trees_only(a) == trees_only(b),
        "{what}: the store's bytes differ beyond how file trees are encoded \
         ({} and {} bytes); file trees: {}",
        x.len(),
        y.len(),
        file_trees(a, b)
    );
}

fn set_phase(dir: &Path, phase: &str) {
    let path = dir.join(dataset::MANIFEST);
    let mut m: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    m["phase"] = phase.into();
    std::fs::write(&path, serde_json::to_vec(&m).unwrap()).unwrap();
}

#[test]
fn an_interrupted_build_resumes_to_the_same_dataset() {
    let key = Key::new(7, N);
    let clean = tempfile::tempdir().unwrap();
    let clean = build(clean.path(), key);
    // Resumed at a phase boundary: the same bytes. Resumed inside one
    // (work done, not recorded): the same content; a redone phase may
    // leave SQLite's pages laid out differently.
    let same_content = |d: &dataset::Dataset, what: &str| same_content(d, &clean, what);
    let same = |d: &dataset::Dataset, what: &str| {
        same_content(d, what);
        same_bytes(&d.db(), &clean.db(), what);
    };
    // Stopped after each phase, then finished.
    for phase in ["stored", "enriched", "scraped", "rollups", "indexed"] {
        let dir = tempfile::tempdir().unwrap();
        dataset::build_until(dir.path(), key, &mut |_| {}, Some(phase)).unwrap();
        same(&build(dir.path(), key), &format!("stopped after {phase}"));
    }
    // A crash after the work but before the manifest recorded it: the
    // dictionary isn't trained twice...
    let dir = tempfile::tempdir().unwrap();
    dataset::build_until(dir.path(), key, &mut |_| {}, Some("stored")).unwrap();
    set_phase(dir.path(), "");
    same_content(&build(dir.path(), key), "stored, not recorded");
    // ...and rollups compacted once are never folded again.
    let dir = tempfile::tempdir().unwrap();
    dataset::build_until(dir.path(), key, &mut |_| {}, Some("rollups")).unwrap();
    set_phase(dir.path(), "scraped");
    same_content(&build(dir.path(), key), "rollups, not recorded");
}

#[test]
fn a_dataset_without_this_builds_index_is_reindexed_never_empty() {
    // A search schema or tokenizer bump names a new index folder: the
    // old one is gone from this build's view, as removing it shows.
    let dir = tempfile::tempdir().unwrap();
    let ds = build(dir.path(), Key::new(7, N));
    let index = lodestar_search::index_dir(&ds.index_root());
    std::fs::remove_dir_all(&index).unwrap();
    let again = build(dir.path(), Key::new(7, N));
    assert!(again.built_now);
    let docs = lodestar_search::SearchIndex::open(&index)
        .unwrap()
        .num_docs();
    assert_eq!(docs, N);
}

#[test]
fn an_index_rebuild_cut_short_resumes() {
    let dir = tempfile::tempdir().unwrap();
    let key = Key::new(7, N);
    let ds = build(dir.path(), key);
    let index = lodestar_search::index_dir(&ds.index_root());
    std::fs::remove_dir_all(&index).unwrap();
    // Stopped right after it chose to re-index, as a crash there would.
    dataset::build_until(dir.path(), key, &mut |_| {}, Some("reindex")).unwrap();
    let m: Value =
        serde_json::from_slice(&std::fs::read(dir.path().join(dataset::MANIFEST)).unwrap())
            .unwrap();
    assert_eq!(m["phase"], "rollups", "the rebuild isn't recorded");
    // An empty index folder from the cut-short rebuild: resumed, not refused.
    lodestar_search::SearchIndex::open(&index).unwrap();
    let again = build(dir.path(), key);
    assert!(again.built_now);
    let docs = lodestar_search::SearchIndex::open(&index)
        .unwrap()
        .num_docs();
    assert_eq!(docs, N);
}

/// A run as root never builds: no dataset, or an unfinished one, is an
/// error naming the fix, and nothing is written.
#[test]
fn open_built_refuses_to_build() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("10m");
    let err = dataset::open_built(&missing, Key::new(7, N)).unwrap_err();
    assert!(
        err.to_string().contains("build it first without root"),
        "{err}"
    );
    assert!(!missing.exists());
    dataset::build_until(dir.path(), Key::new(7, N), &mut |_| {}, Some("stored")).unwrap();
    let before = dataset::digest(&dir.path().join(dataset::DB)).unwrap();
    let err = dataset::open_built(dir.path(), Key::new(7, N)).unwrap_err();
    assert!(err.to_string().contains("isn't finished"), "{err}");
    let err = dataset::open_built(dir.path(), Key::new(8, N)).unwrap_err();
    assert!(
        err.to_string().contains("build it first without root"),
        "{err}"
    );
    assert_eq!(
        dataset::digest(&dir.path().join(dataset::DB)).unwrap(),
        before
    );
}

#[test]
fn another_store_schema_is_refused_naming_both() {
    let dir = tempfile::tempdir().unwrap();
    let key = Key::new(7, N);
    let ds = build(dir.path(), key);
    let before = dataset::digest(&ds.db()).unwrap();
    let manifest = dir.path().join(dataset::MANIFEST);
    let mut m: Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    let now = lodestar_store::latest_schema_version();
    m["store_schema"] = (now - 1).into();
    std::fs::write(&manifest, serde_json::to_vec(&m).unwrap()).unwrap();
    let err = dataset::open_or_build(dir.path(), key, &mut |_| {}).unwrap_err();
    let refused = err.downcast_ref::<Refused>().expect("a refusal");
    assert!(
        matches!(refused, Refused::StoreSchema { .. }),
        "{refused:?}"
    );
    let msg = refused.to_string();
    assert!(msg.contains(&format!("store schema {}", now - 1)), "{msg}");
    assert!(msg.contains(&format!("this build's is {now}")), "{msg}");
    assert_eq!(dataset::digest(&ds.db()).unwrap(), before);
}

#[test]
fn a_folder_in_the_data_directory_is_refused_and_never_created() {
    let data = tempfile::tempdir().unwrap();
    let target = data.path().join("bench/10m");
    let run = Command::new(env!("CARGO_BIN_EXE_size-report"))
        .env("LODESTAR_DATA_DIR", data.path())
        .args(["--dataset", target.to_str().unwrap(), "--torrents", "300"])
        .output()
        .unwrap();
    assert!(!run.status.success());
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(stderr.contains("LODESTAR_DATA_DIR"), "{stderr}");
    assert_eq!(
        std::fs::read_dir(data.path()).unwrap().count(),
        0,
        "something was written to the data directory"
    );
}

#[test]
fn the_generator_stream_depends_only_on_its_seed() {
    let take = |seed| {
        Generator::new(seed)
            .take(200)
            .map(|t| t.info)
            .collect::<Vec<_>>()
    };
    assert_eq!(take(3), take(3));
    assert_ne!(take(3), take(4));
}

#[test]
fn another_key_is_refused_naming_both() {
    let dir = tempfile::tempdir().unwrap();
    let key = Key::new(7, N);
    build(dir.path(), key);
    let before = dataset::digest(&dir.path().join(dataset::DB)).unwrap();
    for wanted in [Key::new(8, N), Key::new(7, N + 1)] {
        let err = dataset::open_or_build(dir.path(), wanted, &mut |_| {}).unwrap_err();
        let refused = err.downcast_ref::<Refused>().expect("a refusal");
        assert_eq!(
            refused,
            &Refused::Mismatch {
                dir: dir.path().into(),
                found: key,
                wanted
            }
        );
        let msg = refused.to_string();
        assert!(msg.contains(&key.to_string()), "{msg}");
        assert!(msg.contains(&wanted.to_string()), "{msg}");
    }
    // A dataset an older generator built names that generator.
    let manifest = dir.path().join(dataset::MANIFEST);
    let mut m: Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    m["key"]["generator"] = 0.into();
    std::fs::write(&manifest, serde_json::to_vec(&m).unwrap()).unwrap();
    let msg = dataset::check(dir.path(), key).unwrap_err().to_string();
    assert!(
        msg.contains("generator 0") && msg.contains("generator 1"),
        "{msg}"
    );
    assert_eq!(
        dataset::digest(&dir.path().join(dataset::DB)).unwrap(),
        before,
        "a refusal wrote to the dataset"
    );
}

#[test]
fn a_folder_that_is_not_a_dataset_is_never_written() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("lodestar.db"), b"someone's data").unwrap();
    let err = dataset::open_or_build(dir.path(), Key::new(1, N), &mut |_| {}).unwrap_err();
    assert!(matches!(
        err.downcast_ref::<Refused>(),
        Some(Refused::NotADataset(_))
    ));
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["lodestar.db"]);
    assert_eq!(
        std::fs::read(dir.path().join("lodestar.db")).unwrap(),
        b"someone's data"
    );
}

#[test]
fn a_verdict_passes_only_when_every_row_does() {
    let pass = Row::measured("a", "A", 10.0, "ms", Some(30.0));
    let reported = Row::measured("b", "B", 10.0, "s", None);
    let off = Row::without("c", "C", "ms", Some(1.0), Status::Off("by design".into()));
    assert_eq!(pass.status, Status::Pass);
    assert_eq!(reported.status, Status::Reported);
    assert_eq!(dataset::verdict(&[pass.clone(), reported, off]), "pass");
    let fail = Row::measured("d", "D", 31.0, "ms", Some(30.0));
    assert_eq!(fail.status, Status::Fail);
    assert_eq!(dataset::verdict(&[pass.clone(), fail]), "fail");
    let missing = Row::without("e", "E", "ms", Some(1.0), Status::Unavailable("x".into()));
    assert_eq!(dataset::verdict(&[pass.clone(), missing]), "incomplete");
    for bad in [f64::NAN, -1.0, f64::INFINITY] {
        let r = Row::measured("f", "F", bad, "ms", Some(30.0));
        assert!(matches!(r.status, Status::Unmeasured(_)), "{bad}");
        assert_eq!(dataset::verdict(&[pass.clone(), r]), "incomplete");
    }
}

fn report(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn row<'a>(report: &'a Value, id: &str) -> &'a Value {
    report["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == id)
        .unwrap_or_else(|| panic!("no row {id} in {report}"))
}

fn ids_starting(report: &Value, prefix: &str) -> usize {
    report["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["id"].as_str().unwrap().starts_with(prefix))
        .count()
}

fn size_report(dir: &Path, json: &Path, extra: &[&str]) -> Value {
    let run = Command::new(env!("CARGO_BIN_EXE_size-report"))
        .args([
            "--dataset",
            dir.to_str().unwrap(),
            "--torrents",
            &N.to_string(),
        ])
        .args(["--seed", "7", "--json", json.to_str().unwrap()])
        .args(extra)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    report(json)
}

#[test]
fn size_report_gives_every_size_figure_and_leaves_the_dataset_as_built() {
    let dir = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let ds = build(dir.path(), Key::new(7, N));
    let digest = dataset::digest(&ds.db()).unwrap();

    // The 10M flags are refused without a dataset, so the CI gate's
    // command can't change by accident.
    let refused = Command::new(env!("CARGO_BIN_EXE_size-report"))
        .arg("--low-resource")
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(2));

    // `--gate` on a dataset fails unless the verdict is `pass`: a small
    // corpus is `incomplete` (unpopulated fields) or `fail`, never `pass`.
    let gated = Command::new(env!("CARGO_BIN_EXE_size-report"))
        .args([
            "--dataset",
            dir.path().to_str().unwrap(),
            "--torrents",
            &N.to_string(),
        ])
        .args(["--seed", "7", "--gate"])
        .output()
        .unwrap();
    assert_eq!(
        gated.status.code(),
        Some(1),
        "--gate let a non-pass verdict through"
    );

    let r = size_report(dir.path(), &out.path().join("size.json"), &[]);
    assert_eq!(r["report_format"], dataset::REPORT_FORMAT);
    assert_eq!(r["tool"], "size-report");
    assert_eq!(r["mode"], "default");
    assert_eq!(r["dataset"], Key::new(7, N).to_json());
    assert!(r["machine"]["cores"].as_u64().unwrap() >= 1);
    assert!(r["machine"]["disk"].is_string());
    assert!(r["git"].get("rev").is_some());
    for (id, budget) in [
        ("size.file-tree-blob-mean", Some(600.0)),
        ("size.canonical-store", Some(976.0)),
        ("size.torrent-index", Some(560.0)),
        ("size.file-index-smart", Some(800.0)),
        ("size.store-torrent-index", Some(1536.0)),
        ("size.all-in", None),
    ] {
        let row = row(&r, id);
        assert_eq!(row["budget"].as_f64(), budget, "{id}");
        assert!(row["value"].as_f64().unwrap() > 0.0, "{id}");
    }
    // Fields no producer fills yet are named, never passed over.
    assert!(ids_starting(&r, "size.fields.") > 0);
    // A small corpus may miss a budget too; either way, never a pass.
    assert_ne!(r["verdict"], "pass");

    // Low-resource: no file-index budget; the missing switches say so.
    let r = size_report(
        dir.path(),
        &out.path().join("low.json"),
        &["--low-resource"],
    );
    assert_eq!(r["mode"], "low-resource");
    assert_eq!(row(&r, "size.file-index-smart")["status"], "off");
    assert_eq!(row(&r, "size.store-torrent-index")["budget"], 1536.0);
    assert_eq!(
        row(&r, "low_resource.switch.file_index_off")["status"],
        "unavailable"
    );
    assert_ne!(r["verdict"], "pass");
    assert_eq!(
        dataset::digest(&ds.db()).unwrap(),
        digest,
        "a run wrote to the dataset"
    );
}

fn search_bench(dir: &Path, json: &Path, extra: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_search-bench"))
        .args([
            "--dataset",
            dir.to_str().unwrap(),
            "--torrents",
            &N.to_string(),
        ])
        .args(["--seed", "7", "--queries", "5", "--steady-secs", "1"])
        .args(["--nrt-rounds", "2", "--ingest-rate", "20"])
        .args(["--json", json.to_str().unwrap()])
        .args(extra)
        .output()
        .unwrap()
}

#[test]
fn search_bench_reports_every_row_and_leaves_the_dataset_as_built() {
    let dir = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let ds = build(dir.path(), Key::new(7, N));
    let digest = dataset::digest(&ds.db()).unwrap();
    // What a `--drop-caches` run opens: the finished dataset.
    assert!(
        !dataset::open_built(dir.path(), Key::new(7, N))
            .unwrap()
            .built_now
    );

    // Flags that only mean something on a dataset are refused without
    // one, so the CI gate's command can't change by accident; and a
    // dataset run can't leave rows out.
    let d = dir.path().to_str().unwrap();
    for flag in [
        &["--ingest-rate", "5"][..],
        &["--low-resource"],
        &["--nrt-rounds", "50"],
        &["--steady-secs", "60"],
        &["--drop-caches"],
        &["--dataset", d, "--no-hot"],
        &["--dataset", d, "--only", "regex"],
        &["--dataset", d, "--dir", d],
        &["--dataset", d, "--nrt-rounds", "0"],
    ] {
        let refused = Command::new(env!("CARGO_BIN_EXE_search-bench"))
            .args(flag)
            .output()
            .unwrap();
        assert_eq!(refused.status.code(), Some(2), "{flag:?}");
    }

    let json = out.path().join("search.json");
    let run = search_bench(dir.path(), &json, &[]);
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let r = report(&json);
    assert_eq!(r["report_format"], dataset::REPORT_FORMAT);
    assert_eq!(r["tool"], "search-bench");
    assert_eq!(r["mode"], "default");
    assert_eq!(r["dataset"], Key::new(7, N).to_json());
    assert!(r["machine"]["cores"].as_u64().unwrap() >= 1);
    assert!(r["git"].get("rev").is_some());
    // Every §3 row has a line.
    for prefix in [
        "search.1-common-word.p95",
        "search.browse-cached.p95",
        "search.regex.p95",
        "search.api.",
        "search.recovery.",
        "search.broad-file-cold.p95",
        "search.broad-file-warm.p95",
        "facets.",
        "suggest.",
        "insights.overview.",
        "insights.stages.",
        "cold_start.page-cache-as-found.first_query",
        "nrt.idle.max",
        "nrt.ingest.max",
        "nrt.ingest.p95",
        "rss.steady.max",
    ] {
        assert!(ids_starting(&r, prefix) > 0, "no {prefix} row in {r}");
    }
    assert_eq!(ids_starting(&r, "insights."), 8);
    assert_eq!(row(&r, "search.regex.p95")["budget"], 500.0);
    assert_eq!(row(&r, "nrt.ingest.max")["budget"], 5.0);
    assert!(row(&r, "nrt.ingest.rate")["value"].as_f64().unwrap() > 0.0);
    for id in ["insights.overview.all.p95", "facets.broad-word.p95"] {
        assert!(row(&r, id)["value"].as_f64().unwrap() > 0.0, "{id}");
    }
    // Decimal: "< 1 GB" is a byte under 10⁹, in MB.
    let budget = row(&r, "rss.steady.max")["budget"].as_f64().unwrap();
    assert!((999.9..1000.0).contains(&budget), "{budget}");
    // Not built by the bench's process, so measured (pass or fail).
    let rss = row(&r, "rss.steady.max")["status"].as_str().unwrap();
    assert!(["pass", "fail", "unavailable"].contains(&rss), "{rss}");
    if rss != "unavailable" {
        // Mapped index pages are reported beside the gated figure.
        assert_eq!(row(&r, "rss.steady.anon")["status"], "reported");
    }
    // The trickle is measured, or named as starved; never a silent pass.
    let ingest = row(&r, "nrt.ingest.max")["status"].as_str().unwrap();
    assert!(["pass", "fail", "unmeasured"].contains(&ingest), "{ingest}");
    assert!(
        !dataset::scratch_dir(dir.path()).exists(),
        "scratch left behind"
    );
    assert_eq!(
        dataset::digest(&ds.db()).unwrap(),
        digest,
        "a run wrote to the dataset"
    );

    // Low-resource: file and regex rows are off, the missing switches
    // say so, and the RSS budget is RQ8's. `--gate` on a dataset fails
    // unless the verdict is `pass`, and the report is still written.
    let low = out.path().join("low.json");
    let run = search_bench(dir.path(), &low, &["--low-resource", "--gate"]);
    assert_eq!(
        run.status.code(),
        Some(1),
        "--gate let a non-pass through: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let r = report(&low);
    assert_eq!(r["mode"], "low-resource");
    assert_eq!(row(&r, "search.broad-file-cold.p95")["status"], "off");
    assert_eq!(row(&r, "search.regex.p95")["status"], "off");
    assert_eq!(row(&r, "search.word-file-clause.p95")["status"], "off");
    // None goes missing: `/search`'s and recovery's rows are off too.
    assert_eq!(row(&r, "search.api.regex.p95")["status"], "off");
    assert_eq!(row(&r, "search.api.word-file-clause.p95")["status"], "off");
    assert_eq!(
        row(&r, "search.recovery.word-file-clause.p95")["status"],
        "off"
    );
    assert_eq!(
        row(&r, "low_resource.switch.file_index_off")["status"],
        "unavailable"
    );
    let budget = row(&r, "rss.steady.max")["budget"].as_f64().unwrap();
    assert!((511.9..512.0).contains(&budget), "{budget}");
    assert_ne!(r["verdict"], "pass");
    assert_eq!(dataset::digest(&ds.db()).unwrap(), digest);
}

/// A dataset this process built is still in the page cache and its
/// build's memory: cold start and steady-state RSS say so, never a figure.
#[test]
fn a_run_that_builds_the_dataset_leaves_cold_start_and_rss_unmeasured() {
    let dir = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let json = out.path().join("built.json");
    let run = search_bench(dir.path(), &json, &["--queries", "2"]);
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let r = report(&json);
    for id in ["cold_start.first_query", "rss.steady.max"] {
        assert_eq!(row(&r, id)["status"], "unmeasured", "{id}");
    }
    assert_ne!(r["verdict"], "pass");
}

#[test]
fn a_scratch_copy_is_the_dataset_and_goes_away() {
    let dir = tempfile::tempdir().unwrap();
    let ds = build(dir.path(), Key::new(7, N));
    let scratch = dataset::scratch(&ds).unwrap();
    assert_eq!(
        dataset::digest(&scratch.join(dataset::DB)).unwrap(),
        dataset::digest(&ds.db()).unwrap()
    );
    let index = lodestar_search::index_dir(&scratch.join("index"));
    let docs = lodestar_search::SearchIndex::open(&index)
        .unwrap()
        .num_docs();
    assert_eq!(docs, N);
    dataset::drop_scratch(&ds).unwrap();
    assert!(!dataset::scratch_dir(dir.path()).exists());
    // The dataset still opens as built.
    assert!(!build(dir.path(), Key::new(7, N)).built_now);
}

/// A file tree counts by the tree it holds, not by how it's encoded: one
/// blob encoded another way (another zstd level, the same dictionary)
/// leaves the digest and the store's bytes the same, and a different tree
/// changes both.
#[test]
fn a_file_tree_counts_by_its_tree_not_its_encoding() {
    use lodestar_store::rusqlite::{Connection, params};
    let dir = tempfile::tempdir().unwrap();
    let ds = build(dir.path(), Key::new(7, N));
    let edit = |name: &str, change: &dyn Fn(&mut Vec<u8>), level: i32| -> PathBuf {
        let path = dir.path().join(name);
        std::fs::copy(ds.db(), &path).unwrap();
        let c = Connection::open(&path).unwrap();
        let (dict_id, dict): (i64, Vec<u8>) = c
            .query_row("SELECT id, data FROM zstd_dicts", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        let (id, data): (i64, Vec<u8>) = c
            .query_row(
                "SELECT torrent_id, data FROM torrent_files WHERE codec = 1 ORDER BY torrent_id LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let mut codec = lodestar_store::BlobCodec::new(level);
        codec.add_dict(dict_id, dict);
        codec.set_active(Some(dict_id));
        let mut tree = codec
            .decompress(lodestar_store::Codec::Zstd, Some(dict_id), &data)
            .unwrap();
        change(&mut tree);
        let z = codec.compress(&tree).unwrap();
        assert_eq!(
            (z.codec, z.dict_id),
            (lodestar_store::Codec::Zstd, Some(dict_id))
        );
        assert_ne!(z.data, data, "encoded another way");
        c.execute(
            "UPDATE torrent_files SET data = ?2 WHERE torrent_id = ?1",
            params![id, z.data],
        )
        .unwrap();
        path
    };
    let digest = |p: &Path| dataset::digest(p).unwrap();
    let reencoded = edit("reencoded.db", &|_| {}, 3);
    assert_eq!(digest(&reencoded), digest(&ds.db()));
    same_bytes(&ds.db(), &reencoded, "the same tree encoded another way");
    let changed = edit("changed.db", &|t| *t.last_mut().unwrap() ^= 1, 19);
    assert_ne!(digest(&changed), digest(&ds.db()));
    assert!(trees_only(&changed) != trees_only(&ds.db()));
    let why = dataset::explain_difference(&ds.db(), &changed)
        .unwrap()
        .expect("they differ");
    assert!(why.starts_with("torrent_files ("), "{why}");
}

/// A store whose content is the same but whose pages are laid out
/// differently (a row written to another table and deleted again) isn't
/// the same bytes: the rebuild that excuses file-tree encodings is only
/// for them.
#[test]
#[should_panic(expected = "not only in how file trees are encoded")]
fn a_layout_change_is_not_the_same_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let ds = build(dir.path(), Key::new(7, N));
    let moved = dir.path().join("moved.db");
    std::fs::copy(ds.db(), &moved).unwrap();
    let c = lodestar_store::rusqlite::Connection::open(&moved).unwrap();
    c.execute(
        "INSERT INTO settings (key, value) VALUES ('bench.layout', ?1)",
        [vec![7u8; 64 << 10]],
    )
    .unwrap();
    c.execute("DELETE FROM settings WHERE key = 'bench.layout'", [])
        .unwrap();
    drop(c);
    assert_eq!(
        dataset::explain_difference(&ds.db(), &moved).unwrap(),
        None,
        "the same content"
    );
    assert!(masked(&ds.db()) != masked(&moved), "laid out differently");
    same_bytes(&ds.db(), &moved, "a layout change");
}

/// A file-tree blob that won't decode fails the digest, naming its
/// torrent, rather than counting as its bytes.
#[test]
fn a_file_tree_that_wont_decode_fails_the_digest() {
    let dir = tempfile::tempdir().unwrap();
    let ds = build(dir.path(), Key::new(7, N));
    let broken = dir.path().join("broken.db");
    std::fs::copy(ds.db(), &broken).unwrap();
    lodestar_store::rusqlite::Connection::open(&broken)
        .unwrap()
        .execute(
            "UPDATE torrent_files SET data = x'00010203' WHERE torrent_id = 5",
            [],
        )
        .unwrap();
    let e = dataset::digest(&broken).unwrap_err().to_string();
    assert!(e.contains("torrent 5's file tree won't decode"), "{e}");
    assert!(dataset::digest(&ds.db()).is_ok());
}

/// A dataset that can't be opened still gets its report, naming why.
#[test]
fn a_dataset_that_cannot_be_opened_still_writes_the_report() {
    let out = tempfile::tempdir().unwrap();
    let not_a_dir = out.path().join("not-a-dir");
    std::fs::write(&not_a_dir, b"x").unwrap();
    let json = out.path().join("err.json");
    let run = search_bench(&not_a_dir, &json, &[]);
    assert_eq!(run.status.code(), Some(1));
    let r = report(&json);
    assert_eq!(row(&r, "run")["status"], "unmeasured");
    assert_ne!(r["verdict"], "pass");
}
