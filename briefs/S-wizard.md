# S-wizard: first-run wizard (part 1)
**Model:** Sonnet. Svelte screens built on the Settings form engine. An Opus reviewer follows. Runner brief, 2026-10-31. Slice entry: `runner/slices.yaml` → `S-wizard`.

## Read first
- CLAUDE.md UI rules, `runner/standards.md` and `docs/ui-glossary.md`.
- **`docs/architecture.md` §10.10.10** (the spec), §10.10 (the Settings engine the wizard shares), §10.10.11 (accessibility, tests), §3 "Cold-start reality" (the expectation the wizard sets), §12.2 (updates contact GitHub).
- `web/src/lib/features/settings/form/README.md`: render steps with `SettingField` and save through the multi-section patch, exactly as Settings does. **Use the engine; don't re-implement or edit it.**
- `web/src/lib/features/auth/**`: `/setup` already does steps 1–2 (setup token, admin password), then lands on `/welcome` (`nav.ts` `toWelcome`). `WelcomePage.svelte` holds the auth check.
- The APIs in `web/src/lib/api/schema.d.ts`: `/network/check` (GET and the 30 s-limited POST), `/settings` and `/settings/{section}`, `/settings/test`, `/updates`. Use only what exists.
- How hand-built screens split into lazy parts: `docs/decisions/2026-10-31-S-logs-2.md`, `-X-settings-registry-slim.md`, `-S-about.md` (if merged).

## Do (part 1)
1. **Wizard shell at `/welcome`** for a signed-in user, replacing the current links page:
   - stepped progress showing steps 1–2 (done in `/setup`) and the rest, one primary action per step, Back and Skip on every optional step;
   - keep `WelcomePage`'s auth behaviour (setup required → `/setup`, signed out → login, the retry state, the no-focus-steal rule);
   - progress may sit in `sessionStorage` (wrapped in try/catch) so a reload resumes; nothing gates the rest of the app on finishing.
2. **Steps in this part:**
   - 3. **Network check:** live state from `GET /network/check`, a Retest button (`POST`, honour `429 rate_limited` with a plain message), and plain help when not reachable (port forward, VPN). Poll with a deadline, never stack requests, stop polling when the step unmounts.
   - **"How it works":** a static inline SVG sequence (DHT → metadata → index → search) with a text equivalent. Set the cold-start expectation from §3: the index grows over days and weeks.
   - 4. **Intensity** and 5. **Storage & retention:** the same `SettingField`s, previews and validation as Settings, from the schema. Each step saves through the multi-section patch.
   - 8. **Updates:** Off / Check and notify / Install automatically. Nothing preselected beyond the current server value (Off by default); say plainly that checking contacts GitHub. A locked value (env var) shows as locked.
   - 9. **Done:** links to Settings for everything skipped, and to Search.
3. **Leave slots** for 6 (import from bitmagnet), 7 (download client) and the rule-pack step (§20.6, packs pre-checked): they are **part 2**, not this PR. Name them in the decisions file.
4. **Rules:**
   - A failed save leaves the server's value on screen with the engine's error; Skip sends nothing.
   - The wizard never changes a content-safety setting in this part.
   - Keyboard-only path through every step; focus moves to each step's heading; errors use the engine's summary.
5. **Sizes:**
   - The `/welcome` first view ≤ 45 KB with ≥ 0.3 KB headroom; later steps load on demand. Add its size entry at measured + 10%.
   - **Alert, Search results, Alerts and Results toolbar must not grow at all.** X-alert-search-headroom is reworking their chunks in parallel; don't touch `vite.config.*` or their size entries. If the wizard moves them, stop and tell the runner.
   - Other first views must not grow. Never raise a limit.
6. **i18n:** `wizard_*` keys in the feature's own `messages/{en,zh}.json`, regenerated with the repo's generator.
7. **Tests:**
   - vitest: step navigation and resume, Skip sends nothing, save failure keeps the server value, network-check polling (deadline, no stacking, stops on unmount, 429).
   - Playwright against the mock: the full keyboard-only path with axe; the existing `welcome.routes` and auth specs still pass (update them only where the page legitimately changed).
   - Condition waits only.
8. **Decisions file:** `docs/decisions/<date>-S-wizard.md`.

## Own
- `web/src/lib/features/wizard/**`
- `web/src/routes/welcome/**`, `web/src/lib/features/auth/WelcomePage.svelte` and its test
- `web/e2e/welcome*` and a new `web/e2e/wizard*`
- its messages and regenerated maps, its mock fixtures, its size entry, your decisions file

**Don't edit** the engine (`settings/form/**`), other Settings screens, `features/filters/**`, `features/settings/about/**`, `lib/api/**` or `vite.config.*`. If something is missing there, tell the runner.

## Size
≲ 1,500 changed lines. If it grows past that, ship steps 3, "How it works" and Done first.

## Done when
These pass in `web/`:
- `npm run check`
- `npm run e2e:typecheck`
- `npx vitest run`
- `npm run build` then `npx size-limit`
- `npx playwright test e2e/welcome* e2e/wizard* e2e/auth*` (3× for new specs)

Chromium is at /opt/pw-browsers; never run `playwright install`. If the pinned headless shell is missing, a symlink to the installed one is fine.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the size table. Use the `send_message` tool of the Claude Code Remote MCP server; `SendMessage` doesn't reach the runner. Never merge. Work frugally. Push once per round of fixes.
