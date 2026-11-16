# GS-3: collapse-by-group (best version plus N others)

**Model:** Opus (search ranking, counts and pagination are core). **Branch:** `slice/GS-3`. **PR title:** `GS-3: <title>`.

## Why
GS-2 (#804, nits #817) built the grouping producer: `group_key` (title identity) and `group_minhash` per torrent. GS-3 makes search use it. The owner decided (owner-decisions §2.66 item 2) that **"Group versions" is on by default** once group data is ready, remembered per account and kept in the URL. That UI is GS-5 (Sonnet, after you). GS-3 is the search and API side it needs.

## What to build
1. **Collapse in search** (`crates/search/src/collapse.rs`, new): for a query with `collapse=group`, return one row per `group_key`, the group's best version, plus `others: N` (how many more torrents share the group within this query's matches). "Best" follows the query's own sort: the top-ranked member under relevance or the chosen sort, with a fixed tie-break so results are stable.
2. **Counts** (`count.rs`): report both **groups and torrents**, so the UI can say "128 groups · 412 torrents". Keep the existing rule: exact up to 100k, then "≈" from the segment-level estimate (the groups figure is "≈" too above that line), and exact on demand.
3. **Stable pages** (`cursor.rs`): a collapsed page must never repeat or skip a group across pages, under the searcher lease (leases still expire after 10 min). The cursor carries what it needs to resume a collapsed scan. Old cursors without collapse state still work.
4. **Fallback:** while `group_key` isn't Ready in the population snapshot (SB-6's population contract), search answers **ungrouped** and says so: a machine-readable reason the UI can show in one line. Never an error.
5. **API** (`crates/api/src/search.rs`, `dto/search.rs`): the `collapse` parameter on GET and POST search, `others` on each row, `groups` and `torrents` on the count, and the not-ready reason. Regenerate `openapi.json` and `schema.d.ts` with the repo's tooling (shared generated files: regenerate, never hand-edit).
6. **index.rs** takes one shared hook line only, as the slice notes say.

## Bounds and budgets (not loosened)
- Collapsing must stay inside the search time budget and cancellation rules (architecture §8): check the request token between segments and in the collapse scan, and keep any per-request collapse state bounded (cap the number of groups held per page scan, and say the cap in the decisions file).
- The keyword p95 with collapse on is measured in G-v07-scale, which decides whether the default stays on. Add a bench case for a broad query with collapse on, and report your numbers in the PR. Don't change any budget number.
- Bound memory for a query matching millions of torrents (no unbounded HashMap of all groups).

## Tests
- Unit and property tests: no group appears twice across pages; `others` + 1 sums to the torrent count for a full scan; the best member matches the sort; the tie-break is stable; collapse off gives today's results exactly.
- Pagination across an NRT commit under one lease gives no duplicates or gaps.
- Not-ready fallback answers ungrouped with the reason.
- Count above and below 100k (use the existing estimate test pattern).
- API tests for the new parameter and fields, including an old cursor.
- No fixed sleeps; condition waits need deadlines of at least 10 s.

## Rules
- Owns: `crates/search/src/collapse.rs`, `count.rs`, `cursor.rs`, `crates/search/tests/collapse.rs`, `crates/api/src/search.rs`, `crates/api/src/dto/search.rs`, plus the one hook line in `index.rs`, the regenerated OpenAPI files, a bench case, and `docs/decisions/2026-11-16-GS-3.md`. Ask the runner before touching anything else. Not the web UI: that's GS-5.
- `query.rs` is shared with X-units-typed and RX-1, which aren't running; don't edit it unless you have to, and tell the runner if you do.
- Before the PR: fmt, clippy `-D warnings` (Linux and `x86_64-pc-windows-gnu`), `cargo test -p lodestar-search -p lodestar-api`, the relevance gate, size-report.
- Add your own `runner/slices.yaml` entry and placement rule only if they're missing; edit nothing else in that file. Run `npm test` in `tools/status-connector` if you touch it.
- ≲ 1,500 changed lines; if it grows past that, tell the runner and propose a split (for example counts and cursor first, API second).
- Use the steward skill. Report `ready <sha>` to the runner when green.
