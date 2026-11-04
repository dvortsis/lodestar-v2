# SRC-3: search `verified:`, `source:` and `network:` fields, unverified hidden by default
**Model:** Opus. Core: search and the query language, an untrusted-input parser. `/security-review`. The runner reviews with a fresh Opus reviewer. Runner brief, 2026-11-04.

## Why
SB-6 (#415) and SB-6b (#418) have defined the v6 fields and the population contract. SRC-3 is their first producer, and it's on the v0.6 sources critical path (SRC-3 → SRC-17 → SRC-G).

## Read first
- `docs/architecture.md` §18.6 (provenance in search; unverified hidden unless asked for) and §8.
- `docs/plan/2026-11-01-v06-sources.md` §4.2 SRC-3.
- `docs/decisions/2026-11-04-SB-6.md` and `2026-11-04-SB-6b.md`: the population contract. SRC-3 is a producer and must ship its fields' backfill under it: the generation, the resumable reindex_log walk, completion and truthful availability.
- `crates/search/src/{schema,doc,query}.rs`, the query-language parser and its fuzz target, and SRC-1 and SRC-2's store APIs for unverified items and claims (`crates/store/src/unverified.rs`, `claims.rs`).

## Do
1. **Fill `verified`, `source` and `network`** from the store for every indexed torrent, through the population contract: a generation bump, then a backfill. Unverified items are indexed so they can be found when asked for.
2. **Query language:**
   - `verified:yes|no`, `source:<id>` and `network:<name>`, with bounded values;
   - **`verified:no` results are hidden by default**: a query without `verified:` adds `verified:yes`;
   - while the field is still backfilling, results say so through the existing availability reporting, and the prefilter never treats an unpopulated value as "no match".
3. **Wire `SearchIndex::unpopulated()`** into `/search/state` and the query notices, as SB-6's review asked.
4. **Tests:**
   - golden query tests;
   - hidden-by-default;
   - backfill restart;
   - `verified:no` explicit;
   - an unknown source or network.
   - Extend the query-language fuzz target and keep it passing.
5. **Decisions file:** `docs/decisions/2026-11-04-SRC-3.md`.

## Own
`crates/search/src/{schema,query,doc}.rs` and their tests, the query fuzz target, `crates/api/src/search.rs` and `crates/api/src/dto/search.rs` (for `/search/state` and the notices), and your decisions file.

**Shared edits** (list them in the PR):
- `openapi.json` and `schema.d.ts`, regenerated, never merged by hand;
- minimal touches to `crates/search/src/index.rs` for the generation. X-merge-policy will later touch only the writer setup in `index.rs`; message the runner before editing it.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass, the search latency and mmap-retention CI jobs stay green, and `/security-review` and a fresh self-review are done.

Branch `slice/SRC-3`. The PR is titled "SRC-3: Search verified/source/network fields, unverified hidden by default". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with `send_message`. Never merge. Push once per round.
