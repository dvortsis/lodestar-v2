# S-search-recover-ui decisions

UI for `recovery` on `/search` and `GET /search/state` (`2026-10-31-B-search-recovery.md`, "UI follow-up"). Code: `web/src/lib/features/search/results/{recover.ts,Recover.svelte,IndexState.svelte,ResultsPanel.svelte}`.

- **No results:** `recovery.clauses[]` with `total > 0` become "Removing `{clause}` gives {N}" buttons (`≈` when `total_approx`); one click searches the server's `without` AST through `search.setAst`, never re-parsed. Entries without `total`, and `skipped`, show nothing. The existing polite region announces the new count once the reduced query answers. The per-chip "Remove X" buttons and "Clear all filters" stay below.
- **Index state:** `/search/state` replaces `/stats` and `COLD_START_BELOW` on the Search screen (the thresholds are the server's). It is fetched once per Search visit and again when the shell's `rebuild` snapshot changes phase; no polling. The bare-page title ("Search N torrents") now reads `searchable_torrents` from it, so the screen makes one request fewer.
  - `rebuild` (not done or failed) gives the "Index rebuilding" banner with done/total and the `unavailable` fields; it wins over `growing`.
  - `growing` gives the "still growing" banner; `growth_per_hour` null or 0 omits the rate (never "+0/h").
  - `cold_start` gives the full-page state on the bare search and on a zero-result answer.
- **Cold start page:** on the bare search only, with the `searching_crawling` scene (§10.19) in the IndexState chunk (+1.3 KB, its own size entry), a title, "{N} torrents so far[, +{rate}]", and "Start typing". The runner restored the art under §2.26. The bitmagnet import link comes when that page exists.
- **No layout shift:** the banner chunk is lazy and loads inside the `/search/state` fetch the rows already wait for (at most one second; a failed chunk load just leaves the banner out). The bare page's empty state waits for the same fetch, so the cold-start page never swaps in over it. A live rebuild still shows if `/search/state` fails. A rebuild that starts mid-visit shows its banner without unmounting the rows. The "Removing X" block stays hidden until its chunk lands, so the buttons below never move.
- **Rate:** `formatRate` (`+2.1K/h`); a rate under 1/h is omitted rather than shown as "+0/h".
- **Cold start with a zero-result query** shows the "still growing" banner above the normal No results block (recovery stays visible); the full-page state is for the bare search only.
- **Follow-up, not built:** the top-level `unavailable` of `/search/state` (fields missing with no rebuild running) is not shown; only the rebuild banner lists fields. A banner for it is a small follow-up.
- Message keys live in `search/messages` (`search_recover_*`, `search_state_*`); the two old `search_results_*` banner keys went.
