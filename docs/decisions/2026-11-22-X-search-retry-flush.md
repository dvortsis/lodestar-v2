# X-search-retry-flush decisions

This supersedes three points of `2026-11-22-X-search-chunk-retry.md` (#1057): "a Retry pressed within that window reloads at the previous query", "the first load to settle uses it up", and the drawer's missing failure path. The runner's calls are under §2.26; their step-0 answers are recorded below.

## Retry sends the query just typed (§10.3, §10.4)
- **W5 gains `flush()`, and the debounce calls the same write (runner, step 0).** `setText`'s 200 ms timer used to run `parse(t)` then `commit(canonical ?? t, false)` inline. That body is now `write()`, and the timer calls it. `flush()` calls the same function, so the text reaches the URL exactly as the bar writes it:
  - the server's `/parse` and its canonical form;
  - the raw text on a parse error;
  - `replaceState`, never a new history entry.
- **`flush()` resolves at once when nothing is due.** Typed text is "due" from `setText` until its write commits. A structured edit or a submit leaves it set, so a later `flush()` repeats the parse, and the commit makes no history entry when the URL already matches.
- **The wait is bounded at 1 s (`FLUSH_PARSE_MS`, runner).** Retry runs after something has already failed, often the network. If `/parse` hasn't answered within 1 s, the parse is cancelled and the raw text is written, as a parse error writes it. A debounce parse already in flight is restarted under the bound.
- **A second Retry joins the flush already running (self-review).** Without that, its `slot.cancel()` would abort the first click's parse, and the first click would then reload at the old query.
- **`flush()` is for Retry only, right before a reload.** After a timeout the raw text is in the URL with no AST for it, which the reload resolves. If `flush()` rejects (a failed `goto`), the click's promise rejects and the page doesn't reload; Retry can be pressed again. A `.catch` would cost bytes the route doesn't have.
- **The chain:** `Live.flush()`, then `QueryBar.flush()`, then `+page.svelte`'s Retry: `await bar?.flush()`, then the marker, then `location.reload()`. Before the editor has loaded there is no draft in W5, and nothing would ever write the URL, so Retry reloads as before.
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
- **What shows, while `page.state` holds `detail`:** a `role="alert"` panel at the bottom edge with "Details couldn't load.", Retry and Close. The strings are route-only keys: `search_page_detail_failed` and `search_page_close`, with `search_page_retry` reused (en and zh).
- **Placement uses only classes already in the global sheet,** so Search CSS stays 19,792 B: `flex flex-wrap items-center gap-4 p-6 fixed inset-x-4 bottom-4 z-50 bg-base-100 shadow large:left-auto`.
  - **Why fixed:** `#search-drawer` is `display: contents` in the grid, so an in-flow alert would land under the rail or below the list.
  - **Below `large`** it spans the width above the bottom edge, where the phone drawer sheet sits.
  - **From `large` up** `large:left-auto` sizes it to its content at the right edge, where the overlay drawer opens.
  - **Checked at 360 and 1440 px:** it never reaches the query bar, and it covers less of the list than the drawer would.
- **Focus doesn't move to the panel.** `role="alert"` announces it, and the row that opened the drawer keeps focus. Retry and Close follow the results' own controls in Tab order (DOM order: `#results`, then `#search-drawer`).
- **Retry is a plain `location.reload()` at the drawer's URL (`/t/<hash>?q=…`), with no marker.**
  - **What's lost:** SvelteKit doesn't restore shallow-routing `page.state` on a reload, so the torrent opens as its full detail page, and the results beside it are gone.
  - **What's kept:** the torrent, and Back returns to the search at its query.
  - **Why no marker:** `window.name` would outlive the reload into the next `/search` mount and move focus there.
- **Close is `history.back()` (runner), then focus goes to the results' active row (self-review).** That's where a drawer's own close returns focus; without it, focus fell to the body when the panel unmounted (WCAG 2.4.3). It isn't `closeDetail()` from a dynamic import of `detail/open`, which cost 31 B more.
  - **Opens from Search:** the two are the same, because every row open pushes the entry.
  - **Back or Forward into a drawer entry:** `pushed` is false there, so `closeDetail()` would replace the entry with `/search?q=…`. `history.back()` steps back one entry instead.
- **No Escape (runner: drop it before Close).** It cost 27 B, and with focus left on the row, Escape on the panel would rarely be reachable.

## Size (§10.1; the 45 kB cap stays, runner)
- **Search route JS, measured with `npm run size:ci`:**
  - 44,847 B before;
  - 45,142 B for the first draft (+142 over the cap);
  - **44,992 B final, with `main` merged: 8 B of headroom.**
- **Other entries:** Search CSS 19,792 B, Search results (lazy) 40,822 B and Detail drawer 28,274 B, all unchanged.
- **Lighthouse `total-byte-weight` on bare `/search`:** 101,471 B on all three mobile runs, against 102,400 B. That's 101,325 B before.
- **Behaviour-neutral trims in owned files, each measured on the tree with this slice's code:**
  - **QueryBar (−43 B):**
    - Safari's `autocorrect="off"` is set in `onMount` instead of through a spread (−8 B). The spread made every attribute of the editor dynamic.
    - The label and message ids are written as literals instead of `LABEL`/`MESSAGE` constants (−19 B).
    - No `type="button"` on the Syntax button, which isn't in a form (−5 B).
    - `onclick={openSyntax}`, and the inline styles without spaces (−11 B).
    - The static combobox now trips Svelte's `a11y_role_has_required_aria_props`, because the spread used to hide it. It's ignored where the existing comment explains it: ARIA 1.2 needs `aria-controls` only while the popup shows, which it never does yet.
  - **`+page.svelte` (−16 B; Close's focus hand-back then costs +5 B):**
    - The first-run check shares the keys' `onMount` (−11 B).
    - The skip link's handler is inline (−5 B).
  - **The drawer panel's compact layout and right alignment cost +10 B** over a plain full-width row.
- **Close is in, at about 57 B.** If a later slice needs those bytes, Close is the piece to weigh first, because Back already closes the drawer.

## Tests
- **W5 (`search.test.ts`):**
  - no-op when nothing is due;
  - writes now, as the debounce would, with `replace`;
  - the raw text on a parse error;
  - the raw text after a parse that never answers, at exactly the 1 s bound (fake timers);
  - a debounce parse in flight is restarted under the bound;
  - a second flush joins the first: one request, one write.
- **e2e (`chunk-load.routes.test.ts`):**
  - Retry within 100 ms of typing reloads at the typed query;
  - Retry while `/parse` hangs reloads at the raw text (two spaces kept). Only the bound can write it, so a missing bound fails at the 10 s deadline. There's no wall-clock assertion: the 1 s itself is pinned with fake timers;
  - after Retry on bare `/search`, a load the 30 s timer starts (`page.clock`) doesn't move focus off the body. The test waits for that load to settle: every chunk fetch answered, and the empty state shown where the browser refetched;
  - a blocked drawer chunk shows the alert with no page error and the row still focused, and Retry opens the full page;
  - Close leaves `/search?q=…` with no drawer entry and focus on the row, and Back also hides the alert.
  - **No fixed sleeps.** The drawer chunk is found by the one chunk that preloads `DetailHostBody`'s stylesheet.
