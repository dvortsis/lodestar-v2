# SB-6: search SCHEMA_VERSION 5 → 6, with the population contract
**Model:** Opus (core: search, on-disk index format). The runner reviews with a fresh Opus reviewer. Runner brief, 2026-11-04.

## Why
Search allows one `SCHEMA_VERSION` bump per milestone (build-plan §1.3). SB-6 is the only 5 → 6 bump. It gates SRC-3 (and so the v0.6 sources gate SRC-G), X-search-files-3 and the 10M import pilot. That makes it the longest path in the plan.

## Read first
- `docs/build-plan.md` §0.2 Track M7 (the SB-6 bullet and its **population contract (r3)** and **cost** sub-bullets), §1.3, and Track I (I1b, I2, I4b).
- `docs/architecture.md` §8 "Index evolution", §18.6 (verified/source/network fields).
- `docs/plan/2026-11-01-v06-sources.md` §4.2 SRC-3.
- `docs/decisions/2026-10-31-X-search-files-2.md` (why the file index needs `size`).
- `crates/search/src/rebuild.rs` (`missing_fields()`, `unavailable` reporting, blue/green), `schema.rs`, `doc.rs`, `index.rs`, `files.rs`, `live.rs` (`reindex_log`).
- **`docs/decisions/2026-11-03-X-mmap-cache-prune.md`** and #406. `index.rs` now keeps `MmapDirectory` clones and prunes their cache after each commit, at open and after a rebuild swap. Keep that intact; your blue/green swap must prune both new directories and drop the old ones whole. `bench/tests/mmap_retention.rs` (the `mmap-retention` CI job) must stay green.

## Size first
Estimate the change before coding. If it's over ~3,000 lines, stop and message the runner with a split. A suggested split:
- **SB-6a:** the fields, the version bump and the blue/green rebuild;
- **SB-6b:** the population generation, the resumable backfill and truthful availability.

The runner confirms the split before you start. Between ~1,500 and ~3,000 lines, keep it whole and state the reason in the PR.

## Do
1. **Schema v6 fields, in one bump:**
   - SRC-3's `verified`, `source` and `network`;
   - `payload_sig` and the grouping fields;
   - the regex trigram field;
   - file category and `pieces_root`;
   - `size` in the file index.

   Fields whose producers don't exist yet are defined and left empty. Don't build the producers.
2. **The population contract,** as build-plan §0.2 Track M7 describes it:
   - a per-field population generation, recorded durably;
   - a resumable `reindex_log` backfill in bounded batches. It survives restarts, deletions and filter changes made while it runs;
   - completion criteria;
   - truthful availability on top of `missing_fields()`/`unavailable`: a query on a partly populated field says so, and a prefilter never treats an unpopulated field as "no match".
3. **SB-6's own rebuild** fills what the store already has: file `size`, `pieces_root` and file category. X-search-files-3 depends on it.
4. **Blue/green:**
   - v5 → v6 with serving during the rebuild;
   - an interrupted resume;
   - the disk high-water mark reported.
5. **Decisions file:** `docs/decisions/2026-11-04-SB-6.md`.

## Own
`crates/search/src/{schema,doc,index,files}.rs`, `crates/search/tests/{index,rebuild}.rs`, and new files under `crates/search/src/` for the backfill and population state (name them in the PR). Also your decisions file.

**Shared-file edits** (`rebuild.rs`, `live.rs`, `lib.rs`, `docs/metrics.md`): keep them minimal and list them in the PR. Don't touch `crates/store/**`. If you need a store change, message the runner first: migrations are queued.

## Done when
These pass:
- `cargo fmt --check`;
- `cargo clippy --workspace --all-targets -- -D warnings`;
- `cargo test --workspace`;
- `cargo test --release -p lodestar-bench --test mmap_retention -- --test-threads=1`.

And these tests exist and pass:
- v5 → v6 upgrade on an existing index;
- a backfill restart mid-run;
- a delete during the backfill;
- truthful availability.

The existing search benches show no regression. A fresh self-review is done. No fixed sleeps; waits use conditions with ≥ 10 s deadlines.

Branch `slice/SB-6`. Open the PR titled "SB-6: Search SCHEMA_VERSION 5 -> 6, population contract". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the `send_message` tool of the Claude Code Remote MCP server. Never merge. Push once per round. Work frugally. Don't background long jobs with `&`.
