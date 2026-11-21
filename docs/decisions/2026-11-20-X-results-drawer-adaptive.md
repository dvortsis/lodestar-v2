# X-results-drawer-adaptive decisions

Slice X-results-drawer-adaptive (Track U step 3): UI review findings I1 and I3, built together under owner decision §2.66 items 1 to 3, plus I3's Discover note and L5 (`docs/decisions/2026-11-14-runner-ui-review-placement.md`). Architecture §10.15, §10.4 and §10.2. Choices made by the slice session on 2026-11-20Z for the runner to confirm.

## The drawer beside the results from 1366 px (I1, §2.66 item 1)

- **When the drawer is in flow.** From the xx-large range (1366 px), the drawer sits beside the results whenever the drawer's minimum (`--drawer-min-width`) fits beside the results' floor (`--results-min-width-drawer`). Both are 480 px. Below that, it overlays as before.
  - At 1366 px that leaves the results about 542 px, as the owner's decision expects.
- **The results' floor.** While the drawer is in flow, the grid's results track is `minmax(var(--results-min-width-drawer), 1fr)` instead of `minmax(var(--results-min-width), 1fr)`.
  - **A new token, `--results-min-width-drawer` (480 px)** (runner, 2026-11-20). It's the results track's floor while the drawer is in flow. Reusing `--drawer-min-width`, which has the same value, would have tied two meanings together, and the tokens are a contract.
  - **Edited outside this slice's owns, as the runner approved:** one line in `web/src/lib/theme/tokens.css` and one row in `docs/design-tokens.md`. No token test or theme-compiler list enumerates the layout tokens, so nothing else changed.
- **The drawer's width.** It is the stored width, or `--drawer-overlay-width` if none is stored, capped at the most that still leaves the results `--results-min-width` (560 px). It is never under `--drawer-min-width`; there the results give way instead, down to their floor.
  - **A cap, not a step.** The owner's wording is "opens at `--drawer-min-width` until the window is wide enough for the stored width". The slice reads that as a cap rather than a jump: between the two, the drawer grows with the window. That avoids a jump of up to 160 px at one exact width.
  - **Resizing never rewrites the stored width.** The cap is the Drawer's `maxWidth`, so the Drawer clamps the shown width and the stored value stays as the user chose it. A test checks that 640 stays stored at 1440 px, and is shown again at 1920 px.
  - **Neither does a resize press at the cap** (fixed in `Drawer.svelte` in this slice). The Drawer stored a clamped width whenever it differed from the stored one, so widening at the cap would have saved 536 over 640. It now compares against the width shown: a press the cap clamps stores nothing, and a press that narrows the drawer is stored as before. A keyboard test covers both.
  - **Dragging stops at the cap.** The resize handle can't push the results below `--results-min-width`. The floor only applies while the drawer is at its own minimum.

## Rows adapt to their own width (I3, L5)

- **A fact drops whole, never mid-word.** The facts line now wraps, and only its first line shows (`height: 1lh`). A fact that doesn't fit therefore moves to the hidden second line instead of being cut at the end ("3 hours ag").
  - `1lh` follows the line height, so it still holds under WCAG 1.4.12 text spacing (L5). A test injects the 1.4.12 overrides.
  - Dropped facts stay in the accessibility tree, and they are all in the drawer and in Table mode.
- **Its limits.** The first fact on the line can't wrap away, so a first fact wider than the whole row is still cut: a feature's own fact, or the badge group. Under 1.4.12 text spacing, a badge can be about half a pixel taller than `1lh`, so its bottom border clips; the text doesn't. Compact rows don't wrap, since their facts don't flex; a 768 px test shows none is cut there.
- **What drops first.** Facts drop from the end of the line, so the line's order is its priority. It is now: a feature's own fact, the badges, size, availability, discovered, then the composition bar, the file count and, last, the technical trackers fact (§10.4 keeps that off the row; X-row-rail removes it).
  - **Why the composition bar and file count moved behind availability and discovered.** CI's first 1440 px baseline, with the drawer in flow, showed rows about 560 px wide that kept the bar and the file count but had dropped the seeders and the discovered time. Those are what a choice rests on (I3). The reorder costs no CSS. A test now requires size, availability and discovered on the visible line with the drawer in flow.
  - The 480 px container tier (`@container result-row`) now also keeps the discovered time, so a phone shows when each torrent was found (I3).
  - The tiers read the row's own width, not the viewport's (§10.15).
