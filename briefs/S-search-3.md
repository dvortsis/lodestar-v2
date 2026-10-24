# S-search-3: Search — results toolbar and infinite scroll (the last v0.5 slice)
**Model:** this session (it built S-search-1/2, so it knows the code). **Branch:** `slice/S-search-3`, from `main` after #169 merges. Start locally now; push only after #169 merges. No `/security-review`: nothing here touches auth, filters or untrusted data beyond rendering server values as text.

**Spec:**
- `runner/standards.md`, `docs/owner-decisions.md`, CLAUDE.md UI rules.
- Architecture §10.4 "Results toolbar" (line ~432): parameters, kept separate from filters, all persisted in the URL.
- §10.1 budgets; §10.2 control heights (32 px dense toolbars); §10.13 (announcements, single-key rule); §10.15 (toolbar at each breakpoint); §10.16 (keys).
- §7 sorts: relevance, discovered, size and files, asc/desc. Take the exact sort enum from `schema.d.ts` / `crates/api/src/search.rs`.
- `docs/metrics.md` (count formatting, "≈" only when the server marks it approximate).
- 2a's `results/README.md` (the store contract) and both decisions files (2a, 2b).

## Scope for v0.5
1. **Toolbar** (`web/src/lib/features/search/toolbar/**`), mounted at the route shell's named toolbar line:
   - **Result count:** from the results store, through `lib/format`, with a `data-testid`. It is visual only; the polite announcement stays 2a's.
   - **Sort:** a select or menu covering the server's sorts and direction. It persists in the URL through W5's state. If W5 has no sort field, stop and tell the runner: `lib/state` isn't yours.
   - **Density:** Comfortable/Compact. It persists in the URL and feeds the shared row's density.
   - **Save search:** only if `POST /saved-searches` exists on main. It opens a dialog with a visible name label; notification channels come later with alerts. If the endpoint is missing, leave it out.
   - **Out of scope, and not shown as disabled placeholders:** Group versions (M7), Export (B10b) and select mode (S-search-5). Record each one in the decisions file as deferred, and name the slice that adds it.
2. **Infinite scroll:** check what 2a's `loadMore` already does.
   - Make sure a sentinel near the list's end loads the next cursor page, with no double fetches (latest-wins is already in the store).
   - Add a visible "Load more" button as the keyboard and AT fallback, plus an end-of-results state.
   - Keep focus when a page appends; don't announce each page (§10.13).
   - If 2a already covers all of this, just add the tests that prove it.
3. **Sizes:** /search first-load JS is at about 40.8/45 KB. Load the toolbar's interactive parts (the sort menu, the Save dialog) on demand where they'd push first load. Report every size-limit delta, and don't raise any limit without asking the runner.
4. **v0.5 gate prep:** add (or extend) a fixture-backend e2e that runs keyboard only: search → results → open the drawer → copy magnet. Run it against the real binary per S-search-1's fixture spec pattern. The G-v05 gate reuses it.

## Own
- `web/src/lib/features/search/toolbar/**`;
- the toolbar mount line in `web/src/routes/search/**`;
- `search_toolbar_*` keys in `features/search/messages/{en,zh}.json`;
- e2e `web/e2e/search-toolbar.*.test.ts`;
- the fixture e2e above;
- `docs/decisions/<date>-S-search-3.md`;
- small edits to `results/**` for the scroll fallback (justify each in the PR).

**Don't edit:** `lib/state/**`, `lib/keys/**`, `lib/shell/**`, `lib/ui/**`, `lib/api/**`.

## Done when
- `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit` and the search/shell/boot Playwright specs pass, 3× in a row for anything new.
- No fixed sleeps; deadlines are ≥ 10 s.
- ≲ 1,500 changed lines.
- Self-review against `runner/standards.md` before opening the PR.
- Then message the runner with the PR number, head sha and sizes.
