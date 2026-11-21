# X-page-frame decisions (#1028)

Slice session, 2026-11-21Z. It answers UI review I11 and P15 (`docs/review/2026-11-13-ui-ux/`, placed by `docs/decisions/2026-11-14-runner-ui-review-placement.md`): every destination framed its page differently, page edges missed each other by a pixel, and Companions and Outputs had no `h1`. The runner approved the wider file list (the `h1` and outer-wrapper lines of the feature components, and one line of `QueryBar.svelte`) in this session.

## The frame

- **One set of class strings, `web/src/lib/ui/layout/classes.ts`, used by every destination's wrapper and `h1`.** `PAGE_FRAME` (the gutters), `PAGE_BODY_FULL`, `PAGE_BODY_WIDE` and `PAGE_BODY_MANUSCRIPT` (the content box and its cap), `PAGE_HEADER` (the heading row, where a page puts actions beside its `h1`) and `PAGE_H1`. The old per-page classes (`.al-page`, `.dsc-page`, `.cmp-page`, `.ls-detail-page`'s box, `.sc-h1`, `.as-h1`, the `h1` rules of Compare, Discover and Alerts) are removed where nothing else used them.
- **Runner call (§2.26): class constants, not components.** A first version had `PageFrame.svelte` and `PageHeader.svelte`. Built, they were 329 B and 399 B gzipped plus their classes, about 0.75 to 0.9 kB in every route that used them, and with them Search's first view measured 45.31 kB against the §10.1 cap of 45 kB. The consistency comes from the classes, and the e2e spec below enforces the frame, so the components were deleted. Search is 44.70 kB.
- **Gutters:** `px-4` (16 px) below `large` (640 px), `large:px-6` (24 px) from it; `py-4` / `large:py-6` likewise. Tailwind's default steps, all on the §10.2 ramp. Print has none (`print:p-0`), as the Insights print rule had.
- **Widths:** `full` has no cap (Search with its rail and drawer, Insights, Logs, the Settings shell); `wide` is `max-w-7xl` (80 rem, 1,280 px; Alerts, Compare, Detail; Discover is `full`, because its side panels need the width and it had no cap); `manuscript` is `--manuscript-width` (720 px; the unavailable page). Settings sections keep their `--manuscript-width` column inside the shell. No new token: `docs/design-tokens.md` is unchanged.
- **The `h1`:** one style everywhere, `text-display font-semibold` (28 / 32), `wrap-anywhere`, the shared focus ring and `tabindex="-1"` (a script focus target, never a tab stop), for the pages that move focus to it after a change.
- **Left-aligned, never centred.** `mx-auto` was why the edges differed at 1440 px (Alerts at 84, Detail at 176). The cap now limits only the right edge, so every page starts at the same gutter at every width.

## UX calls (runner calls under `docs/owner-decisions.md` §2.26)

1. **Left-aligned pages at wide widths** (above). Alerts, Detail and Insights lose their centred column (Insights was capped at 96 rem and still is); the content stays left, as the Search page already was.
2. **One heading size, 28 px.** Settings screens, Detail and Insights stage pages were 20 px (`text-title`), the others 28. Settings screens now match the rest (I11). Files beyond the brief's list, all approved by the runner or recorded in `runner/slices.yaml` `owns`: the feature components' `h1` and wrapper lines, `QueryBar.svelte` (one class line), `app.html` (the static Search heading and query bar), `detail.routes.test.ts`, `.size-limit.mjs`.
3. **Insights shows its `h1`.** The overview's was `sr-only`; I11 names "not visible on Insights" as the defect, so it is visible. The About screen's `h1` stays `sr-only`: its logo and name header is its visible heading, and no review finding asks otherwise.
4. **Companions and Outputs gain their `h1`**, reusing the nav titles (`settings_companions_title`, `settings_outputs_title`). No new message, so no en or zh file changes.
5. **Settings at 1024 px and up keeps the section list left of the content,** so its `h1` sits at 312 px (24 px gutter + the 16 rem list + the 32 px gap), the same on every Settings screen, while every other destination is at 24 px. At 390 px all are at 16 px. A literal one-edge rule at 1024 px would put the `h1` above the section list, and each Settings screen renders its own `h1`, so it needs a Settings layout slice; it is not done here. (The old edge was 304 px.)
6. **Detail keeps its header cap.** The page header stays at `--manuscript-width` and top-aligned, as before; the heading row for the other pages is centred.
7. **Logs' gutter is now the frame's** (24 px from `large`; it was 16 px at every width). **Insights' `h1` is hidden in print** (`print:hidden`): the report prints its own title. **Alert detail's `h1` uses `PAGE_H1`** like every other page.
8. **Spacing differences removed:** Discover's larger gap at `large` (24 px between regions) is now the frame's 16 px; Detail widens from 1,120 to 1,280 px (its summary keeps `--manuscript-width`); Search's heading row has the frame's 16 px gap to the query bar (it was 8 px).

