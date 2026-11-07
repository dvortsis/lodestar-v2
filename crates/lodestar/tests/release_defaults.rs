//! G2 and G3 of the v0.5.2 manifest
//! (`docs/plan/2026-11-06-v052-feature-manifest.md` §3.3), for the binary:
//! it keeps the pipeline's defaults for adult enrichment (none wired) and
//! the verification cursor (off), and no config key turns the cursor on.
//! `crates/pipeline/tests/release_defaults.rs` pins the defaults
//! themselves; `crates/api/src/ops.rs`'s
//! `not_yet_editable_fields_are_named` pins the API's refusal.
//!
//! The binary builds its `PipelineConfig` in `config.rs` and `services.rs`
//! and has no library target, so the first two tests read its sources:
//! any line that sets either field fails them until the manifest says the
//! feature ships.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Every `.rs` file under `dir`, recursively.
fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs")
            && path
                .file_name()
                .is_some_and(|n| n != "tests.rs" && n != "test_log.rs")
        {
            out.push(path);
        }
    }
}

/// The `file:line`s of the binary's non-test sources that name `field`,
/// once `field` is checked to still be the pipeline's name for it (a
/// rename would make this guard pass vacuously).
fn naming(field: &str) -> Vec<String> {
    let pipeline = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../pipeline/src/lib.rs"),
    )
    .unwrap();
    assert!(
        pipeline.contains(&format!("pub {field}:")),
        "PipelineConfig no longer has `{field}`: point this guard at its new name"
    );
    let mut files = Vec::new();
    sources(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    assert!(files.len() > 20, "found the sources: {files:?}");
    let mut hits = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        for (n, line) in text.lines().enumerate() {
            if line.contains(field) {
                hits.push(format!("{}:{}", f.display(), n + 1));
            }
        }
    }
    hits
}

/// G2: the binary names no adult enricher, so the pipeline's empty
/// default stands and no registered plugin (the torrent cache is of the
/// enrichment kind too) can attach an adult-enrichment section.
#[test]
fn the_binary_wires_no_adult_enrichment_source() {
    assert_eq!(naming("adult_enrichers"), Vec::<String>::new());
}

/// G3: nothing in the binary (config, settings, services) sets the
/// verification cursor, so it stays at the pipeline's `false`.
#[test]
fn the_binary_never_sets_the_verification_cursor() {
    assert_eq!(naming("verify_unverified"), Vec::<String>::new());
}

/// G3: a config file that tries to turn the cursor on is refused at
/// start, not read silently.
#[test]
fn a_config_key_for_the_verification_cursor_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        "[sources]\nverify_unverified = true\n",
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_lodestar"))
        .env("LODESTAR_DATA_DIR", dir.path())
        .env("LODESTAR_HTTP_BIND", "127.0.0.1:0")
        .env("LODESTAR_DHT_ENABLED", "false")
        .env("LODESTAR_SCRAPE_ENABLED", "false")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            child.kill().ok();
            child.wait().ok();
            panic!("the binary started with a verify_unverified key");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut err = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut err).unwrap();
    assert!(!status.success(), "{err}");
    // No config section reaches the cursor: `sources` is an unknown field.
    assert!(
        err.contains("unknown field") && err.contains("sources"),
        "refused for the key, not another reason: {err}"
    );
}
