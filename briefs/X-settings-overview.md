# X-settings-overview: Settings Overview shows Needs attention and Changed from default
**Model:** Sonnet (UI, following an existing pattern). The runner reviews on Opus. Runner brief, 2026-11-02.

## Why
The Settings Overview (`web/src/routes/settings/+page.svelte`) is still a placeholder. §10.10.8 says the Overview has two parts: **Needs attention** and **Changed from default**, fed by `/system`, `/stats` and the network check.

## Read first
- `docs/architecture.md` §10.10 (Settings), especially §10.10.8 and any Overview wording in §10.10.x.
- How the generated screens get settings schema defaults and current values (`web/src/lib/features/settings/form/**`, `screens*`), and the existing API client calls for `/system` and `/stats` (`web/src/lib/api`).
- CLAUDE.md "UI rules", `docs/design-tokens.md` and `docs/ui-glossary.md`. Read `anthropic-skills:application-interface-design` only for what those leave open.

## Do
1. **Needs attention.** Show only signals the API already exposes; no new endpoints:
   - for example, the health or degraded state from `/system`, an index rebuild, a pending update or restart;
   - grep the DTOs for what's available.

   Each row:
   - says what's wrong in one line;
   - links to the screen that fixes it;
   - has an empty state ("Nothing needs attention").
2. **Changed from default.**
   - List each setting whose current value differs from the schema default, grouped by screen.
   - Each row has the label, the current value and the default, and links to its field.
   - Reuse the form code's schema and labels; don't re-list settings by hand.
   - Don't show secrets: show "set" or "not set" for any secret field.
3. **States:** loading, error with retry, and empty, per §10. Keyboard and screen-reader friendly. Use tokens only; no hex or arbitrary values.
4. **Messages:** en and zh, in the settings feature's messages.
5. **Tests:**
   - vitest for the diff-from-default logic, including secrets and nested values;
   - a Playwright case on the fixture for both sections and the empty state.
6. **Size:** load the Overview code on demand. Report the size-limit numbers; don't raise a limit without a stated reason.

## Own
`web/src/routes/settings/+page.svelte`, `web/src/lib/features/settings/overview/**`, the settings messages keys you add, and their tests.

**Don't edit:** `web/src/lib/shell/**` (X-overflow-links owns it) or the API crates.

## Done when
In `web/`, these all pass: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`, and `npx playwright test` for the touched spec. Use `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium-1243/chrome-linux64/chrome`, and never run `playwright install`.

Branch `slice/X-settings-overview`. Open the PR titled "X-settings-overview: Needs attention and Changed from default". Follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the `send_message` tool of the Claude Code Remote MCP server. Never merge. Push once per round. Work frugally. Don't background long jobs with `&`.
