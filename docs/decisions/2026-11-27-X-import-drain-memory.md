# X-import-drain-memory decisions

The Magnetico import ladder on the test VM (2026-11-27, main 68799e1b) measured the server's resident memory while it drained an import: 1.005 GB at 1M and **4.45 GB at 10M**, against the §3 budget of under 1 GB of VmRSS. This slice finds what grows and bounds it. The runner's go (2026-11-27T19:58Z) covered proposals 1 and 2 below; `/healthz` and a side finding went to their own slices.

## What grew
Measured with the smoke generator (`docs/operations/imports/pilots/smoke/gen_magnetico.py`, seed 20261102) at 1M and 2M, the offline import, then the server sampled every 0.25 s (`/proc/<pid>/status`, `smaps_rollup`, `smaps` per mapping) from its start until `import status` read `complete`. The build was the shipped one (static musl through `cargo zigbuild`, mimalloc as the C allocator too). A glibc build retains about 400 MB more at 1M in glibc's arenas (the zstd contexts R-mem-triage found), so it isn't used for the figures.

- **The title-suggestion set (B7, `crates/search/src/suggest/titles.rs`) was almost all of the growth.**
  - About 0.8 titles per torrent: 814,473 at 1M, 1,587,713 at 2M.
  - The build held every distinct spelling in a map, then a sorted vector, a merged vector and the FST, all in memory.
  - The previous set stayed live while it built, and the builder rebuilt after commits all through a drain.
  - A sampled heap profile at the 2M peak: the FST being built 160 MiB, the map 95 MiB, the merged vector 48 MiB, everything else about 45 MiB.
  - Steady, the set kept about 87 B per title. Each build added about 125 MB per million titles.
- **Flat from 1M to 2M:** tantivy's writers, merges and document channel (200–250 MB of anonymous memory with the title spikes taken out), the have-filter (2–4 MiB) and SQLite's page cache.
- **File-backed pages are bounded and aren't the growth.** SQLite's map is capped by `mmap_size` (256 MiB; 188 MB resident at 1M), and the index segments' pages come and go with merges.

## Calls
- **The title set is a file beside the index, memory-mapped** (§8: "a title FST kept beside the index"). A directory index's `Titles` builds in `<index>/titles/` (`TITLES_DIR`); a RAM index's still builds in memory.
  - **The build's memory is bounded.** Spellings collect in a map up to `RUN_BYTES` (32 MiB), are sorted into a run and spilled to `<index>/titles/build-<generation>/`. The runs are k-way merged, each spelling's counts added up across runs, grouped into titles and streamed straight into the FST file.
  - **The precomputed rankings are built in the same pass.** A stack holds the prefixes of the current title, each with its count and a 24-entry heap per visibility. A prefix's ranking is kept when it closes with more than `SCAN_CAP` titles. Memory is bounded by the longest title (160 bytes folded) and by the rankings kept.
  - **Results are identical.** A property test compares the bounded build with a brute-force reading of the spec (every whole-char prefix of more than 4,096 titles, its best 24 by count and then key order), at any split into runs, in memory and on disk. It compares every FST entry, every precomputed ranking and lookups for every visibility. Ties at the 24th place are common in it: a mutation that let a tie displace the worst entry fails it.
