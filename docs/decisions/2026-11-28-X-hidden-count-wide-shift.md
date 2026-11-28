# X-hidden-count-wide-shift decisions

Slice `X-hidden-count-wide-shift`, the follow-up X-hidden-count recorded (r24). The runner decided the points below (§2.26; L3 was the runner's layout, not an owner decision) and flags them to the owner (§2.19).

## What moved
From 128rem of window the toolbar was layout L3: `justify-content: flex-start` and `max-width: 960 px`, so Sort and View followed the count instead of sitting at the far edge. The count was only as wide as its text, so when "N hidden by filters · Show" arrived the controls moved right by the note's width.

## Decision
- **L3 now means: the toolbar is the rows' width and its controls end at that box's edge (runner, refines §10.4).** The `>= 128rem` block in `toolbar.css` is gone, so the toolbar behaves as it does below 128rem: `space-between` across the whole box. Sort and View sit at the rows' right edge and never move, whatever the count's length or language.
- **Alignment checked.** At 2,048 and 2,560 px the results rows are the full track wide (1,712 and 2,224 px), not 960 px. The 960 px cap made the toolbar narrower than the rows, so the controls could not line up with the rows' end. Without the cap the toolbar box has the rows' x and width at both widths (e2e asserts it).
- **A fixed floor on the count was tried and dropped.** A 384 px floor plus the 16 px gap plus the controls (560.7 px in en) is 960.7 px, over the 960 px cap, so every English page wrapped to a two-row toolbar. It also did not hold the real worst cases, measured at the UI font:

  | Case | Measured text width |
  | ---- | ------------------- |
  | zh "≈ 1234.6万 个结果" (totals of 10M to 100M) | 403.8 px |
  | zh exact "199,999 个结果" | 384.1 px |
  | en exact "12,345,678 results" | 397.9 px |

  Any width floor is fragile against text length and language.
- **Rejected: a fixed floor and the 960 px cap** (above), and a controls anchor independent of the box (the same result as the chosen fix, with more CSS).

## Tests
`web/e2e/search-toolbar.routes.test.ts`: the width loop no longer skips Sort's x from 2,048 px and adds 2,560 px. A worst-case case per language (en exact 12,345,678; zh "≈ 1234.6万", each with 9,999,999 hidden) at 2,048 and 2,560 px asserts the full text, a one-row toolbar (as tall as a dense control), the toolbar's box equal to the rows' x and width, and Sort's box, the toolbar's height and row 1's y unchanged when the note arrives. Condition waits only.

## Size
Measured with `npm run size:ci`: no budget moves (the change removes CSS), so `.size-limit.mjs` is unchanged.
