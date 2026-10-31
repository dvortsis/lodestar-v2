# X-polish-1031b: crawler review polish, an API schema guard, a rollback docs line
**Model:** Opus: the DHT node (untrusted input, concurrency). Runner brief, 2026-10-31. Optional findings from the CR-1a-H (#246) and CR-1a-S (#252) reviews, one latent API bug class (#255), and one docs follow-up (#245). No schema change. No `/security-review` needed.

## Read first
CLAUDE.md, `runner/standards.md`, `docs/decisions/2026-10-30-CR-1a-H.md`, `2026-10-31-CR-1a-S.md`, `-CR-1a-S-2.md`, the CR-1a-H and CR-1a-S sections of `docs/metrics.md`, and the `user-docs` skill (for item 4).

## Do
1. **Hints (`crates/dht/src/node/hints.rs`), from #246's review:**
   - Bound `order` growth *between ticks*. Today it's pruned only when it passes `4 * HINT_CAPACITY`, inside the tick. A burst of captures between ticks can push far more than that. Cap it in `capture` itself, with an O(1) amortised path, and keep the time-order invariant that pruning relies on.
   - Add a test for the `send_failed` outcome: a hinted direct `get_peers` whose send fails falls back as the decisions file says, and is counted under the documented counter.
2. **Streaming batches (`crates/dht/src/node/actor.rs`), from #252's review:**
   - The O3 `first_batch` clear in `back_off` is dead code. Either remove it, or make it do what the CR-1a-S decisions file says and fix that sentence. Say which in your decisions file.
   - In `docs/metrics.md`, say plainly that the two arms of "first fetch ms" (the streaming switch on and off) measure different things, and compare them only with that caveat.
   - Make `docs/metrics.md`'s wording for the first-batch histogram match the metric's help text ("first streamed batch" vs "first kept batch"). Change the doc, not `crates/lodestar/src/metrics.rs`, which another slice may be touching.
3. **API schema guard (#255):** add a test in `crates/api` that fails when two `ToSchema` types register the same component name in `openapi.json`. Today the second silently overwrites the first, as with `DiscoverResponse` and `HistogramBin` before #255.
   - Check the latent pairs the follow-up names: `Channel` (api `dto/system.rs` vs `lodestar/settings/updates.rs`) and `Mode` (`settings/preferences.rs` vs `updates.rs`).
   - If either is in the spec today, rename with `#[schema(as = …)]` and regenerate `openapi.json` and `web/src/lib/api/schema.d.ts` with the repo's tooling. If neither is, the test is enough.
4. **Docs (#245):** the user guide's rollback guidance (`docs/user/`, wherever rollback is covered) says to check Filters after a rollback, because holds and rules added after the backup aren't in the restored store. One short paragraph, per the user-docs skill. If v0.5.1's release notes cover rollback, add one line there too.
5. **Tests:** condition waits only, no fixed sleeps, deadlines ≥ 10 s.
6. **Decisions file:** `docs/decisions/<date>-X-polish-1031b.md`, one line per item.

## Own
`crates/dht/src/node/{hints.rs,actor.rs}` and their tests, `docs/metrics.md` (the CR-1a sections only), the `crates/api` test, any `#[schema(as)]` rename plus the regenerated `openapi.json`/`schema.d.ts`, `docs/user/**` (the rollback paragraph), and your decisions file. **Don't touch** `crates/search/**`, `crates/pipeline/**`, `crates/store/src/import.rs`, `crates/lodestar/src/**` or anything else in `web/`: B-import-server-1, X-search-files-perf and S-settings-2b are running now.

## Size
≲ 400 changed lines.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` and the openapi drift check all pass, plus `npm run check` in `web/` if `schema.d.ts` changed.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head. Use the `send_message` tool of the Claude Code Remote MCP server; `SendMessage` doesn't reach the runner. Never merge. Work frugally. Push once per round of fixes.
