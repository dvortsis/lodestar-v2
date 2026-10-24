# features/search/toolbar

The results toolbar (architecture §10.4, S-search-3): parameters, kept apart from the filters, each one in the URL. Decisions: `docs/decisions/2026-10-24-S-search-3.md`.

- **`ResultsToolbar.svelte`:** the page loads it with the results panel (`routes/search/+page.svelte`) and it shows once there is a query (`results.status !== 'empty'`).
  - **Count:** the rows' count from the results store, `formatCount` ("1,284 results"), or `formatCompact` with "≈" when the server marks it approximate (`docs/metrics.md`, "Result count"). Visual only: the panel's polite region announces it (§10.13).
  - **Sort:** a native select over `SORTS` (`toolbar.ts`), written through W5's `setSort`, so it's a history entry.
  - **Density:** Comfortable/Compact, the `density` URL parameter the rows read. It replaces the entry: Back walks searches, not views.
  - **Save search:** the alerts' `SaveSearchDialog` (`features/alerts/README.md`), loaded on the first press, with the shown query and sort. It stays on the page and toasts.
  - Every write calls `ensureSearchPath()` first (`query/path.ts`), so a change made with the drawer open closes it instead of navigating to the detail page.
- **`toolbar.ts`:** the sorts and the density link, pure and unit-tested.
- **`toolbar.css`:** its styles, loaded with it (tokens only).

Not here yet: Group versions (M7), Export (B10b) and select mode (S-search-5). They're left out, not shown disabled.
