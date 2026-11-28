# X-watch-search-wait (runner brief, 2026-11-28)

**Slice:** X-watch-search-wait (runner/slices.yaml). Opus, core.
**Branch:** `slice/X-watch-search-wait` from current `main`.
**PR title (exact):** `X-watch-search-wait: The watch-folder test waits for search on a condition the indexer meets, not a fixed minute`

## The failure
On #1161's CI (ubuntu-latest), `crates/lodestar/tests/watch.rs`,
`a_served_instance_takes_files_from_its_watch_folder`, timed out at
`until("search finds it", ...)` (60 s deadline). `main` was green, and a re-run passed.

Note: the good file is taken *before* the broken file's 60 s+ wait, so search had
well over a minute after the take and still answered `"total":1` false. That makes
"slow indexer" an unlikely explanation by itself. Find the real cause before choosing
a wait. Candidates to check:
- the index commit or refresh cadence;
- whether a watch-folder take reaches the indexer by the same path as an upload;
- the search response shape (`"total":1` vs the count when a total isn't exact);
- the test's query string;
- interference from the broken file's handling.

## Do
1. Step 0: reproduce or explain the failure. Send the runner the cause and the
   proposed fix before writing it.
2. Make the test wait on a condition the product actually meets, with a deadline of
   at least 10 s. No fixed sleeps (owner §2.24).
3. Never skip, loosen, or `#[ignore]` what the test proves.
4. If the cause is in product code, **ask the runner before touching it**: the slice
   owns only `crates/lodestar/tests/watch.rs` and its decisions file.
5. Decisions file: `docs/decisions/<date>-X-watch-search-wait.md`.
6. Follow `.claude/skills/steward/SKILL.md`. Run the full pre-push checks in
   CLAUDE.md. Report `ready` with the PR number and head to the runner session.

Build with `CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0`; disk is tight.