## Measurements (CSS px; the `h1` is the text's left edge; "content" is the page's inner box)

Measured with `e2e/page-frame.routes.test.ts` (`PAGE_FRAME_TABLE=1`) on `main` and on this branch, against the OpenAPI mock. "Before" content edges are the page root's content box, which is the heading's wrapper; Search's wrapper had no padding of its own (its heading, query bar and grid each had `px-4`), so its 16 / width-16 below is read from the source. "none" is a page with no `h1`; "sr" is the clipped `sr-only` one.

| Destination | Width | Before: h1 | content L | content R | After: h1 | content L | content R |
|---|---|---|---|---|---|---|---|
| Search | 390 | 16 | 16 | 374 | 16 | 16 | 374 |
| Logs | 390 | 16 | 16 | 374 | 16 | 16 | 374 |
| Settings, Keyboard | 390 | 32 | 16 | 374 | 16 | 16 | 374 |
| Discover | 390 | 16 | 16 | 374 | 16 | 16 | 374 |
| Alerts | 390 | 16 | 16 | 374 | 16 | 16 | 374 |
| Alert detail | 390 | 16 | 16 | 374 | 16 | 16 | 374 |
| Insights | 390 | sr | 16 | 374 | 16 | 16 | 374 |
| Insights stage | 390 | 16 | 16 | 374 | 16 | 16 | 374 |
| Compare | 390 | 16 | 16 | 374 | 16 | 16 | 374 |
| Detail (`/t/…`) | 390 | 16 | 16 | 374 | 16 | 16 | 374 |
| Settings (the other screens) | 390 | 16 | 16 | 374 | 16 | 16 | 374 |
| Settings, Companions and Outputs | 390 | none | 16 | 374 | 16 | 16 | 374 |
| Logs | 1024 | 16 | 16 | 1008 | 24 | 24 | 1000 |
| Settings, Keyboard | 1024 | 320 | 16 | 1008 | 312 | 24 | 1000 |
| Search | 1024 | 16 | 16 | 1008 | 24 | 24 | 1000 |
| Discover | 1024 | 24 | 24 | 1000 | 24 | 24 | 1000 |
| Alerts | 1024 | 24 | 24 | 1000 | 24 | 24 | 1000 |
| Alert detail | 1024 | 24 | 24 | 1000 | 24 | 24 | 1000 |
| Insights | 1024 | sr | 24 | 1000 | 24 | 24 | 1000 |
| Insights stage | 1024 | 24 | 24 | 1000 | 24 | 24 | 1000 |
| Compare | 1024 | 16 | 16 | 1008 | 24 | 24 | 1000 |
| Detail | 1024 | 16 | 16 | 1008 | 24 | 24 | 1000 |
| Settings (the other screens) | 1024 | 304 | 16 | 1008 | 312 | 24 | 1000 |
| Settings, Companions and Outputs | 1024 | none | 16 | 1008 | 312 | 24 | 1000 |
| Logs | 1440 | 16 | 16 | 1424 | 24 | 24 | 1416 |
| Settings, Keyboard | 1440 | 320 | 16 | 1424 | 312 | 24 | 1416 |
| Search | 1440 | 16 | 16 | 1424 | 24 | 24 | 1416 |
| Discover | 1440 | 24 | 24 | 1416 | 24 | 24 | 1416 |
| Alerts | 1440 | 84 | 84 | 1356 | 24 | 24 | 1304 |
| Alert detail | 1440 | 84 | 84 | 1356 | 24 | 24 | 1304 |
| Insights | 1440 | sr | 24 | 1416 | 24 | 24 | 1416 |
| Insights stage | 1440 | 24 | 24 | 1416 | 24 | 24 | 1416 |
| Compare | 1440 | 16 | 16 | 1424 | 24 | 24 | 1304 |
| Detail | 1440 | 176 | 176 | 1264 | 24 | 24 | 1304 |
| Settings (the other screens) | 1440 | 304 | 16 | 1424 | 312 | 24 | 1416 |
| Settings, Companions and Outputs | 1440 | none | 16 | 1424 | 312 | 24 | 1416 |