- **No middle tiers.** Tiers at 720 and 600 px, which would drop the composition bar and file count first while keeping §10.4's order, would have cost about 30 B gzipped, and Search results had 54 B of headroom. The reorder does the same job at no cost, and it also made the 480 px tier's `order` rules unnecessary, so they're gone.
- **Discover's Newest leads with the discovered time (I3).** The rule `[aria-labelledby='discover-newest-h'] .rr-discovered { order: -4 }` is in `row.css`, so Discover's markup (outside this slice's owns) is unchanged.
- **The facts order, against §10.4.** §10.4 lists badges, composition bar, size, files, availability and discovered, in that order. The row now shows size, availability and discovered before the composition bar and file count. This is the slice's call, made so that narrow rows keep the choice facts. **For the runner:** X-row-rail's two-tier row may settle the order for good; if it keeps this one, §10.4's list should follow.

## Sizes

- **One limit raised, by the runner (§2.26, 2026-11-20).** The global "CSS" entry in `web/.size-limit.mjs` goes from 19.72 KB to 19.73 KB. That covers only the new token: 5 B gzipped, which takes the sheet from 19,717 B to 19,722 B, 2 B over the old limit. The built sheet is otherwise byte-identical to `main`'s. That one line is outside this slice's owns, and the change was made after X-session-draft-keep, which was editing the same file, merged.
- Every other `size-limit` entry passes unchanged.
- Search results (lazy) is at 40,599 of 40,610 B. The next change to the row (X-row-rail) needs a budget review.
- Detail drawer is at 28,111 of 28,128 B.
- A comment using the word "shrink" had added a `.shrink` utility to the global sheet. It was reworded, so Tailwind's scan of `.svelte` comments costs nothing here.

## Tests and baselines

- **`e2e/search-results-adaptive.routes.test.ts`:**
  - the drawer is in flow at 1366 and 1440 px, beside the results, not over them;
  - at 1366 px the drawer is 480 px and the results at least 530 px;
  - the toolbar's last control and the rows are uncovered;
  - a stored 640 px width gives way at 1440 px and returns at 1920 px;
  - 1365 px still overlays;
  - no fact is cut at 390, 768, 1024, 1366 or 1920 px, nor under 1.4.12 text spacing;
  - size, availability and the discovered time show at every width;
  - Discover's Newest shows the discovered time first;
  - a 1440 px baseline with the drawer open.
  - On `main`'s code, 10 of its 13 tests fail.
- **Baselines are generated in CI** with the repo's marker-file workflow (`web/e2e/README.md`). The refreshed 768 and 1024 px search baselines come from that run too, along with any other baseline that shows a result row, such as Discover. None were made in this container.

## Edits outside this slice's owns (runner-approved)

- `web/src/lib/theme/tokens.css` and `docs/design-tokens.md`: the `--results-min-width-drawer` token (above).
- `web/.size-limit.mjs`: the "CSS" raise (above).
- `web/e2e/soak-followups.routes.test.ts`: one assertion. L4's test expected the drawer to overlay at 1366 px, which §2.66 item 1 replaces; it now expects it in flow. Its other checks are unchanged and pass for both stored widths: the drawer fits the window, Close is in view, and it's in flow at 1920 px.
- `web/e2e/.update-snapshots`: the repo's baseline marker, removed again once CI commits the baselines.

## Not here

- Group versions (GS-5) and "Identified as" (ENR-13: text, IDs and links only) are unchanged.
- Removing the technical facts from the row, and the two-tier row, are X-row-rail's.
