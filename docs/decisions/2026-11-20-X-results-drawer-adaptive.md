# X-results-drawer-adaptive decisions

Slice X-results-drawer-adaptive (Track U step 3): UI review findings I1 and I3, built together under owner decision §2.66 items 1 to 3, plus I3's Discover note and L5 (`docs/decisions/2026-11-14-runner-ui-review-placement.md`). Architecture §10.15, §10.4 and §10.2.

- **Who decided:** choices marked "runner" are the runner's calls (§2.26, flagged to the owner under §2.19). The rest are the slice session's, made on 2026-11-20Z for the runner to confirm.

## The drawer beside the results from 1366 px (I1, §2.66 item 1)

- **When the drawer is in flow.** From the xx-large range (1366 px), the drawer sits beside the results whenever its minimum (`--drawer-min-width`) fits beside the results' floor (`--results-min-width-drawer`). Both are 480 px. Below that, it overlays as before.
  - At 1366 px that leaves the results about 542 px, as the owner's decision expects.
- **The results' floor.** While the drawer is in flow, the grid's results track is `minmax(var(--results-min-width-drawer), 1fr)` instead of `minmax(var(--results-min-width), 1fr)`.
  - **A new token, `--results-min-width-drawer` (480 px)** (runner, 2026-11-20). It's the results track's floor while the drawer is in flow. Reusing `--drawer-min-width`, which has the same value, would have tied two meanings together, and the tokens are a contract.
  - No token test or theme-compiler list enumerates the layout tokens, so nothing else changed.
