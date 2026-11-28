//! `cold-start` (X-healthz-cold-start): how soon a large store answers
//! `/healthz` after a start.
//!
//! `serve` binds the HTTP listener (and so `/healthz`, the UI and search)
//! once `Pipeline::start` returns, so the time to the first `/healthz` is
//! the time `Pipeline::start` takes. This opens a synthetic store of N
//! torrents, starts the pipeline with no sources, and reports:
//!
//! - `start_ms`: `Pipeline::start` (the time to the first `/healthz`);
//! - `filled_ms`: until the have-filter is complete and the intake admits
//!   keys (crawling starts);
//! - `count_ms` and `bound_ms`: `count(*)` and the filter's size bound
//!   alone, for comparison.
//!
//! ```sh
//! cargo run --release -p lodestar-bench --example cold-start -- [TORRENTS] [DIR]
//! ```
//!
//! With `DIR`, the store is built there once and reused, so a second run
//! measures a start from the page cache only. The default is 1,000,000
//! torrents in a temporary directory. Rows are written straight into
//! `torrents` (hashes, a name and the counters a start reads), which is
//! all a start scans; it isn't a realistic library for anything else.

use std::path::{Path, PathBuf};
use std::time::Instant;

use lodestar_pipeline::{Pipeline, PipelineConfig};
use lodestar_store::rusqlite::params;
use lodestar_store::{Store, StoreConfig};

/// Every eighth torrent is a hybrid, so it has two swarm hashes.
const HYBRID_EVERY: u64 = 8;

fn hash(i: u64, salt: u8) -> [u8; 20] {
    // A cheap, well-spread hash: splitmix64 of the index, twice.
    let mut out = [0u8; 20];
    let mut z = i.wrapping_add(u64::from(salt) << 56);
    for chunk in out.chunks_mut(8) {
        z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut x = z;
        x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        x ^= x >> 31;
        let b = x.to_le_bytes();
        chunk.copy_from_slice(&b[..chunk.len()]);
    }
    out
}

fn build(db: &Path, torrents: u64) -> Result<(), Box<dyn std::error::Error>> {
    let store = Store::open(db, StoreConfig::default())?;
    drop(store);
    let mut conn = lodestar_store::rusqlite::Connection::open(db)?;
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO torrents (id, info_hash, info_hash_v2, name, size, piece_len,
                 piece_count, file_count, info_size, flags, discovered_at, metadata_at,
                 last_seen_at)
             VALUES (?1, ?2, ?3, ?4, 1048576, 262144, 4, 1, 200, 0, 1600000000,
                 1600000000, 1600000000)",
        )?;
        for i in 1..=torrents {
            let v2 = (i % HYBRID_EVERY == 0).then(|| {
                let mut h = [0u8; 32];
                h[..20].copy_from_slice(&hash(i, 2));
                h
            });
            stmt.execute(params![
                i as i64,
                &hash(i, 1)[..],
                v2.as_ref().map(|h| &h[..]),
                format!("synthetic torrent {i}"),
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let torrents: u64 = args.next().map_or(Ok(1_000_000), |a| a.parse())?;
    let (dir, _tmp): (PathBuf, Option<tempfile::TempDir>) = match args.next() {
        Some(d) => (PathBuf::from(d), None),
        None => {
            let t = tempfile::tempdir()?;
            (t.path().to_path_buf(), Some(t))
        }
    };
    std::fs::create_dir_all(&dir)?;
    let db = dir.join(format!("cold-start-{torrents}.db"));
    if !db.exists() {
        let t = Instant::now();
        build(&db, torrents)?;
        eprintln!("built {torrents} torrents in {:?}", t.elapsed());
    }

    let reader = Store::open_reader(&db, StoreConfig::default())?;
    let t = Instant::now();
    let counted = reader.count()?;
    let count_ms = t.elapsed().as_millis();
    let t = Instant::now();
    let bound = reader.torrent_bound()?;
    let bound_ms = t.elapsed().as_millis();
    drop(reader);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let started = Instant::now();
        let store = Store::open(&db, StoreConfig::default())?;
        let pipeline = Pipeline::start(PipelineConfig::default(), store, Vec::new()).await?;
        let start_ms = started.elapsed().as_millis();
        pipeline.monitor().have().filled().await;
        let filled_ms = started.elapsed().as_millis();
        println!(
            "{{\"torrents\":{torrents},\"counted\":{counted},\"bound\":{bound},\
             \"start_ms\":{start_ms},\"filled_ms\":{filled_ms},\
             \"count_ms\":{count_ms},\"bound_ms\":{bound_ms}}}"
        );
        pipeline.shutdown().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })
}
