# X-requeue-2: the classification re-queue when search is off, and a resume that doesn't stall
**Model:** Opus: content safety (§20.9 classification). Runner brief, 2026-10-31. Follow-up to X-polish-1031c (#286, merged 4a35f2ea). `/security-review` is required. **It must not loosen anything.**

## Why (from #286's review)
1. In `crates/api/src/filters.rs` (`requeue` ~2463, `requeue_marks` ~2481, the no-index branch): with no search index (`search.enabled = false`), `requeue_marks` returns `(None, false)` and `requeue` then **clears** the `classify.requeue_pending` entry. Take this sequence:
   - a shutdown happens mid-re-queue;
   - the server restarts with search off;
   - search is later turned back on.

   That rule's existing matches stay unmarked. Adding a class rule while search is off has the same gap today: nothing ever marks its existing matches once an index exists.
2. `resume_requeues` (~2523) stops at the first failing entry (`?`), so one entry that fails every time blocks every entry after it at each start.
3. No test covers the search sibling-stop in `crates/search/src/files.rs` (~711).

## Read first
`docs/decisions/2026-10-31-X-polish-1031c.md`, `-X-search-files-2.md`, `docs/architecture.md` §20.9 and §20.11, `crates/api/src/filters.rs` (the re-queue, `push_pending_requeue`/`drop_pending_requeue`, `resume_requeues`), `crates/lodestar/src/services.rs` (the start hook), and how search is enabled and disabled at runtime (`grep -rn "search.enabled\|SearchIndex::open" crates`).

## Do
1. **Keep the pending entry until an index has done the work:** clear it only when an index was present and every chunk committed. Dedupe identical query texts in `push_pending_requeue`, so the list stays bounded while search is off.
2. **Re-queue on index availability:** when the search index becomes available, whether at start or when search is turned on, run `resume_requeues`. Make sure rules added while search was off are recorded as pending, so their matches are marked once an index exists.
3. **A resume that doesn't stall:** `resume_requeues` logs a failing entry (no query text or names in the log line; follow the Private preset), keeps it, and continues with the rest. It stops only on shutdown.
4. **The search sibling-stop test:** inject a failing segment (a test-only hook is fine). Assert that the real error is returned, the siblings stopped, and nothing was cached.
5. **Tests:** add a rule with search off, turn search on, and its existing matches get marked; a shutdown mid-re-queue followed by a restart with search off keeps the entry, and turning search on later marks the matches; one entry failing doesn't block the others; dedupe. Condition waits only, no fixed sleeps.
6. **Decisions file:** `docs/decisions/<date>-X-requeue-2.md`.

## Own
`crates/api/src/filters.rs` (the re-queue functions only), `crates/lodestar/src/services.rs` (the index-available hook only), wherever search is enabled at runtime (the hook call only; name it in the PR), `crates/search/src/files.rs` (the test and its hook only), their tests and your decisions file. **Don't touch** `filters.rs`'s release functions or `crates/filter/src/held.rs`: B-held-exact (#287) is open there. Whichever PR merges second brings in main.

## Size
≲ 400 changed lines.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass (with `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0`), plus the openapi drift check. Run `/security-review`.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head. Use the `send_message` tool of the Claude Code Remote MCP server; `SendMessage` doesn't reach the runner. Never merge. Work frugally: the seven-day usage warning is escalating. Push once per round of fixes.
