# X-size-ci-script decisions (#1041)

Runner brief, 2026-11-22Z, from #1038's fresh review. It follows X-web-build-determinism (#1038). The shipped build doesn't change.

## `npm run size:ci`
- **One command for CI and contributors.** `web/scripts/size-ci.mjs` runs CI's size measurement in order:
  1. `npm run build`, the shipped build with its `prebuild`: themes and their contrast gates (§10.18), brand assets (§10.19) and messages. Size is measured on freshly generated files, and a theme edit without `npm run themes` still fails here.
  2. `vite build` again with `LODESTAR_BUILD_VERSION=0000000` and `LODESTAR_STABLE_NAMES=1`.
  3. `size-limit`.

  It stops with the first failing step's exit code.
- **`web.yml` runs it** in place of its three steps, so CI and a local run can't drift.
- **A node wrapper, not inline `VAR=…`** (runner):
  - each step runs on the same Node with its script file (npm's own, vite's and size-limit's `bin`), never through a shell, so it behaves the same on Windows;
  - the build steps' output goes to stderr and size-limit's to stdout, so `npm run size:ci -- --json` gives clean JSON.

## Stylesheets get stable names too
- **Why:** 50 JS files embed `assets/*.css` names (the app entry, route nodes and shared chunks). Under #1038 those names were still content hashes, so a style change in one component moved the sizes of the files that name its stylesheet.
- **The rule** (`web/vite.config.ts`, under `LODESTAR_STABLE_NAMES` only):
  - a `.css` asset is named `[name].<8 characters>.css`;
  - it's keyed on the module rolldown names as its origin (`originalFileNames`, the emitting chunk's facade module, not the CSS sources);
  - rolldown gives none for 12 of the 44 stylesheets, so those are keyed on the asset's name, which is unique in this build. Two different stylesheets on one name key throw, so a later collision is loud;
  - fonts and images keep content hashes, because code changes don't touch them;
  - the plugin throws if SvelteKit's asset pattern changes, as it already does for its entry pattern.
- **Probe:** a one-character change to `EmptyState.svelte`'s style, measured on 73bb8b68:
  - with JS-only stable names (as #1038 merged it), 3 entries moved by +1 B each: Search route JS (initial), Route table (app.js) and Total first load;
  - with this rule, none moved.
- **Byte-stable:** `npm run size:ci` run twice gives identical client output and 115 equal entries.

## The offset now
The measured size against the same tree built with content-hash names, both with `LODESTAR_BUILD_VERSION=0000000`, on main 73bb8b68:
- **All 115 entries sit between −19 and +35 B.** JS-only stable names gave −19 to +33 B, so stylesheet names add at most 3 B to an entry.
- **The five largest, plus the largest negative:**

  | Entry | Content-hash names | Stable names | Change |
  |---|---|---|---|
  | Settings KeyboardScreen (first view) | 36,201 | 36,236 | +35 |
  | Settings SecurityScreen (first view) | 35,255 | 35,286 | +31 |
  | Settings SourcesScreen (first view) | 33,992 | 34,022 | +30 |
  | Settings OutputsScreen (first view) | 34,142 | 34,168 | +26 |
  | Settings UpdatesScreen (first view) | 40,979 | 41,005 | +26 |
  | Filters pack panels (on demand) | 7,912 | 7,893 | −19 |

- **Every entry passes,** so `web/.size-limit.mjs` doesn't change.
- **A correction:** this PR's first push gave −35 to +12 B. Its content-hash column had been built without `LODESTAR_BUILD_VERSION=0000000`, so it differed in its version name too (#1041 review).
- **#1038's file fixes:** it now names the tree each range was measured on, and its limit column is set to main at 73bb8b68. That changes Settings messages to 4,260 B and Filters rule names to 4,020 B, plus three rows that other PRs had raised.
