# B5a: Searcher leases with cursors, cancel tokens, exact count on demand, and the Serving wiring
**Model:** Opus (core: search concurrency). **Branch:** `slice/B5a`.
**Spec:**
- `runner/standards.md` (the bar);
- architecture §8: "Stable pagination", "Cancellation", "Counts", "Result cache" (only what's named below), "Index evolution";
- §9 (API), §10.4 (search screen states: infinite scroll, "≈" counts, exact on demand), `docs/metrics.md` (count and `total_approx` contract);
- §16 entries "M5 decisions (index rebuild)" (B5b's `Serving`, absent fields, `missing_fields()`) and "B1 decisions" (typed DTOs, the 501 pattern, the deferred field list);
- the code: `crates/search/src/{index.rs,query.rs,rebuild.rs,live.rs,lib.rs}`, `crates/api/src/search.rs`, `crates/api/src/dto/search.rs`, `crates/lodestar/src/services.rs`.

## Own
- `crates/search/**`. **Prefer new modules** (`lease.rs`, `cursor.rs`, `cancel.rs`, `count.rs`) and keep edits to `index.rs`, `query.rs`, `compile.rs`, `filter.rs`, `sorted.rs` minimal and local: the M4 PR (#48, not merged, waiting on the owner) rewrites large parts of those files, and it'll be merged after you.
- `crates/api/src/search.rs`, `crates/api/src/dto/search.rs`;
- the regenerated `web/src/lib/api/openapi.json` + `schema.d.ts` (by the tools only, never by hand);
- in `crates/lodestar/src/services.rs`, only the search-wiring lines (F1 #55 also edits that file; keep your change small and merge `main` if it lands first);
- a §16 entry.

## Build
1. **Wire B5b in.** Replace `SearchIndex::open` + `LiveIndexer::spawn` in `services.rs` with `Serving::open_or_rebuild(root, store_path, …)`. `/api` search takes `serving.index()` once per request. Expose `RebuildProgress` (phase, done/total, `unavailable` fields) through the existing status surface only if it's a one-liner; otherwise leave a TODO for B14.
   - **Carry-forward from B5b's review:** if the new live indexer fails to start at the swap, the old one has already been stopped, so the old index serves but stops taking new torrents until restart. Fix it: pass `store_path` + `StoreConfig` into the rebuild thread so it can reopen a store connection and restart the old indexer on failure. Add a test.
2. **Searcher leases.** A lease pins one Tantivy `Searcher` (snapshot) under a random 128-bit id, for 10 minutes from its last use (§8), from whichever index `Serving` was serving when it was taken; a swap never invalidates a live lease (the old index stays mapped until its last lease drops; check that B5b's trash-and-delete waits for that, and fix it if not, with a test).
   - **Bounds:** a cap on live leases (e.g. 1,024) and a per-client cap; past the cap, evict the least recently used. An expired or unknown lease is a typed error the API maps to a clear "results changed, reload" envelope code (e.g. `cursor_expired`, 410).
   - Memory: a lease holds only the searcher handle plus the compiled query and sort; nothing per hit.
3. **Cursors.** The search response carries `next_cursor` (opaque, URL-safe, ≤ 256 bytes): lease id, query fingerprint, sort, and the position after the last hit (score/sort key + doc address tie-break, so it's exact within a snapshot).
   - **Authenticated:** HMAC the cursor with a per-process key (or the instance seal key), so a client can't forge positions or another session's lease; bind it to the session if cheap. A tampered cursor is `bad_cursor` (400).
   - `cursor` and `offset` are mutually exclusive. Keep `offset` (≤ `MAX_WINDOW`) for the first pages and for compatibility; deep paging goes through cursors with no 10,000 window. Within the two-phase ranking's re-rank window, order stays exactly as §8 describes; across windows, document how the cursor continues.
4. **Cancellation.** A `CancelToken` checked between segments (and periodically in long collectors). The API cancels when the client disconnects (axum drops the handler future; make the search run in `spawn_blocking` with the token wired to a drop guard). Test that a cancelled search stops early.
5. **Counts.** Exact up to 100,000 matches; above that, `total` is a segment-level estimate with `total_approx: true` (per metrics.md; `total_approx` is one of B1's deferred fields, now yours). `?exact=1` (or a field in the POST body) computes the exact count on demand, under the cancel token and a time budget, using the lease if one is given.
6. **The deferred hit fields** B1's §16 entry lists as "no slice yet" are **yours where the data already exists in the store**: `title`, `resolved_via`, `content_type`, `adult_kind`, `composition`, `junk_*`, `hidden_junk`. Serve them typed (nullable where the data may be missing) from the same row read the hit already does; if a field needs data the store doesn't have yet, leave it out and say which in the PR. Update metrics.md §7 statuses for what you serve.

## Tests
- Paging through a lease while commits land: no duplicates, no gaps, the same total, across a B5b swap mid-scroll.
- Lease expiry, eviction, unknown and forged cursors, cursor + offset together, a cursor from a different query.
- Cancellation stops work early (measure with a slow fixture).
- Exact-on-demand equals the true count; the estimate is within a documented tolerance on the 200k bench set.
- A property test: random commit/scroll interleavings never yield a duplicate or a skipped id.
- The search-bench gate (`cargo run --release -p lodestar-bench --bin search-bench -- --torrents 200000 --gate`) still passes; add a deep-cursor class if cheap.

## Done when
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` and the search-bench gate pass, after merging `origin/main`.
- `npm run api:types` leaves no diff, and the openapi.json drift job is green.
- The PR description gives the lease memory cost, the cursor format, and the bench numbers before/after.
