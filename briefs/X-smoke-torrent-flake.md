# X-smoke-torrent-flake (runner brief, 2026-11-28)

**Slice:** X-smoke-torrent-flake. Opus (flake root cause).
**Branch:** `slice/X-smoke-torrent-flake` from current `main`.
**PR title (exact):** `X-smoke-torrent-flake: The smoke snapshots wait for the page to settle before axe and the capture`
**Owns:** `web/e2e/smoke.test.ts`, `web/e2e/support/routes.ts`, `web/e2e/snapshot.ts` if needed,
`web/e2e/shell.routes.test.ts` (item 4 only), `docs/decisions/*-X-smoke-torrent-flake.md`.

## Source
`[bp1366-riverstone-light-comfortable] e2e/smoke.test.ts > smoke torrent` failed on #1177: the /t
shot was 2% (30,759 px) off its baseline, after "captured a stable screenshot". The same job
passed on #1176 against the same `main`, so it's nondeterministic, not a stale baseline.

Read-only finding by X-drawer-detach-flake:
- In `smoke.test.ts:14-25`, the torrent route (`support/routes.ts:166-170`) has no `heading`.
  After `go()` the test checks only the theme attributes, then runs axe and
  `expectPageSnapshot`.
- On /t, `DetailView` (mode 'page') loads Contents, Parsed, Swarm, Technical and Related at
  once. Each shows a Skeleton (`aria-busy="true"`) until its chunk, and for some its data,
  lands.
- With animations off, a still skeleton passes the two-identical-captures check.

## Do
1. **Prove the race first.** Force a delay (on the torrent GET, /siblings or a section chunk)
   in a temporary diagnostic, the way #1177 did. The current test must fail every time and
   the fixed one must pass. Don't commit the diagnostic. Report the before and after in the
   PR body.
2. **Fix, test-only.** Add an optional `ready?: (page) => Promise<void>` to `RouteCase`, and
   call it in smoke.test.ts before axe and the snapshot. For the torrent route, wait for
   `detail-view` to be visible and for no `[aria-busy="true"]` inside it, with a timeout of at
   least 10 s, plus a section that only renders when settled.
   - First check that no settled section keeps `aria-busy="true"` (there are about 6 uses in
     features/detail).
   - Check the other smoke routes for the same gap, and give each a `ready` where its page
     loads lazily.
3. **Baselines** should be unchanged, because they were made from settled pages. If any
   baseline must change, stop and tell the runner why before updating it.
4. **Hygiene.** `web/e2e/shell.routes.test.ts:249` and `:584` call `events.drop()` without
   `await`. Add the `await` (line 236 already has it).
5. No fixed sleeps, no skips, and no weakened assertions or thresholds.
6. Decisions file, the CLAUDE.md pre-push checks for `web/`, and the smoke and shell specs run
   locally. Follow `.claude/skills/steward/SKILL.md` and report `ready <PR> <sha>` to the runner.
