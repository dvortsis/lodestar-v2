# S-filters: Filters & Safety screens, Block this…, Adult facets
**Model:** Sonnet. Svelte screens built on the existing dialog state machines (M-filters-dialogs, merged). It's content-safety UI, so an **Opus reviewer runs `/security-review`, and the safety invariants below are blocking.** Runner brief, 2026-10-31. Slice entry: `runner/slices.yaml` → `S-filters`.

## Read first
- CLAUDE.md UI rules, `runner/standards.md` and `docs/ui-glossary.md`.
- **`docs/architecture.md` §20.7 and §20.9** (the spec), plus §20.3, §20.5.5, §20.10 and §20.11 for the rules the UI must respect.
- `docs/owner-decisions.md`: the content-safety decisions (locked floor, 18+ handling).
- `web/src/lib/features/filters/flows/**`: the dialog state machines for warnings, the password, the impact preview and 18+. **Use them; don't re-implement them.**
- The APIs in `web/src/lib/api/schema.d.ts`: the filters endpoints, rules, lists, exceptions, holds, release, correct and any facets.
- How hand-built screens split into lazy parts and how they register: `docs/decisions/2026-10-31-X-settings-registry-slim.md`, `-S-logs-2.md` and `-M-security-settings.md`.

## Do
1. **Build the Filters & Safety screens per §20.7.** These cover rules, lists, exceptions, the hold queue and release. Every risky or loosening action goes through the flows' dialogs (warnings, password, impact preview).
2. **"Block this…"** on a torrent or result, per §20.9: it creates a rule through the flow, with an impact preview.
3. **Adult facets** per §20.9: they show only when 18+ is enabled, through the existing 18+ flow.
4. **Safety invariants (blocking if broken):**
   - The locked floor can't be weakened from the UI: no control, URL parameter or keyboard path does it.
   - Every loosening action (release, disable a rule, add an exception, turn off a list) needs its dialog. Cancel sends nothing.
   - A failed or refused request leaves the safer state on screen.
   - Held or blocked items never render their names or thumbnails beyond what §20 allows.
   - Nothing adult appears unless 18+ is on.
5. **Registry, if Filters & Safety is a Settings screen:**
   - Add one registry line, with its purpose line in the lazy `about.ts` pattern (#293).
   - S-about is adding a registry line in parallel. Whichever merges second brings in main; the conflict is one line.
6. **Sizes:**
   - The first view must be ≤ 45 KB with ≥ 0.3 KB headroom. Dialogs and big lists load on demand.
   - Other first views grow only by the measured registry cost.
   - Alert, Search results and Results toolbar must not grow.
   - New entries go in at measured + 10%. Never raise a limit.
7. **i18n:** `filters_*` keys in en and zh, regenerated.
8. **Tests:**
   - vitest for each safety invariant, and Cancel-sends-nothing for every loosening action.
   - Playwright, keyboard-only, with axe.
   - Condition waits only.
   - If Settings baselines change, use the `.update-snapshots` marker and check only the expected images changed.
9. **Decisions file:** `docs/decisions/<date>-S-filters.md`.

## Own
- `web/src/lib/features/filters/screens/**`
- its registry line, if needed
- its messages and maps, and its mock fixtures
- `web/e2e/filters*`
- its size entries
- your decisions file

**Don't edit** `filters/flows/**`. If a flow is missing something, stop and tell the runner. Also leave the engine, `lib/api/**` and other screens alone.

## Size
≲ 1,500 changed lines. If it grows past that, split it. **2a:** the screens and release. **2b:** "Block this…" and the facets. Ship 2a first.

## Done when
These pass in `web/`:
- `npm run check`
- `npm run e2e:typecheck`
- `npx vitest run`
- `npm run build` then `npx size-limit`
- `npx playwright test` for the new specs (3×)

Chromium is at /opt/pw-browsers; never run `playwright install`. Run `/security-review`.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the size table. Use the `send_message` tool of the Claude Code Remote MCP server; `SendMessage` doesn't reach the runner. Never merge. Work frugally. Push once per round of fixes.
