# X-import-rate-1m decisions

X-test-vm's 1M import (Magnetico 1M, main@92b181bd, 2026-11-14) ran end to end at 990 torrents/s, against IMP-4b's offline pass line of 2,000/s (`2026-10-31-IMP-4b.md`), while the 100k smoke stage ran at 3,061/s. This slice finds where the per-torrent cost grows with scale and cuts it in the importer and store paths. The pass line isn't lowered (owner number). This file says only what the code now does.

## Where the time went
Measured with G-import-smoke's own generator (`docs/operations/imports/pilots/smoke/gen_magnetico.py`, seed 20261102) at 100k and 1M, and a timing driver that runs the same two phases as `smoke.py`:
- **write:** `lodestar import start` with the server stopped;
- **drain:** server launch until `import status` reads `complete`.

The config also carries a blocklist of 1% of the hashes, as in the smoke stage. Stacks were sampled with `gdb` during each phase (`perf` isn't available).

**The write phase is what grows with scale.** Before this change, its single thread spent its time as follows (25 samples at 1M):
- **11 in the store's commit,** most of it SQLite's WAL work:
  - a checkpoint `fsync` at nearly every commit: the default auto-checkpoint is 1,000 pages, 16 MiB, which is about one batch of the store's 16 KiB pages;
  - `walRewriteChecksums` reads: a batch's dirty pages overflowed the default 2 MiB page cache, spilled into the WAL mid-transaction and were rewritten there.

  Once the file outgrows `mmap_size` (256 MiB, reached at about 400k torrents), the hash and id b-trees' pages are also read again through that 2 MiB cache. And random info hashes touch a new leaf page of the hash index for almost every record once that index has more leaves than a batch has records. All of this grows with the store.
- **9 in zstd,** compressing each file tree at the store's level 19.
- **4 in judging.**

**The drain doesn't grow with scale here:** 6,568/s at 100k, 7,588/s at 1M. Of the server's busy threads during a 1M drain:
- tantivy's indexing and merge threads take two thirds;
- building and tokenizing documents in the indexer's catch-up loop takes most of the rest;
- the store's reads take about 5%, mostly SQLite's shared WAL lock, taken and released for every query.

All of the drain's own work is in `crates/search` (`index.rs`'s catch-up loop and commit cadence), which belongs to X-index-bytes-1m. This slice doesn't touch it; see "Left for the search index".

