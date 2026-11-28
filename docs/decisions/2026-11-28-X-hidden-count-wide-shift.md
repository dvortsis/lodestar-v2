# X-hidden-count-wide-shift decisions

Slice `X-hidden-count-wide-shift`, the follow-up X-hidden-count recorded (r24). The runner decided the points below (§2.26) and flags them to the owner (§2.19).

## What moved
From 128rem of window the toolbar is layout L3: `justify-content: flex-start`, so Sort and View follow the count instead of sitting at the far edge. The count was only as wide as its text, so when "N hidden by filters · Show" arrived the controls moved right by the note's width. Below 128rem the controls are pinned to the far edge and never moved.

## Decision
- **The count's slot has a floor from 128rem (runner).** `.tb-count` gets `flex: none; min-inline-size: calc(var(--spacing) * 96)` (384 px) in the `>= 128rem` block of `toolbar.css`, the width equivalent of the `min-height: 1lh` it already has. L3 is unchanged: the controls still follow the count, the 960 px max-width and left alignment stay. No markup, wording or token change.
- **Sized from the worst realistic case, not the typical one.** "≈ 2.4M results" (the approximate total) with a 7-digit hidden count and the full words, measured as the text's own width at 2,048 px in the UI font (Chromium, e2e):

  | Language | String | Measured text width |
  | -------- | ------ | ------------------- |
  | en | "≈ 2.4M results · 9,999,999 hidden by filters · Show" | 360.9 px |
  | zh | "≈ 240万 个结果 · 9,999,999 个已被过滤器隐藏 · 显示" | 380.2 px |

  Step 96 (384 px) holds both, so it stays. zh leaves 4 px, so a CJK fallback font wider than the one measured would show up as the e2e case failing, not as a silent shift.
- **Residual.** A longer count than the worst case measured (a grouped total with a long hidden count) still pushes the controls, as X-hidden-count already says for narrow widths.
- **Rejected: anchoring the controls independently** (e.g. `margin-inline-start: auto`). It changes L3's look, which is the owner-decided layout.

## Tests
`web/e2e/search-toolbar.routes.test.ts`: the width loop no longer skips Sort's x from 2,048 px and adds 2,560 px; a new case per language (en, zh-CN) holds Sort's box, the toolbar's height and row 1's y at 2,048 px with an approximate 2.4M total and a 9,999,999 hidden count. Condition waits only.

## Size
Measured with `npm run size:ci`: Results toolbar (lazy) 5,590 → 5,610 B, so its limit moves 5.60 → 5.61 KB (the reserved width is the toolbar chunk's CSS). The CSS entry is unchanged at 19,816 B (limit 19.82 KB).
