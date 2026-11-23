# X-index-banner-late (runner brief, 2026-11-23Z)

Web slice, Opus (layout timing under load). Fresh Opus review before merge. Placed by `docs/decisions/2026-11-23-runner-records-r14.md`; its slices.yaml entry is `X-index-banner-late`.

## Why
From #1075 (X-search-toolbar): under load, the index-state banner ("still growing" and its siblings) can arrive after the result rows, because `IndexState` waits up to `FIGURES_WAIT_MS` (1 s) for its figures. When it lands, it pushes the rows down 38-60 px. That's a layout shift the user sees, and it can move a row under the pointer just as they click.

## Goal
1. Measure it first: an e2e at 390 and 1440 px with the figures response delayed past the rows (and a 4x CPU variant), recording the layout shift and the first row's y before and after the banner arrives. It must fail on main.
2. Decide the banner's space before the rows paint. Options to weigh at step 0:
   - (a) the index-state decision (show a banner or not) comes from data the panel already has when the rows render, and only the figures inside the banner fill in later in reserved space;
   - (b) reserve the banner's height while the decision is pending, and collapse it only if no banner shows (a collapse above the rows is also a shift, so it's only acceptable if it happens before the rows paint);
   - (c) a late banner appears in a place that doesn't move the rows (an overlay or below the toolbar's fixed area), if that's within §10.4's design.
   Recommend one by UX: no row ever moves after it's painted, and no blank band stays when there's no banner.
3. Layout shift is 0 (bound 1e-9, as #1075 set below 1024 px) on the delayed-figures runs at both widths. The first row's y doesn't change after it's painted.

## Step 0 (send me, then wait for "go")
1. The measured shift on main (numbers), and where the banner's decision and figures come from (file:line).
2. Your recommended option and why.
3. Owns, to refine: `web/src/lib/features/search/results/IndexState.svelte`, `ResultsPanel.svelte`, its e2e spec(s), `docs/decisions/2026-11-23-X-index-banner-late.md`. Name anything else.
4. Size: no raises are possible right now (`web/.size-limit.mjs` is held by X-size-own-modules). Search results (lazy) is 40.28 / 40.86 kB, index-state banners 4.01 / 4.1 kB, the Search route 44.98 / 45 kB. Say what the fix costs and where it's paid from.

## Rules
- Tokens only, no hex, no arbitrary values; en and zh messages if copy changes; status is icon plus text.
- No fixed sleeps in tests; condition waits of at least 10 s.
- Never beside X-row-rail or X-facet-exclude-fill (they share the results layout); the runner keeps them apart.
- Never write the bare word "divider" in a source file (runner/standards.md §4). Format only with `npm run format`.

## Branch and PR
- Branch `slice/X-index-banner-late`. PR title "X-index-banner-late: the still-growing banner never pushes rows down".
- Gates per CLAUDE.md for `web/`. Push once per round, then send "ready <pr> <full sha>". Follow `.claude/skills/steward/SKILL.md`; never merge.
