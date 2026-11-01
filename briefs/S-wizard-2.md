# S-wizard-2: first-run wizard part 2 (download client, rule packs)
**Model:** Sonnet (same session as part 1). The rule-pack step is content-safety UI, so an **Opus reviewer runs `/security-review`, and the rules below are blocking.** Runner brief, 2026-11-01.

## Read first
- Your part 1 decisions file (`docs/decisions/2026-10-31-S-wizard.md`), especially the `SLOTS` note in `wizard/steps.ts`.
- `docs/architecture.md` §10.10.10 (step 7), §20.6 (rule packs: **off until chosen, all five pre-checked in the wizard**), §20.3, §20.10 and §20.11 (the filters change rules), §18.10 and §9 (download clients), and `docs/owner-decisions.md` §2.8 and §2.9 (packs off until chosen; blocklists stay out of this slice).
- `web/src/lib/features/filters/flows/**`: the change dialog, preview and password state machines. **Use them; don't re-implement or edit them.**
- The APIs in `schema.d.ts`: `/filters` (packs and their switches), `/filters/preview`, `/filters/apply`, the download-client settings section and `/settings/test` (Test connection).

## Do
1. **Step 7, Download client (optional).** Render the client fields with the engine's `SettingField`s through `form/embed.ts`, exactly as Settings does, including **Test connection** via `/settings/test`. Recommend qBittorrent in the copy (§10.10.10). Skip sends nothing. The same load guard as part 1: a failed or pending load never marks the step done.
2. **Rule-pack step (after step 7, before Updates).**
   - List the five packs from `GET /filters` with one plain line each, **all pre-checked**. The user can uncheck any.
   - Continue applies the chosen packs through the filters flows (preview and whatever the flows require for turning packs on). Skip, or Continue with none checked, sends nothing, so packs stay off (the server default).
   - If the server already has a stored pack choice (packs turned on or off before), show the server's state instead of pre-checking, and say they can change it in Settings → Filters & Safety.
3. **Step 6 (import from bitmagnet) stays a hidden slot.** Online import is deferred (B-import-server-2), so there's nothing to call. Don't add a placeholder.
4. **Content-safety rules (blocking):**
   - The wizard can only turn packs **on**, never loosen anything: no control here turns off the locked floor, a list, a rule or a pack the server has on.
   - A failed or refused apply leaves the server's state on screen, with the error, and doesn't mark the step done.
   - Nothing adult is shown or named in pack descriptions beyond what §20 allows when 18+ is off.
5. **Sizes:** the Welcome route stays ≤ 16.5 KB; the wizard steps entry may grow, at measured + 10% if it needs a new limit. Load the flows on demand from the pack step only. **CSS must not grow** (about 40 B left; X-web-hygiene-1101 is freeing room). Other first views must not grow. Never raise a limit. Don't touch `vite.config.*`.
6. **i18n:** `wizard_*` keys in en and zh, regenerated.
7. **Tests:**
   - vitest for each blocking rule: Skip sends nothing, none-checked sends nothing, a stored choice is shown not pre-checked, a failed apply keeps the server state and doesn't mark done, nothing loosens; plus client Test connection and the load guard.
   - Each test must fail with its guard removed: wait on conditions, not right after the click (the part 1 review found this).
   - Playwright: the keyboard-only path through both new steps, with axe. Condition waits only.
   - `welcome-*` baselines may change: use the marker, check only those images changed, then remove it.
8. **Decisions file:** append a "Part 2" section to your part 1 decisions file, or add `docs/decisions/<date>-S-wizard-2.md`.

## Own
`web/src/lib/features/wizard/**`, `web/e2e/wizard*` and `web/e2e/welcome*`, the wizard messages and size entries, your decisions file.

**Don't edit** `filters/flows/**`, `filters/screens/**`, `settings/form/**` (embed.ts included, unless one more re-export is all you need; tell the runner first), `settings/updates/**`, `lib/shell/**` or `vite.config.*`.

## Size
≲ 1,200 changed lines.

## Done when
These pass in `web/`:
- `npm run check`
- `npm run e2e:typecheck`
- `npx vitest run`
- `npm run build` then `npx size-limit`
- `npx playwright test e2e/wizard* e2e/welcome* e2e/auth*` (3× for new specs)

Chromium is at /opt/pw-browsers; never run `playwright install`. Run `/security-review`.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the size table. Use the `send_message` tool of the Claude Code Remote MCP server. Never merge. Work frugally. Push once per round of fixes.