After the change every destination's `h1` is at 16 px at 390 px and at 24 px at 1024 and 1440 px, except the Settings screens at 1024 px and up (call 5), which share 312 px. The old spread was 16, 24, 84, 176 and 304 px, with 15 / 23 px for Insights's clipped heading and no heading on Companions and Outputs. No two edges differ by 1 px.

## Tests

- `web/src/lib/ui/layout/layout.test.ts`: the class constants carry the gutters, the caps and the `h1` style, never centre, and use no hex or arbitrary value.
- `web/e2e/page-frame.routes.test.ts` (the `core` project): every destination (including Logs, Keyboard, Companions and Outputs) at 390, 1024 and 1440 px has exactly one `h1` inside the page frame, at 16 px (24 px from `large`), and every Settings screen shares one edge. It waits on conditions with the 10 s deadline and has no fixed sleeps.
- Existing specs updated: `detail.routes.test.ts` (the page is the shared frame, not a 1,120 px centred box) and the static boot shell in `app.html` (the Search heading and query bar now use the frame's classes, so `boot-shell.routes.test.ts` still finds no shift).
- Snapshots: none regenerated here, because a local PNG is not valid (`web/e2e/README.md`); CI generates them. Changed: `search-results-adaptive.routes.test.ts` (`search-rows-768`, `search-rows-1024`, `search-drawer-1440`) and the matrix smoke baselines for the key routes Search, Settings and Detail at every breakpoint and theme.

## Not done, and follow-ups

- **Filters (`features/filters/screens/FiltersScreen.svelte`) still has its own `text-title` `h1`.** It is inside the Settings content column, so its edge matches the other Settings screens, but it is 20 px, not 28. It is X-safety-undo-danger's; a one-line change to `PAGE_H1` after that slice lands. The e2e spec does not visit it for the same reason.
- The Settings layout above (call 5), if the owner wants one edge at 1024 px and up.

## Size

Search's first-load JS is 44.70 kB (main 44.39 kB), under the 45 kB cap. The Settings first views and Insights stage pages grow by 30 to 150 B: the `h1` is now the 28 px display style with the focus ring and `wrap-anywhere`, where it was `text-title font-semibold`, and one string serves every page. Raises (runner approved, §2.26), each the measured size rounded up to 0.01 kB:

| Entry | Limit | Measured | New limit | Why |
|---|---|---|---|---|
| CSS | 19.73 kB | 19.783 kB | 19.79 kB | the frame's `large:px-6`, `large:py-6`, `print:p-0`, `max-w-7xl` |
| Settings section | 45.5 kB | 45.578 kB | 45.58 kB | the shared `h1` string |
| Settings FiltersScreen | 31.42 kB | 31.525 kB | 31.53 kB | the shared `h1` string |
| Settings InsightsScreen | 45.3 kB | 45.445 kB | 45.45 kB | the shared `h1` string |
| Settings LanguageScreen | 45.1 kB | 45.231 kB | 45.24 kB | the shared `h1` string |
| Settings NotificationsScreen | 45.51 kB | 45.655 kB | 45.66 kB | the shared `h1` string |
| Settings SourcesScreen | 34 kB | 34.031 kB | 34.04 kB | the shared `h1` string |
| Insights stage pages | 40.85 kB | 40.975 kB | 40.98 kB | the shared `h1` string |
| Builder (on demand) | 8.43 kB | 8.432 kB | 8.44 kB | chunk placement |
| Search results (lazy) | 40.83 kB | 40.832 kB (CI) | 40.84 kB | not this PR: `main` measures 40,832 B in CI's build and this PR adds 0 B to the chunk; the old limit left `main` 2 B inside its edge (§2.18 port) |

The Settings screens' `h1` moves from `text-title` (20 px) to the display size (28 px): a runner call under §2.26 (call 2 above).
