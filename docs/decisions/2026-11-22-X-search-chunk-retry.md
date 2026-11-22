# X-search-chunk-retry decisions (#1057)

- **A failed Search results chunk shows an alert where the results go (§10.4, §10.1).** The page shows `role="alert"`, "Results couldn't load." and a Retry button, inside `#results` in place of the panel. It uses the Insights stage page's markup and classes (`flex flex-col gap-4 p-6`, `btn btn-primary`), all already in the global sheet, so Search CSS doesn't change. The strings are two route-only keys, `search_page_results_failed` and `search_page_retry` (en and zh). Reusing `search_query_retry` would put the shared message in its own chunk, a static import of the route node and one more request on every first load (runner review). The message stays terser than the sibling screens' "Check your connection, then try again.", because that clause costs about 60 B gzipped on the first load. The query bar stays usable. Why: before this, the page showed only the query bar and raised an uncaught rejection (#1036).
- **Retry reloads the page, as Insights does (runner, step 0, §2.26).** Measured on Chrome for Testing 153 (`/opt/pw-browsers/chromium`, which the e2e projects run): a failed dynamic import stays failed for the whole module graph.
  - A second `import()` of the same chunk fails at once without a request, even with a cache-busting query on the top module, because the shared static dependency stays failed.
  - The 157 headless shell does refetch.
  - Retry therefore calls `location.reload()` at the current URL. The URL holds the query once the bar has written it, after its 200 ms debounce and parse. A Retry pressed within that window reloads at the previous query; the bar's text isn't flushed first, because the bar is another slice's.
  - Before reloading, Retry sets `window.name` to a one-shot marker. The new document reads it and clears it at once, and the first load to settle uses it up. A success sends focus to the results region (its active row, or the region itself) only while nothing else has focus, checked again inside the timeout. On a bare `/search`, the load that settles is the one the user started by clicking into the bar, so a slow chunk never pulls focus from typing, which is QueryBar's rule (runner review, blocking). A failure just discards the marker. The marker never enters the URL, so a copied link, a bookmark or a later refresh never moves focus, and `window.name` never throws, unlike storage. A first draft used a `#results` hash, but every URL the bar writes keeps the hash, so later loads could take focus from the bar (self-review).
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

  **Measured with `npm run size:ci`: 44,847 B of 45,000 B, so 153 B of headroom** (44,955 B before the route-only Retry key) for the next slice that touches the Search route. Search route CSS (19.79 kB) and Search results (lazy) (40,822 B) don't change.
- **Lighthouse's first-load weight: comments out of `app.html`, not a higher limit (runner, §2.26).** Lighthouse checks `total-byte-weight` on bare `/search` against 102,400 B (`web/lighthouserc*.json`, §10.1's 100 KB first load).
  - **This PR alone went over:** 102,468 B on CI at 052815ff (102,443 B locally). `main` measures about 102,228 B locally, worked out from the files that differ.
  - **The alert can't move out of the route:** it must show exactly when lazy chunks fail, so its cost stays on the first load.
  - **Following S-locale-boot's precedent, the load shrank instead of the limit rising.** The three explanatory HTML comments in `web/src/app.html` shipped in every first load. Their text moved unchanged to `web/src/app-html.md`, each headed by the element it described.
    - Both `<!-- prettier-ignore -->` markers and the indentation stay.
    - Nothing reads those comments: `scripts/themes.ts` and `scripts/messages.ts` rewrite only the prepaint and boot-locale script bodies.
    - The built `index.html` is byte-identical apart from the comments, its three scripts included.
  - **Results:** `index.html` gzipped went from 3,646 to 2,966 B. Lighthouse on bare `/search` measured 101,767 B locally, mobile and desktop, with the comments out. With the route-only Retry key added, the final head measures **101,325 B** on all three mobile runs, 1,075 B under the limit.
  - **Not done:** collapsing the indentation would save about 145 B more. It was left out, because it needs a build step and the margin is already enough.
