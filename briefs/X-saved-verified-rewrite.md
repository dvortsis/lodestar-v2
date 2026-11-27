# X-saved-verified-rewrite: Saved searches and alerts made with Show unverified keep finding imports

**Runner brief, 2026-11-27T14:33Z.** Core, Opus (stored user data rewrite; the percolator's input). **A release blocker:** it must land before the next release. Target < 500 changed lines. `/security-review` before you report ready.
- **Branch:** `slice/X-saved-verified-rewrite` from fresh `origin/main` (with #1139, X-import-verified-no, merged as 10db45b4).
- **PR title:** `X-saved-verified-rewrite: Saved searches and alerts made with Show unverified keep finding imports`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do. A fresh Opus review gates the merge.
- **Add this slice's own `runner/slices.yaml` entry** (kind core, model opus, deps [X-import-verified-no], your owns, notes "runner 2026-11-27T14:33Z, release blocker from #1139") and run `cd tools/status-connector && npm test`. Edit nothing else in that file or in `runner/dashboard.yaml`.

## Why
#1139 gave imports a third provenance value, `verified:imported`, shown by default. "Show unverified" now sends `verified:yes,imported,no`. Before #1139 it sent `verified:yes,no`, and imports matched that only because they were indexed `yes`. So a saved search or alert stored with `verified:yes,no` now leaves imports out, with no notice (see `docs/decisions/2026-11-27-X-import-verified-no.md`, the "saved search or URL" and "Follow-up, a release blocker" lines). Nothing released is affected yet. This slice makes sure nothing released ever is.

## Do
1. **Find every stored form that needs it.** Saved searches and alerts are rows in `saved_searches` (`crates/store/src/saved.rs`), holding both the query text and the AST JSON. Check which top-level `verified` clauses the UI ever wrote (`web/src/lib/features/search/unverified/toggle.ts` and its history) and which a user could have typed:
   - **Rewrite:** a `verified` node whose value list is exactly {yes, no}, in any order, at any depth. It meant "verified and unverified", and both of those then included imports. Rewrite it to {yes, imported, no}.
   - **Don't rewrite:** a lone `verified:yes`, typed by a user, now means "hashed here", which is the intended new meaning. Neither are `verified:no`, negations, or a list that already has `imported`. Record this in the decisions file, and say whether `verified:yes` alone could ever have come from the UI. If it could, stop and message the runner before going further.
   - Check any other place a stored query lives: API keys' scoped queries, RSS feed tokens, the percolator's promoted queue. Rewrite those too, or say why they can't hold one.
2. **Rewrite once, in one transaction, with a done marker.**
   - The rewrite runs at start, before the percolator compiles any saved search. Put it in the store, or at the point in `crates/lodestar/src/services.rs` where the store is opened and before `percolator::spawn` (about line 687). Choose whichever keeps parsing out of the store (`saved.rs`'s header says parsing belongs to the search crate).
   - Rewrite both the AST JSON and the query text, using the search crate's canonical `Node::to_query`. Leave the watermark, channels, paused, `updated_at` and every other field as they are. A rewritten row must not re-alert on old hits.
   - A `settings` done marker (for example `saved_verified_rewrite_v1`) is written in the same transaction, so a crash re-runs it whole and a finished run never runs again. **No schema migration:** V16 belongs to X-claims-size. If you find you truly need one, stop and message the runner.
   - A row whose AST won't parse is left as it is and counted, never dropped.
   - Emit one Info event with the counts (rewritten, skipped as unparseable). If that needs a log-catalogue or web event-names entry, the owns below cover it; report the size figure and don't raise any limit.
3. **Old URLs and bookmarks:** they can't be rewritten. Confirm the toggle already reads `verified:yes,no` as on and replaces it with all three values when toggled (#1139 says so); add a test only if one is missing.
4. **Tests:**
   - a store with `verified:yes,no` searches and alerts (top level, nested, in reverse order) is rewritten, and the rest are unchanged;
   - the percolator then matches an imported torrent for a rewritten alert, and doesn't re-fire old hits;
   - a second start does nothing (the marker);
   - a failure mid-way rolls back with no marker;
   - an unparseable row is kept as it is;
   - a lone `verified:yes`, and a list that already holds `imported`, are untouched.
   - Mutate the done marker and the {yes, no} match, and show a test fails for each.

## Owns
`crates/store/src/saved.rs` and its store tests; a new module or function in `crates/search/src/` for the AST rewrite (prefer a new file; `query.rs` only for a small `pub` hook) with its tests; `crates/lodestar/src/services.rs` (only the call before `percolator::spawn`) and `crates/lodestar/src/percolator.rs` tests; the log-catalogue entry if you add the event (the crate's `log_events.rs`, `crates/lodestar/src/settings/golden/logs_catalog.txt`, and the matching `web/src/lib/features/settings/logging/event-names/*.ts` and `logging/messages/{en,zh}.json`); `docs/decisions/*-X-saved-verified-rewrite.md`; a one-line pointer from `docs/decisions/2026-11-27-X-import-verified-no.md`'s release-blocker line; and this slice's `runner/slices.yaml` entry.

**Not yours:**
- `crates/store/src/lib.rs`, `schema.rs`, `claims*`, `identify_sweep.rs` and `crates/pipeline/src/identify.rs` / `writer.rs` (X-claims-size);
- `crates/lodestar/src/config.rs`, `metrics.rs`, `docs/metrics.md` and `crates/dht/**` (CR-0w);
- `crates/lodestar/src/watch.rs` (X-watch-open-fstat).
Ask the runner before touching anything else.

## Done
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test -p lodestar-store -p lodestar-search -p lodestar`, or the workspace if the disk allows. Say which you ran. Run the web checks from CLAUDE.md if web changed. Report `ready <n> <full sha>` to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) once ci-ok, web-ok and e2e-ok are green. Push once per round of fixes.

No AWS or cloud calls, no new accounts, no spending, no force-push. Held security detail stays off every branch.
