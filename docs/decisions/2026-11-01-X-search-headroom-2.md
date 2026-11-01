# X-search-headroom-2 decisions

"Search results" sat at 43,204 B against a 43.3 KB limit (~96 B left) after #321 and #327. Runner brief, 2026-11-01 (§2.26: performance change to an owner-set number). No behaviour or visual change beyond items 4–5.

- **Search results leaves out the j/k / x registration.** The panel already imported `$lib/keys` and `results/commands.ts` after mount, with nothing waiting on them, but the entry counted both (the key registry, 2.8 KB gzip, has no source key). They are now `onDemand` for the entry (`resultsKeys`, matched by chunk name as Discover's and the Alerts' are) and gated on their own: "Search results key commands (on demand)", 3,216 B, limit 3.4 KB. `resultsLazy` still holds everything the panel loads and stays the "beyond" list of the entries that load later, so none of them changes. No import or load order changed; first paint and the request count are as before.
- **Not found:** `ResultRow` (3.8 KB), `VirtualList` (11.8 KB) and `events`/`status` are needed on first view; no duplicate helpers or dead code in `search/results`.
- **Limit** (never raised): 43.3 → 40.61 KB. Measured + 10% of the cut would be 40.33 KB (under the 0.6 KB minimum), so it is measured + 0.6 KB: 600 B headroom. A comment beside the entry names what not to import into it. The entry's name match means a static `$lib/keys` import would not show here; the entry's comment and the unchanged `carried` guard are what stop it (same as Discover's).
- **`s` waits for the torrent (item 4).** `DetailModel` publishes the loaded torrent (`setLoadedHash`) on success and clears it in `clear()` and on a refused (401/403/404) hit lookup; `send.ts`'s `when` for `s` requires, while a drawer or `/t` is open, that its torrent is the loaded one. A focused row with no drawer still works as before. `c` and `o` are unchanged (they fetch what they need). Tests: `send.test.ts`, `target.page.test.ts` (loads, clears on unmount, a 404 never becomes the target).
- **A neutral flows-door error (item 5).** `commands.ts`'s `failed(true)` shows "Couldn’t open that." (zh 无法打开。) for the drawer's "Block this…" load failure only; magnet failures keep their string. `DetailActions.door.test.ts` checks the text.

Measured (bytes, gzipped; `npx size-limit --json`; local builds; two runs identical):

| Entry                                   | `main` | This branch | Limit before → after |
| --------------------------------------- | ------ | ----------- | -------------------- |
| Search results (lazy)                   | 43,204 | 40,010      | 43.3 → 40.61 KB      |
| Search results key commands (on demand) | n/a    | 3,216       | new, 3.4 KB          |
| Detail drawer (own JS + CSS)            | 27,279 | 27,328      | 27.4 KB unchanged    |
| /t/[hash] route (own JS + CSS)          | 20,923 | 20,969      | 21.0 KB unchanged    |
| Detail sections (on demand)             | 37,202 | 37,298      | 37.3 KB unchanged    |
| Send to client (on demand)              | 4,614  | 4,654       | unchanged            |

The drawer and /t grew by 49 and 46 B (the model's calls and the exported state), inside their unchanged limits; /t has 31 B left. Detail sections grew by 96 B (the string and the flag in `failed`), inside its limit. Everything else moved by at most 3 B (chunk naming). Alert, Alerts, Results toolbar, Discover, the search route and CSS did not grow. Baselines unchanged.
