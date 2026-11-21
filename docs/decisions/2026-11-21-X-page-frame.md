# X-page-frame decisions (#TBD)

Slice session, 2026-11-21Z. It answers UI review I11 and P15 (`docs/review/2026-11-13-ui-ux/`, placed by `docs/decisions/2026-11-14-runner-ui-review-placement.md`): every destination framed its page differently, page edges missed each other by a pixel, and Companions and Outputs had no `h1`. The runner approved the wider file list (the `h1` and outer-wrapper lines of the feature components, and one line of `QueryBar.svelte`) in this session.

## The frame

- **One primitive pair under `web/src/lib/ui/layout/`.** `PageFrame` is the container and `PageHeader` is the heading row (the one `h1`, an optional one-line description, an optional actions slot). No second primitive exists: the old per-page classes (`.al-page`, `.dsc-page`, `.cmp-page`, `.ls-detail-page`'s box, `.sc-h1`, `.as-h1`, the `h1` rules of Compare, Discover and Alerts) are removed where nothing else used them.
- **Gutters:** `px-4` (16 px) below `large` (640 px), `large:px-6` (24 px) from it; `py-4` / `large:py-6` likewise. Tailwind's default steps, all on the §10.2 ramp. Print has none (`print:p-0`), as the Insights print rule had.
- **Widths, by `size`:** `full` has no cap (Search with its rail and drawer, Insights, Logs, the Settings shell); `wide` caps at `5 × --rail-width` (1,320 px, what Alerts already used; Alerts, Compare, Discover, Detail); `manuscript` is `--manuscript-width` (720 px; the unavailable page). Settings sections keep their `--manuscript-width` column inside the shell. No new token: `docs/design-tokens.md` is unchanged.
- **The `h1`:** one style everywhere, `text-display font-semibold` (28 / 32), `wrap-anywhere`. A page that moves focus to its heading passes `focusable` (`tabindex="-1"` and the shared focus ring) and may `bind:heading`.
- **Left-aligned, never centred.** `mx-auto` was why the edges differed at 1440 px (Alerts at 84, Detail at 176). The cap now limits only the right edge, so every page starts at the same gutter at every width.

## UX calls (runner calls under `docs/owner-decisions.md` §2.26)

1. **Left-aligned pages at wide widths** (above). Alerts, Detail and Insights lose their centred column; the content stays left, as the Search page already was.
2. **One heading size, 28 px.** Settings screens, Detail and Insights stage pages were 20 px (`text-title`), the others 28. Settings screens now match the rest (I11).
3. **Insights shows its `h1`.** The overview's was `sr-only`; I11 names "not visible on Insights" as the defect, so it is visible. The About screen's `h1` stays `sr-only`: its logo and name header is its visible heading, and no review finding asks otherwise. `PageHeader srOnly` keeps it in the one place.
4. **Companions and Outputs gain their `h1`**, reusing the nav titles (`settings_companions_title`, `settings_outputs_title`). No new message, so no en or zh file changes.
5. **Settings at 1024 px and up keeps the section list left of the content,** so its `h1` sits at 312 px (24 px gutter + the 16 rem list + the 32 px gap), the same on every Settings screen, while every other destination is at 24 px. At 390 px all are at 16 px. A literal one-edge rule at 1024 px would put the `h1` above the section list, and each Settings screen renders its own `h1`, so it needs a Settings layout slice; it is not done here. (The old edge was 304 px.)
6. **Spacing differences removed:** Discover's larger gap at `large` (24 px between regions) is now the frame's 16 px; Detail widens from 1,120 to 1,320 px (its summary keeps `--manuscript-width`); Search's heading row has the frame's 16 px gap to the query bar (it was 8 px).

## Measurements (CSS px; the `h1` is the text's left edge; "content" is the page's inner box)

Measured with `e2e/page-frame.routes.test.ts` (`PAGE_FRAME_TABLE=1`) on `main` and on this branch, against the OpenAPI mock. "Before" content edges are the page root's content box, which is the heading's wrapper; Search's wrapper had no padding of its own (its heading, query bar and grid each had `px-4`), so its 16 / width-16 below is read from the source. "none" is a page with no `h1`; "sr" is the clipped `sr-only` one.

| Destination | Width | Before: h1 | content L | content R | After: h1 | content L | content R |
|---|---|---|---|---|---|---|---|
| Search | 390 | 16 | 16 | 374 | 16 | 16 | 374 |
| Discover | 390 | 16 | 16 | 374 | 16 | 16 | 374 |
| Alerts | 390 | 16 | 16 | 374 | 16 | 16 | 374 |
| Alert detail | 390 | 16 | 16 | 374 | 16 | 16 | 374 |
| Insights | 390 | sr | 16 | 374 | 16 | 16 | 374 |
| Insights stage | 390 | 16 | 16 | 374 | 16 | 16 | 374 |
| Compare | 390 | 16 | 16 | 374 | 16 | 16 | 374 |
| Detail (`/t/…`) | 390 | 16 | 16 | 374 | 16 | 16 | 374 |
| Settings (all screens) | 390 | 16 | 16 | 374 | 16 | 16 | 374 |
| Settings, Companions and Outputs | 390 | none | 16 | 374 | 16 | 16 | 374 |
| Search | 1024 | 16 | 16 | 1008 | 24 | 24 | 1000 |
| Discover | 1024 | 24 | 24 | 1000 | 24 | 24 | 1000 |
| Alerts | 1024 | 24 | 24 | 1000 | 24 | 24 | 1000 |
| Alert detail | 1024 | 24 | 24 | 1000 | 24 | 24 | 1000 |
| Insights | 1024 | sr | 24 | 1000 | 24 | 24 | 1000 |
| Insights stage | 1024 | 24 | 24 | 1000 | 24 | 24 | 1000 |
| Compare | 1024 | 16 | 16 | 1008 | 24 | 24 | 1000 |
| Detail | 1024 | 16 | 16 | 1008 | 24 | 24 | 1000 |
| Settings (all screens) | 1024 | 304 | 16 | 1008 | 312 | 24 | 1000 |
| Settings, Companions and Outputs | 1024 | none | 16 | 1008 | 312 | 24 | 1000 |
| Search | 1440 | 16 | 16 | 1424 | 24 | 24 | 1416 |
| Discover | 1440 | 24 | 24 | 1416 | 24 | 24 | 1344 |
| Alerts | 1440 | 84 | 84 | 1356 | 24 | 24 | 1344 |
| Alert detail | 1440 | 84 | 84 | 1356 | 24 | 24 | 1344 |
| Insights | 1440 | sr | 24 | 1416 | 24 | 24 | 1416 |
| Insights stage | 1440 | 24 | 24 | 1416 | 24 | 24 | 1416 |
| Compare | 1440 | 16 | 16 | 1424 | 24 | 24 | 1344 |
| Detail | 1440 | 176 | 176 | 1264 | 24 | 24 | 1344 |
| Settings (all screens) | 1440 | 304 | 16 | 1424 | 312 | 24 | 1416 |
| Settings, Companions and Outputs | 1440 | none | 16 | 1424 | 312 | 24 | 1416 |

After the change every destination's `h1` is at 16 px at 390 px and at 24 px at 1024 and 1440 px, except the Settings screens at 1024 px and up (call 5), which share 312 px. The old spread was 16, 24, 84, 176 and 304 px, with 15 / 23 px for Insights's clipped heading and no heading on Companions and Outputs. No two edges differ by 1 px.

## Tests

- `web/src/lib/ui/layout/layout.test.ts`: the frame's gutters, caps, class and style pass-through; the header's one `h1`, description, actions, focus and `srOnly`.
- `web/e2e/page-frame.routes.test.ts` (the `core` project): every destination at 390, 1024 and 1440 px has exactly one `h1` inside the page frame, at 16 px (24 px from `large`), and every Settings screen shares one edge. It waits on conditions with the 10 s deadline and has no fixed sleeps.
- Snapshots: none regenerated here; CI generates them (`web/e2e/README.md`). The key screens that change are Search, Settings and Detail at every breakpoint.

## Not done, and follow-ups

- **Filters (`features/filters/screens/FiltersScreen.svelte`) still has its own `text-title` `h1`.** It is inside the Settings content column, so its edge matches the other Settings screens, but it is 20 px, not 28. It is X-safety-undo-danger's; a one-line change to `PageHeader` after that slice lands. The e2e spec does not visit it for the same reason.
- The Settings layout above (call 5), if the owner wants one edge at 1024 px and up.

## Size

Search's first-load JS is 44.39 kB on `main` and 45.47 kB here (+1.08 kB): the two primitives pull Svelte's snippet and bindable helpers into the first load. Raises, each the measured size rounded up, are in `.size-limit.mjs`; the runner approved the numbers in this session.