- **The drawer's width.** It is the stored width, or `--drawer-overlay-width` if none is stored, capped at the most that still leaves the results `--results-min-width` (560 px). It is never under `--drawer-min-width`; there the results give way instead, down to their floor.
  - **A cap, not a step** (runner, 2026-11-21: accepted as the runner's refinement of owner §2.66 item 1). The owner's wording is "opens at `--drawer-min-width` until the window is wide enough for the stored width". Read as a cap rather than a jump, the drawer grows with the window between those two widths and the results stay at 560 px. That avoids a jump of up to 160 px at one exact width.
  - **Resizing the window never rewrites the stored width.** The cap is the Drawer's `maxWidth`, so the Drawer clamps the shown width and the stored value stays as the user chose it. A test checks that 640 stays stored at 1440 px, and is shown again at 1920 px.
  - **Neither does a resize press at the cap** (fixed in `Drawer.svelte`). The Drawer stored a clamped width whenever it differed from the stored one, so widening at the cap would have saved 536 over 640. It now compares against the width shown: a press the cap clamps stores nothing, and a press that narrows the drawer is stored as before. A keyboard test covers both.
  - **Dragging stops at the cap.** The resize handle can't push the results below `--results-min-width`. The floor only applies while the drawer is at its own minimum.

## Rows adapt to their own width (I3, L5)

- **§10.4's order, with container tiers** (runner, 2026-11-21). The facts keep §10.4's order: a feature's own fact, badges, the composition fact, size, files, availability, discovered, then the technical trackers fact last (§10.4 keeps it off the row; X-row-rail removes it). Tiers on the row's own width drop facts when it is too narrow, in this order:
  - **at 720 px and under,** the composition fact;
  - **at 600 px and under,** the file count;
  - **at 480 px and under,** everything but size, availability, the discovered time and a feature's own fact. The discovered time is new in this tier, so a phone shows when each torrent was found (I3).
  - Every dropped fact is in the drawer and in Table mode.
- **The widths are the row's content box,** which is what a container query reads. That's the row's width less its 24 px of padding. Measured with the fixtures:
  - Search rows at a 1024 px window are 704 px (680 inside), so only the composition fact goes.
  - With the drawer in flow at 1366 px they're 542 px (518 inside), so the file count goes too. That leaves size, availability and the discovered time on the line.
  - Discover's side panels are 429 px at a 1366 px window, so they're in the 480 px tier inside a desktop window.
- **A fact drops whole, never mid-word.** The facts line wraps, and only its first line shows (`height: 1lh`). Past the tiers, a fact that doesn't fit moves to the hidden second line instead of being cut at the end ("3 hours ag").
  - `1lh` follows the line height, so it still holds under WCAG 1.4.12 text spacing (L5).
  - Dropped facts stay in the accessibility tree.
- **Its limits.**
  - The first fact on the line can't wrap away, so a first fact wider than the whole row is still cut: a feature's own fact, or the badge group.
  - Under 1.4.12 text spacing, a badge can be about half a pixel taller than `1lh`, so its bottom border clips; the text doesn't.
  - Compact rows don't wrap, since their facts don't flex. A compact test at 768 px checks that none reaches past the row's text column.
- **Discover's Newest leads with the discovered time (I3).** The rule `[aria-labelledby='discover-newest-h'] .rr-discovered { order: -4 }` is in `row.css`, so Discover's markup, outside this slice's owns, is unchanged. A test covers it, since a rename in Discover would drop the rule silently.
- **An earlier reorder was withdrawn.** One push put availability and discovered before the composition fact instead of adding tiers. The runner kept §10.4's order, because that reorder changed the visible order even on wide rows where everything fits.

## Sizes

- **"CSS" from 19.72 KB to 19.73 KB** (runner, §2.26, 2026-11-20). This covers only the new token: 5 B gzipped, taking the global sheet from 19,717 B to 19,722 B, 2 B over the old limit. The built sheet is otherwise byte-identical to `main`'s.
  - The raise was made after X-session-draft-keep (#999), which was editing `web/.size-limit.mjs`, merged.
- **"Search results (lazy)" from 40.61 KB to 40.62 KB** (runner, §2.26, 2026-11-21).
  - The two tiers and the `rr-files` class take the entry to 40,617 B, 7 B over. `main` measures 40,556 B.
  - The raise is the overage rounded up to 0.1 kB.
- Every other `size-limit` entry passes unchanged.
  - Detail drawer: 28,111 of 28,128 B.
  - Discover: 42,151 of 43,000 B.
  - Alerts: 45,959 of 46,000 B.
- A comment using the word "shrink" had added a `.shrink` utility to the global sheet. It was reworded, so Tailwind's scan of `.svelte` comments costs nothing here.

## Tests and baselines

- **`e2e/search-results-adaptive.routes.test.ts`:**
  - **The drawer in flow:**
    - it sits beside the results at 1366 and 1440 px, not over them;
    - at 1366 px the drawer is 480 px and inside the window, and the results are under 560 px and at least 480 px;
    - the toolbar's last control and the rows are uncovered;
    - size, availability and the discovered time stay on the visible line;
    - a stored 640 px width gives way at 1440 px and returns at 1920 px;
    - widening at the cap stores nothing;
    - 1365 px still overlays.
  - **`factsWhole`, a geometry check:**
    - every facts line is one line tall;
    - every fact on it is wholly inside the part that shows (the line, as far as the row's text column reaches) or wrapped out of it.
    - It runs at 390, 768, 1024, 1366 and 1920 px, in compact rows, under 1.4.12 text spacing, and with the drawer in flow.
  - **The tiers follow the row, not the window:**
    - Search at 1024, 1920, and 1366 px with the drawer in flow;
    - Discover's side panels at 1366 and 1920 px.
  - **Discover's Newest** leads with the discovered time.
  - **Baseline cells** at 768 and 1024 px with result rows (review 02 §3a), and at 1440 px with the drawer in flow.
  - The runner's review mutants now fail tests: 480 → 400 px, `@container` → `@media`, and no `height: 1lh`.
- **Baselines are generated in CI** with the repo's marker workflow (`web/e2e/README.md`). None were made in this container.
  - Before this slice, no baseline at 768 or 1024 px showed result rows: the search key screens' rows weren't in those shots, and a full regeneration left them byte-identical. The new 768, 1024 and 1440 px cells are this slice's.

## Edits outside this slice's owns (runner-approved)

- `web/src/lib/theme/tokens.css` and `docs/design-tokens.md`: the `--results-min-width-drawer` token.
- `web/.size-limit.mjs`: the "CSS" and "Search results (lazy)" raises.
- `web/e2e/soak-followups.routes.test.ts`: one assertion. L4's test expected the drawer to overlay at 1366 px, which §2.66 item 1 replaces; it now expects it in flow.
  - Its other checks are unchanged and pass for both stored widths: the drawer fits the window, Close is in view, and it's in flow at 1920 px.
- `web/e2e/.update-snapshots`: the repo's baseline marker, removed again once CI commits the baselines.

## Not here

- Group versions (GS-5) and "Identified as" (ENR-13: text, IDs and links only) are unchanged.
- Removing the technical facts from the row, and the two-tier row, are X-row-rail's.
