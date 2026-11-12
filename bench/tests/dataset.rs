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
    assert_eq!(
        dataset::digest(&da.db()).unwrap(),
        dataset::digest(&db.db()).unwrap()
    );
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
    // Reused, not rebuilt.
    let again = build(a.path(), key);
    assert!(!again.built_now);
    assert_eq!(
        dataset::digest(&again.db()).unwrap(),
        dataset::digest(&da.db()).unwrap()
    );
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
    let (bytes, digest) = (store_bytes(&clean), dataset::digest(&clean.db()).unwrap());
    // Resumed at a phase boundary: the same bytes. Resumed inside one
    // (work done, not recorded): the same content; a redone phase may
    // leave SQLite's pages laid out differently.
    let same_content = |d: &dataset::Dataset, what: &str| {
        assert_eq!(dataset::digest(&d.db()).unwrap(), digest, "{what}");
    };
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
