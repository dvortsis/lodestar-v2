# /search's first load no longer grows with the route count

Runner request, 2026-10-23, after S-search-1. Every route that landed made `/search`'s first load bigger: 93.7 KB in Lighthouse at #132, then 94.8 KB and 97.6 KB on later branches, against the owner's 102,400 B (§10.1). Discover, Settings, Insights and Logs would have pushed it over. The owner's numbers are unchanged.

## Where the growth came from

Measured on `main` at 9c270ad (`/search`, gzipped), against a copy with only `/` and `/search` (the same tree with the other routes deleted):

| Piece                                                | `/search` only | All 14 routes | Per route  |
| ---------------------------------------------------- | -------------- | ------------- | ---------- |
| Route table (kit's `app.js`)                         | 1,261 B        | 2,215 B       | about 80 B |
| Vendor chunk (Svelte runtime and other dependencies) | 29,792 B       | 30,416 B      | about 50 B |
| Everything else                                      | the same       | the same      | 0          |

- **The route table** carried Vite's preload map (`__vite__mapDeps`): for every route node, the list of JS chunks to fetch with it. That list is on `/search`'s first load, and grows with every route and every chunk a route adds.
- **The vendor chunk** holds only modules `/search` reaches (#132's stray check). But it is shared, so when another route uses more of those modules' exports, tree-shaking keeps them, and `/search` downloads them.

## What changed

- **The route table preloads no route's JS** (`routePreloads` in `web/scripts/first-load.mjs`, set as Vite's `build.modulePreload.resolveDependencies`).
  - For kit's entry only, a route node's preload list keeps its CSS and drops its JS. The CSS must load with the node, or the page paints unstyled.
  - The node's JS imports load when the node does. Kit starts that on hover and on touch (`data-sveltekit-preload-data="hover"` in `app.html`), before the navigation itself.
  - **The trade-off:** a route's chunks now load one import level at a time instead of all at once, which mostly happens during the hover. Every other dynamic import keeps Vite's preloads.
  - The route table went from 2,215 B to 1,575 B. Each further route now adds about 27 B: its import and its dictionary entry.
- **The vendor chunk has a ceiling.** Built without tree-shaking, the modules `/search` reaches measured 31.07 KB: every export they have, fully used. Another route can at most bring the chunk up to that ceiling, and never past it. A new dependency of `/search`'s own can, and should then raise the budget on purpose.

## The guard (`npx size-limit`)

- **`app.js` lists no JS to preload.** `preloadedJs` reads the built entry's map, and size-limit throws if it lists any `.js`.
- **"Route table (app.js)"** is budgeted at 1.26 KB, measured with only `/` and `/search`, plus 40 B a route. A route that adds more than that to the table fails.
- **"Search vendor chunk"** is budgeted at its ceiling, 31.1 KB.
- **Measured with a probe route** that uses transitions, animations, `svelte:element`, size bindings, stores, `await` and a boundary:
  - `/search` grew by 322 B in total: 22 B in the route table and 293 B in the vendor chunk;
  - every budget still passed.

## Before and after (`/search`, `main` at 9c270ad)

|                  | Before      | After       |
| ---------------- | ----------- | ----------- |
| size-limit JS    | 41.03 kB    | 40.38 kB    |
| Lighthouse bytes | 95,721 B    | 95,088 B    |
| Mobile LCP       | about 1.5 s | about 1.5 s |

What another route can still add is the route table's 27 B or so per route, plus vendor growth up to the 31.1 KB ceiling. About 0.7 KB of the ceiling is left today, and it is spent only once, however many routes land.

## Also in this PR: S-detail's loading state (runner, named hook edit)

- **What failed:** CI caught an axe `aria-prohibited-attr` on `/t/{hash}`'s loading placeholder (`DetailView.svelte`), a `div` with `aria-label` and no role.
  - The smoke test runs axe once the heading shows, which can be before the torrent arrives. So it failed only when a run caught the loading state: 1 in 36 on CI.
- **The fix:** the placeholder is a polite status (`role="status"`), with its label and `aria-busy`. It goes away when the torrent loads, so the loaded page is unchanged.
- **Test:** `detail.routes.test.ts` holds the torrent, runs axe on the loading state, then on the loaded page. The test fails without the role.
