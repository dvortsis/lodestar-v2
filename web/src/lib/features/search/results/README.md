# features/search/results

The search page's results list (S-search-2a; architecture §10.4, §10.5, §10.12). Decisions: `docs/decisions/2026-10-24-S-search-2a.md`.

## Files

- `ResultRow.svelte`: the shared result row, used by Search, Alerts (`features/alerts/AlertDetailPage.svelte`) and Discover (`features/discover/DiscoverPage.svelte`). Comfortable is the fixed 64 px three-line row (84 px with the "Matched in N files" line), Compact the 36 px line. Props: `hit`, `density`, `testid` (prefix of every `data-testid` in the row), `matched`, and two snippets: `actions` (Search's row actions) and `extra` (a feature's own fact at the end of the facts line). Opening a row is its container's job (VirtualList `onActivate`, or a link around the row, as in Discover's side panels): there are no links inside a row.
- `row.css`: the row's styles. A plain sheet, so each user imports it with its page (a lazily imported component's own `<style>` loads with the route).
- `RowActions.svelte`: Copy magnet and More (Open in a new tab, Copy info hash). Loaded with the first results; the More menu (Bits UI) loads on its first click. Out of the tab order: the keyboard has `c`, `o`, and Shift+F10 or the menu key on a row for More.
- `results.svelte.ts`: the store (below).
- `ResultsPanel.svelte` + `results.css`: the panel in `#results`: every state, the list, j/k. `results.css` also holds the page grid's rule (see the search README).
  - **Paging (S-search-3):** the list's sentinel asks for the next cursor page as it scrolls near the end. Under the list, a visible "Load more" does the same for the keyboard and AT, and an end line replaces it after the last page. One request either way (the store ignores a second `loadMore` while one is out). Focus stays on the button while a page loads, and moves to the end line, or to Retry when the page fails, if the button had it. A new page isn't announced, beyond the list's own "Loading more torrents…" status (§10.13).
- `commands.ts`: j/k in the `search` scope.
- `composition.ts`: a hit's composition as CategoryBar segments, summary and dominant share.
- `links.ts`: resolved links for the empty state and the adult notice.

## Loading

`routes/search/+page.svelte` imports the panel with `row.css` and `results.css` (all three before it mounts) when the URL has `q` or `hidden`, or on the first interaction (`onFirstInteraction`, `KEYS_AFTER_MS`). The bare page's first load never fetches any of it: `scripts/first-load.mjs` strips the preload lists from kit's entry and from route nodes, and `.size-limit.mjs` fails if a first-load file lists JS to preload. "Search results (lazy)" gates the chunk.

## The store, for S-search-3 (toolbar)

`results` (`results.svelte.ts`) is the page's one store. The panel feeds it from W5 (`ast`, `sort`, `hidden`, `exact`); the toolbar only reads it:

| Getter                   | Meaning                                                                                          |
| ------------------------ | ------------------------------------------------------------------------------------------------ |
| `status`                 | `empty` (no query), `loading` (first answer not in, no rows to keep), `ready`, `error` (no rows) |
| `total`, `totalApprox`   | The server's count for the rows shown; `totalApprox` means "about"                               |
| `refining`               | A newer query is out while the previous rows stay (the hairline)                                 |
| `error`                  | The latest request's failure (`ApiError`); with rows, they're the previous query's               |
| `showing(q)`             | The rows answer this query (false while a different one refines, or after it failed)             |
| `hits`, `notices`        | The rows, and `adult_hidden` when the age setting hid matches                                    |
| `hasMore`, `loadingMore` | Cursor paging (`next_cursor`); an expired or unreadable cursor reloads the query from the top    |
| `expired`                | That reload happened (the panel says so); cleared by the next query                              |
| `shown`                  | The key of the query the rows answer; the list resets its scroll when it changes                 |
| `moreError`              | A failed page; `retry()` asks for it again (or reloads the query after a first-page error)       |

Rules: the latest query wins (an older answer is dropped); the same query is never asked for twice (in flight or settled, while the panel is mounted; only `retry` reloads, and the panel's unmount calls `forget()`), so a URL change that isn't a query change keeps the rows and their pages; `load` is untracked, so calling it from an effect doesn't make the effect depend on the rows. A query that hasn't parsed yet (in flight, or a syntax error) doesn't load: the last good rows stay, as the strip does.

## Not yet (follow-ups)

The live "N new" banner, per-clause zero-result counts, hidden counts, highlights, Send (`s`), recent searches, "In library" and matched file paths. The empty state's "+N in the last hour" waits for an API figure (runner decision 2).
