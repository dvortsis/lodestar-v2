# X-size-ci-script decisions (#1041)

Runner brief, 2026-11-22Z, from #1038's fresh review. It follows X-web-build-determinism (#1038). The shipped build doesn't change.

## `npm run size:ci`
- **One command for CI and contributors.** `web/scripts/size-ci.mjs` runs CI's size measurement in order:
  1. `vite build`, the shipped build;
  2. `vite build` again with `LODESTAR_BUILD_VERSION=0000000` and `LODESTAR_STABLE_NAMES=1`;
  3. `size-limit`.

  It stops with the first failing step's exit code. Arguments go to size-limit, for example `npm run size:ci -- --json`.
- **`web.yml` runs it** in place of its three steps, so CI and a local run can't drift.
- **A node wrapper, not inline `VAR=…`** (runner): it sets the variables itself, so it also runs on Windows.

## Stylesheets get stable names too
- **Why:** 50 JS files embed `assets/*.css` names (the app entry, route nodes and shared chunks). Under #1038 those names were still content hashes, so a style change in one component moved the sizes of the files that name its stylesheet.
- **The rule** (`web/vite.config.ts`, under `LODESTAR_STABLE_NAMES` only):
  - a `.css` asset is named `[name].<8 characters>.css`, keyed on its sorted source files;
  - rolldown gives no source file for 12 of the 44 stylesheets, so those are keyed on the asset's name, which is unique in this build;
  - fonts and images keep content hashes, because code changes don't touch them;
  - the plugin throws if SvelteKit's asset pattern changes, as it already does for its entry pattern.
- **Probe:** a one-character change to `EmptyState.svelte`'s style, measured on 73bb8b68:
  - with JS-only stable names (as #1038 merged it), 3 entries moved by +1 B each: Search route JS (initial), Route table (app.js) and Total first load;
  - with this rule, none moved.
- **Byte-stable:** `npm run size:ci` ran twice gives identical client output and 115 equal entries.

## The offset now
The measured size against the same tree with content-hash names, on main 73bb8b68:
- **All 115 entries sit between −35 and +12 B.** That's the same range as JS-only stable names, and 36 entries differ between the two by a few bytes.
- **The five largest:**

  | Entry | Content-hash names | Stable names | Change |
  |---|---|---|---|
  | Wizard steps (on demand) | 54,330 | 54,295 | −35 |
  | Insights stage pages (own JS + CSS) | 40,975 | 40,947 | −28 |
  | Alert route (own JS + CSS) | 45,842 | 45,816 | −26 |
  | Settings security keys and proxy (on demand) | 30,938 | 30,912 | −26 |
  | Detail drawer (own JS + CSS) | 28,296 | 28,274 | −22 |

- **Every entry passes,** so `web/.size-limit.mjs` doesn't change.
- **#1038's file fixes:** it now names the tree each range was measured on, and its limit column is set to main at 73bb8b68 (Settings messages 4,260 B, and three rows that other PRs had raised).
