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
    // SQLite's header counts checkpoints in two fields (the file change
    // counter, bytes 24..28, and version-valid-for, 92..96), and when a
    // checkpoint runs depends on timing; every other byte must match.
    let masked = |p: PathBuf| {
        let mut b = std::fs::read(p).unwrap();
        b[24..28].fill(0);
        b[92..96].fill(0);
        b
    };
    let (x, y) = (masked(da.db()), masked(db.db()));
    let differ: Vec<usize> = (0..x.len().min(y.len()))
        .filter(|&i| x[i] != y[i])
        .collect();
    assert!(
        x.len() == y.len() && differ.is_empty(),
        "two builds of one key differ: {} and {} bytes, {} bytes differ, first at {:?}",
        x.len(),
        y.len(),
        differ.len(),
        &differ[..differ.len().min(20)]
    );
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

/// Whether two stores' file-tree blobs that differ hold the same trees:
/// each is decompressed with its own store's dictionary.
fn file_trees(a: &Path, b: &Path) -> String {
    use lodestar_store::rusqlite::{Connection, OpenFlags};
    type Blob = (i64, i64, Option<i64>, Vec<u8>);
    let read = |p: &Path| -> (Vec<Blob>, Vec<(i64, Vec<u8>)>) {
        let c = Connection::open_with_flags(p, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let blobs = c
            .prepare("SELECT torrent_id, codec, dict_id, data FROM torrent_files ORDER BY 1")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let dicts = c
            .prepare("SELECT id, data FROM zstd_dicts ORDER BY 1")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        (blobs, dicts)
    };
    let raw = |(_, codec, dict, data): &Blob, dicts: &[(i64, Vec<u8>)]| -> Vec<u8> {
        if *codec != 1 {
            return data.clone();
        }
        let d = dict
            .and_then(|id| dicts.iter().find(|(i, _)| *i == id))
            .map_or(&[][..], |(_, d)| d.as_slice());
        zstd::bulk::Decompressor::with_dictionary(d)
            .unwrap()
            .decompress(data, 1 << 30)
            .unwrap()
    };
    let ((ba, da), (bb, db)) = (read(a), read(b));
    let (mut blobs, mut trees) = (0, 0);
    let mut first = None;
    for (x, y) in ba.iter().zip(&bb) {
        if x == y {
            continue;
        }
        blobs += 1;
        let same = x.1 == y.1 && x.2 == y.2 && raw(x, &da) == raw(y, &db);
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

/// The store's bytes but for SQLite's two checkpoint counters (see
/// `the_same_key_builds_the_same_bytes`).
fn store_bytes(ds: &dataset::Dataset) -> Vec<u8> {
    let mut b = std::fs::read(ds.db()).unwrap();
    b[24..28].fill(0);
    b[92..96].fill(0);
    b
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
    let bytes = store_bytes(&clean);
    // Resumed at a phase boundary: the same bytes. Resumed inside one
    // (work done, not recorded): the same content; a redone phase may
    // leave SQLite's pages laid out differently.
    let same_content = |d: &dataset::Dataset, what: &str| same_content(d, &clean, what);
    let same = |d: &dataset::Dataset, what: &str| {
        same_content(d, what);
        assert!(store_bytes(d) == bytes, "{what}: the store's bytes differ");
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
        ("size.canonical-store", Some(1024.0)),
        ("size.torrent-index", Some(512.0)),
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
