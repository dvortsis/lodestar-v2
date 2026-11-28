# X-facet-exclude-fill, finished in-house (Sonnet; runner brief 2026-11-28)

**Branch:** `slice/X-facet-exclude-fill-r2`, created from #1145's head (`ee034ab5fc1e35d04c00ab79d2ffb8ba61683590`, branch `slice/X-facet-exclude-fill`).
**PR title:** exactly `X-facet-exclude-fill: Facet exclude toggle shows a filled icon when it's on`. The dashboard reads the id from the title.

## Why
The API lane built #1145, and its pre-review said MERGE at `ee034ab5`. Then the lane's credit ran out (round 2 ended, 2026-11-28). One change is left: the runner's grant of 2026-11-27 (§2.26).
- In `web/.size-limit.mjs`, raise only the **"Facet rail (lazy)"** entry, to the measured size rounded up to the next 64 B.
- Add a one-line comment saying why: the filled exclude icon.

No lane session owns `slice/X-facet-exclude-fill` any more. Even so, never push to it: work on your own branch.

## Task
1. `git fetch origin slice/X-facet-exclude-fill main`, then branch `slice/X-facet-exclude-fill-r2` from `ee034ab5`.
2. Bring `origin/main` in with a merge, never a rebase. Resolve any conflicts with the repo's tooling. `web/.size-limit.mjs` may conflict with other merged size changes: keep main's other entries and change only the Facet rail (lazy) line.
3. In `web/`, run `npm ci` if needed, then `npm run build`, `npx size-limit` and the JSON output, to measure the Facet rail (lazy) entry. Set its limit to that size rounded up to 64 B, with the comment. Don't change any other entry.
4. Run the CLAUDE.md web checks: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`, and `npx playwright test` for any spec the PR touches (`PW_CHROMIUM_PATH=/opt/pw-browsers/chromium`; never `playwright install`).
5. Open the PR with the exact title above. In the body, link #1145 and say the PR carries its commits plus the size-limit line.

## Owns
- #1145's files, unchanged.
- The one `web/.size-limit.mjs` line and its comment.
- A line in #1145's decisions file noting the size grant, if one exists. If not, add `docs/decisions/2026-11-28-X-facet-exclude-fill.md` with one paragraph.

Ask the runner before touching anything else.

## Rules
- No model identifiers in commits, PRs or code. UTC `Z` times only. Never merge.
- No cloud calls, and no AWS tools even when announced.

## Done
- CI is green.
- Self-review per `.claude/skills/steward/SKILL.md`.
- Send the runner `ready <full sha>` with the claude-code-remote send_message tool (session_id session_01EYhgh4JX3AmHzKXPjYCoUg). The runner then closes #1145.
