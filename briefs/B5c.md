# B5c: Result cache and `?explain=1`
**Model:** Opus (core search, concurrency). **Branch:** `slice/B5c`.

**Spec:**
- `runner/standards.md` (the bar); `docs/owner-decisions.md`.
- `docs/build-plan.md`: B5c.
- Architecture §8:
  - "**Result cache:** an LRU keyed by (normalized AST, index generation). Filter-only 'browse' queries and the global facet counts are cached and reused until the next commit that affects them."
  - Ranking: two-phase, "All weights live in config, and `?explain=1` returns a per-result score breakdown."
  - B6's note: facets are a "pure function of (lease snapshot, AST, parameters), so B5c can cache it".
- §3 budgets; §16: B5a's (leases, cursors, cancel), B5b's (rebuild/swap), B6's (facets) and B17's (`BaseQuery`, visibility) entries.
- The code: `crates/search/src/{lease.rs,lease/*,query.rs,compile.rs,count.rs,facets.rs,live.rs,handle.rs,cursor.rs}`, `crates/api/src/{search.rs,facets.rs}`.

## Own
- New: `crates/search/src/cache.rs`, `crates/search/src/explain.rs`, `crates/search/tests/cache.rs`, `crates/api/tests/explain.rs`.
- Minimal hooks in the search and facets entry points and where the generation advances; name every hook line in the PR.
- `/search`'s DTO, only to add the optional `explain` block (additive), and the regenerated `openapi.json` + `schema.d.ts` (tools only).
- **B5a polish (carry-forward):** in B5a's tests, replace `cancelled_after_30ms` in the exact-count test with a tripwire (deterministic), use a `wait_for` helper in `expired_leases_are_swept`, and join the rebuild thread if the sweeper spawn fails.
- `docs/decisions/<date>-B5c.md`.
- **Don't edit:** `schema.rs`, the suggest files (B7 runs alongside: coordinate through `main` merges), the filter crate, `web/`. X-flakes (#88) and B4b (#87) also touch `crates/search`; merge `main` when they land.

## Build
1. **Cache.**
   - Key: (normalized AST, sort, visibility, page parameters, **index generation** of the snapshot). Never key on text alone; two requests that see different visibility (Adult shown/hidden, hidden-by-filter) never share an entry.
   - What's cached: first-page result ids + count + approx flag for search, and whole facet responses. Leases still govern later pages: a cache hit must still hand back a lease (or the cursor scheme B5a defines) that reads the same snapshot, so page 2 after a cached page 1 is consistent. Explain this in the PR.
   - Invalidation: a new generation is a miss by construction; old entries age out. "Until the next commit that affects them": state whether you do the simple version (any commit invalidates) or a finer one, and justify it with numbers.
   - **Filter changes** (a new hide rule, a release) must never let a cached response show what's now hidden. Show how visibility changes reach the key (generation bump or a filter epoch) and test it.
   - Bounded memory (entries and bytes), concurrent access without holding a lock across a search, single-flight for identical concurrent misses if cheap. Metrics: hits, misses, evictions, bytes (add rows to `docs/metrics.md` only via the PR description if you can't own it).
2. **`?explain=1`.** Per result: BM25 phase-1 score, each phase-2 feature (phrase/proximity, exact-name, title-match, file contribution cap) and each prior (seeders, freshness, junk, trackers, swarm-consistency), with the weights from config and the final score; stable field names, documented. Explain never changes ranking, bypasses the cache (or is keyed separately), and is capped (first page only, ≤ 100 results). It's available to authenticated users only (like everything else) and never reveals hidden torrents.
3. **Budget:** a cached browse query and cached facets return in ≤ 2 ms p95; uncached paths are unchanged within noise (run `search-bench --gate` before and after and put both in the PR).

## Tests
- Cache: hit/miss by generation; different visibility never shares; a filter change invalidates; eviction bounds; concurrent readers and a commit (no stale result after the generation advances); page 2 consistent after a cached page 1.
- Explain: components sum (or combine) to the final score for a fixture; ordering identical with and without `explain`; capped.
- The B5a polish items deterministic under load (run them 50× locally).

## Done when
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass after merging `origin/main`.
- `cargo run -p lodestar --bin openapi` + `cd web && npm run api:types` leave no diff.
- `search-bench --gate` passes; before/after numbers in the PR.
- A PR titled `B5c: Result cache and explain`, giving the key, invalidation rule, the filter-change path, memory bounds, explain's field list and the hook lines.
