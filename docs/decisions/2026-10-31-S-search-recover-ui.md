# S-search-recover-ui decisions

UI for `recovery` on `/search` and `GET /search/state` (`2026-10-31-B-search-recovery.md`, "UI follow-up"). Code: `web/src/lib/features/search/results/{recover.ts,Recover.svelte,IndexState.svelte,ResultsPanel.svelte}`.

- **No results:** `recovery.clauses[]` with `total > 0` become "Removing `{clause}` gives {N}" buttons (`≈` when `total_approx`); one click searches the server's `without` AST through `search.setAst`, never re-parsed. Entries without `total`, and `skipped`, show nothing. The existing polite region announces the new count once the reduced query answers. The per-chip "Remove X" buttons and "Clear all filters" stay below.
- **Index state:** `/search/state` replaces `/stats` and `COLD_START_BELOW` on the Search screen (the thresholds are the server's). It is fetched once per Search visit and again when the shell's `rebuild` snapshot changes phase; no polling. The bare-page title ("Search N torrents") now reads `searchable_torrents` from it, so the screen makes one request fewer.
  - `rebuild` (not done or failed) gives the "Index rebuilding" banner with done/total and the `unavailable` fields; it wins over `growing`.
  - `growing` gives the "still growing" banner; `growth_per_hour` null or 0 omits the rate (never "+0/h").
  - `cold_start` gives the full-page state on the bare search and on a zero-result answer.
- **Refinement for the runner to flag (owner-decisions §2.19): the full-page cold-start state is text-only.** §10.19 pairs it with the `searching_crawling` scene, but importing `StateScene` into the results chunk grew "Search results (lazy)" by about 1.5 KB gzipped past its 43.2 KB limit, which is not to be raised. The art can come back with an on-demand size entry for it.
- **No layout shift:** the banner chunk is lazy, so the rows wait for it (as they already waited for the figures, at most one second) rather than being pushed down.
- Message keys live in `search/messages` (`search_recover_*`, `search_state_*`); the two old `search_results_*` banner keys went.
