# X-size-limit-one-build (runner brief, 2026-11-22Z)

Kind: helper (web tooling). No deps. Recorded in `docs/decisions/2026-11-22-runner-records-r12.md` (PR #1068).

## The gap
- CI gates on `npm run size:ci`, a build with stable chunk names.
- CLAUDE.md's pre-push list says `npx size-limit`. After a plain `npm run build`, that measures a slightly different build: on #1065, the Search route was +7 B, Facet rail +6 B and Insights stage pages +17 B. So a contributor can see red locally when CI is green, or green when CI is red.

## What to do
1. At step 0, explain why the two builds differ: read `web/package.json` scripts, `web/.size-limit.mjs` and the vite/kit config the size:ci build uses.
2. Make a plain `npx size-limit` measure the same build as `size:ci`, for example by having size-limit's config run or require that build. If that's not clean, make the local docs name `size:ci` as the gate instead.
3. Don't change any limit.
4. Step 0: the cause, the option, files and an estimate. Then wait for "go".

## Rules
- CLAUDE.md is not in your owns. If it should change, put the exact wording in your PR description and I'll take it.
- No limit changes. No changes to what CI runs, unless I approve them at step 0.

## Owns
- `web/package.json` (scripts only)
- `web/.size-limit.mjs`
- `web/e2e/README.md`
- `docs/decisions/2026-11-22-X-size-limit-one-build.md`

## Branch and PR
- Branch `slice/X-size-limit-one-build`. PR title "X-size-limit-one-build: Local size-limit measures the build CI gates on".
- Gates: `npm run check`, `npm run build`, `npx size-limit`, `npm run size:ci`.
- Push once, then send "ready <pr> <full sha>". Follow `.claude/skills/steward/SKILL.md`; never merge.