## Calls
- **The offline import tunes its own store connection** (`Store::set_bulk_writes`, called by `lodestar import start` and `resume` around the driver). Only that connection changes. Nothing is written to the file, and the settings go back to SQLite's and rusqlite's defaults when the driver returns.
  - **Page cache: up to 256 MiB** (`BULK_CACHE_MIB`) instead of 2 MiB, and **never more than `store.mmap_mib`** (256 by default), the memory map the config already allows the store (runner, #961 review). A batch's dirty pages stay in memory until it commits, and the b-trees' inner pages stay cached between batches.
    - The import runs with the server stopped (IMP-4b), so this is memory the server would otherwise hold. The server's own connections keep their settings.
    - **Why the cap:** a NAS or container with a 512 MiB memory limit mustn't be killed mid-import, and again on resume. The review put the import's anonymous peak at about 350–400 MiB with the full cache. Lowering `mmap_mib` lowers both, and `docs/operations.md` says so in one line.
    - The config has no low-resource mode yet (arch. §3 plans one). When it lands, it shrinks this cache through `mmap_mib` or beside it.
  - **WAL auto-checkpoint: 16,384 pages** (`BULK_CHECKPOINT_PAGES`, 256 MiB at 16 KiB pages) instead of 1,000. That's one checkpoint and its `fsync` every few dozen batches instead of every batch, and the WAL stays bounded by it. **This is the largest single gain at 1M:** with 4,096 pages (64 MiB) the 1M write ran at 5,189/s, and with 16,384 at 7,676/s and 7,863/s. `synchronous` stays `NORMAL`, so a committed batch survives a crash of the process as before, and IMP-1's replay from the cursor is unchanged.
    - **On power loss** (not a process crash), WAL mode at `NORMAL` can lose the batches committed since the last checkpoint: up to about 256 MiB of WAL. The cursor commits in the same transaction as its batch, so it's lost with them, and `resume` reads and commits those batches again. Nothing is half-written or skipped (runner, #961 review).
    - **Free space** (`lodestar import inspect`, `start`, `resume`) now also counts the WAL the import may grow before a checkpoint (`BULK_WAL_BYTES`: the checkpoint's pages plus one batch's transaction, about 288 MiB).
  - **Prepared-statement cache: 128** (`BULK_STATEMENTS`) instead of rusqlite's 16. A batch's commit runs more than 16 distinct statements for each record, so they evicted each other and were compiled again record by record. The gain is small: 7,481/s with 16, against 7,676/s and 7,863/s with 128, near run-to-run noise. It costs nothing, so it's kept.
  - **Set before the progress file and its heartbeat start** in `run_job`, so a failure to set it leaves neither behind (runner, #961 review).
  - **Why the CLI and not the driver:** a later online import through the server's writer (IMP-1, F16's 1,000/s line) may reuse `driver::run`. A 256 MiB cache there would count against the server's memory budget, so the driver leaves the connection's settings alone, and only the offline caller sets them.
- **Each batch is prepared and judged on several threads** (`driver.rs`: `prepare_all`, `par_map`).
  - **Threads:** the machine's cores, at most four (`MAX_PREPARE_THREADS`). Past four, the single writer is the limit. `Options::threads` sets it (`0`, the default every caller passes, takes the cores), so a test can compare one thread with four.
  - **Prepare:** each thread has its own `Preparer` (`Store::preparer`, the same dictionaries and level), so the rows are byte for byte what one preparer gives. A test checks that, record for record and in order.
  - **Judge:** the same filter (`Engine`, the sealer) on every thread, reading only.
  - **What stays on the writer thread, in the batch's order:**
    - the store's reads, i.e. the always-adult claims (`Store::adult_finders`);
    - the commit, which is still one transaction per batch;
    - the per-reason counts;
    - the crash points (`Fault::MidBatch` still fires halfway through the batch, before the commit).

    Every IMP decision holds: refusal and filter behaviour, private candidates, conservation of the counters, resumability and cancellation (the stop flag is still checked between batches).
  - **Small batches** (under 64 records) run on the writer alone.
  - **The threads are scoped to each batch,** started and joined per batch. A pool kept for the whole run would save their start-up (microseconds a batch, against milliseconds of work) and is a possible follow-up, for performance only (runner, #961 review).
- **A duplicate write is gone.** `commit_torrent` wrote a new torrent's enrichment after `insert_new` had already written it: one `UPDATE` and one upsert per torrent, twice. It's written once now.

## Measured
This sandbox: Intel Xeon @ 2.10 GHz, 4 vCPU, 16 GB RAM, a virtio block device (backing unknown), Linux 6.18, release build. The same sources and the same driver throughout. Each figure is one run; repeated runs fell within about 4% of each other.

| | 100k write | 100k drain | **100k end to end** | 1M write | 1M drain | **1M end to end** |
|---|---|---|---|---|---|---|
| Before (main@b1758dd3) | 5,333/s | 6,568/s | **2,943/s** | 4,261/s | 7,588/s | **2,729/s** |
| After | 9,874/s | 6,551/s* | **3,912/s*** | 7,863/s | 7,597/s | **3,864/s** |

\* The 100k drain and end-to-end figures are from the build before the statement cache and checkpoint widening. Those settings don't touch the drain, and its write ran at 9,711/s.

- **The write nearly doubles at both scales; its slope with scale stays about the same.** The 1M write is 20% slower than the 100k write, before (5,333 to 4,261) and after (9,874 to 7,863). An intermediate build, before the checkpoint widening, fell 47% (9,711 to 5,189): once preparing left the writer, the checkpoints dominated. Most of what remains is the hash index's random leaves, which is the price of an index on random keys at a batch of 500 records. A larger batch would amortise it, but the batch bounds are IMP-2's.
- **This machine passed 2,000/s before the change** (2,729/s at 1M); X-test-vm didn't (990/s). That VM ran both phases about 2.8 times slower than this sandbox (write 1,541/s, drain about 2,770/s), presumably on a slower disk and fewer cores. Scaled the same way, its write would rise to about 2,800/s, but its drain wouldn't move. **So the 1M end-to-end line on that VM also needs the drain to go faster,** which is the search index's work (below). Re-running the 1M pilot on X-test-vm is the check.
- **CI:** CI runs no 1M import. `driver::tests::throughput_on_a_synthetic_100k_source` stays an ignored measurement, now with the CLI's bulk tuning applied as the CLI applies it, and IMP-4b's line stays as it is.

## Left for the search index
For X-index-bytes-1m, or a slice after it in `crates/search`, which this slice doesn't touch:
- The drain is bound by tantivy's indexing and merges and by the catch-up loop's document building.
- The catch-up loop runs each store read as its own read transaction. Reading a batch of ids inside one transaction would save SQLite's shared WAL lock per query, about 5% of the drain's busy time at 1M.
- The commit cadence for an import's catch-up (arch. §8: every 2 s or 5,000 documents) is another candidate.

## Tests
- **`lodestar-store`:** `bulk_writes_tune_only_this_connection_and_go_back`. The page cache and auto-checkpoint change on this connection only, another connection keeps the defaults, and setting them back restores them.
- **`lodestar-sources-importers`:**
  - `preparing_on_several_threads_matches_one`: 300 records with varied file lists, prepared by four preparers and by one, give the same rows in the same order. Preparers keep no state between batches.
  - `one_thread_and_four_import_the_same` (runner, #961 review): the 1,200-record fixture with your drop, quarantine and flag rules (with a sealer), a floor hash, and records an always-adult source claims, some held only on the adult reading. Imported at one thread and at four, it leaves the same rows, file trees, enrichment, filter hits, tombstones, quarantine (less the sealed bytes, whose nonces are random), provenance and counters. With the threads' results put out of order, it fails.
- **`lodestar-store`:** the bulk-writes test also checks the cache cap: 1,024 MiB is held to 256, 64 stays 64, and 0 is held to SQLite's own 2 MiB.
- **Every existing driver, store and CLI import test passes unchanged,** including the crash-point, pause, refusal, conservation and resumability tests, and the parity test (`cli::import::tests::the_cli_and_the_server_build_the_same_filter`).
