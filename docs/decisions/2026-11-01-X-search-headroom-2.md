# X-search-headroom-2 decisions

"Search results" sat at 43,204 B against a 43.3 KB limit (~96 B left) after #321 and #327. Runner brief, 2026-11-01 (§2.26: performance change to an owner-set number). No behaviour or visual change beyond items 3–4.

- **Search results leaves out the j/k / x registration.** The panel already imported `$lib/keys` and `results/commands.ts` after mount, with nothing waiting on them, but the entry counted both (the key registry, 2.8 KB gzip, has no source key). They are now `onDemand` for the entry (`resultsKeys`, matched by chunk name as Discover's and the Alerts' are) and gated on their own: "Search results key commands (on demand)", 3,216 B, limit 3.4 KB. `resultsLazy` still holds everything the panel loads and stays the "beyond" list of the entries that load later, so none of them changes. No import or load order changed; first paint and the request count are as before.
- **Not found:** `ResultRow` (3.8 KB), `VirtualList` (11.8 KB) and `events`/`status` are needed on first view; no duplicate helpers or dead code in `search/results`.
- **Limit** (never raised): 43.3 → 40.6 KB. Measured + 10% of the cut would be 40.3 KB (under the 0.6 KB minimum), so it is measured + 0.6 KB rounded up: 609 B headroom. A comment beside the entry names what not to import into it.
- **`s`, `c` and `o` wait for the torrent (item 3).** `DetailModel` now publishes the key target when the torrent has loaded (`setDrawerTarget(hash)`) and clears it in `clear()`, replacing the calls in `DetailHostBody.svelte` and `/t`. Before, the target was set at once, so `s` on `/t` sent a torrent that was still loading (or never would). On a drawer opened from a row, the focused row's hash is the target meanwhile (unchanged). Test: `target.page.test.ts`. Left as is: the 401/403/404 error paths keep the old target, as before.
- **A neutral flows-door error (item 4).** `commands.ts` gains `flowsFailed` ("Couldn't open that. Try again." / zh), used only by the drawer's "Block this…" load failure; the toast store and the string load on demand. `DetailActions.door.test.ts` checks the text.

Measured (bytes, gzipped; `npx size-limit --json`; local builds; two runs identical):

| Entry                                   | `main` | This branch | Limit before → after |
| --------------------------------------- | ------ | ----------- | -------------------- |
| Search results (lazy)                   | 43,204 | 39,991      | 43.3 → 40.6 KB       |
| Search results key commands (on demand) | n/a    | 3,216       | new, 3.4 KB          |
| Detail drawer (own JS + CSS)            | 27,279 | 27,290      | 27.4 KB unchanged    |
| /t/[hash] route (own JS + CSS)          | 20,923 | 20,933      | 21.0 KB unchanged    |
| Detail sections (on demand)             | 37,202 | 37,324      | unchanged            |

The drawer and /t grew by 11 and 10 B (the model's two calls and import, net of the three calls removed from the host and the page); both stay inside their unchanged limits. Detail sections grew by 122 B: the new string and `flowsFailed`, on the on-demand side. Search cards, table mode and column chooser moved by ±1 B (chunk naming). Alert, Alerts, Results toolbar, Discover, the search route and CSS did not grow. Baselines unchanged.
