# X-hidden-count-grouped (runner brief, 2026-11-28)

**Slice:** X-hidden-count-grouped. Sonnet, UI. Same session as X-hidden-count-wide-shift.
**Branch:** `slice/X-hidden-count-grouped` from current `main`.
**PR title (exact):** `X-hidden-count-grouped: A grouped count with the hidden note never wraps the toolbar`
**Owns:** `web/src/lib/features/search/toolbar/**`, `web/e2e/search-toolbar.routes.test.ts`,
`docs/decisions/*-X-hidden-count-grouped.md`, and the #1179 decisions file (its heading and the
line 9 wording only).

## Source
The fresh review of #1179 (merged) found a gap that predates it, in X-hidden-count's no-note rule:
grouped counts aren't accounted for. "≈ 12.3M groups · ≈ 12.3M torrents · 9,999,999 hidden by
filters · Show" wraps the toolbar from 32 to 64 px when the note arrives, so Sort jumps (from x
1,106 to 643 at a 1,024 px box) and the rows move. It happens on main at 1,366 px (a 1,030 px box)
and at the old 960 px cap too.

## Do
1. **Step 0.** Measure the grouped worst cases in en and zh at every width the toolbar spec
   covers, with the drawer open and closed. Propose the rule: when the short form is used, when
   no note is shown ("Show hidden" stays in Filters), and how it's decided (the container query
   thresholds, or a class from the count's kind). Nothing may move when the note arrives.
   Send it to the runner and wait for the reply.
2. **Tests:** grouped worst cases at the widths where they matter, asserting one row and that
   Sort's box, the toolbar's height and row 1's y are unchanged. Also add the #1179 review's low
   item: assert that the controls' right edge equals the toolbar's right edge from 128rem.
   Condition waits only.
3. **Docs:** fix the #1179 decisions file's heading (`# … decisions (#1179)`) and line 9's wording
   (560.7 px is the controls' width, not the room).
4. UI rules, a decisions file, the CLAUDE.md `web/` checks, `.claude/skills/steward/SKILL.md`,
   and report `ready <PR> <sha>`.