- **The file is a rebuildable cache, never a source of truth** (runner's conditions 1–3).
  - **Format:** a 32-byte header (magic `LDSTITLE`, format version 1, 4 reserved bytes, the title count, the FST's length), then the FST.
  - **Checks when mapped:** a file that is missing, shorter than the header, foreign, of another version, of another length or with another title count fails the build. The builder then keeps the set it has and tries again after the interval (`search.suggest_titles_failed`), so a bad file never serves and never panics. A full disk fails the build the same way: a warning, the old set kept, and the start and the drain unaffected.
  - **Each build writes a new generation** (`set-<pid>-<nanos>-<n>.fst`), synced to disk before it's mapped. The set owns its file and removes it when it's dropped, after the mapping: Windows refuses to delete a mapped file.
  - **Cleanup:** a build's runs folder and unfinished set file are removed when the build ends, however it ends (a drop guard). Each build first sweeps what earlier builds left (a set Windows wouldn't let go yet) except the current set's file. A truncated run is an error, not a short run.
  - **In-process only, never reused across starts** (runner, 2026-11-27T21:14Z: a refinement of its conditions 1–2, flagged under §2.19). `Titles::at`, when the index opens, sweeps `titles/` before anything maps or serves, and each start rebuilds the set, as main did.
    - "A crash mid-build leaves the last good set usable" holds within the process: a failed build keeps serving the old set.
    - The header checks guard the file each build writes and maps.
    - **Why not reuse it at start:** content safety (owner §2.17: a copy of the data folder mustn't yield deleted torrents). A set written before a deletion would be served again after a restart, and kept on disk, until the first rebuild finished, which takes minutes at 10M.
  - **Deleted titles leave the disk as they leave the index.** A deletion is a commit, so the next build drops the title, and the old generation's file is removed once nothing maps it. That matches the index, where a deleted torrent's name stays in its segment until tantivy merges it; main forces no merge. Quarantined names never reach `torrent_parsed`, so they never reach a set. During a drain the rebuild waits for the indexer, as the index's own merges do. A test deletes a title and finds it in no file under `titles/`.
- **The builder reads the store without a memory map** (`mmap_size` 0 on its own connection). Pages one connection maps count in VmRSS again on top of the other connections' maps of the same file: with a map, the build after a 1M drain put 428 MB of `lodestar.db` in VmRSS.
- **No title build while the live indexer has a backlog** (runner's condition 6). A catch-up step that comes back full of new or changed torrents (`added + replayed ≥ batch`, an import's drain) marks the titles behind. A population backfill isn't counted: it fills every step while its walk lasts, and holding titles back for a whole re-index walk isn't the point. Commits meanwhile keep the set dirty, and the first short step starts the build they wanted. Following the store again (a restart, mid-drain or not) clears it and builds at once. During a drain this saves a full read of `torrent_parsed` per rebuild; suggestions catch up when the drain ends.
- **`TitleSet::memory_bytes` counts the FST only when it's in memory;** a mapped set counts its rankings alone.
- **Disk:** `search_index_bytes` includes `titles/` (`docs/metrics.md`, granted by the runner): the set is about 87 B per title, and a build briefly adds its runs (about every spelling plus 56 B, roughly 0.8 GB at 10M) and a second set. A full disk fails the build only; the old set keeps serving.
- **search-bench's swap row** builds in a folder, as the server does (`bench/src/bin/search-bench.rs`, granted by the runner).
- **Not changed:** the writer heap, the merge policy, the commit cadence, SQLite's map for the server's own connections. All are flat from 1M to 2M; the VM's 10M re-run is the check.

## Handed to the runner
- **`/healthz` at 10M** (X-healthz-cold-start). HTTP binds only after `Services::start` returns, which waits for `Pipeline::start`'s `prepare` (`crates/pipeline/src/lib.rs:1537-1539`): `store.count()` and `for_each_swarm_hash` over every row, to fill the have-filter. That's 1.56 s at 1M and 2.94 s at 2M from a cold page cache here, about 1.4 s per million, so roughly the VM's 10.6 s at 10M.
- **An offline import doesn't invalidate the hot-columns snapshot** (X-import-hot-stamp). The server's first start after an import logs `app.hot_columns torrents=0 from=snapshot` with 990k stored, so imported torrents' swarm stats are missing from the hot columns until an unclean start.

## Measured
This sandbox: Intel Xeon @ 2.10 GHz, 4 vCPU, 16 GB, a virtio block device. The shipped build (static musl, mimalloc override), main@59b0d3e6 before and this branch after. The same imported data folders for both (smoke generator, seed 20261102, a 1% blocklist). Sampled every 0.25 s from the server's start until 40 s after `import status` read `complete`, so the build that follows catch-up is included. One run each, nothing else running.

| | 1M before | 1M after | 2M before | 2M after |
|---|---|---|---|---|
| **Peak VmRSS** | 689 MB | 688 MB | 796 MB | **678 MB** |
| Peak RssAnon | 267 MB | 273 MB | 578 MB | **273 MB** |
| RssFile at the peak | 422 MB | 414 MB | 217 MB | 405 MB |
| VmRSS 40 s after `complete` | 366 MB | 339 MB | 256 MB | 152 MB |
| **Drain** (start to `complete`) | 205.6 s | 198.3 s | 406.2 s | 404.9 s |
| `/healthz` after start | 0.46 s | 0.34 s | 0.61 s | 0.69 s |

- **The peak no longer grows with the import** (688 MB at 1M, 678 MB at 2M). Before, anonymous memory doubled from 1M to 2M (267 to 578 MB) with the title builds.
- **At 1M both peaks are a merge, not the titles:** about 134 s into the drain, anonymous memory plus the index segments' pages. An earlier 1M baseline run peaked at 465 MB of anonymous memory when a title build fell there; where the builds land varies from run to run.
- **What's left at the peak:** about 270 MB of anonymous memory (tantivy's writers and merges), SQLite's map of the store (186 MB at either size, capped at 256 MiB) and the segments a merge is reading. Those are flat from 1M to 2M, but the largest merges grow with the index, so the VM's 10M re-run is the check. Projected from these figures, 10M should peak about 0.7–0.9 GB here.
- **Drain time doesn't get worse:** 3.6% faster at 1M (no rebuilds mid-drain) and the same at 2M (−0.3%).
- **The build alone** (`mem-titles`, glibc build, 1 MiB runs): the folder build peaked at 12.4 MiB at 200k torrents and 13.4 MiB at 400k; main's in-memory build peaked at 58.4 and 96.4 MiB.
- **An earlier run of this branch, with the builder's store connection still memory-mapped,** peaked at 817 MB at 1M: the build after catch-up put 428 MB of `lodestar.db` pages in VmRSS. That's why the builder now reads unmapped.

## Tests
- **`suggest::titles` unit tests:**
  - `the_bounded_build_matches_the_in_memory_one` (property, 6 cases of 9,000–12,000 occurrences) and `the_bounded_build_matches_on_edge_cases` (no titles, one, a tie split across runs, prefixes and non-ASCII, a wide prefix of equal counts). The oracle is main's own `merge_spellings` and `big_prefixes`, kept in the test module.
  - `a_build_from_the_store_spills_and_merges_runs`: the real read path with a run every few spellings, against one run;
  - `a_deleted_title_leaves_every_set_file`: also the start's sweep;
  - `a_build_in_a_folder_maps_its_file_and_removes_its_runs`;
  - `a_bad_set_file_is_refused`: each bad case above;
  - `a_failed_build_keeps_the_last_set_and_leaves_nothing`;
  - `a_build_sweeps_what_crashed_builds_left`;
  - `builds_wait_while_the_indexer_is_behind_and_keep_the_dirty_flag`: condition waits with 10 s deadlines.
- **`live` unit tests:** `a_full_step_is_a_backlog`, and `titles_build_once_a_backlog_is_caught_up` (a backlog read three at a time ends with the set built and holding the last title).
- **`bench/examples/mem-titles.rs`:** N and 2N synthetic torrents (N = 200k by default), each build in a fresh child process sampling `Anonymous` every millisecond, with 1 MiB runs. It fails if the folder build's peak at 2N exceeds its peak at N by more than 8 MiB; the margin's reasons are in the file. It runs by hand.
