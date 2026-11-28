# X-all-themes-settle (runner brief, 2026-11-28)

**Slice:** X-all-themes-settle. Opus (flake prevention). Same session as X-smoke-torrent-flake.
**Branch:** `slice/X-all-themes-settle` from current `main`.
**PR title (exact):** `X-all-themes-settle: The nightly all-themes shots share the smoke test's settle wait`
**Owns:** `web/e2e/all-themes.test.ts`, `web/e2e/smoke.test.ts`, `web/e2e/support/routes.ts`, a new
`web/e2e/support/settle.ts`, and `docs/decisions/*-X-all-themes-settle.md`.

## Source
#1180 (merged) made smoke.test.ts wait for a settled page (no `[aria-busy="true"]`, no skeleton,
the route's `ready`) before axe and the shot. The nightly `all-themes.test.ts` takes the same
key-screen shots, torrent included, without that wait.

## Do
1. Move the wait into a shared `settle(page, route)` helper (heading, `ready`, then no
   placeholders, with a deadline of at least 10 s), and call it from both specs.
2. Prove it with a forced delay on all-themes for the torrent route, as #1180 did. Don't commit
   the diagnostic.
3. Leave the baselines unchanged. If one must change, ask the runner first.
4. The all-themes project is nightly and skipped on PRs. Run it locally for at least the
   torrent route across a few themes, and say in the PR body what you ran.
5. No fixed sleeps, no skips, and no weakened thresholds. Write a decisions file, run the
   CLAUDE.md pre-push checks for `web/`, follow `.claude/skills/steward/SKILL.md`, and report
   `ready <PR> <sha>`.
