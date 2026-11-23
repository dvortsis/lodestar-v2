# X-size-own-modules decisions

Runner records r14 (`docs/decisions/2026-11-23-runner-records-r14.md`, "Size measurement"), API lane item 30. `web/.size-limit.mjs` was handed over by the runner's `after` note: X-detail-depth-compare (#1094) merged, and no open PR touched the file.

## What changed

- **The route entries are measured by module set** (`routeEntry` in `web/.size-limit.mjs`, `web/scripts/size-modules.mjs`). These are the eleven "… (own JS + CSS)" entries built from `own()`: Compare, Discover, Logs, Alerts, Alert, Settings overview, Settings section, `/t/[hash]`, Insights stage pages, Insights overview and Welcome. For each, the files `own()` reaches become one JS stream and one CSS stream, and size-limit gzips those two files. So the figure is still "gzipped JS + CSS", but:
  - **No partition:** the stream holds the chunks of the route's module set, ordered by the source modules each holds (from `.svelte-kit/output/chunk-graph.json`), not by file name, and joined with nothing between them.
  - **No links:** the `import … from` and `export { … }` statements between chunks are dropped (`parseAst`, not a regex). The bindings they name are Rolldown's partition, not the route's code. Dynamic `import()` calls, declarations and default exports stay.
  - **No hashes:** every output file's 8-character name left in the code (`import("./<hash>.js")`, preload lists, stylesheet links) becomes a fixed placeholder. Only the names of real output files are replaced, so an 8-letter word before `.js` in a string the app shows stays.
- **`own()` still returns the files.** The `beyond` lists and the checks that read them (the carried-code and flows guards) are unchanged. `ownChunk`, `onDemandOf` and the plain file entries keep the per-file measure.
- **The measurement build guard stays** (version `0000000`), and the new measure also refuses to run without the chunk graph. The streams are written to `.svelte-kit/output/size-modules/`, which is emptied each time the config loads. Two runs give byte-identical results.

## Why a single stream

- **Checked against the case r14 names.** I built `main` before and after X-detail-depth (#1081), the "Compare +0.25 kB, the same 24 modules" case, and measured Compare three ways:
  - per-file gzip (the old measure): 30,895 → 31,135 B (+240);
  - per-file gzip with links dropped and names normalised: 27,250 → 27,429 B (+179);
  - the module-set stream: 22,273 → 22,226 B (−47), the chunks around the split sorting differently.
    On the same pair, Welcome went −1 → 0 B and Results +3 → +1 B. Each file's own gzip header and dictionary is most of the partition cost, so normalising the files one by one isn't enough.
- **What stays partition-dependent:** the minifier's names inside a chunk and the statement order within it, and where a regrouped chunk sorts among the others: tens of bytes, either way.
- **Tests** (`web/scripts/size-modules.test.ts`):
  - moving a shared module from one chunk to the next, which changes the old per-file figure, leaves the new figure unchanged;
  - renaming a lazy chunk leaves both its importer's figure and its own unchanged;
  - the order the files are listed in doesn't matter;
  - added code still grows the figure;
  - plus the link stripping, the name normalising and the measurer's errors.

## Limits (runner, §2.26; the owner's size-limit rules)

Every route entry's figure fell by 15–30% with no code change. No limit is raised.

- **Lowered to the new measure rounded up to 0.01 KB (the same rule), where the old limit sat at its measure (within 0.01 KB):**

  | Entry                     | Old measure | New measure | Old limit | New limit |
  | ------------------------- | ----------- | ----------- | --------- | --------- |
  | Compare route             | 31,790 B    | 22,878 B    | 31.79 KB  | 22.88 KB  |
  | Alert route (protected)   | 45,996 B    | 35,024 B    | 46 KB     | 35.03 KB  |
  | Settings section          | 45,692 B    | 32,434 B    | 45.7 KB   | 32.44 KB  |
  | /t/[hash] route           | 22,033 B    | 15,477 B    | 22.04 KB  | 15.48 KB  |
  | Insights stage pages      | 41,111 B    | 30,849 B    | 41.12 KB  | 30.85 KB  |
  | Insights overview         | 65,027 B    | 48,298 B    | 65.03 KB  | 48.3 KB   |
  | Welcome route (protected) | 16,497 B    | 12,078 B    | 16.5 KB   | 12.08 KB  |

- **Kept, because their limits had deliberate headroom and the rules allow a lower limit only at the measure:**

  | Entry             | Old measure | New measure | Limit   |
  | ----------------- | ----------- | ----------- | ------- |
  | Discover route    | 42,121 B    | 34,625 B    | 43 KB   |
  | Logs route        | 36,916 B    | 31,345 B    | 39.6 KB |
  | Alerts route      | 24,508 B    | 17,738 B    | 26.3 KB |
  | Settings overview | 14,343 B    | 10,133 B    | 45 KB   |

  Their headroom grew by the drop. **Open for the runner:** keep the same headroom in bytes instead. That gives Discover 35.51 KB, Logs 34.03 KB, Alerts 19.53 KB and Settings overview 40.79 KB (or keep its 45 KB §10.1 cap).

- **All numbers are from a local `npm run size:ci`.** The PR's CI run is the check that CI measures the same.

## Flagged (§2.19)

- **The §10.1 per-view caps** (for example 45 KB) were set in the per-file measure. A route's figure is now lower than its transfer size by the partition overhead, so the same cap allows more code. The re-set limits above keep each route as tight as it was. Whether the §10.1 numbers themselves should be restated in the new measure is the runner's and the owner's call.
- **Lowering the protected Alert and Welcome limits** follows the owner's rule that a limit may fall to the measured size rounded up. Neither was raised.

## Not done

- **The other entries** (`ownChunk`, `viewOf`, `onDemandOf`: Search results, Detail drawer, the on-demand entries) keep the per-file measure. Results' +3 B rename noise (X-hit-resolution) is one of them. `routeEntry` would apply to them unchanged, but it would move about a hundred figures, including the protected Results, Search cards and Results toolbar limits. That is a follow-up for the runner to order, not part of this slice's owns.
