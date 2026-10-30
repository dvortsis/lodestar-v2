# R-gauges: storage and cache size gauges for the v0.5.1 rc

Owner ruling on #189 (2026-10-30, step 5): add cheap gauges to the rc so that a persistent memory or disk ramp in the 24 h Linux soak can be explained. It must not change the crawler lifecycle, the sampler or the endpoint policy (build-plan §0.4); that would force another soak.

## Scope
1. **New `/metrics` gauges** (prefix `lodestar_`, following `crates/lodestar/src/metrics.rs`):
   - `store_db_bytes`: the size of the main SQLite file.
   - `store_wal_bytes`: the size of its `-wal` file; 0 when absent.
   - `search_index_bytes`: the total size of the Tantivy index directory.
   - The in-process caches that can grow, with their current bytes and entries where a bound exists:
     - the metainfo file cache (8 MiB budget);
     - the detail cache;
     - the SQLite page cache (`PRAGMA cache_size` against the pages in use, if cheap), or document why not.
   - Reuse `have_bytes` and `hot_columns_bytes` where they already exist; don't duplicate them.
   - `storage_sampled_seconds`: the Unix time of the last sample.
2. **Sampling:**
   - A background task samples the file sizes every 60 s, using `spawn_blocking` for the directory walk.
   - `/metrics` reads the cached values. It never touches the filesystem on the scrape path.
   - Failures leave the last value and count `storage_sample_errors_total`.
   - The walk is bounded: no symlink following, a capped entry count.
   - The task stops on shutdown with the other services, and it must not delay shutdown.
3. **Collector:** if `docs/soak/tools` (CR-COL and the Windows collector) record a fixed list of metric families, add the new ones, so the Linux soak captures them. Keep the analyzers' tests green.
4. **Docs:**
   - list the gauges wherever `/metrics` families are documented (docs/operations.md or the metrics catalog);
   - add a decisions file `docs/decisions/2026-10-30-R-gauges.md`.

## Constraints
- **No changes to:** the DHT, sources, peer-wire, the sampler, the endpoint policy or the pipeline lifecycle.
- **Read-only, file-level** sampling: no DB queries on the writer connection, and no locks the writer or indexer hold.
- **Tests:**
  - the values against files of known size;
  - WAL absent gives 0;
  - an error keeps the last value and counts;
  - shutdown isn't delayed;
  - render output includes the families once.
  - No fixed sleeps: wait on a condition with a deadline of at least 10 s.
- **Target ≲ 600 changed lines.** Branch `slice/R-gauges` from `main`.
- **The runner cherry-picks the merge onto `release/v0.5.1`** for `v0.5.1-rc.2`, so keep it self-contained: no dependency on post-cut main changes. Check it with `git cherry-pick` onto `origin/release/v0.5.1` locally before opening the PR, and say in the PR whether it applies cleanly.
