# X-hidden-count-grouped decisions

Slice `X-hidden-count-grouped`, from the fresh review of #1179: a grouped count ("≈ 12.3M groups · ≈ 12.3M torrents") plus a 7-digit hidden note wrapped the toolbar from 32 to 64 px when the note arrived, so Sort jumped and the rows moved. It happened on `main` at 900, 1,024 and 1,366 px windows and from 128rem, and X-hidden-count's no-note rule did not account for the longer line. The runner decided the points below (§2.26) and flags them to the owner (§2.19).

## Measured
Widths at the UI font, px, with 9,999,999 hidden (grouped is the default view; exact totals stop at 200,000):

| Piece | en approx / exact | zh approx / exact |
| ----- | ----------------- | ----------------- |
| count text | 246 / 253 (≈ 123.5M: 252) | 269 / 230 |
| count + full note + Show | 525 (≈ 123.5M) / 512 | 537 / 497 |
| count + short note + Show | 442 / 448 | 473 / 433 |
| controls, labels visible | 560.7 | 484 |
| controls, labels hidden | 474.4 | 430 |

The line needed is count + note + a 16 px gap + controls. The longest, en "≈ 123.5M groups · ≈ 123.5M torrents" with the full note and visible labels, is 525 + 16 + 560.7 = 1,102 px ("≈ 12.3M" is 1,088 px); zh approximate is 1,037 px. The short form with visible labels is 1,025 px (en), which does not fit the 1,024 px box with any room; with hidden labels it is 939 px.

## Decisions
- **A grouped count (`tb-grp`, set while the rows are grouped) shows its note only from 72rem (1,152 px) of toolbar, and never below it (runner, refines X-hidden-count; the runner's 71rem became 72rem once the longest English line measured 1,102 px, not 1,088: 71rem would leave 34 px).** The full form with visible labels is used, 50 px of room beyond the 1,102 px line. Below 72rem "Show hidden" stays in Filters, and Sort and View keep their visible labels. The rule is on the toolbar's own width, a container query, so the drawer opening or closing needs no extra case. Plain counts keep the existing thresholds unchanged.
- **Rejected: a short note with the labels hidden between 62rem and 72rem.** Grouped is the default and 1,366 px laptops (a 1,030 px box) are common, so it would take Sort and View's visible labels off the default search view to fit a note that Filters already offers.
- **The L3 cap from 128rem goes from 1,024 to 1,152 px (`calc(var(--spacing) * 288)`).** At 1,024 px the box is below 72rem, so no wide window would ever show a grouped note. At 1,152 px the full line fits with 50 px to spare, and the box is still far narrower than the 1,712 and 2,224 px tracks, so the UI review's L3 finding holds..
- **Nothing moves** when the note arrives, whether the note shows or not, or when the drawer is open. Whether it shows depends on the box, never on whether the count has arrived.

## Tests
`web/e2e/search-toolbar.routes.test.ts`: grouped worst cases in en and zh, approximate ("≈ 123.5M" in en) and exact, with 9,999,999 hidden, at 900, 1,024, 1,366, 1,600, 2,048 and 2,560 px, and at 2,560 px with the drawer open. Each asserts: the full text where the box is at least 72rem, no note below it, and Sort's box and the toolbar's height unchanged when the note arrives (row 1's y too, where the drawer is closed). They fail without the change. The #1179 worst cases assert the controls' right edge equals the toolbar's right edge from 128rem, and a box of exactly 1,152 px (the cap). Condition waits only.

## Size
Measured with `npm run size:ci`: Results toolbar (lazy) 5,610 → 5,647 B (the `tb-grp` class and its container query), so its limit moves 5.60 → 5.65 KB (runner §2.26). CSS (19,816 B) and the other entries are unchanged.
