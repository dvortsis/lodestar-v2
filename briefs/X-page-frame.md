# X-page-frame

Runner brief, 2026-11-21Z. Slice id `X-page-frame`, branch `slice/X-page-frame`, PR title `X-page-frame: <title>`. The plan entry is in `runner/slices.yaml`. The findings are I11 in `docs/review/2026-11-13-ui-ux/01-review.md` (line 77 on) and P15 in `02-skills-pass-and-future-modules.md` (line 75, and the "one page frame and header primitive" note at line 99), placed by `docs/decisions/2026-11-14-runner-ui-review-placement.md`. An Opus reviewer checks the result.

## Why
Every destination frames its page differently. Page `h1` left edges measure 16, 24, 47, 139 and 304 px across destinations, content edges miss each other by a pixel (40/41, 711/712 px on Insights), and Companions and Outputs have no `h1` at all. One page frame and header primitive fixes both and gives every new screen a place to land.

## What
1. **One frame primitive** under `web/src/lib/ui/layout/` (look first for an existing one to extend, rather than adding a second): a page container with the §10.2 side gutters and max width, and a page header (the `h1`, an optional one-line description, an optional actions slot). Spacing only from the §10.2 ramp (`docs/design-tokens.md` §5, Tailwind default steps), colours only from tokens, no hex, no arbitrary values.
2. **Every destination uses it.** Each route's `+page.svelte` changes only its header lines and its outer wrapper; the screen components inside don't change. Settings screens: follow the Settings shell (`web/src/lib/features/settings/shell/**`) so every Settings screen gets the same `h1` edge; Companions and Outputs gain their `h1`, with en and zh messages (reuse the nav labels if they already exist).
3. **One `h1` per page,** and the heading order below it unchanged. The document title stays as it is.
4. **Measure.** At 390, 1024 and 1440 px, record each destination's `h1` left edge and content left and right edges before and after, in a table in the decisions file. After, every destination's `h1` edge is the same at each width, and no edge differs from another by 1 px.
5. **Tests.** A unit test for the primitive, and one e2e spec that visits every destination at the three widths and asserts the single `h1` and the shared left edge (conditional waits, deadlines of at least 10 s, no fixed sleeps). Existing snapshots that change are regenerated the CI way (`web/e2e/README.md`) and each is listed in the PR.
6. **A decisions file** `docs/decisions/2026-11-21-X-page-frame.md`, heading with `(#<this PR>)`: the frame's widths and gutters with their tokens, the measurements, and any UX call you made (a runner call under §2.26; name it).

## Owns (refined at launch)
- `web/src/lib/ui/layout/**`
- `web/src/lib/features/settings/shell/**`
- `web/src/routes/**/+page.svelte` (header and outer wrapper lines only)
- the en and zh message files for the new `h1`s (tell me which)
- a new `web/e2e/page-frame.test.ts`, and the existing snapshots the frame changes
- `docs/decisions/*-X-page-frame.md`; this slice's `runner/slices.yaml` entry only

Not yours: `web/src/lib/features/filters/**` (X-safety-undo-danger is running there). If the Filters destination needs a change inside `features/filters/`, stop and tell me; its route file's header lines are fine. Send me the exact file list before you edit, and before any size-limit change (with measured numbers; a raise is the measured size rounded up to 0.01 kB, after the code is as lazy as it can be).

## Bars
`runner/standards.md`, CLAUDE.md's web checks (`npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`, `npx playwright test --trace off` for the changed specs, `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium`; never `playwright install`). Phosphor icons are `-bold` (`-fill` for an on state). Push once per round, and send "ready <pr> <sha>" when CI is green. Follow `.claude/skills/steward/SKILL.md`; the runner merges.
