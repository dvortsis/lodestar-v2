# S-search-meta-web: the Builder reads its fields from the server; "≈" on estimated suggestion counts
**Model:** Sonnet (Svelte, the search assist pattern). The runner reviews and runs `/security-review` (the Builder must stop offering Adult while it's hidden). Runner brief, 2026-11-01. It uses B-search-meta (#335).

## Why
1. **The Builder's field list is hard-coded** in `web/src/lib/features/search/assist/builder.ts` (`FIELDS`, `OPERATORS`). It drifts from the parser, and it **always lists `adult`**, even with "Show adult content" off (§20.9 says nothing Adult shows unless 18+ is on). #335 added `GET /search/fields`: every field the parser accepts, with aliases, value type, operators, fixed values and `facetable`. It leaves out `adult:` and `type:adult` while Adult is hidden.
2. **Suggestion counts can be estimates.** #335 added `count_approx` to suggest items (a title's count past 1,000 comes from the title set). The assist list shows the count without "≈".

## Read first
- `docs/architecture.md` §8 (query language, "≈"), §10.4 (Builder, assist), §20.9.
- `docs/decisions/2026-11-01-B-search-meta.md` (the shape; when values are absent; `count_approx`).
- `web/src/lib/features/search/assist/{builder.ts,Builder.svelte,BuilderRow.svelte,Assist.svelte,options.ts,suggest.ts}` and their tests; `web/src/lib/api/schema.d.ts` (`SearchFieldsResponse`, `SuggestItem.count_approx`). How search totals already render "≈" (reuse that formatter or message).
- `web/.size-limit.mjs` (the assist and Builder entries).

## Do
1. **The Builder's fields come from `GET /search/fields`.**
   - Fetch once when the Builder first opens (it's already on demand), cache it for the session, and refetch when "Show adult content" changes (the existing visibility signal or event the app already has; find it).
   - The field Select lists the server's fields (aliases parse the same, so list names only). Operators come from the field's `operators`; a field with `values` offers a Select of them instead of free text.
   - While loading, the Builder shows its existing loading state. If the request fails, the Builder says so plainly and offers Retry. **Never fall back to a hard-coded list that includes `adult`.**
   - Remove the hard-coded `FIELDS`/`OPERATORS` once nothing needs them. If `rowText`/`withNode` still need per-kind logic, derive the kind from the server's `type`.
   - Existing rows built from a query (`rowOf`) keep working for every field the parser accepts.
2. **"≈" on estimated counts** in the assist list when `count_approx` is true, the same way a search total shows it (the visible text and the screen-reader text). Exact counts are unchanged.
3. **Safety (blocking):** with Adult hidden, neither the Builder nor the assist list offers `adult` or `type:adult` from any client-side list. Add a vitest that fails if a hard-coded `adult` comes back.
4. **Tests:** vitest for: fields from the response, operators per type, a `values` field showing a Select, Adult hidden vs shown (a refetch on change), the failure state with Retry, and "≈" vs exact. Update the Builder e2e spec if one covers the field list; condition waits only.
5. **Sizes:** the Builder entry may grow or shrink. Removing the hard-coded table should pay for the fetch. Search results, Results toolbar and the search route must not grow (≤ +0.05 KB each). Never raise a limit. Report before → after.
6. **Decisions file:** `docs/decisions/<date>-S-search-meta-web.md`.

## Own
`web/src/lib/features/search/assist/**` (incl. messages), its size entries, specs and your decisions file.
**Don't edit** `crates/**`, `web/src/lib/api/**`, other features or `vite.config.*`. Don't run formatters over files you didn't change; check each commit's file list (`git show --stat`) before pushing.

## Size
≲ 450 changed lines.

## Done when
These pass in `web/`: `npm run check`, `npm run e2e:typecheck`, `npx vitest run`, `npm run build` then `npx size-limit` (twice; identical), and `npx playwright test` for search-query*, search-assist*/builder* (whichever exist) and core.

Chromium is at /opt/pw-browsers; never run `playwright install`. If the pinned headless shell is missing, point `PLAYWRIGHT_BROWSERS_PATH` at a scratch folder of symlinks to the installed one. Don't background long jobs with `&`.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the size table. Use the `send_message` tool of the Claude Code Remote MCP server. Never merge. Work frugally. Push once per round of fixes.
