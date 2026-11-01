# X-detail-headroom decisions

The Detail drawer, `/t/[hash]` and Detail sections sit within bytes of their limits (29 KB, 22.6 KB, 34.5 KB). This slice looked for real bytes. It found little, and the 0.5 KB headroom target is **not met**. No limit was changed.

- **`detail.css` is split so each entry carries only what it uses.** `/t/[hash]` carried the drawer host's placement rules, the drawer carried the page layout, and both carried the raw viewer's styles, which only the dev route's `RawViewer` uses. They now live in `host.css` (imported by `DetailHostBody`), `page.css` (imported by the `/t` page) and `raw.css` (imported by `RawViewer`). Nothing in production renders differently: the same selectors apply wherever they were used.
- **Measured contributors, not needed on first view, that can't move without growing Detail sections.** Detail sections has 42 B of headroom, so anything moved from the first view to an on-demand import lands there: the event client and SSE (about 2.3 KB, from `visibility.ts`), the nine junk-reason messages and `summary/junk.ts`, and the status icons that `Badge` loads for the flag badges (`ui/status.ts`, outside this slice's paths). Moving them would need equal cuts in the sections (the key registry, the virtual list and the tree are all in use).
- **Tried, no effect:** importing the formatters from their own modules instead of the `$lib/format` barrel (identical bytes, reverted).
- **Not done:** the magnet toast on a failed chunk load (brief Do 4), because Detail sections didn't gain 0.2 KB.
- **Measured** (bytes, gzipped; `npx size-limit --json`; local builds):

  | Entry | `main` | This branch | Limit |
  |---|---|---|---|
  | Detail drawer | 28,899 | 28,865 | 29 KB (unchanged) |
  | /t/[hash] route | 22,539 | 22,494 | 22.6 KB (unchanged) |
  | Detail sections (on demand) | 34,458 | 34,456 | 34.5 KB (unchanged) |

- **For the runner:** reaching the targets needs either a cut in shared UI (`ui/status.ts` loading four icons for the flag badge; `Badge` is outside this slice) or a decision to move the event client to on demand and pay for it in Detail sections' limit.
