# X-detail-depth-compare (runner brief, 2026-11-23Z)

Web slice, Sonnet, reviewed on Opus before merge. X-detail-depth's PR 2 (PR 1 was #1081). Placed by `docs/decisions/2026-11-23-runner-records-r14.md`; its slices.yaml entry is `X-detail-depth-compare`.

## Goal
Compare gets a **"Differences only"** toggle (P11 in the 2026-11-13 UI review, 02 §4e; architecture §10.6): when on, rows where every compared torrent has the same value are hidden, so the user sees what sets them apart.
- The toggle is a labelled switch or checkbox in Compare's toolbar, keyboard operable, with its state announced. Off by default; remembered for the session only if §10.6 or the glossary says so, otherwise not remembered.
- When it hides rows, say how many ("12 rows hidden because they're the same"), worded per `docs/ui-glossary.md`, en and zh. When every row is the same, show a plain empty state, not a blank table.
- Equality uses the values the table shows (formatted), not raw bytes, unless §10.6 says otherwise. Null and "—" count as values (two nulls are the same; null against a value differs).
- Layout shift 0 when toggling at 390 and 1440 px is not required (it's a user action), but focus stays on the toggle and the table doesn't jump scroll position.
- Read `docs/decisions/2026-11-22-X-detail-depth.md` first (its Size section explains Compare's figure).

## Step 0 (send me, then wait for "go")
1. What the user sees now and after, at 390 and 1440 px, and the spec lines you follow (file:line).
2. Owns, to refine: `web/src/lib/features/compare/**`, the compare e2e spec(s), en and zh compare messages, `web/.size-limit.mjs` (the Compare entry only), `docs/decisions/2026-11-23-X-detail-depth-compare.md`.
3. Size: measure Compare with `npm run size:ci` on main (31.14 kB limit). The runner allows a raise to the measured size rounded up to 0.01 kB for this feature, with the measured numbers sent at step 0 and again at ready.
4. An estimate of the changed lines.

## Rules
- `web/.size-limit.mjs` is handed to you by the runner now. You edit only the Compare entry. When you merge, it goes to X-size-own-modules (API lane).
- Never beside GS-4 (it waits on you).
- Tokens only, no hex, no arbitrary values; spacing from the §10.2 ramp; status is icon plus text; focus visible; 44 px targets under `pointer: coarse`.
- Never write the bare word "divider" in a source file; format only with `npm run format` (runner/standards.md §4).
- No fixed sleeps; condition waits of at least 10 s.

## Branch and PR
- Branch `slice/X-detail-depth-compare`. PR title "X-detail-depth-compare: Compare shows Differences only".
- Gates per CLAUDE.md for `web/`, plus screenshots before and after at 390 and 1440 px in the PR description. Push once per round, then send "ready <pr> <full sha>". Follow `.claude/skills/steward/SKILL.md`; never merge.
