# S-updates: Settings → Updates (part 1)
**Model:** Sonnet (a Svelte screen, pattern-following). It's update-trust UI, so an **Opus reviewer runs `/security-review`, and the rules below are blocking.** Runner brief, 2026-11-01. Slice entry: `runner/slices.yaml` → `S-updates`.

## Read first
- CLAUDE.md UI rules, `runner/standards.md` and `docs/ui-glossary.md`.
- **`docs/architecture.md` §10.10.14** (the spec), §12.2 (updates and trust), §10.10.7 (risky changes), and `docs/owner-decisions.md` on updates (Off by default; checking contacts GitHub).
- The APIs in `web/src/lib/api/schema.d.ts`: `/updates`, `/updates/check`, `/updates/pause`, `/updates/skip`, `/updates/history/clear`, the `updates` settings section (if the update policy is a Settings section, render it with the engine), and the `updates` SSE topic if served. Use only what exists; for anything missing, leave a slot and say so.
- The notices this screen must surface (F-updater): `updater_unreadable`, `check_not_saved`, `policy.held_in_process`. Grep `crates/lodestar/src/update*` and the API for their exact shape.
- How hand-built screens split into lazy parts and register: `docs/decisions/2026-10-31-S-about.md` (the closest pattern: registry entry, `body.ts` forwarder, the securityLoader regex in `.size-limit.mjs`), `-S-logs-2.md`, `-X-settings-registry-slim.md`, and `-X-alert-search-headroom.md` (what not to import into shared chunks).

## Do (part 1)
1. **Build Settings → Updates per §10.10.14**, except the items listed under part 2:
   - the status card (up to date / available / in progress, with Check now, Install now and Skip this version where the API supports them, and the post-rollback explanation);
   - **How to update** (Off / Check and notify / Install automatically / Container image updates on Docker and Unraid), the level choice under automatic, and **Always install security fixes** with its per-mode default and visibility;
   - pause for 1, 7 or 30 days, with Resume;
   - the notices (revoked key, clock, damaged trust data, plus the three F-updater notices);
   - the can't-update-itself reason next to the disabled automatic options;
   - release notes (badges, date, notes, "Read on GitHub" with `rel="noopener noreferrer"`, https links only);
   - the history table.
2. **Rules (blocking):**
   - The screen shows only what the server says. A failed or refused request leaves the last server state on screen with an error. Nothing claims "installed" or "up to date" without a server response saying so.
   - Nothing preselected beyond the server's value. Off stays Off until the user picks otherwise; say plainly that checking contacts GitHub.
   - A locked value (env var `LODESTAR_UPDATES`) shows as locked with the reason; no control changes it.
   - Install now and Skip each ask for confirmation through the existing dialog pattern; Cancel sends nothing.
   - Damaged trust data blocks Install now in the UI too (the server enforces it; the UI must not offer it).
3. **Registry entry:** one line, following S-about's pattern (purpose line in the lazy `about.ts`, the body through a tiny forwarder). S-about (#299) is landing now: **merge origin/main after #299 merges** before you open the PR; the conflict is one registry line.
4. **Sizes:**
   - The Updates first view ≤ 45 KB with ≥ 0.3 KB headroom. Release notes, history and dialogs load on demand. New entries at measured + 10%.
   - Other Settings first views grow only by the measured registry cost (state it in bytes).
   - Alert, Alerts, Search results and Results toolbar must not grow. Never raise a limit. Don't touch `vite.config.*`.
5. **i18n:** `settings_updates_*` keys in the feature's own `messages/{en,zh}.json`, regenerated.
6. **Tests:**
   - vitest for each blocking rule (failure keeps the server state, Cancel sends nothing, locked value, trust-damaged blocks Install now, security toggle defaults per mode, the three F-updater notices).
   - Playwright against the mock: the screen renders, keyboard-only, with axe. Condition waits only.
   - Settings baselines change (the nav gains Updates): use `web/e2e/.update-snapshots`, check only Settings images changed, then remove the marker.
7. **Decisions file:** `docs/decisions/<date>-S-updates.md`, listing the slots and part 2.

## Part 2 (not this PR)
Advanced (channel dialog, wait, window, index rebuilds, notification choices), **Roll back** in the Danger zone, the Overview's Needs attention entries, the status pill's "Updating…", the restart overlay, and the wizard's Updates step hookup.

## Own
- `web/src/lib/features/settings/updates/**`
- its registry line, its messages and regenerated maps, its mock fixtures
- `web/e2e/settings-updates.*`
- its size entries, your decisions file

**Don't edit** the engine (`form/**`), other screens, `features/wizard/**` (S-wizard is running), `lib/api/**`, `lib/ui/**` or `vite.config.*`. If something is missing there, tell the runner.

## Size
≲ 1,500 changed lines.

## Done when
These pass in `web/`:
- `npm run check`
- `npm run e2e:typecheck`
- `npx vitest run`
- `npm run build` then `npx size-limit`
- `npx playwright test e2e/settings*` (3× for new specs)

Chromium is at /opt/pw-browsers; never run `playwright install`. If the pinned headless shell is missing, point `PLAYWRIGHT_BROWSERS_PATH` at a scratch folder of symlinks to the installed one. Run `/security-review`.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the size table. Use the `send_message` tool of the Claude Code Remote MCP server; `SendMessage` doesn't reach the runner. Never merge. Work frugally. Push once per round of fixes.
