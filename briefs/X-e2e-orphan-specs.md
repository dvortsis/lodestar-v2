# X-e2e-orphan-specs: real-server Playwright specs that match no CI project

Runner brief, 2026-11-13. Kind: helper. Model: Sonnet. Branch `slice/X-e2e-orphan-specs`. PR title `X-e2e-orphan-specs: run orphaned real-server Playwright specs in CI`.

## Goal
`runner/slices.yaml` on main describes the slice (Track Q TEST-01): three real-server Playwright specs match no CI project, so they never run. Put each one in a project that runs in CI, either the nightly fixture job or a required one, or remove it if it's dead.

## Owns
- `web/playwright.config.ts`
- `.github/workflows/e2e.yml`
- `web/e2e/*fixture*.test.ts`
- `web/e2e/README.md`
- `docs/decisions/*-X-e2e-orphan-specs.md`

Anything else needs the runner's OK first.

## Steps
1. On fresh main, list every `web/e2e/**/*.test.ts` and every `testMatch`/`testIgnore` in `web/playwright.config.ts`. Find each spec that no project matches, and say which CI job, if any, runs each project.
2. For each orphan, choose one:
   - **Wire it in:** add it to a project and a CI job, preferring the job that already runs the real server or the fixture. It must pass on CI. If it fails because the product is broken, stop and tell the runner; don't change product code.
   - **Remove it:** only when it's dead (it duplicates another spec, or tests a removed feature). Give the reason per file in the decisions file.
3. Add a guard that fails when a spec matches no project, so new orphans can't appear. A small check in the config, or a vitest/node test over the file list, is enough. Test the guard against a near miss.
4. Update `web/e2e/README.md` with which project runs which specs.

## Rules
- Chromium is at `/opt/pw-browsers`; set `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium`. Never run `playwright install`.
- No fixed sleeps or `waitForTimeout`. Waits are conditions with deadlines of at least 10 s.
- Never skip, disable or quarantine a test to get green.
- `e2e.yml` is shared with X-ui-guards, which isn't running. Whichever lands second brings main in.
- Before pushing, in `web/`: `npm run check`, `npx vitest run`, `npm run build`, and `npx playwright test` for the projects you touched.
- Disk on this host is limited. Don't build Rust unless a real-server project needs the binary, and then only once.
- Follow `.claude/skills/steward/SKILL.md`. Send the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) `ready <PR> <full sha>` once CI is green. The runner merges; you never do.
- No cloud or AWS calls. No model identifiers in commits, PRs or code.
