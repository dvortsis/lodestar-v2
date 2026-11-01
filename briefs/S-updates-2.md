# S-updates-2: Settings → Updates part 2
**Model:** Sonnet (same session as part 1). Update-trust and a destructive action, so an **Opus reviewer runs `/security-review`, and the rules below are blocking.** Runner brief, 2026-11-01.

## Read first
- Your part 1 decisions file (`docs/decisions/2026-11-01-S-updates.md`) and its slots list.
- `docs/architecture.md` §10.10.14 (Advanced, Roll back, Elsewhere), §10.10.6 (restart overlay), §10.10.7 (Danger zone and destructive confirmations), §12.2.
- The APIs in `schema.d.ts`: the `updates` settings fields (channel, wait, window, index rebuilds, notifications), any rollback endpoint, the Overview's needs-attention data, and the `updates` SSE topic. Use only what exists; anything missing stays a slot.

## Do
1. **Advanced** (closed by default): channel (Stable / Beta / Nightly; changing it opens a confirmation dialog that says what the channel means), wait after a release, install window, allow index rebuilds, and what to be notified about. Use the engine's `SettingField`s through `form/embed.ts` if these are settings-section fields; otherwise follow part 1's store pattern.
2. **Roll back to <version>** in a Danger zone, only if the API offers rollback. Say what it restores and how many torrents would be lost, if the API gives that number (don't invent it). Use the existing destructive-confirmation pattern (typed confirmation or password, as §10.10.7 says for this class). Cancel sends nothing. If there's no rollback API, it stays a slot.
3. **Elsewhere:**
   - the Overview's Needs attention shows available, failed and rolled-back updates, plus a pending install with its time, if the Overview reads a data source you can extend without touching other screens' code; otherwise list it as a slot;
   - the status pill shows "Updating…" during an install (from the `updates` SSE state);
   - the restart overlay (§10.10.6) is reused for the restart step, if the overlay is exposed for reuse.

   Each of these touches shared shell or overview code. Keep the edit to the smallest hook, name every file outside your folder in the PR, and leave anything bigger as a slot.
4. **Carry from part 1:** O2 (level and security badges on the status line) if it now fits in < 50 B, and the Install-now test TODO, if an install API has appeared.
5. **Rules (blocking):**
   - The channel dialog: Cancel sends nothing, and moving to Nightly or Beta says plainly what changes.
   - Roll back confirms first, Cancel sends nothing, and a failed or refused request shows the server's state.
   - The pill and the overlay only reflect the server's update state; nothing claims progress the server didn't send.
6. **Sizes:**
   - The Updates first view stays ≤ 45 KB with ≥ 0.3 KB headroom; Advanced and Roll back load on demand.
   - Any shell hook (the pill) adds ≤ 0.05 KB to first loads.
   - Alert, Alerts, Search results and Results toolbar must not cross their limits; CSS must not grow.
   - Never raise a limit; don't touch `vite.config.*`.
7. **Tests:** vitest per blocking rule, with each test failing when its guard is removed (wait on conditions, not right after the click). Playwright: Advanced and the channel dialog, keyboard-only, with axe. Condition waits only.
8. **Decisions file:** append "Part 2" to the part 1 file, or add `docs/decisions/<date>-S-updates-2.md`.

## Own
`web/src/lib/features/settings/updates/**`, `web/e2e/settings-updates*`, its messages and size entries, your decisions file, and the smallest hooks named in item 3.

**Don't edit** `settings/form/**` (embed.ts may gain a re-export only if needed; tell the runner), `features/wizard/**` (S-wizard-2 is running), the stale-tab banner, or `vite.config.*`.

## Size
≲ 1,200 changed lines.

## Done when
These pass in `web/`:
- `npm run check`
- `npm run e2e:typecheck`
- `npx vitest run`
- `npm run build` then `npx size-limit`
- `npx playwright test e2e/settings*` (3× for new specs)

Run `/security-review`. Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the size table. Use the `send_message` tool of the Claude Code Remote MCP server. Never merge. Work frugally. Push once per round of fixes.
