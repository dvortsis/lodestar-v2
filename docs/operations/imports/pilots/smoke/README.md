# G-import-smoke: offline import ladder at 10k and 100k

Runner smoke stages for build-plan §0.6 ("Import scale"), Track I step I1, run 2026-11-02 in a cloud sandbox. They prove the offline ladder end to end (backup, `lodestar import` with the server stopped, start, index drain) and measure every item on §0.6's capacity list, so the owner's 1M and 10M pilots start from a working procedure and known baselines.

**Everything here is synthetic.** The sources are generated from a seed; no real torrent name or hash is used. The 1M and 10M figures are **projections**, marked as such, not measurements.

## Headline

| | Magnetico 10k | Magnetico 100k | Bitmagnet 10k | Line |
|---|---|---|---|---|
| **Import speed, end to end** (write, then launch to job `complete`) | 1,573/s | **3,061/s** | 1,497/s | 2,000/s (IMP-4b) |
| Write time / rate | 2.24 s / 4,470/s | 17.5 s / 5,708/s | 2.43 s / 4,109/s | |
| Launch to job `complete` (start + index drain) | 4.12 s | 15.2 s | 4.25 s | |
| **Bytes per record, all in** | 1,198 B | **1,141 B** | 1,202 B | ≈ 2,300 B (arch. §3) |
| Removal by job | exact, no collateral | exact, no collateral | exact, no collateral | |
| Relevance fixture | NDCG@10 0.9186, PASS | (same binary) | | floor 0.9176 |

- **100k passes the 2,000/s line with a 1.5× margin.** Two earlier runs, timed to searchable rather than to `complete`, gave 3,189/s and 3,165/s.
- **10k misses it, on the index's commit timing rather than throughput.** Launch to searchable took between 1.45 s and 4.3 s across the five recorded 10k runs, depending on where the 2 s commit tick falls (arch. §8: commit every 2 s or 5,000 documents). End to end, that's 1,497–3,069/s. The pass line applies to the pilots, 1M and up.
- **Nothing is over budget at 100k.** The watch items are under "Findings and watch items".

## Where and with what

Every number in this report comes from this setup.

| | |
|---|---|
| CPU | Intel Xeon @ 2.10 GHz (AVX2, AVX-512), **4 vCPU** |
| Memory | 16 GB, no swap; no cgroup CPU or memory limit visible |
| Disk | virtio block device (backing unknown); about 30 GB writable allowance per session |
| OS | Linux 6.18 (cloud sandbox) |
| Build | `main` at `b39912f7`, `cargo build --release -p lodestar` (rustc 1.97.0), store schema V14, bundled SQLite (`libsqlite3-sys` 0.38.2). No `web/build`, so the embedded UI is the stub page |
| Server | `lodestar serve` as root (`PUID=0 PGID=0 LODESTAR_ALLOW_ROOT=1`), `LODESTAR_DHT_ENABLED=false`, `LODESTAR_SCRAPE_ENABLED=false`, bound to 127.0.0.1; default config plus one blocklist file and three name rules |
| Client | the driver, on the same 4 cores, over HTTP on loopback |
| Tools | Python 3.13.18 (SQLite 3.53.1); PostgreSQL 16.15 for the Bitmagnet leg |

- **Cold** means the OS page cache was dropped (`echo 3 > /proc/sys/vm/drop_caches`) before the start.
- **Latencies are measured at the client,** HTTP included.
- **Run 3 is the one reported.** It's the run after the review fixes, with rules and timing to `complete`. Runs 1 and 2 ran before them, with only the blocklist, so their speed figures appear in brackets where they help and their counters aren't comparable.

## Source data

`gen_magnetico.py` (Python, standard library only) writes a magneticod **v3-schema** SQLite database, the layout the IMP-2 adapter's `Layout::V3` fixture builds.

- **Determinism:** seed **20261102**. Infohashes are SHA-1(`lodestar-smoke:<seed>:<index>`). The same arguments give a byte-identical file with SQLite 3.53.1 (checked).
- **Names:** release-style video names, music names and plain phrases, from a made-up vocabulary with a Zipf-like spread, plus about 2 % non-ASCII words.
- **Files:** 45 % single-file; the rest log-normal (median about 8, cap 2,000).
- **Sizes:** log-normal, about 40 MiB median for single files and 4 MiB inside packs.
- **Dates:** `discovered_on` is spread over 2019–2026; 30 % carry swarm counts.
- **Journal mode:** the file is left in DELETE mode, not magneticod's WAL, so the adapter's WAL checks aren't exercised.

