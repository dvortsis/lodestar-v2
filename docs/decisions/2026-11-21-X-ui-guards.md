# X-ui-guards decisions

Track U's first slice (runner brief, 2026-11-21Z). Architecture §10.2 (icons, breakpoints) and §10.17 (accessibility gates). No product behaviour changes: icon weight is the only visual change.

## Weighted icons
- **All 124 unweighted Phosphor imports (62 files) now import the Bold glyph** (`~icons/ph/<name>-bold`). §10 is Phosphor Bold, with Fill only for selected or on states (favourite, active filter).
  - **One of the 124 marks an on state:** the Search facets' exclude toggle (`search/facets/TermGroup.svelte`, `aria-pressed={v.excluded}`), an active-filter state. It's left Bold here, because swapping glyphs on press is a behaviour change. The runner has queued X-facet-exclude-fill to use `-fill` while it's pressed.
  - The other candidates checked are plain glyphs: a checkbox's check, the wizard's "later" dot, a half circle used as a warning, and the pause states.
  - Every resulting name exists in `@iconify-json/ph`.
- **The ban (`UNWEIGHTED_ICON` in `scripts/bans.mjs`)** fails on a `~icons/ph/` import whose name doesn't end in `-bold` or `-fill`, in both `check-bans.mjs` (`npm run check`) and the eslint rule.
  - Thin, Light and Duotone count as unweighted, since the design uses neither.
  - A query (`-bold?raw`) is allowed.
  - **The ban matches both forms:** a source line, with the import's quotes, and a bare import string, which is what eslint passes the rule. An eslint `RuleTester` case in `bans.test.ts` runs the rule itself. The first version required a quote after the name, so its eslint half never fired.
  - **The rule lives in `scripts/bans.mjs`,** beside its patterns, so the test can run it; `eslint.config.js` imports it from there.

## Dead breakpoint classes
- **The theme resets `--breakpoint-*` to the §7 ranges** (`medium:` 480 px, `large:` 640, `x-large:` 1024, `xx-large:` 1366, `xxx-large:` 1920), so Tailwind's default `sm:`, `md:`, `lg:`, `xl:` and `2xl:` compile to nothing.
- **The four on main are removed**, which changes nothing visually:
  - `sm:grid-cols-2` in the companions install dialog, in the companions templates and in the logging levels;
  - `xl:p-6` in the Settings shell.
  - The three `sm:grid-cols-2` look like they meant two columns at 640 px and wider (`large:grid-cols-2`). Restoring them is a visual change, which the runner has queued as X-grid-two-col-large.
- **The ban (`DEAD_BREAKPOINT`)** fails on one of those prefixes, or its `max-`/`min-` form, used as a class: at the start of a class, or after another variant, and glued to a utility (`sm:grid-cols-2`, `hover:md:underline`, `md:!p-2`, `max-md:hidden`).
  - Code that only looks like one isn't flagged: an object key (`{ md: 1 }`), a parameter (`md: string`), a ternary (`wide ? lg : sm`), a label (`'Size: md'`) or a comment.
- **Proof:** over main's `src`, the two bans flag exactly the 124 icons and the 4 classes; on this branch they flag none.
- **Existing test strings:** five `bans.test.ts` strings used `md:` as an ordinary variant. They now use the theme's `medium:`, with the same expected results.
- **Left alone (runner's call):** the breakpoint ban also flags prose that happens to read like a class (`sm:ss`), and the icon ban doesn't cover `virtual:icons/ph/…` or uppercase names. Neither appears in the code.

## Axe on fixture data
- **`e2e/axe.fixture.test.ts`** runs axe (WCAG 2.2 AA tags, as `e2e/a11y.ts` does) against a real `lodestar serve --fixture` server, over two surfaces:
  - **/insights** at 7 days, once its KPIs, funnel and annotations render, and again with the trend's table open;
  - **every first-run wizard step**, from Network check to Done.
- **It fails on serious or critical violations**, as the brief asks. Moderate and minor ones are attached to the report as annotations.
- **The wizard is walked with Skip, and Continue on "How it works".** Neither sends anything, because the wizard keeps its progress in `sessionStorage`. So the spec reads only, and it's in the `fixture` project, not `fixture-writes`.
- **Axe scans only the loaded screen.** Before each scan, the spec waits until:
  - no `aria-busy` element is left inside the wizard or `main`;
  - the step's own control is showing: a field, the update radios, the pack or blocklist list, or the network check's result;
  - no error banner is showing.

  So a slow runner can't scan "Loading…" or a load error and pass.
- **Waits are conditions**, each with a deadline of at least 10 s (15 s), and there are no fixed sleeps.
- **In CI** it runs in `e2e.yml`'s `fixture` job, nightly and on dispatch, because that job's project matches `*.fixture.test.ts`. There's no workflow change (runner's call): moving fixture specs onto pull requests is a separate decision, outside this slice.
- **Locally,** against `serve --fixture`: 2 of 2 pass.

## Sizes
Glyph weights change the icon modules, and Rolldown regroups the shared ones.
- **Most entries move by a few bytes.**
- **The largest drops:** Discover −612 B, Search live banner −362 B, Search cards −338 B, Alert route −221 B.
- **The largest rises:** Detail sections +263 B, Compare +225 B, Sources dialogs +174 B, Settings SourcesScreen +162 B, Search results (lazy) +153 B.
- **Search's three limits (runner's call, §2.26).** The shared search glyphs moved into the results chunk from the cards and the live banner, so search as a whole shrinks by 547 B.
  - The results limit rises to the measured size, rounded up at 0.01 kB.
  - The other two fall to their measured sizes, rounded up at 0.01 kB, so the freed bytes can't be quietly spent.
  - No other limit moves, the tight ones included.

| Entry | Before | After | Limit before | Limit after |
|---|---|---|---|---|
| Search results (lazy) | 40,600 B | 40,753 B | 40.62 kB | 40.76 kB |
| Search cards (lazy) | 8,303 B | 7,965 B | 8,364 B | 7.97 kB |
| Search live banner (lazy) | 2,938 B | 2,576 B | 3.02 kB | 2.58 kB |

## Snapshots
CI regenerated the baselines through the `web/e2e/.update-snapshots` marker (e2e/README.md "Updating snapshots"). Only one moved past the 0.001 tolerance: `smoke.test.ts-snapshots/settings-bp2560-riverstone-dark-comfortable-linux.png`. Its diff is the Bold glyphs plus one finding:
- **The old baseline predated Outputs in the Settings nav.** The new shot has the Outputs item under Connections, which `shell/screens.ts` always lists (an eager glob of every `screen.ts`, since OUT-2), so every row below it moves down by one.
- **The regenerated baseline is kept,** because it matches current main (runner's call).
- **Other baselines may be stale the same way.** The light 2560 baseline, which wasn't rewritten, also lacks Outputs. Why it still passes, whether it isn't compared in pull-request runs or the tolerance hides a nav row, is the runner's follow-up slice X-snapshot-stale-outputs. This slice touches no other baseline.
