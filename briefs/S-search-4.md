# S-search-4: Search — autocomplete, smart chips, natural phrasing, Builder
**Model:** Sonnet: UI per §10.4, following the S-search-1/2/3 pattern. Runner brief, 2026-10-31. M5 screen backlog: "Search completion", part 1 of 3. Its dependencies have merged: S-search-3 and B7 (`/suggest`, natural phrasing on `/parse`). No `/security-review`: this renders server values as text and sends only the user's own query.

## Read first
- CLAUDE.md's UI rules, `runner/standards.md` and `docs/owner-decisions.md`.
- `docs/architecture.md` §10.4, "Query bar" (around line 413). That's the spec for autocomplete, smart chips, "Natural phrasing" and the Builder.
- Also: §10.1 (budgets), §10.2 (control heights), §10.13 (combobox/listbox pattern, announcements, the single-key rule), §10.15 (breakpoints) and §10.16 (keys: Tab accepts).
- The API: `crates/api/src/suggest.rs` and `/parse` in `crates/api/src/`. Take the exact shapes from `web/src/lib/api/schema.d.ts`; never invent fields.
- The existing code: `web/src/lib/features/search/query/**` (QueryBar, tokenize, spans, clauses, editor, the syntax popover), `search/README.md`, and the S-search-1/2/3 decisions files.

## Do
1. **Autocomplete** (`features/search/assist/**`), mounted from QueryBar. A WAI-ARIA combobox/listbox showing:
   - fields, and values with counts (from `/suggest`), units, saved searches and recent queries;
   - "did you mean", when the server offers it.

   Behaviour:
   - Tab or Enter accepts; Esc closes; arrow keys move through the list.
   - Requests are debounced, with latest-wins and abort on new input.
   - Recent queries are kept in `localStorage`, with every access in try/catch, and capped.
   - Counts use `lib/format`, with "≈" only when the server marks a count approximate.
   - Everything announces through the existing polite region, not a new one.
2. **Smart chips:** a recognized token (for example `2160p`) is offered as a `res:2160p` chip suggestion, and accepting it rewrites the query through the existing clause and AST path.
3. **Natural phrasing:** an action that sends the text to `/parse`'s natural-phrasing mode and shows the resulting chips as a preview the user applies or dismisses. Nothing changes until they apply it.
4. **Builder:** a toggle opens an inline row builder (field / operator / value) that round-trips with the text through the **server AST**: edits in either view update the other. Fields and operators come from the schema the server exposes, not a hard-coded list, unless the existing query code already has the canonical list; reuse it if so.
5. **Sizes:** /search first load must not grow. Load the assist panel, the natural-phrasing preview and the Builder on demand (on focus or first keystroke, or the toggle). Add size-limit entries for the new chunks at measured + 10%. Report every delta, and never raise an existing limit.
6. **i18n:** `search_assist_*` keys in `features/search/messages/{en,zh}.json`. zh can use the documented fallback.
7. **Tests:**
   - vitest: combobox keyboard behaviour, debounce and abort, recent-query storage (including a corrupt or unavailable `localStorage`), chip rewrite, the natural-phrasing preview with apply and dismiss, and the Builder round-trip;
   - Playwright: a keyboard-only spec (type, arrows, Tab accepts, results update), plus Builder round-trip, against the existing mock or fixture backend, with axe;
   - condition waits only, no fixed sleeps;
   - **no baseline changes expected.** If screenshots change, stop and tell the runner before regenerating.
8. **Decisions file:** `docs/decisions/<date>-S-search-4.md`.

## Own
`web/src/lib/features/search/assist/**`, minimal mount edits in `features/search/query/QueryBar.svelte` (justify each in the PR), the `search_assist_*` message keys, `web/e2e/search-assist.*`, the size-limit entries for new chunks, and the decisions file.

**Don't edit:** `lib/state/**`, `lib/keys/**`, `lib/shell/**`, `lib/ui/**`, `lib/api/**`. If you need something there, stop and tell the runner.

## Size
≲ 1,500 changed lines. If it grows past that, split it:
- **4a:** autocomplete and chips;
- **4b:** natural phrasing and the Builder.

## Done when
These pass in `web/`:
- `npm run check`
- `npx vitest run`
- `npm run build`
- `npx size-limit`
- `npx playwright test` for the new and the existing search specs, 3× for anything new (Chromium is at /opt/pw-browsers; never run `playwright install`)

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head SHA and the size deltas. Never merge. Work frugally: the account is under a usage warning. Push once per round of fixes.