| Source | Torrents | Files (mean per torrent) | Size |
|---|---|---|---|
| `m10k` (`gen_magnetico.py m10k.sqlite3 10000`) | 10,000 | 102,401 (10.2) | 8.87 MB (887 B/torrent) |
| `m100k` (`… m100k.sqlite3 100000`) | 100,000 | 1,052,719 (10.5) | 91.4 MB (914 B/torrent) |
| `o10k` (`… o10k.sqlite3 1000 20261102 9500`) | 1,000, 500 shared with `m10k` | 10,610 | 0.95 MB |
| `o100k` (`… o100k.sqlite3 1000 20261102 99500`) | 1,000, 500 shared with `m100k` | 11,663 | 1.01 MB |

- **The `o*` sources** are the removal check's second job. Its shared hashes carry different names, as a second crawler's copy would.
- **The Bitmagnet leg uses the same data.** `gen_bitmagnet.py` writes `m10k`'s hashes, names, files and sizes into Bitmagnet 0.10.0's three importer tables. Single-file torrents get `files_status = single` and no file rows, and `torrent_pieces` gets zero bytes of the right length.

## Procedure

`smoke.py` runs one stage from an empty work folder and prints the aggregates as JSON (`results/`). Raw logs stay in the work folder and aren't committed.

```sh
cargo build --release -p lodestar --bins
cargo build --release -p lodestar-bench --bin relevance
cargo build --release -p lodestar-sources-importers --bin bitmagnet_check
G=docs/operations/imports/pilots/smoke
python3 $G/gen_magnetico.py src/m10k.sqlite3 10000
python3 $G/gen_magnetico.py src/o10k.sqlite3 1000 20261102 9500
python3 $G/smoke.py --bin target/release/lodestar --source src/m10k.sqlite3 \
  --overlap src/o10k.sqlite3 --work w10k > m10k.json
# 100k: the same with 100000 and first index 99500, and fresh work folders
# Bitmagnet 10k (as root, from the repository root):
$G/pg_bitmagnet.sh up src/m10k.sqlite3 /var/tmp/pgsmoke   # cluster, fill, role, checker
python3 $G/smoke.py --bin target/release/lodestar --kind bitmagnet \
  --pg-host /var/tmp/pgsmoke --pg-password-file /var/tmp/pgsmoke/reader_pw \
  --source src/m10k.sqlite3 --overlap src/o10k.sqlite3 --work b10k > b10k.json
$G/pg_bitmagnet.sh down /var/tmp/pgsmoke
target/release/relevance --gate
```

For each stage, the driver:

1. **Starts the server once** on a fresh data folder, so the store and sources exist, and sets the admin password. It then adds **three name rules** through `/api/v1/filters/apply`, each a made-up word: **flag** (in about 1 % of names), **quarantine** (about 0.5 %) and **drop** (about 0.5 %). Then it stops the server.
2. **Adds a blocklist file** to `config.toml` with 1 % of the source's hashes (every 100th). That's the floor.
3. **`lodestar backup --to …`**: the backup the ladder requires before a stage.
4. **`lodestar import inspect`, then `lodestar import start`,** with the server stopped. This is the write time.
5. **Starts the server** and times from its launch:
   - to the drain's end, polling `/api/v1/stats` every 100 ms until `searchable_torrents == stored_torrents` and `index_lag_s == 0`;
   - to the job reading `complete`, polling `lodestar import status` once a second, and once more as soon as the drain ends.

   End to end is write time plus launch to `complete`, as IMP-4b defines it.
