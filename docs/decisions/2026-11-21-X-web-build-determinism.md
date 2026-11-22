# X-web-build-determinism decisions (#1038)

Runner brief, 2026-11-21Z: size-limit numbers moved by 5–20 B gzip on routes a PR didn't touch, so limits set at their measured size flaked (#1028, #1031, #1033).

## What was found
- **The build is byte-stable for the same source.** These all gave byte-identical JS, CSS and `build/` files and 115 equal size-limit entries (main d42b71c6):
  - 3 clean builds;
  - the CI flow 3 times (`npm run build` with a different `GITHUB_SHA`, then `LODESTAR_BUILD_VERSION=0000000 npx vite build`, then `npx size-limit`);
  - builds pinned to 1, 2 and 4 CPUs.
  - The generated sources (`npm run prebuild`: themes, assets, the paraglide messages) are identical too.
  - Only `.svelte-kit/output/chunk-graph.json` (the `chunkGraph` plugin's diagnostic) differs, in key order. It's neither shipped nor measured.
- **The noise is content-hash names spreading to chunks that didn't change.**
  - SvelteKit enforces `chunkFileNames: _app/immutable/chunks/[hash].js` and `[name].[hash].js` for entries. A chunk's name changes whenever its bytes do.
  - Every chunk, node and route table that imports it embeds that name. Gzip packs the new random string a few bytes differently.
  - A probe changed one word of one filters message (`filters_undo_title`). It moved **29 entries by −2 to +10 B**: Search route JS +1, Total first load +3, the route table, and 20 settings screens.
  - That matches the runner's spread across trees (Search results 40,821, 40,827, 40,832).

## The fix: the measurement build names chunks after their modules
- **A plugin in `web/vite.config.ts`, `lodestar:stable-names`, about 40 lines, no new dependency.** When `LODESTAR_STABLE_NAMES=1`, client chunk and entry names come from the chunk's sorted module ids, relative to `web/`, not from its bytes.
  - A name now changes only when the chunk's make-up does.
  - With it, the same probe moves **1 entry**: Settings FiltersScreen (first view), +3 B, the screen that shows the changed message.
- **The names look like rolldown's.** They are 8 base64url characters, and the first is B, C or D about 78% of the time, as in rolldown's own names. Each reference then costs gzip about what a real one does.
  - With uniformly random names, the settings first views measured up to +88 B over the content-hash build. With rolldown-like names, the offset is −18 to +32 B.
- **Measurement build only.** CI's size step sets the variable, and nothing else does. The shipped build (`npm run build` in e2e.yml and release.yml) keeps content hashes, which cache-busting needs: a file under `_app/immutable/` must change its name when its bytes change.
  - Server and worker builds keep their names.
  - If SvelteKit's entry pattern ever changes, the plugin throws rather than silently doing nothing.
- **Why not a smaller change:**
  - Content hashes can't be made to ignore dependencies: an importer's bytes contain its dependencies' names.
  - Hash-free names (`[name].js`) collide and would break caching in the shipped build.
  - Normalising names inside the size check would need `web/.size-limit.mjs`, which #1033 owns.

## No build-twice guard (runner, §2.26)
- **The runner dropped the guard the brief asked for.** Determinism was checked by hand instead, as listed above: 3 clean builds, the CI flow 3 times, and 1, 2 and 4 CPUs. All came out byte-identical.
- **Why:** a guard would add about 1.5 minutes to every push and protects against nothing observed. If nondeterminism ever appears, it will show as a size entry that moves with no change in its own chunks.
- **web.yml:** only the measurement build changes, gaining `LODESTAR_STABLE_NAMES=1`. The first build (`npm run build`, the commit's version) keeps the shipped config.

## Sizes: content-hash names → module-set names
**The limits are not exact shipped bytes.** size-limit measures a build whose chunk names differ from the shipped build's. On main d42b71c6, the measured size sits between **−18 and +32 B** of the same tree built with content-hash names (table below). Read a limit as a budget on that measurement, not as the byte count users download.

The table is main d42b71c6, both columns built with `LODESTAR_BUILD_VERSION=0000000`; the last column is the limit on main at 73bb8b68, #1038's merge.
- **The range depends on the tree.** #1038's review measured **−19 to +33 B** on d6c83c56 (the largest, Settings KeyboardScreen, 36,201 → 36,234 B), and X-size-ci-script measured −35 to +12 B on 73bb8b68. The offset is a property of the names, so it moves as the code does; X-size-ci-script (`docs/decisions/2026-11-22-X-size-ci-script.md`) gives stylesheets stable names too and runs the measurement as `npm run size:ci`.
- **Over its current limit:** one entry, Settings messages (on demand), 4,247 → 4,259 B against 4.25 kB. It goes to 4.26 kB in `web/.size-limit.mjs` (runner, §2.26), with a comment naming the offset. This slice took the file into its owns after #1033 merged.
- **Search cards (lazy)** goes back from #1033's stopgap 8.02 kB to 8.01 kB (runner): 7,998 B with stable names, 7,992 B with content hashes. No other entry changes.
- **Re-measured after merging main (d6c83c56)** with the CI steps (`npm run build`, then `LODESTAR_BUILD_VERSION=0000000 npx vite build` with and without `LODESTAR_STABLE_NAMES=1`, then `npx size-limit`). The numbers match the table: Settings messages 4,247 / 4,259 B and Search cards 7,992 / 7,998 B (content-hash / stable names).
- **Once stable,** limits can sit at their measured size. Only a change to an entry's own chunks moves it.

| Entry | Content-hash names | Module-set names | Change | Limit on main at 73bb8b68 |
|---|---|---|---|---|
| Search route JS (initial) | 44,684 | 44,685 | +1 | 45,000 |
| CSS | 19,780 | 19,780 | +0 | 19,800 |
| Route table (app.js) | 1,806 | 1,806 | +0 | 1,900 |
| Search vendor chunk | 30,980 | 30,979 | -1 | 31,100 |
| Total first load | 87,562 | 87,562 | +0 | 100,000 |
| Fonts on first paint | 19,452 | 19,452 | +0 | 35,000 |
| Mono font (lazy) | 16,003 | 16,003 | +0 | 25,000 |
| Compare route (own JS + CSS) | 30,895 | 30,886 | -9 | 31,000 |
| Discover route (own JS + CSS) | 41,713 | 41,705 | -8 | 43,000 |
| Logs route (own JS + CSS) | 36,224 | 36,233 | +9 | 39,600 |
| Alerts route (own JS + CSS) | 24,485 | 24,490 | +5 | 26,300 |
| Alert route (own JS + CSS) | 45,821 | 45,816 | -5 | 46,000 |
| Settings overview (own JS + CSS) | 14,205 | 14,209 | +4 | 45,000 |
| Settings section (own JS + CSS) | 45,541 | 45,553 | +12 | 45,580 |
| Settings controls (on demand) | 35,999 | 36,008 | +9 | 38,000 |
| Settings edit chunk (on demand) | 11,232 | 11,233 | +1 | 11,300 |
| Insights digest preview (on demand) | 1,708 | 1,706 | -2 | 1,900 |
| Insights print layout (on demand) | 1,563 | 1,563 | +0 | 1,620 |
| Privacy banner (on demand) | 3,375 | 3,376 | +1 | 3,710 |
| Settings messages (on demand) | 4,247 | 4,259 | +12 | 4,260 |
| Settings connection messages (on demand) | 4,129 | 4,134 | +5 | 4,300 |
| Settings connection screens (on demand) | 12,567 | 12,563 | -4 | 14,200 |
| Settings security parts (on demand) | 24,605 | 24,599 | -6 | 26,000 |
| Settings security keys and proxy (on demand) | 30,915 | 30,911 | -4 | 33,100 |
| Settings about parts (on demand) | 3,132 | 3,133 | +1 | 3,440 |
| Settings about Legal Notice (on demand) | 3,071 | 3,069 | -2 | 3,300 |
| Settings about icon and purpose (on demand) | 349 | 349 | +0 | 390 |
| Settings updates parts (on demand) | 2,954 | 2,955 | +1 | 3,200 |
| Settings updates advanced (on demand) | 4,440 | 4,441 | +1 | 4,650 |
| Settings updates icon and purpose (on demand) | 716 | 713 | -3 | 800 |
| Settings companions parts (on demand) | 28,801 | 28,804 | +3 | 29,000 |
| Settings companions icon and purpose (on demand) | 862 | 864 | +2 | 1,000 |
| Settings outputs parts (on demand) | 26,308 | 26,308 | +0 | 27,000 |
| Settings outputs icon and purpose (on demand) | 695 | 695 | +0 | 750 |
| Settings logging messages (on demand) | 1,028 | 1,030 | +2 | 1,050 |
| Settings logging output messages (on demand) | 696 | 698 | +2 | 770 |
| Settings logging parts (on demand) | 17,422 | 17,426 | +4 | 17,550 |
| Settings logging event names, system (on demand) | 6,463 | 6,464 | +1 | 7,110 |
| Settings logging event names, auth (on demand) | 1,325 | 1,328 | +3 | 1,630 |
| Settings logging event names, dht (on demand) | 814 | 815 | +1 | 1,120 |
| Settings logging event names, fetch (on demand) | 571 | 571 | +0 | 610 |
| Settings logging event names, filters (on demand) | 1,440 | 1,441 | +1 | 1,740 |
| Settings logging event names, search (on demand) | 1,945 | 1,946 | +1 | 2,250 |
| Settings logging event names, sources (on demand) | 704 | 706 | +2 | 768 |
| Settings logging event names, workers (on demand) | 5,785 | 5,787 | +2 | 6,370 |
| Settings logging event names, api (on demand) | 349 | 351 | +2 | 650 |
| Settings logging event names, companions (on demand) | 856 | 857 | +1 | 1,160 |
| Settings logging event names, integrations (on demand) | 1,013 | 1,014 | +1 | 1,060 |
| Settings logging event names, pipeline (on demand) | 2,807 | 2,809 | +2 | 3,010 |
| Settings logging files and preview (on demand) | 9,276 | 9,276 | +0 | 10,800 |
| Settings AppearanceScreen (first view) | 28,998 | 29,021 | +23 | 45,000 |
| Settings BlocklistsScreen (first view) | 29,785 | 29,801 | +16 | 30,400 |
| Settings FiltersScreen (first view) | 31,493 | 31,509 | +16 | 31,530 |
| Settings JobsScreen (first view) | 27,786 | 27,803 | +17 | 28,500 |
| Settings AboutScreen (first view) | 33,326 | 33,338 | +12 | 45,000 |
| Settings CompanionsScreen (first view) | 32,741 | 32,762 | +21 | 33,000 |
| Settings LoggingScreen (first view) | 44,787 | 44,807 | +20 | 45,000 |
| Settings OutputsScreen (first view) | 34,145 | 34,164 | +19 | 45,000 |
| Settings OverviewScreen (first view) | 29,554 | 29,570 | +16 | 45,000 |
| Settings InsightsScreen (first view) | 45,404 | 45,414 | +10 | 45,450 |
| Settings IntegrationsScreen (first view) | 44,771 | 44,783 | +12 | 45,000 |
| Settings LanguageScreen (first view) | 45,184 | 45,201 | +17 | 45,240 |
| Settings NotificationsScreen (first view) | 45,613 | 45,624 | +11 | 45,660 |
| Settings SecurityScreen (first view) | 35,256 | 35,282 | +26 | 45,000 |
| Settings UpdatesScreen (first view) | 40,977 | 41,002 | +25 | 45,000 |
| Settings KeyboardScreen (first view) | 36,202 | 36,234 | +32 | 45,000 |
| Settings SourcesScreen (first view) | 33,994 | 34,020 | +26 | 34,040 |
| Appearance theme picker (on demand) | 5,114 | 5,114 | +0 | 5,500 |
| Appearance 18+ dialog (on demand) | 20,107 | 20,111 | +4 | 20,600 |
| Appearance theme editor (on demand) | 11,859 | 11,867 | +8 | 13,000 |
| Filters dialogs (on demand) | 36,022 | 36,018 | -4 | 36,040 |
| Filters review queue (on demand) | 2,807 | 2,807 | +0 | 3,100 |
| Filters rule names (on demand) | 4,013 | 4,013 | +0 | 4,019 |
| Filters packs list (on demand) | 1,555 | 1,546 | -9 | 1,560 |
| Filters pack panels (on demand) | 7,910 | 7,893 | -17 | 7,920 |
| Filters pack impact (on demand) | 2,140 | 2,142 | +2 | 2,400 |
| Sources dialogs (on demand) | 41,542 | 41,538 | -4 | 41,800 |
| Sources config dialog (on demand) | 5,082 | 5,077 | -5 | 6,000 |
| Sources companion chip and setup (on demand) | 26,874 | 26,871 | -3 | 27,000 |
| Filters flows door (on demand) | 575 | 575 | +0 | 580 |
| Send to client (on demand) | 4,653 | 4,652 | -1 | 5,000 |
| Settings screen extras (on demand) | 5,485 | 5,488 | +3 | 25,000 |
| Settings screen purposes (on demand) | 868 | 869 | +1 | 960 |
| Restart banner (on demand) | 4,921 | 4,921 | +0 | 5,300 |
| Restart flow (on demand) | 26,616 | 26,601 | -15 | 29,400 |
| Settings screen icons (on demand) | 1,526 | 1,518 | -8 | 1,660 |
| Alert dialogs (on demand) | 25,249 | 25,246 | -3 | 27,000 |
| Alerts key commands (on demand) | 3,172 | 3,175 | +3 | 3,500 |
| Save search loader (on demand) | 498 | 500 | +2 | 550 |
| Search results (lazy) | 40,825 | 40,820 | -5 | 40,840 |
| Search results key commands (on demand) | 3,215 | 3,215 | +0 | 3,400 |
| Search recovery counts (on demand) | 742 | 744 | +2 | 840 |
| Search index-state banners (on demand) | 4,014 | 4,007 | -7 | 4,100 |
| Search Block settle (on demand) | 478 | 477 | -1 | 600 |
| Facet rail (lazy) | 18,816 | 18,819 | +3 | 18,850 |
| Results toolbar (lazy) | 4,285 | 4,282 | -3 | 4,600 |
| Search live banner (lazy) | 2,574 | 2,578 | +4 | 2,580 |
| Search cards (lazy) | 7,992 | 7,998 | +6 | 8,010 |
| Search table mode (on demand) | 5,921 | 5,916 | -5 | 6,500 |
| Search column chooser (on demand) | 3,261 | 3,261 | +0 | 3,400 |
| Search bulk bar (on demand) | 3,474 | 3,474 | +0 | 3,700 |
| Search row checkboxes (on demand) | 1,690 | 1,695 | +5 | 1,700 |
| Search assist (on demand) | 6,363 | 6,360 | -3 | 6,590 |
| Natural phrasing (on demand) | 1,714 | 1,711 | -3 | 1,750 |
| Builder (on demand) | 8,421 | 8,423 | +2 | 8,450 |
| Detail drawer (own JS + CSS) | 28,286 | 28,275 | -11 | 28,310 |
| /t/[hash] route (own JS + CSS) | 22,074 | 22,058 | -16 | 22,130 |
| Detail sections (on demand) | 38,792 | 38,810 | +18 | 38,820 |
| Detail adult-enrichment section (on demand) | 1,629 | 1,629 | +0 | 1,700 |
| Detail identified-as section (on demand) | 1,845 | 1,842 | -3 | 2,000 |
| Insights stage pages (own JS + CSS) | 40,965 | 40,947 | -18 | 40,980 |
| Insights stage charts (uPlot, on demand) | 24,066 | 24,066 | +0 | 24,200 |
| Welcome route (own JS + CSS) | 16,455 | 16,461 | +6 | 16,500 |
| Wizard steps (on demand) | 54,302 | 54,295 | -7 | 54,400 |
| Blocklist wizard step (on demand) | 7,720 | 7,717 | -3 | 8,300 |

## Left alone
- **`chunk-graph.json` key order** (`web/scripts/first-load.mjs`). It's diagnostic and unmeasured, and the runner chose to leave it.
