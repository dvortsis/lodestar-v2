# X-pipeline-1101: refetch funnel, backfill retry, test comment
**Model:** Opus (pipeline concurrency and the Insights funnel). Runner brief, 2026-11-01. Three follow-ups from IMP-5, #282 and #274.

## Why
1. **Refetches pollute the funnel (B-refetch-funnel, from IMP-5).** `REFETCH` leases (strategy `REFETCH`, cohort `REFETCH_COHORT`) go through `crates/pipeline/src/{worker,coordinator,writer}.rs` and, depending on the path, record funnel events or cohort rows. So FILES_PARTIAL imports queued at startup (`lib.rs` around l.970, `store.partial_hashes(PARTIAL_REFETCH)`) can land in the start hour's crawl funnel. Insights' crawl-yield numbers must count network discoveries only (master plan Part F14, IMP-5). Check what the code does now, and make refetches record no cohort and no `FUNNEL_EVENTS` row (`is_crawl_cohort`, `lib.rs` around l.99, may already be the hook).
2. **The backfill error arm doesn't retry (#282).** In `crates/pipeline/src/lib.rs`, the backfill's error arm gives up (or stalls) where the candidate sweep retries with backoff. Give it the same retry-with-backoff, sharing the helper if one exists. A transient store error must not end backfill for the run, and a persistent one must not spin.
3. **A wrong comment (#274 optional).** `crates/pipeline/tests/pipeline.rs:517` says the sighting arrives while the refetch lease is out. Reword it: the imported row is in the have-filter, so the sighting is a rediscovery and never reaches the coordinator.

## Read first
- CLAUDE.md, `runner/standards.md`.
- `docs/architecture.md` §5 (pipeline) and the Insights funnel sections (`docs/metrics.md`: funnel and cohort definitions).
- `docs/decisions/` for IMP-5 and #282 (grep `REFETCH`, `funnel`, `backfill`).

## Do
- Fix 1 and 2 with tests:
  - a refetch completing (success and failure) writes no cohort and no `FUNNEL_EVENTS` row, and a normal network fetch still does;
  - backfill retries after a transient error with bounded backoff and resumes where it left off; a persistent error backs off without a hot loop, and shutdown during backoff is prompt.
  - Use a fake clock or condition waits, never fixed sleeps (deadline ≥ 10 s where waiting is needed).
- Fix 3.
- If item 1 turns out to be already correct on main, say so with the test that proves it, and keep the test.
- Decisions file only if you change a behaviour someone relies on: `docs/decisions/<date>-X-pipeline-1101.md`.

## Own
`crates/pipeline/**` (src and tests), and your decisions file. Don't touch `crates/store` schema, other crates or `web/`. If a fix needs one of those, tell the runner.

## Size
≲ 400 changed lines.

## Done when
These pass:
- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p lodestar-pipeline` (or the crate's package name), then `cargo test --workspace`

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head. Use the `send_message` tool of the Claude Code Remote MCP server; `SendMessage` doesn't reach the runner. Never merge. Work frugally. Push once per round of fixes.
