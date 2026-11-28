# X-hidden-count-wide-shift decisions (#1179)

Slice `X-hidden-count-wide-shift`, the follow-up X-hidden-count recorded (r24). The runner decided the points below (§2.26; L3 was the runner's layout, not an owner decision) and flags them to the owner (§2.19).

## What moved
From 128rem of window the toolbar was layout L3: `justify-content: flex-start` and `max-width: 960 px`, so Sort and View followed the count instead of sitting at the far edge. The count was only as wide as its text, so when "N hidden by filters · Show" arrived the controls moved right by the note's width.

## Decision
- **L3 keeps its content-width box, and the controls end at that box (runner, refines §10.4).** From 128rem the toolbar is a box capped at `calc(var(--spacing) * 256)` (1,024 px), and the controls sit at the box's end (`space-between`, the toolbar's own rule), not after the count. They never move, whatever the count's length or language. The box matters: the UI review's L3 finding (`docs/review/2026-11-13-ui-ux/03-not-done.md:12`) was that at 2,560 px the rows' text uses about the left 35% of the screen while controls pinned to the far right sit about 1,700 px from it. The row boxes are full-track wide (1,712 and 2,224 px at 2,048 and 2,560 px) but their visible content isn't.
- **The cap goes from 960 to 1,024 px.** At 960 px the longest English line leaves too little room beside the controls (which are 560.7 px wide). Measured at the UI font, the room left in the box after the text, the 16 px gap and the controls, with the full note and 9,999,999 hidden (exact totals stop at 200,000, `crates/search/src/count.rs`):

  | Case | Room left (incl. the 16 px gap) |
  | ---- | ------------------------------- |
  | en "≈ 123.5M results" | 82.2 px |
  | en "199,999 results" | 88.9 px |
  | zh "≈ 1234.6万 个结果" | 136.2 px |
  | zh "199,999 个结果" | 155.9 px |

  Each leaves at least 48 px beyond the gap; the e2e asserts it. The same cases at 960 px would leave 18 px for the first row.
- **Rejected: a fixed floor on the count.** A 384 px floor plus the gap plus the controls is 960.7 px, over the old cap, so every English page wrapped to two rows, and it missed the longer counts anyway (zh "≈ 1234.6万" 403.8 px, zh exact 384.1 px). Any width floor is fragile against text length and language.
- **Rejected: removing the cap** (controls at the full track's end). It puts the controls about 1,700 px from the text at 2,560 px, the L3 finding above.

- **Later change:** X-hidden-count-grouped raised the cap to 1,152 px (`calc(var(--spacing) * 288)`) for grouped counts; see `2026-11-28-X-hidden-count-grouped.md`.

## Tests
`web/e2e/search-toolbar.routes.test.ts`: the width loop no longer skips Sort's x from 2,048 px and adds 2,560 px. Four worst-case cases (en approximate "≈ 123.5M", en exact 199,999, zh "≈ 1234.6万", zh exact 199,999, each with 9,999,999 hidden) at 2,048 and 2,560 px assert the full text, a one-row toolbar, its width at most the cap, the 48 px of room, and Sort's box, the toolbar's height and row 1's y unchanged when the note arrives. Condition waits only.

## Size
Measured with `npm run size:ci`: no budget moves, so `.size-limit.mjs` is unchanged.
