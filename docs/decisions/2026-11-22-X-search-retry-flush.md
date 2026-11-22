# X-search-retry-flush decisions

This supersedes three points of `2026-11-22-X-search-chunk-retry.md` (#1057): "a Retry pressed within that window reloads at the previous query", "the first load to settle uses it up", and the drawer's missing failure path. The runner's calls are under §2.26; their step-0 answers are recorded below.

## Retry sends the query just typed (§10.3, §10.4)

- **W5 gains `flush()`, and the debounce calls the same write (runner, step 0).** `setText`'s 200 ms timer used to run `parse(t)` then `commit(canonical ?? t, false)` inline. That body is now `write()`, and the timer calls it. `flush()` calls the same function, so the text reaches the URL exactly as the bar writes it:
  - the server's `/parse` and its canonical form;
  - the raw text on a parse error;
  - `replaceState`, never a new history entry.
- **`flush()` resolves at once when nothing is due.** Typed text is "due" from `setText` until the URL holds it: `due` clears only after the commit, so a Retry during the debounce's own commit still writes (runner review). An outside URL change (`sync()`) clears it too, so Retry after "type, then Enter" makes no extra `/parse` (runner review). A structured edit or a submit leaves it set; a later `flush()` then repeats the parse, and the commit makes no history entry when the URL already matches.
- **The wait is bounded at 1 s (`FLUSH_PARSE_MS`, runner).** Retry runs after something has already failed, often the network. If `/parse` hasn't answered within 1 s, the parse is cancelled and the raw text is written, as a parse error writes it. A debounce parse already in flight is restarted under the bound.
- **A second Retry joins the flush already running (self-review).** Without that, its write's parse would supersede the first click's (`latest()` aborts the previous run), and the first click would then reload at the old query. `flush()` has no `slot.cancel()` of its own: `latest().run()` already aborts the run in flight (runner review).
- **A flush during the debounce's own commit can send a second `/parse`.** The text is still due while that commit runs. If the second parse hangs past 1 s, the raw text replaces the canonical `q` the first commit wrote, and the reload resolves it.
- **Leaving Search during the flush's wait writes nothing (runner review).** `cancelPending()`, which Live calls when it's destroyed, bumps a generation, and a write that started earlier never commits after it. In the browser, the abort that `cancelPending()` sends already ends the parse; the generation check covers a parse that ignores the abort, which the unit test pins. Without it, the 1 s bound could write `q` into the next page's URL (`/insights?q=…`).
- **`flush()` is for Retry only, right before a reload.** After a timeout the raw text is in the URL with no AST for it, which the reload resolves. If `flush()` rejects (a failed `goto`), the click's promise rejects and the page doesn't reload; Retry can be pressed again. A `.catch` would cost bytes the route doesn't have.
- **The chain:** `Live.flush()`, then `QueryBar.flush()`, then `+page.svelte`'s Retry: `await bar?.flush()`, then the marker, then `location.reload()`. The marker and reload happen only if the page is still `/search`: a user who went to Insights during the flush's wait isn't reloaded there, and no marker leaks into the next `/search` mount (runner review). Before the editor has loaded there is no draft in W5, and nothing would ever write the URL, so Retry reloads as before.
- **Owns widened at step 0 (runner):**
  - `web/src/lib/state/search.svelte.ts` and its test;
  - `web/src/lib/features/search/query/{QueryBar.svelte,Live.svelte,types.ts}`.

## The Retry marker is honoured only by the reload's own load

- **The window is the mount itself (runner, step 0).** When the `window.name` marker is set, the page starts `loadResults()` at mount. That load is always the one that uses the marker, and it clears it whether it succeeds or fails.
- **A later load never sees the marker.** A load started by the 30 s fallback timer or a browse-mode keydown finds `retried` already false. This holds even when the mount's load failed and cleared the cached promise.
- **Why the mount and not a time window:** the mount has no time constant to tune, and it costs about 20 B against a `performance.now()` check's 35 B.
- **The effect on bare `/search`:** Retry loads the empty state at once, and focus goes to the results only while the body has focus, as in the `?q=` case. That's right, because the user asked for results (runner).

## A failed detail drawer says so (§10.5)

- **The drawer's `import()` has a rejection handler.** A failed `DetailHost` chunk no longer raises an uncaught rejection.
- **What shows, while `page.state` holds `detail`:** a `role="alert"` panel with "Details couldn't load." and Retry. The string is one route-only key, `search_page_detail_failed`, with `search_page_retry` reused (en and zh).
- **The `hostFailed` flag resets before each `import()`** (runner review), so on a browser that refetches, an attempt in flight never shows an earlier failure.
  - **It has no automated test.** Chrome for Testing 153, which the e2e projects run, keeps a failed `import()` failed for the module graph, so e2e can't reach a refetch.
  - **A component test would need restructuring.** It would have to mount the whole Search route (`$app/state`, QueryBar and its lazy chain) with a module mock that fails on its first evaluation and pends on its second, and Vitest's module cache can't do that within one mounted instance. Moving the import into a loader module it could mock costs route bytes the 45 kB cap doesn't have.
  - **It stays a one-line assignment,** recorded here so a later slice with room can add the loader and its test.
- **Placement: sticky at the top of `#results`, before the toolbar (runner review):** `sticky top-0 z-10 flex flex-wrap items-center gap-4 p-6 rounded-box border border-base-300 bg-base-100`.
  - **The tokens are the Toaster's** (`rounded-box border border-base-300`, design-tokens.md §6). There's no `shadow`: it's Tailwind's default, not a token.
  - **Why not fixed at the bottom, as first built:** that's the Toaster's own slot (`fixed inset-x-4 bottom-4 z-50`, `large:left-auto`), so a toast would cover the panel.
  - **Why not fixed at the top:** the global sheet has only `top-0` for that, which covers the shell's top bar. At 360 px the panel would also come within about 9 px of the query bar (bar top y=121).
  - **Why nothing new:** Search CSS has 8 B of headroom (19,792 / 19,800 B), so no new utility can be added. Every class above is already in the global sheet.
  - **Where it sits:** in flow it's under the query bar and above the toolbar, with `mb-4` between them (runner review; 3 B). While the list scrolls it sticks to the top of the results column, so it's in view wherever the row was opened. Checked at 360 and 1440 px: it's clear of the bar and of toasts.
  - **The costs:** it pushes the list down about 90 px while shown, a layout shift on failure only. Retry comes before the row in Tab order, and focus isn't moved; the panel is `role="alert"`, so it's announced (runner: acceptable).
- **Focus doesn't move to the panel.** `role="alert"` announces it, and the row that opened the drawer keeps focus.
- **Retry is a plain `location.reload()` at the drawer's URL (`/t/<hash>?q=…`), with no marker.**
  - **What's lost:** SvelteKit doesn't restore shallow-routing `page.state` on a reload, so the torrent opens as its full detail page, and the results beside it are gone.
  - **What's kept:** the torrent, and Back returns to the search at its query.
  - **Why no marker:** `window.name` would outlive the reload into the next `/search` mount and move focus there.
- **No Close button, for the 45 kB cap (runner).**
  - **It cost about 57 B:** the button and its en/zh "Close" key. It was built as `history.back()` with focus handed back to the row, and the review round's fixes left no room for it.
  - **Back is the way out,** as it is for the drawer itself. Back clears `page.state.detail`, so the panel goes, and focus stays on the row that opened it. Opening another row replaces the state, so one panel shows, never two.
  - **A later slice can add Close when there's room.** `history.back()` matches `closeDetail()` for every open from Search. It differs only for a drawer entry reached by Back or Forward, where `closeDetail()` replaces the entry with `/search?q=…`.
- **No Escape either:** it cost 27 B, and with focus left on the row, Escape on the panel would rarely be reachable.

## Size (§10.1; the 45 kB cap stays, runner)

- **Search route JS, measured with `npm run size:ci`:**
  - 44,847 B before;
  - 45,142 B for the first draft (+142 over the cap);
  - 44,992 B at #1065's first review, with Close;
  - 45,008 B with the runner review's fixes and the panel moved;
  - 44,945 B (`size:ci`) and 44,951 B (plain build) with Close dropped;
  - **44,948 B final (`size:ci`) and 44,955 B in a plain `npm run build` with size-limit: 52 B and 45 B of headroom.** The leave guard's test and the generation check add nothing to the route (W5 is lazy); `mb-4` adds 3 B.
- **The redundant `slot.cancel()` removed from `flush()` saves no route bytes.** W5 ships in the lazy Live and results chunks, not in the route's initial JS.
- **Other entries, as `size:ci` measures them on the merged tree:** Search CSS 19,792 B, Search results (lazy) 40,841 B and Detail drawer 28,280 B. This slice changes none of them; they moved with `main`.
- **Lighthouse `total-byte-weight` on bare `/search`:** 101,421 B on all three mobile runs, against 102,400 B. That's 101,325 B before.
- **Behaviour-neutral trims in owned files, each measured on the tree with this slice's code:**
  - **QueryBar (−43 B):**
    - Safari's `autocorrect="off"` is set in `onMount` instead of through a spread (−8 B). The spread made every attribute of the editor dynamic.
    - The label and message ids are written as literals instead of `LABEL`/`MESSAGE` constants (−19 B).
    - No `type="button"` on the Syntax button, which isn't in a form (−5 B).
    - `onclick={openSyntax}`, and the inline styles without spaces (−11 B).
    - The static combobox now trips Svelte's `a11y_role_has_required_aria_props`, because the spread used to hide it. It's ignored where the existing comment explains it: ARIA 1.2 needs `aria-controls` only while the popup shows, which it never does yet.
  - **`+page.svelte` (−16 B):**
    - The first-run check shares the keys' `onMount` (−11 B).
    - The skip link's handler is inline (−5 B).

## Tests

- **W5 (`search.test.ts`):**
  - no-op when nothing is due;
  - writes now, as the debounce would, with `replace`;
  - the raw text on a parse error;
  - the raw text after a parse that never answers, at exactly the 1 s bound (fake timers);
  - a debounce parse in flight is restarted under the bound;
  - a second flush joins the first: one request, one write;
  - a flush during the debounce's own commit still writes;
  - an outside URL change (`sync()`) leaves nothing due;
  - a flush waiting on its bound writes nothing once the editor leaves (`cancelPending()`).
- **e2e (`chunk-load.routes.test.ts`):**
  - Retry within 100 ms of typing reloads at the typed query;
  - Retry while `/parse` hangs reloads at the raw text (two spaces kept). Only the bound can write it, so a missing bound fails at the 10 s deadline. There's no wall-clock assertion: the 1 s itself is pinned with fake timers;
  - after Retry on bare `/search`, typing keeps focus and every key while the held results chunk settles. The chunk is held until typing starts, so the "nothing else has focus" guard is what the test proves;
  - after Retry on bare `/search`, a load the 30 s timer starts (`page.clock`) doesn't move focus off the body. The test waits for that load to settle: every chunk fetch answered, and the empty state shown where the browser refetched;
  - a blocked drawer chunk shows the alert with no page error and the row still focused, and Retry opens the full page;
  - opening another row while the alert is up keeps one alert; Back then clears it with focus on the row; another row after that shows one alert, not two;
  - leaving for Insights while Retry waits on a hanging `/parse` (`page.clock` past the bound): no reload, no `q` in the Insights URL, and `window.name` stays empty. Changing the guard to `if (true)` fails it.
  - **No fixed sleeps.** The drawer chunk is found by the one chunk that preloads `DetailHostBody`'s stylesheet.
  - **`blockChunk` retries a chunk fetch the preview server resets** (`ECONNRESET`, under parallel load), up to three tries at once. If every try resets, it aborts the route as a reset. That was a shared flake in this spec (runner: fix it here).
