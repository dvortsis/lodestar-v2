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
  - **Cleanup:** a build's runs folder and unfinished set file are removed when the build ends, however it ends (a drop guard). Each build first sweeps what earlier builds left (a crash's runs and sets, a set Windows wouldn't let go yet) except the current set's file. A restart rebuilds the set, as before; the file isn't reused across starts.
- **No title build while the live indexer has a backlog** (runner's condition 6). A catch-up step that comes back full (`added + replayed ≥ batch`, an import's drain) marks the titles behind. Commits meanwhile keep the set dirty, and the first short step starts the build they wanted. Following the store again (a restart, mid-drain or not) clears it and builds at once. During a drain this saves a full read of `torrent_parsed` per rebuild; suggestions catch up when the drain ends.
- **`TitleSet::memory_bytes` counts the FST only when it's in memory;** a mapped set counts its rankings alone.
- **Not changed:** the writer heap, the merge policy, the commit cadence, SQLite's map. All are flat from 1M to 2M; the VM's 10M re-run is the check.

## Handed to the runner
- **`/healthz` at 10M** (X-healthz-cold-start). HTTP binds only after `Services::start` returns, which waits for `Pipeline::start`'s `prepare` (`crates/pipeline/src/lib.rs:1537-1539`): `store.count()` and `for_each_swarm_hash` over every row, to fill the have-filter. That's 1.56 s at 1M and 2.94 s at 2M from a cold page cache here, about 1.4 s per million, so roughly the VM's 10.6 s at 10M.
- **An offline import doesn't invalidate the hot-columns snapshot** (X-import-hot-stamp). The server's first start after an import logs `app.hot_columns torrents=0 from=snapshot` with 990k stored, so imported torrents' swarm stats are missing from the hot columns until an unclean start.

## Measured
MEASURED-TABLE

## Tests
- **`suggest::titles` unit tests:**
  - `the_bounded_build_matches_the_in_memory_one` (property, 6 cases of 9,000–12,000 occurrences) and `the_bounded_build_matches_on_edge_cases`: no titles, one, a tie split across runs, prefixes and non-ASCII, a wide prefix of equal counts.
  - `a_build_in_a_folder_maps_its_file_and_removes_its_runs`;
  - `a_bad_set_file_is_refused`: each bad case above;
  - `a_failed_build_keeps_the_last_set_and_leaves_nothing`;
  - `a_build_sweeps_what_crashed_builds_left`;
  - `builds_wait_while_the_indexer_is_behind_and_keep_the_dirty_flag`: condition waits with 10 s deadlines.
- **`bench/examples/mem-titles.rs`:** N and 2N synthetic torrents, each build in a fresh child process sampling `Anonymous` every millisecond. It fails if the folder build's peak at 2N exceeds its peak at N by more than 8 MiB; the margin's reasons are in the file. It runs by hand.
