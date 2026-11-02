# G-import-smoke: offline import ladder at 10k and 100k

Runner smoke stages for build-plan §0.6 ("Import scale"), Track I step I1, run 2026-11-02 in a cloud sandbox. They prove the offline ladder end to end (backup, `lodestar import` with the server stopped, start, index drain) and measure every item on §0.6's capacity list, so the owner's 1M and 10M pilots start from a working procedure and known baselines.

**Everything here is synthetic.** The sources are generated from a seed; no real torrent name or hash is used. The 1M and 10M figures below are **projections**, marked as such, not measurements.

## Headline

| | 10k | 100k | Line |
|---|---|---|---|
| **Import speed, end to end** (write + start + index drain) | **1,958/s** (run 1: 1,538/s) | **3,165/s** (run 1: 3,189/s) | 2,000/s (IMP-4b) |
| Write time / rate | 1.61 s / 6,215/s | 17.4 s / 5,761/s | |
| Index drain after write (incl. start) | 3.50 s | 14.2 s (≈ 7,000/s) | |
| **Bytes per record, all in** | 1,242 B | **1,178 B** | ≈ 2,300 B (arch. §3) |
| Removal by job | 0 left, 0 collateral | 0 left, 0 collateral | exact |
| Relevance fixture | NDCG@10 0.9186, PASS | (same binary) | floor 0.9176 |

- **100k passes the 2,000/s line with a 1.6× margin.** 10k misses it (1,538–1,958/s) on fixed costs, not throughput: the server's start and the index's commit cadence (every 2 s or 5,000 documents, arch. §8) are about 3 s whatever the size. That's not a scale problem, and the pass line applies to the pilots (1M and up).
- **Nothing is over budget at 100k.** The watch items are under "Findings and watch items".

## Where and with what

Every number below comes from this setup.

| | |
|---|---|
| CPU | Intel Xeon @ 2.10 GHz (AVX2, AVX-512), **4 vCPU** |
| Memory | 16 GB, no swap; no cgroup CPU or memory limit visible |
| Disk | virtio block device (backing unknown); about 30 GB writable allowance per session |
| OS | Linux 6.18 (cloud sandbox) |
| Build | `main` at `b39912f7`, `cargo build --release -p lodestar` (rustc 1.97.0), store schema V14, bundled SQLite (`libsqlite3-sys` 0.38.2). No `web/build`, so the embedded UI is the stub page |
| Server | `lodestar serve` as root (`PUID=0 PGID=0 LODESTAR_ALLOW_ROOT=1`), `LODESTAR_DHT_ENABLED=false`, `LODESTAR_SCRAPE_ENABLED=false`, bound to 127.0.0.1, default config plus one blocklist file |
| Client | the driver on the same 4 cores, over HTTP on loopback |
| Tools | Python 3.13.18; PostgreSQL 16.15 for the Bitmagnet leg |

- **Cold** means the OS page cache was dropped (`echo 3 > /proc/sys/vm/drop_caches`) before the start.
- **Latencies are measured at the client,** HTTP included.
- **Each stage ran twice** (run 2 is reported; run 1 is in brackets where it matters). Run 1 had no single-client pass and no have-filter sample.

## Source data

`gen_magnetico.py` (Python, standard library only) writes a magneticod **v3-schema** SQLite database, the layout the IMP-2 adapter's `Layout::V3` fixture builds.

- **Determinism:** seed **20261102**. Infohashes are SHA-1(`lodestar-smoke:<seed>:<index>`). The same arguments give a byte-identical file (checked).
- **Names:** release-style video names, music names and plain phrases, from a made-up vocabulary with a Zipf-like spread, plus about 2 % non-ASCII words.
- **Files:** 45 % single-file; the rest log-normal (median about 8, cap 2,000). **Mean 10.5 files per torrent.**
- **Sizes:** log-normal, about 40 MiB median for single files and 4 MiB inside packs.
- **Dates:** `discovered_on` is spread over 2019–2026; 30 % carry swarm counts.

