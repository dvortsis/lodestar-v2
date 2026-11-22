# X-search-chunk-retry decisions

- **A failed Search results chunk shows an alert where the results go (§10.4, §10.1).** The page shows `role="alert"`, "Results couldn't load." and a Retry button, inside `#results` in place of the panel. It uses the Insights stage page's markup and classes (`flex flex-col gap-4 p-6`, `btn btn-primary`), all already in the global sheet, so Search CSS doesn't change. The strings are one new key, `search_page_results_failed` (en and zh), plus the existing `search_query_retry`. The query bar stays usable. Why: before this, the page showed only the query bar and raised an uncaught rejection (#1036).
- **Retry reloads the page, as Insights does (runner, step 0, §2.26).** Measured on Chrome for Testing 153 (`/opt/pw-browsers/chromium`, which the e2e projects run): a failed dynamic import stays failed for the whole module graph.
  - A second `import()` of the same chunk fails at once without a request, even with a cache-busting query on the top module, because the shared static dependency stays failed.
  - The 157 headless shell does refetch.
  - Retry therefore calls `location.reload()` at the current URL. The URL holds the query once the bar has written it, after its 200 ms debounce and parse. A Retry pressed within that window reloads at the previous query; the bar's text isn't flushed first, because the bar is another slice's.
  - Before reloading, Retry sets `window.name` to a one-shot marker. The new document reads it and clears it at once, and the first load to settle uses it up. A success sends focus to the results region (its active row, or the region itself), not to `body`; a failure just discards it. The marker never enters the URL, so a copied link, a bookmark or a later refresh never moves focus, and `window.name` never throws, unlike storage. A first draft used a `#results` hash, but every URL the bar writes keeps the hash, so later loads could take focus from the bar (self-review).
- **The cached promise is still cleared on failure,** so the next URL change (a new query) calls `import()` again. That recovers in place wherever the browser refetches. Where it fails again, the same alert node stays, so it is announced once.
- **No Enter-triggered reload (runner).** A reload must never fire while someone is typing. Retry covers the new-query case.
- **The Search route JS cap stays at 45 kB (runner; the owner's smallest-UI priority, #1028).** It measured 44,682 B before this slice and 45,051 B with the first draft. Without raising the cap, the draft got back under it in the route itself, with no behaviour change:
  - the shorter wording;
  - no `type="button"` (the button isn't in a form);
  - no `data-testid` on the alert (tests find it by role inside `#results`);
  - no whitespace text node in the alert;
  - `failed` never reset (the panel takes precedence);
  - the URL test written the way QueryBar's is, so the two compress together;
  - the detail drawer loaded with a plain `import()` instead of the `import.meta.glob` that stood in until `DetailHost.svelte` existed. It is the same lazy chunk, and the Detail drawer sizes don't move.

  **Measured with `npm run size:ci`: 44,955 B of 45,000 B, so 45 B of headroom** for the next slice that touches the Search route. Search route CSS (19.79 kB) and Search results (lazy) (40,822 B) don't change.
