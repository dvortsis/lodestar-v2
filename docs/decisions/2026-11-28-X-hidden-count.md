# X-hidden-count decisions

Slice `X-hidden-count`, the rest of P7 from the UI review of 2026-11-13 (`docs/review/2026-11-13-ui-ux/01-review.md` P7, `02-skills-pass…md:66`): "2,196 results", "2,208 torrents" and "2.2K" appeared together on Search with nothing saying what each counts. The runner decided the points marked "runner" (§2.26) and flags them to the owner (§2.19).

## What each count was
- **"2,196 results"** (toolbar): `GET /search` `total`, the matches after the default exclusions (filter-hidden, unverified, and Adult while it's hidden). Correct; unchanged.
- **"2,208 torrents"** ("Index still growing" banner, Home's "Search … torrents"): `GET /search/state` `searchable_torrents`, the serving index's `num_docs`, so every indexed torrent, the hidden ones included. The same figure as Insights' "Searchable torrents".
- **"2.2K"** (status pill, "Idle · 2.2K torrents"): the `crawler` event's `torrents`, rows in the store (`Pipeline::torrents()`), which also holds what the index hasn't committed yet. A third figure, close to the other two but not equal.

## Decisions
- **"N hidden · Show" is built here (runner).** X-search-toolbar (#1075) deferred it (its decisions file): `/search` had no hidden count, so the gap between results and searchable had nothing to explain it.
- **It counts exactly what "Show" brings back (runner; content safety, §20.9, owner §2.12).** The count runs `base_query` with `hidden: true` under the request's own Adult visibility and verified default, ANDed with `hidden:yes`.
  - It never counts Adult while Adult is hidden.
  - It doesn't count unverified torrents: "Show" (`hidden=true`) doesn't bring those back ("Show unverified" does, §18.6). The runner's first wording named unverified torrents too; counting them would break its own rule that the count match what "Show" adds, so the runner approved leaving them out.
  - Quarantined and dropped torrents are never in the index, so they're never counted. A torrent a held write deletes drops out at the next commit, as it drops out of `total`.
  - Tested through the route (`adult_visibility.rs`): an Adult torrent and a film both filter-hidden, with Adult hidden, count 1, and `hidden=true` adds exactly that one; with Adult shown, 2.
- **A follow-up request, not a field on the first page (runner).** Inline on `/search`, the second count added 28–57% to the first-page p95 for ordinary queries and about 700% for swarm queries on the bench corpus (it evaluates the user's query again in full; few hidden torrents don't make it cheaper). So:
  - **`POST /search/hidden`**, a route of its own, rather than a limit-0 form of `/search`. `/search` takes limits of 1–200, and a count-only mode there would have to fake `hits`, `total` and the rest, and keep that mode out of the result cache and cursors. The new route has a one-field response (`SearchHiddenCount`) and nothing to keep out. It takes the search's own body (`q` or `ast`, `hidden`, `tz`), so parsing, validation and auth are the search's (`request_ast`, `tz_param`, `same_origin_json`, behind the same login).
  - **Free skip.** It first counts the `hidden:yes` posting list alone; with no hidden torrent in the index it answers 0 without running the query.
  - **Budget.** The count runs under `HIDDEN_COUNT_BUDGET` (the recovery counts' 20 ms). Past it, or when the count would be an estimate, the answer is `null` and the toolbar shows nothing.
  - **No cache.** The result cache keys first pages and their leases; the hidden count runs on a lease of its own for another query, so keeping it there isn't cheap. A repeat search counts again, within the budget.
  - **The client.** The toolbar asks once the rows have painted (after the rows' render, in the next animation frame), for the query the rows answer (`results.query`), never for a query still loading. A newer query, sort, visibility or reload of the rows aborts the request, and an answer for rows no longer shown is dropped. A failure shows nothing. Tested in `ResultsToolbar.test.ts`, the stale answer included, and in `search-toolbar.routes.test.ts`, which holds the answer and checks the count line keeps its height.
- **The toolbar** reads "2,196 results · 12 hidden · Show" while the count is above 0, and nothing extra for 0, null or a failure. "Show" sets `hidden=true` (`search.setHidden`), so Back undoes it. The button's accessible name is "Show", described by "12 hidden". Under a coarse pointer its 44 px target is padding taken back by a negative margin, so the count line keeps its height when the count arrives.
- **The status pill counts searchable torrents (runner).** `CrawlerEvent` gains `searchable` (the serving index's `num_docs`, as `/stats`'s `searchable_torrents`; null with search off); `torrents` stays for other readers. The pill reads "Idle · 2.2K searchable" and shows no count while it's unknown (before: "— torrents"). `PillState.torrents` is renamed `searchable`.
- **The banner and Home say "searchable" (runner).** "Index still growing: 2,208 searchable." (as short as X-search-toolbar left it for one line at 360–390 px: "searchable" replaces "torrents"), "2,208 searchable torrents so far." and "Search 2,208 searchable torrents".
- **Two figures, each labelled.** Searchable (the index: the pill, the banner, Home, Insights) and results (the toolbar), with the hidden ones that separate them counted beside the results.
- **Glossary.** `searchable` and `hidden_by_filters` say where each count shows and how they relate (`docs/ui-glossary.md`).
- **zh strings are machine-drafted** (`个可搜索`, `个已隐藏`, `显示`), for the translator (W4) to confirm, like the glossary's zh column.
- **Size limits (runner §2.26), measured with `npm run size:ci`, rounded up at 0.01 kB:**

  | Entry                  | main     | this branch | limit before → after |
  | ---------------------- | -------- | ----------- | -------------------- |
  | Results toolbar (lazy) | 5.01 kB  | 5,399 B     | 5.02 → 5.40 KB       |
  | Search home (lazy)     | 2.70 kB  | 2,717 B     | 2.71 → 2.72 KB       |
  | CSS                    | 19.80 kB | 19,812 B    | 19.80 → 19.82 KB     |
  | Search results (lazy)  | 44.20 kB | 44,219 B    | 44.224 KB, unchanged |

  Search route JS (initial) is unchanged at 44,999 B.