6. **Idle:** 15 s, then RSS, `/metrics` (have-filter), `/api/v1/filters/queue`, and 10 Insights rounds (overview plus the Fetch page, the UI's 30 s round). Then it stops and measures sizes: `dbstat` per object, the torrent and file index folders, and the data folder.
7. **Cold restart:** drops the page cache, starts, times `/healthz`, then a first file search (`file:(ext:flac size:>50MB)`).
8. **Query load,** 20 s each:
   - one client;
   - four clients;
   - four clients with an Insights round every second, 30× the UI's rate.

   The query mix is per class: keyword (a mid-frequency token, or a common token plus a mid one), common, rare (≤ 3 names), field (`size:`, `files:`, `ext:`, `largest:`, `discovered:` ranges) and file (`file:(token)`, `file:(ext:flac size:>50MB) token`). Tokens come from the source's names.
9. **`lodestar backup`** of the imported store, then a **restore** into a new folder: copy the backup files in, `lodestar reindex`, start and drain. These are the backup README's steps.
10. **Removal check:**
    1. imports the overlapping Magnetico source as job 2 (with the Bitmagnet leg, it's a second adapter), starts, drains and stops;
    2. reads each job's members from `import_membership`;
    3. runs `lodestar import remove 1 --yes`, starts and drains;
    4. checks that every record only job 1 held is gone, and every record job 2 holds (new or merged) is still stored, with its membership and `import_quality` rows;
    5. confirms 50 sampled hashes each way with `hash:` searches.

`pg_bitmagnet.sh up` does the Bitmagnet leg's setup:
1. `initdb` a PostgreSQL 16 cluster with `SQL_ASCII`, on a socket only.
2. Load the checker fixture's 0.10.0 schema (`crates/sources-importers/tests/fixtures/bitmagnet_check/schema.sql`, without its `not_bitmagnet` table).
3. Fill it with `gen_bitmagnet.py`.
4. Create the runbook's read-only role with `reader-role.sql`, using `scram-sha-256` on the socket.
5. Run `bitmagnet_check --reserve 1GiB`. Every check passed: 10,000 torrents, 97,913 file rows and 10,000 pieces rows; name sample SHA-256 `7218c4b8…`, path sample `f83df03e…`, seed 0.

## Results per stage

Run 3. Bytes per record are the size minus the empty store's, divided by the torrents stored: 9,292 and 94,051, after the floor and the rules held some back.

### Import speed

| | Magnetico 10k | Magnetico 100k | Bitmagnet 10k |
|---|---|---|---|
| `inspect` | 0.011 s | 0.025 s | 0.036 s |
| Write (`import start`) | 2.24 s, 4,470/s (runs 1, 2: 2.23 s, 1.61 s) | 17.5 s, 5,708/s (runs 1, 2: 16.9 s, 17.4 s) | 2.43 s, 4,109/s (without rules: 1.81 s) |
| Launch to `/healthz` | 0.26 s | 0.44 s | 0.40 s |
| Launch to searchable | 4.11 s | 15.14 s | 4.23 s |
| Launch to job `complete` | 4.12 s | 15.15 s | 4.25 s |
| **End to end** | **6.36 s, 1,573/s** | **32.7 s, 3,061/s** | **6.68 s, 1,497/s** |
| Read / new / filtered / quarantined | 10,000 / 9,292 / 357 / 351 | 100,000 / 94,051 / 3,255 / 2,694 | 10,000 / 9,292 / 357 / 351 |
| Adapter findings | none | none | none |

**The two adapters give the same records.** Bitmagnet's write is in the same range as Magnetico's, and the counters are identical. Without rules, Bitmagnet's earlier run took 1.81 s to write and 3.26 s end to end (3,069/s), which shows how much the commit tick moves a 10k figure.

### Size

| Bytes per record | 10k | 100k | Budget (arch. §3) | What it is |
|---|---|---|---|---|
| SQLite tables | 534 | 543 | | `torrent_files` 262, `torrent_parsed` 150, `torrents` 121, filter and quarantine rows ≈ 10 at 100k (14 with their indexes) |
| SQLite indexes | 30 | 36 | | mostly the unique hash index (31) |
| Provenance | 42 | 44 | | `import_quality` 23, `import_membership` 11, `import_membership_job` 10 |
| **SQLite file** | **607** | **623** | ≤ 1,000 | the three above (no free pages, no WAL after a clean stop) |
| **Torrent index** | **360** | **306** | ≤ 500 | |
| **File index** (smart mode) | **231** | **211** | ≤ 800 | |
| Hot columns | 0 | 0 | | swarm columns; empty without scraping |
| **All in** (data folder) | **1,198** | **1,141** | ≈ 2,300 | |

| Other sizes | 10k | 100k |
|---|---|---|
| Source copy (Magnetico file) | 8.87 MB | 91.4 MB |
| Source copy (Bitmagnet database, `pg_database_size`, run 3) | 44.9 MB (4.5 KB/torrent) | not run |
| Backup before the stage (empty store) | 1.56 MB, 0.028 s | 1.56 MB, 0.021 s |
| Backup after import | 7.18 MB, 0.083 s | 59.6 MB, 0.337 s |
| Restore to searchable (copy, reindex, start, drain) | 3.44 s (copy 0.004 s) | 13.3 s (copy 0.027 s) |
| Have-filter (`lodestar_have_bytes`) | 1.31 MB (capacity 1,048,576; 9,292 entries) | 1.31 MB (94,051 entries) |

**Bitmagnet's database is larger because of `torrent_pieces`.** Bitmagnet keeps every torrent's piece hashes (20 bytes a piece) and the importer reads only their length. The real dump's restore space is measured on the import host.

### Filter, review queue and removal

| | 10k | 100k |
|---|---|---|
| Floor (blocklist) hashes | 100: counted `filtered`, no `filter_hits` row | 1,000: the same |
| Drop rule | 257 tombstones | 2,255 tombstones |
| Quarantine rule (the review queue) | 351 in quarantine | 2,694 in quarantine |
| Flag rule | 729 stored and flagged | 7,484 stored and flagged |
| `filter_hits` rows (flag / quarantine / drop) | 1,337 (729 / 351 / 257) | 12,433 (7,484 / 2,694 / 2,255) |
| `filter_hits` and quarantine on disk | 14 B/record (≈ 57 B a hit; ≈ 230 B a quarantined record, sealed name included) | the same |

- **A floor hash gets no `filter_hits` row in either path.** The live crawler refuses one before fetching it (`crates/pipeline/src/lib.rs`, `Loss::Blocked`), and the import's store check refuses it before writing hits. So the import behaves like ingest.
- **The rules hit more records than the word counts suggest.** The flag rule's word is in 952 names but matched 7,484 records: a rule's query matches file names as well as the torrent's name, and multi-file packs repeat a word.
- **`/api/v1/filters/queue` pages at 1,000,** so the queue sizes above are the `quarantine` table's rows.

| Removal | Magnetico 10k | Magnetico 100k | Bitmagnet 10k + Magnetico job 2 |
|---|---|---|---|
| Job 2 (overlap): read / new / merged / filtered / quarantined | 1,000 / 487 / 433 / 18 / 62 | 1,000 / 475 / 459 / 54 / 12 | 1,000 / 487 / 433 / 18 / 62 |
| Members: job 1 / job 2 / both | 9,292 / 920 / 433 | 94,051 / 934 / 459 | 9,292 / 920 / 433 |
| `import remove 1` | 0.63 s: 8,859 deleted, 433 kept (shared) | 7.8 s: 93,592 deleted (≈ 12,060/s), 459 kept (shared) | 0.48 s: 8,859 deleted, 433 kept |
| Job 1's own records left (store / search sample) | 0 / 0 of 50 | 0 / 0 of 50 | 0 / 0 of 50 |
| Job 2's records missing (store / search sample found) | 0 of 920 / 50 of 50 | 0 of 934 / 50 of 50 | 0 of 920 / 50 of 50 |
| Membership rows: job 1 / job 2; `import_quality` rows | 0 / 920; 920 | 0 / 934; 934 | 0 / 920; 920 |
| Stored = searchable after removal | 920, in 2.2 s | 934, in 3.9 s | 920, in 2.1 s |

### Start, memory and search

RSS is the `serve --child` process, which does the work; the `serve` supervisor adds about 16 MB.

| | 10k | 100k |
|---|---|---|
| Cold start to `/healthz` (page cache dropped) | 0.16 s (runs 1, 2: 0.10 s, 0.12 s) | 0.19 s (runs 1, 2: 0.20 s, 0.25 s) |
| First cold file search, then the same again | 31.6 ms, 17.6 ms (runs 1, 2: 18.4 ms, 13.7 ms first) | 20.5 ms, 11.5 ms (runs 1, 2: 22.0 ms, 25.7 ms first) |
| RSS, empty store, idle | 88 MB | 91 MB |
| RSS, idle 15 s after the drain (peak during the drain) | 110 MB (180 MB) | **290 MB (407 MB)**, of which 227 MB is anonymous |
| RSS after a cold restart | 110 MB (anonymous 74 MB) | 191 MB (anonymous 85 MB) |
| Peak RSS under query load, supervisor plus child | 133 MB | 282 MB |

| Search p95 (p50), one client | 10k | 100k | Arch. §3 target (10M, server side) |
|---|---|---|---|
| Keyword | 3.0 ms (2.1) | 4.1 ms (2.8) | < 30 ms |
| Common token | 2.9 ms (2.0) | 3.4 ms (2.3) | |
| Rare token | 2.8 ms (1.9) | 2.8 ms (1.8) | |
| Field / browse | 2.8 ms (1.9) | 2.9 ms (2.1) | < 50 ms |
| File clause | 14.1 ms (9.2) | 21.3 ms (14.5) | < 150 ms |
| Throughput | 235 q/s | 154 q/s | |

| Search p95, four clients (CPU saturated) | 10k | 100k |
|---|---|---|
| Keyword / file / all | 17.0 / 23.1 / 19.3 ms | 26.8 / 40.1 / 32.7 ms |
| Throughput | 438 q/s | 261 q/s |
| All, with an Insights round every second | 19.9 ms | 35.3 ms |

With four clients, the client and the server share the 4 cores, so these figures measure contention, not the index.

### Insights overhead

| | 10k | 100k | Budget |
|---|---|---|---|
| One Insights round (overview + Fetch page), idle | 4.0 ms mean, 16 ms max | 2.8 ms mean, 6 ms max | < 50 ms per stage query |
| Share of the UI's 30 s period | 0.013 % | 0.009 % | ≤ 1 % (`docs/decisions/2026-11-01-G-m5a.md`) |
| A round under four-client load | 24 ms | 32 ms | |
| Four-client search p95 change with a round every second (30× the UI) | +0.6 ms (runs 1, 2: −1.3 ms) | +2.6 ms (runs 1, 2: −1.4 ms) | +≤ 5 % |

- **The operative gate passes by two orders of magnitude.** That's the round's share of the 30 s period.
- **The timed delta isn't a reliable signal.** At 30× the UI's rate on saturated cores it ranges from −6 % to +9 % across runs, which is run-to-run noise. G-m5a found the same, and gates only gross regressions on it.

### Relevance (regression check only)

`target/release/relevance --gate`: **NDCG@10 0.9186**, MRR 0.9247, recall@50 0.9278, against the floor of 0.9176: **PASS**, equal to `bench/relevance/baseline.txt`.

It builds its own synthetic corpus, so it says nothing about ranking on pilot data (§0.6).

## What scales, and projections to 1M and 10M

**These are projections, not measurements.** They're linear from the 100k run unless the row says otherwise.

| Item | Scaling, 10k → 100k | 1M (projected) | 10M (projected) | Basis |
|---|---|---|---|---|
| Write | linear (4.5k–6.2k/s → 5.7k/s) | ≈ 2.9 min | ≈ 29 min | 5,700/s |
| Launch to `complete` | linear past 10k's commit-tick noise | ≈ 2.7 min | ≈ 27 min | ≈ 6,200/s, all in from launch |
| End to end | improves with size | ≈ 5.6 min, ≈ 3,000/s | ≈ 56 min | the two together |
| Store + index on disk | linear (1,198 → 1,141 B) | ≈ 1.1 GB | ≈ 11.4 GB | 1,141 B/record at 10.5 files a torrent |
| Source copy (Magnetico) | linear | ≈ 0.9 GB | ≈ 9.1 GB | 914 B/torrent |
| Backup size / time | linear | ≈ 0.63 GB / ≈ 4 s | ≈ 6.3 GB / ≈ 40 s or more | 634 B/record; `VACUUM INTO` is I/O-bound at size |
| Restore to searchable | linear (reindex) | ≈ 2.4 min | ≈ 24 min | 142 µs/record |
| Removal by job | linear | ≈ 1.4 min | ≈ 14 min | ≈ 12,000/s, 5,000 per step |
| Have-filter | a fixed floor, then linear (code) | 2.5 MB | 25 MB | `capacity_for`: max(2 × stored, 2²⁰) × 10 bits |
| `filter_hits` and quarantine rows | linear in hits, not records | | | ≈ 57 B a hit |
| Cold start to `/healthz` | grows slowly (0.10–0.16 s → 0.19–0.25 s) | not projected | not projected | measure |
| RSS | not linear | not projected | not projected | measure; see below |
| Search p95 | sublinear (keyword 3.0 → 4.1 ms; file 14 → 21 ms) | not projected | not projected | measure |

**Files per torrent aren't the main size driver on their own.** `torrent_files` (262 B) and the file index (211 B) grow with files. Arch. §3's M0 measurement, though, had a 565 B SQLite file per torrent at 31.3 files a torrent, compressed file trees included, against 623 B here at 10.5. Provenance, swarm and filter rows account for the gap. Real data's name lengths and file counts decide the pilots' figure, so it has to be measured.

**What may stop being linear at 1M and 10M:**
- **The unique hash index.** It takes random inserts and is about 31 MB at 1M and 310 MB at 10M, so write speed can fall once it outgrows SQLite's page cache.
- **The search index's segment merges.** Drain speed can fall as segments grow.
- **Memory.** After the drain the server holds near its peak working set (see below).
- **The disk itself.** This sandbox's disk isn't the import host's.

## Findings and watch items

1. **Memory after a large drain stays near its peak.** At 100k the server held 290 MB, 227 MB of it anonymous, 15 s after the drain; a cold restart brings it to 191 MB, 85 MB anonymous.
   - **Likely cause** (not verified): the two index writers' heaps, 256 MB each by design (arch. §8), aren't returned to the OS after the burst.
   - **Why it matters:** it isn't a 100k problem, but the 10M stage should measure RSS both after the drain and after a restart against the < 1 GB budget.
2. **A port already in use logs "Lodestar crashed".** Starting `lodestar serve` while another instance holds its port (`LODESTAR_HTTP_BIND=127.0.0.1:18090`, run twice with different data folders) logs `ERROR app.crash: Lodestar crashed location=crates/pipeline/src/writer.rs:592`, then `serve: binding 127.0.0.1:18090: Address already in use`.
   - **Cause:** the writer's `spawn_blocking(...).await.expect("writer transaction task")` panics when the runtime shuts down under it.
   - **Impact:** it's harmless, but alarming in a user's log.
   - **Not fixed here:** it's in `crates/pipeline/**`, which SRC-14a owns. Reported to the runner.
3. **Not a Lodestar bug: the generator writes `modified_on` explicitly.** In SQLite 3.53, inserting into magneticod's v3 schema without `modified_on` makes the default trigger's UPDATE run before the FTS insert trigger, and SQLite reports the FTS index malformed. magneticod writes the column itself.
4. **build-plan §0.6 cites "§1.6's budgets",** but there's no §1.6 in `build-plan.md`. This report uses the performance and size budgets in `architecture.md` §3.

## Limits of this smoke

- **The data is synthetic:**
  - 10.5 files a torrent;
  - no BEP 47 attributes, private flags or invalid rows (adapter findings are 0);
  - filter traffic comes from three made-up rules, not the default packs.
- **Nothing ran online:** no DHT, no scraping, no concurrent ingest. Online import belongs to B-import-server-2.
- **No blue/green rebuild was measured** (old and new index side by side). That's G-rebuild-10M's job.
- **The client shares the server's cores,** so the four-client latencies are contention figures.
- **The disk is the sandbox's,** and only the page cache was dropped for "cold". The import host's NVMe or HDD will differ.

## Files

- `gen_magnetico.py`: the synthetic Magnetico source.
- `gen_bitmagnet.py`: the same data as Bitmagnet 0.10.0 tables.
- `pg_bitmagnet.sh`: the Bitmagnet leg's throwaway PostgreSQL, with the checker.
- `smoke.py`: the stage driver (`--kind magnetico|bitmagnet`).
- `results/*-run3.json`: the aggregates this report quotes.
- `results/*-run1.json`, `*-run2.json`, `bitmagnet-10k-norules.json`: the runs before the review fixes, quoted only for speed.