| Source | Torrents | Files | Size |
|---|---|---|---|
| `m10k` (`… m10k.sqlite3 10000`) | 10,000 | 102,401 | 8.87 MB (887 B/torrent) |
| `m100k` (`… m100k.sqlite3 100000`) | 100,000 | 1,052,719 | 91.4 MB (914 B/torrent) |
| `o10k` (`… o10k.sqlite3 1000 20261102 9500`) | 1,000, 500 shared with `m10k` | 10,610 | 0.95 MB |
| `o100k` (`… o100k.sqlite3 1000 20261102 99500`) | 1,000, 500 shared with `m100k` | 11,663 | 1.01 MB |

The `o*` sources exist for the removal check: a second job that shares 500 hashes with the first.

## Procedure

`smoke.py` runs one stage from an empty work folder and prints the aggregates as JSON (`results/`). Raw logs stay in the work folder and aren't committed.

```sh
cargo build --release -p lodestar --bins
cargo build --release -p lodestar-bench --bin relevance
G=docs/operations/imports/pilots/smoke
python3 $G/gen_magnetico.py src/m10k.sqlite3 10000
python3 $G/gen_magnetico.py src/o10k.sqlite3 1000 20261102 9500
python3 $G/smoke.py --bin target/release/lodestar --source src/m10k.sqlite3 \
  --overlap src/o10k.sqlite3 --work w10k --load-secs 20 > results-10k.json
# 100k: the same with 100000, first index 99500, and fresh work and data folders
target/release/relevance --gate
```

For each stage, the driver:

