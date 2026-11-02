# SRC-2b: saved-search alerts for promoted torrents
**Model:** Opus. This is store and concurrency work. A fresh Opus reviewer follows. Runner brief, 2026-11-02, based on SRC-2 (#383) and its decisions file.

## Why
SRC-2 promotes a verified unverified item into a stored torrent through `promote`. The promoted id is often **below** both the search watermark and every saved search's percolation watermark:
- `record_percolation` refuses hits at or below a saved search's watermark;
- each run covers only `(watermark, through]`.

So promoted torrents never reach saved-search alerts. `docs/decisions/2026-11-02-SRC-2.md` ("The percolator: a known gap until SRC-2b") records this. The runner ruled out an in-memory queue, because it would lose entries on a restart.

## Read first
- `docs/decisions/2026-11-02-SRC-2.md`: Promotion (§18.5), "Promoted ids below every watermark", and the percolator gap.
- `docs/architecture.md` §18.5 and the saved-search / percolator section (search for "percolat").
- `crates/lodestar/src/percolator.rs`, `crates/store/src/saved.rs`, `crates/store` `promote` and `reindex_log`.
- `crates/search/tests/promoted.rs`.

## Do
1. **Design.** The candidate design consumes the `reindex_log` entries that `promote` writes, with a durable cursor. No in-memory queue, and preferably no new table. Check whether the indexer prunes `reindex_log` before the percolator could read it. If it does, choose another durable source and say why. Write the design in the decisions file before the code.
2. **Percolate each promoted id against every saved search exactly once.**
   - **Dedupe by (search, torrent).** A promoted id the forward run also sees, or one replayed after a crash, never alerts twice.
   - **Crash safety:** advance the cursor in the same transaction as the hits it covers, so a crash replays and doesn't skip.
   - **A merged promotion** (`Promoted::Merged`) adds no new torrent. Decide whether it alerts, and say why. The default is no: the stored torrent was already percolated.
3. **Must-holds:**
   - **Filters:** only torrents that are visible under the current rules alert, the same as the forward run. A held, quarantined or hidden torrent never alerts.
   - **No backfill:** a saved search created *after* the promotion doesn't alert on it, because the forward run's no-backfill semantics stay the same.
   - **Fair to the writer:** batch it, keep transactions short, and report the cost per batch.
   - **Metrics and logs:** reuse the existing percolator counters where they fit. Add any new metric to `docs/metrics.md`.
4. **Tests:**
   - a promoted id below the watermark alerts once;
   - a crash between hit and cursor replays without duplicates;
   - a merge doesn't alert (or does, per your ruling);
   - held and hidden promoted ids don't alert;
   - a saved search created after the promotion doesn't alert;
   - restart continuity.
5. **Decisions file:** `docs/decisions/2026-11-02-SRC-2b.md`, short. Remove "known gap" from the SRC-2 file only by adding a line that points to yours; don't rewrite it.

## Own
- `crates/lodestar/src/percolator.rs` and `crates/store/src/saved.rs`, with their tests.
- Minimal hooks in `crates/store` for the cursor or `reindex_log` reads. List them in the PR.
- Your decisions file and the `docs/metrics.md` rows.

**Don't edit:** `crates/sources/**` (SRC-4 is in review), `docs/soak/**` (CR-COL-3), or the filter crate.

## Size
≲ 600 changed lines.

## Done when
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass.
- No fixed sleeps in tests: wait for a condition with a ≥ 10 s deadline.

Branch `slice/SRC-2b`. Open the PR titled "SRC-2b: saved-search alerts for promoted torrents". Follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the `send_message` tool of the Claude Code Remote MCP server. Never merge. Push once per round. Work frugally. Don't background long jobs with `&`.
