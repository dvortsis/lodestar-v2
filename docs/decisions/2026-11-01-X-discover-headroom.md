# X-discover-headroom decisions

The Discover route sat at 45,076 B against a 45.2 KB limit (after #317), leaving no room for the next feature. Runner brief, 2026-11-01 (§2.26: performance change to an owner-set number).

- **Discover's j/k keys register on demand.** The key registry (`$lib/keys`, about 2.8 KB gzip) was in the route's first view only for two shortcuts. `DiscoverPage.svelte` now loads `$lib/keys` and the new `discover/commands.ts` (the two registrations, unchanged) after mount, the same pattern as the search results and Detail commands. The import is issued during mount and nothing waits on it, so first paint and the request count are unchanged. The shortcuts exist one chunk round trip after mount; if the load fails, j/k don't register (Tab and click still work). The app shell already loads the registry after paint, so in practice the chunk is cached.
- **Measuring on-demand shared chunks.** `own()`/`walk()` in `.size-limit.mjs` take an opt-in `byChunk` flag that also tests `chunk:<name>` against `onDemand`, as `ownChunk()` does, because the registry chunk has no source key. Only the Discover entry sets it, so no other entry's measurement changes (a first version applied it to all entries and loosened `/t/[hash]`'s gate; review caught it).
- **Scope order.** `pushScope('discover')` now runs a chunk round trip after mount, as in `ResultsPanel`. A dialog scope pushed in that window would sit below `discover` until it unmounts; the window is small and shared with search. A failed load is ignored, so j/k just don't register.
- **Not done:** `ResultRow` (3.8 KB with its messages and icons) and `VirtualList` (11.7 KB with TanStack virtual) are needed on first view. No duplicate helpers or dead code found in `features/discover`.
- **Limit** (never raised): 45.2 → 43 KB. Measured + 10% of the cut would be 42.66 KB (269 B headroom), under the 0.6 KB minimum, so the limit is measured + 0.6 KB, rounded up: 608 B headroom.
- **Measured** (bytes, gzipped; `npx size-limit --json`; local builds; two runs identical):

  | Entry                | `main` | This branch | Limit before → after |
  | -------------------- | ------ | ----------- | -------------------- |
  | Discover route       | 45,076 | 42,392      | 45.2 → 43 KB         |
  | Search route initial | 43,096 | 43,093      | unchanged            |

  Every other entry moves by at most 3 B (chunk naming; `/t/[hash]` is unchanged), none by more. Alert, Alerts, Search results, Results toolbar and CSS did not grow. Baselines unchanged.