1. **Starts the server once** on a fresh data folder, so the store and sources exist, sets the admin password, and stops it.
2. **Adds a blocklist file** to `config.toml` with 1 % of the source's hashes (every 100th), so the import's filter path runs.
3. **`lodestar backup --to …`**: the backup the ladder requires before a stage.
4. **`lodestar import inspect`, then `lodestar import start --source magnetico --path …`**, with the server stopped. This is the write time.
5. **Starts the server** and polls `/api/v1/stats` every 100 ms until `searchable_torrents == stored_torrents` and `index_lag_s == 0` (the drain), and `lodestar import status` until the job reads `complete`.
6. **Idle:** 15 s, then RSS, `/metrics` (have-filter), `/api/v1/filters`, `/api/v1/filters/queue`, and 10 Insights rounds (overview plus the Fetch page, the UI's 30 s round). Then it stops and measures sizes: `dbstat` per object, the index folder and the data folder.
7. **Cold restart:** drops the page cache, starts, times `/healthz`, then a first file search (`file:(ext:flac size:>50MB)`).
8. **Query load,** 20 s each:
   - one client;
   - four clients;
   - four clients with an Insights round every second, 30× the UI's rate.

   The query mix is per class: keyword (a mid-frequency token, or a common token plus a mid one), common, rare (≤ 3 names), field (`size:`, `files:`, `ext:`, `largest:`, `discovered:` ranges) and file (`file:(token)`, `file:(ext:flac size:>50MB) token`). Tokens come from the source's names.
9. **`lodestar backup`** of the imported store, then a **restore** into a new folder: copy the backup files in, `lodestar reindex`, start and drain. The backup README's steps.
10. **Removal check:**
    1. imports the overlapping source as job 2, starts, drains and stops;
    2. runs `lodestar import remove 1 --yes`, starts and drains;
    3. checks the store and the search for job 1's own hashes (expected gone) and all of job 2's (expected kept, the 500 shared ones included);
    4. confirms 50 sampled hashes each way with `hash:` searches.

**Bitmagnet leg (optional, done at 10k).**
1. `initdb` a PostgreSQL 16.15 cluster with `SQL_ASCII` (socket only).
2. Load the checker fixture's 0.10.0 schema (`crates/sources-importers/tests/fixtures/bitmagnet_check/schema.sql`, without its `not_bitmagnet` table).
3. Fill it from `m10k` with `gen_bitmagnet.py`: the same hashes, names, files and sizes, and `torrent_pieces` of the right length.
4. Create the reader role with the runbook's `reader-role.sql`, with `scram-sha-256` on the socket.
5. Run `bitmagnet_check --reserve 1GiB`, then the same ladder with `--source bitmagnet --host <socket> --password-file … --allow-plaintext-loopback`.

## Results per stage

Run 2. Bytes per record are the size minus the empty store's, divided by the torrents stored (9,900 and 99,000: the blocklist held back 1 %).

### Import speed

| | 10k | 100k | Bitmagnet 10k |
|---|---|---|---|
| `inspect` | 0.012 s | 0.032 s | 0.033 s |
| Write (`import start`) | 1.61 s, 6,215/s (run 1: 2.23 s) | 17.4 s, 5,761/s (run 1: 16.9 s) | 1.81 s, 5,534/s |
| Server start to `/healthz` | 0.23 s | 0.18 s | 0.20 s |
| Searchable, from launch | 3.50 s | 14.2 s | 1.45 s |
| Job reads `complete`, from launch | 3.51 s | 14.3 s | 1.46 s |
| **End to end** | **5.11 s, 1,958/s** | **31.6 s, 3,165/s** | **3.26 s, 3,069/s** |
| Counters | 10,000 read; 9,900 new; 100 filtered | 100,000 read; 99,000 new; 1,000 filtered | 10,000 read; 10,000 new |
| Adapter findings | none | none | none |

**Note:** at 10k, the drain swings between 1.5 and 4.3 s across runs, depending on where the 2 s commit tick falls.

### Size

| Bytes per record | 10k | 100k | What it is |
|---|---|---|---|
| SQLite tables | 546 | 554 | `torrent_files` 282, `torrent_parsed` 150, `torrents` 121 at 100k |
| SQLite indexes | 31 | 31 | mostly the unique hash index |
| Provenance | 43 | 44 | `import_quality` 23, `import_membership` 11, `import_membership_job` 10 |
| SQLite file | 621 | 629 | the three above (no free pages, no WAL after a clean stop) |
| Search index | 620 | 549 | torrent and file indexes |
| Hot columns | 0 | 0 | swarm columns; empty without scraping |
| **All in** (data folder) | **1,242** | **1,178** | budget ≈ 2,300 |

| Other sizes | 10k | 100k |
|---|---|---|
| Source copy (Magnetico file) | 8.87 MB | 91.4 MB |
| Source copy (Bitmagnet database, `pg_database_size`) | 43 MB | not run |
| Backup before the stage (empty store) | 1.56 MB, 0.03–0.09 s | 1.56 MB, 0.03 s |
| Backup after import | 7.65 MB, 0.077 s | 63.3 MB, 0.317 s |
| Restore to searchable (copy, reindex, start, drain) | 3.65 s (copy 0.004 s) | 15.5 s (copy 0.03 s) |
| Have-filter (`lodestar_have_bytes`) | 1.31 MB (capacity 1,048,576; 9,900 entries) | 1.31 MB (99,000 entries) |

**Bitmagnet's database is larger because of `torrent_pieces`.** Bitmagnet keeps every torrent's piece hashes (20 bytes a piece) and the importer reads only their length. The real dump's restore space has to be measured on the import host.

### Filter, review queue and removal

| | 10k | 100k |
|---|---|---|
| Blocklisted hashes in the source | 100 | 1,000 |
| Counted `filtered` | 100 | 1,000 |
| `filter_hits` rows written | **0** | **0** |
| Tombstones, quarantine, review queue | 0, 0, 0 | 0, 0, 0 |
| Job 2 (overlap) | 1,000 read: 500 new, 495 merged, 5 filtered | the same |
| `import remove 1` | 0.68 s: 9,405 deleted, 495 kept (shared) | 8.1 s: 98,505 deleted (≈ 12,100/s), 495 kept (shared) |
| Job 1's own hashes left (store / search sample) | 0 / 0 of 50 | 0 / 0 of 50 |
| Job 2's hashes missing (store / search sample) | 0 of 995 / 50 of 50 found | 0 of 995 / 50 of 50 found |
| Stored = searchable after removal | 995 = 995, in 2.2 s | 995 = 995, in 4.4 s |

**The review queue didn't grow because no synthetic name matches a default rule.** That shows the queue isn't touched by a clean import, not how it behaves on real data. The real pilots measure that.

### Start, memory and search

RSS is the `serve --child` process, which does the work; the `serve` supervisor adds about 16 MB.

| | 10k | 100k |
|---|---|---|
| Cold start to `/healthz` (page cache dropped) | 0.12 s (run 1: 0.10 s) | 0.25 s (run 1: 0.20 s) |
| First cold file search | 13.7 ms; again 7.4 ms | 25.7 ms; again 10.3 ms |
| RSS, empty store, idle | 89 MB | 89 MB |
| RSS, idle 15 s after the drain (peak during the drain) | 108 MB (210 MB) | **326 MB (434 MB)**, of which 265 MB is anonymous |
| RSS after a cold restart | 111 MB (anonymous 68 MB) | 208 MB (anonymous 89 MB) |
| Peak RSS under query load, supervisor plus child | 133 MB | 324 MB |

| Search p95 (p50), one client | 10k | 100k | Arch. §3 target (10M, server side) |
|---|---|---|---|
| Keyword | 3.2 ms (2.1) | 4.0 ms (2.8) | < 30 ms |
| Common token | 2.9 ms (2.0) | 3.2 ms (2.2) | |
| Rare token | 2.8 ms (1.9) | 2.7 ms (1.8) | |
| Field / browse | 2.8 ms (1.9) | 3.1 ms (2.1) | < 50 ms |
| File clause | 14.4 ms (9.7) | 19.2 ms (14.2) | < 150 ms |
| Throughput | 228 q/s | 158 q/s | |

| Search p95, four clients (CPU saturated) | 10k | 100k |
|---|---|---|
| Keyword / file / all | 19.2 / 26.3 / 21.8 ms | 26.8 / 43.2 / 34.8 ms |
| Throughput | 398 q/s | 254 q/s |
| The same with Insights every second, all | 20.5 ms | 33.4 ms |

With four clients, the client and the server share the 4 cores, so these figures measure contention, not the index.

### Insights overhead

| | 10k | 100k | Budget |
|---|---|---|---|
| One Insights round (overview + Fetch page), idle | 3.1 ms mean, 5.4 ms max | 3.1 ms mean, 5.4 ms max | < 50 ms per stage query |
| Share of the UI's 30 s period | 0.01 % | 0.01 % | ≤ 1 % (G-m5a) |
| A round under four-client load | 20 ms | 34 ms | |
| Search p95 change with a round every second (30× the UI) | −1.3 ms | −1.4 ms | +≤ 5 % |

So there's no measurable cost: the change is inside run-to-run noise.

### Relevance (regression check only)

`target/release/relevance --gate`: **NDCG@10 0.9186**, MRR 0.9247, recall@50 0.9278, against the floor of 0.9176: **PASS**, equal to `bench/relevance/baseline.txt`.

It builds its own synthetic corpus, so it says nothing about ranking on pilot data (§0.6).

## What scales, and projections to 1M and 10M

**These are projections, not measurements.** They're linear from 100k unless the row says otherwise.

| Item | Scaling, 10k → 100k | 1M (projected) | 10M (projected) | Basis |
|---|---|---|---|---|
| Write | linear (6.2k/s → 5.8k/s) | ≈ 3 min | ≈ 29 min | 5,800/s |
| Index drain | linear once past the fixed ≈ 3 s | ≈ 2.4 min | ≈ 24 min | ≈ 7,000/s |
| End to end | improves with size (fixed costs amortise) | ≈ 5.3 min, ≈ 3,150/s | ≈ 53 min | sum of the two |
| Store + index on disk | linear (1,242 → 1,178 B) | ≈ 1.2 GB | ≈ 11.8 GB | 1,178 B/record at **10.5 files a torrent** |
| Source copy (Magnetico) | linear | ≈ 0.9 GB | ≈ 9.1 GB | 914 B/torrent |
| Backup size / time | linear | ≈ 0.63 GB / ≈ 3 s | ≈ 6.3 GB / ≈ 30 s or more | `VACUUM INTO`; I/O-bound at size |
| Restore to searchable | linear (reindex) | ≈ 2.6 min | ≈ 26 min | 155 µs/record |
| Removal by job | linear | ≈ 1.4 min | ≈ 14 min | ≈ 12,100/s, 5,000 per step |
| Have-filter | step function (code) | 2.5 MB | 25 MB | `capacity_for`: max(2 × stored, 2²⁰) × 10 bits |
| Cold start to `/healthz` | grows slowly (0.12 → 0.25 s) | not projected | not projected | measure |
| RSS | not linear | not projected | not projected | measure; see below |
| Search p95 | sublinear (keyword 3.2 → 4.0 ms; file 14 → 19 ms) | not projected | not projected | measure |

**Files per torrent drive the size.** `torrent_files` is ≈ 27 B a file here. The size budget (arch. §3) assumes 30 files a torrent. At that average, the file trees alone would add about 520 B a record, to about 1.7 KB all in, and the file index would grow too, so the total could come close to the 2.3 KB budget. Real Magnetico and Bitmagnet data decides it.

**What may stop being linear at 1M and 10M:**
- **The unique hash index.** It takes random inserts and is about 31 MB at 1M and 310 MB at 10M, so write speed can fall once it outgrows SQLite's page cache.
- **The search index's segment merges.** Drain speed can fall as segments grow.
- **Memory.** After the drain the server holds its peak working set (see below).
- **The disk itself.** This sandbox's disk isn't the import host's.

## Findings and watch items

1. **An import records no `filter_hits` row for a hash on the floor.** A blocklist file is the floor, the CS-1 hash list (`cs_hashes`). This is a provenance divergence, not a safety gap: the record is still held back and counted.
   - **Seen:** 100 of 100 and 1,000 of 1,000 blocklisted hashes were counted `filtered`, but `filter_hits`, `tombstones` and `quarantine` stayed at 0.
   - **Cause:** in `crates/store/src/import.rs`, `commit_torrent` calls `store_hold` before `filter::write`, so a hash already in `cs_hashes` returns `Filtered` without writing the engine's hit. The live writer (`crates/store/src/lib.rs`, the ingest batch) writes every batch's hits.
   - **Repro:** add a blocklist file with one hash from a Magnetico source to `config.toml`, run `lodestar import start`, then `SELECT count(*) FROM filter_hits` returns 0.
   - **Not fixed here:** it's in the store, outside the import CLI and this slice's paths. Reported to the runner.
2. **Memory after a large drain stays near its peak.** At 100k the server held 326 MB, 265 MB of it anonymous, 15 s after the drain; a restart brings it to 208 MB, 89 MB anonymous. The index writers' heaps aren't returned.
   - **What drives it:** the two index writers, 256 MB heap each by design (arch. §8).
   - **Why it matters:** it isn't a 100k problem, but the 10M stage should measure RSS both after the drain and after a restart against the < 1 GB budget.
3. **A port already in use logs "Lodestar crashed".** When `serve` can't bind its port, the log shows `app.crash location=crates/pipeline/src/writer.rs:592` before the bind error. The writer's `spawn_blocking(...).await.expect(...)` panics when the runtime shuts down under it.
   - **Impact:** it's harmless, but alarming in a user's log.
   - **Not fixed here:** it's in `crates/pipeline/**`, which SRC-14a owns. Reported to the runner.
4. **Not a Lodestar bug: the generator writes `modified_on` explicitly.** In SQLite 3.53, inserting into magneticod's v3 schema without `modified_on` makes the default trigger's UPDATE run before the FTS insert trigger, and SQLite reports the FTS index malformed. magneticod writes the column itself.
5. **"§1.6 budgets"** in the brief and in build-plan §0.6 have no matching section in `build-plan.md`. This report uses the performance and size budgets in `architecture.md` §3.

## Limits of this smoke

- **The data is synthetic:**
  - no default-filter, quarantine or review-queue traffic;
  - 10.5 files a torrent;
  - no BEP 47 attributes, private flags or invalid rows (adapter findings are 0).
- **Nothing ran online:** no DHT, no scraping, no concurrent ingest. Online import belongs to B-import-server-2.
- **No blue/green rebuild was measured** (old and new index side by side). That's G-rebuild-10M's job.
- **The client shares the server's cores,** so the four-client latencies are contention figures.
- **The disk is the sandbox's,** and only the page cache was dropped for "cold". The import host's NVMe or HDD will differ.

## Files

- `gen_magnetico.py`: the synthetic Magnetico source.
- `gen_bitmagnet.py`: the same data as Bitmagnet 0.10.0 tables.
- `smoke.py`: the stage driver.
- `results/magnetico-{10k,100k}-run{1,2}.json`, `results/bitmagnet-10k.json`: the aggregates this report quotes.
