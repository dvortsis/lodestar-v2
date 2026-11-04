# X-search-files-3: use the file index's size field so cold file-search p95 stays ≤ 150 ms at 10M
**Model:** Opus. Core: search performance. The runner reviews with a fresh Opus reviewer. Runner brief, 2026-11-04.

## Why
`docs/decisions/2026-10-31-X-search-files-2.md` found that cold file search at 10M needs the file index's size field to stay within the §3 budget (cold p95 ≤ 150 ms). SB-6 (#415) added it as **`size_bucket`** (2 buckets per power of two), not as raw-size postings, to keep the file index at 789 of its 800 B/torrent budget. That's a runner decision, flagged to the owner. It gates the 10M import (Track I, I2/I4).

## Read first
- `docs/decisions/2026-10-31-X-search-files-2.md`, `docs/decisions/2026-11-04-SB-6.md` (the `size_bucket` design) and `docs/decisions/2026-11-04-L4-followups.md` (fresh 1M numbers and the cache).
- `crates/search/src/files.rs` and `crates/search/tests/files.rs`, plus the search-bench "broad file clauses" table.

## Do
1. **Size clauses:**
   - use postings over `size_bucket` for the inner buckets of a range;
   - use the stored size column only for the two edge buckets.

   No size-clause path should scan every file document.
2. **Measure** cold and warm p50 and p95 at 1M on the bench, and at the largest scale it can reach. Show that cold p95 is within budget, with the method in the PR. If 10M can't be built in CI or the container, say so, and give the scaling evidence (per-clause cost against corpus size).
3. **Tests:** golden results identical to a full scan for range edges, open ranges, bucket boundaries and zero or huge sizes. A property test that bucketed and scanned results always agree.
4. **Decisions file:** `docs/decisions/2026-11-04-X-search-files-3.md`.

## Own
`crates/search/src/files.rs` and `crates/search/tests/files.rs`, plus bench reporting only if needed (list it as shared). **Don't edit `index.rs`**: SRC-3 and X-merge-policy are working near it.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass, the search latency CI job is green, and a fresh self-review is done.

Branch `slice/X-search-files-3`. The PR is titled "X-search-files-3: Size-bucket postings keep cold file search in budget". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with `send_message`. Never merge. Push once per round.
