# X-polish-carry-1102: drain small carry-forward items that no running slice owns
**Model:** Sonnet (small, well-specified fixes). The runner reviews on Opus. Runner brief, 2026-11-02.

## Why
`runner-state` `carry_forward` holds small fixes that never got a slice. Each item below touches files no running slice owns. Before fixing an item, **check whether it's still open on `main`.** If it was already fixed, skip it and say so in the PR.

## Items
1. **docs-LODESTAR_LOG:** `docs/operations.md` (around line 34) and `packaging/docker/README.md` (around line 79) must say that setting `LODESTAR_LOG` makes Settings → Logging read-only. Use the `user-docs` skill: plain, short, and recommend the safe choice.
2. **docs-operations-firstrun:** the `docs/operations.md` first-run example (around line 86) is stale. The setup-token line now shows `http://<this host's address>:PORT/`. Match what the binary prints today; grep for the log text.
3. **digest-stall-hours:**
   - `crates/api/src/digest.rs` (around line 511) hard-codes "for 2 hours", copying `STALL_HOURS` in `crates/api/src/insights.rs:194`. Make the constant `pub(crate)` and build the text from it.
   - Format "And N more" through the existing `format_count` helper.
   - Keep the golden test (around line 1537) passing with identical output.
4. **pw_port:** `web/playwright.config.ts` hard-codes port 4173, with `reuseExistingServer` outside CI, so parallel worktrees cross-talk.
   - Read the port from `PW_PORT` (default 4173) for the server command, `port` and `baseURL`.
   - Add `--strictPort` to the preview command.
   - Document it in `web/e2e/README.md`.
5. **tailwind_scans_dev_routes:** Tailwind scans `src/dev-routes`, so showcase-only classes land in production CSS.
   - Exclude `src/dev-routes` from the production Tailwind source set (`@source not`, or equivalent for the installed Tailwind version).
   - Report the first-load CSS size before and after, from `npx size-limit`.
   - The dev showcase must still render in dev.
6. **subscriptions-msg-nit:** add "Restart Lodestar to try again." to the `filters.subscriptions_client_failed` user message, in en and zh. Find the key by grep.
7. **backup_unused_mut_windows:** `crates/lodestar/src/cli/backup.rs` (around line 134) has `let mut builder = builder;` that is unused on Windows only. Make it conditional, or restructure so clippy is clean on every OS. Keep it no-op on Unix.

## Own
Only the files the items above name, plus `web/e2e/README.md` and the messages files for item 6.

**Don't edit:** `crates/api/src/ops.rs`, `crates/pipeline/**`, `crates/sources/**`, `crates/lodestar/src/{ops,metrics,main,services,percolator}.rs`, `crates/store/**` or `crates/sources-plugins/**`. Running slices own them.

## Done when
These pass:
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`;
- in `web/`: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`, and `npx playwright test e2e/shell.routes.test.ts` to prove the port change. Use `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium-1243/chrome-linux64/chrome`, and never run `playwright install`.

The PR description lists each item as fixed or already fixed (with the commit).

Branch `slice/X-polish-carry-1102`. Open the PR titled "X-polish-carry-1102: small carry-forward fixes". Follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the `send_message` tool of the Claude Code Remote MCP server. Never merge. Push once per round. Work frugally. Don't background long jobs with `&`.
