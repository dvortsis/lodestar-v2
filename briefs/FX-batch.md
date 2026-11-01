# FX-n: an M5 design-review fix batch (G-m5b)
Runner brief, 2026-11-01. One brief for every batch; the launch message names yours (FX-1 … FX-6). It clears findings from the M5 `design-review` pass, the second half of the M5 exit gate.

## Read first
- **The report:** `docs/review/2026-11-01-m5-design-review.md` (from #340; if #340 hasn't merged yet, read it with `git show origin/slice/G-m5b:docs/review/2026-11-01-m5-design-review.md`). Read:
  - your batch's row in "Fix batches": its findings, its owned files, its model and its size;
  - every finding row it lists (the Fix column is the spec);
  - "Removing the per-feature coarse-pointer copies (B5)".
- CLAUDE.md "UI rules", `docs/architecture.md` §10 (the §s each finding cites), `docs/design-tokens.md`, `docs/ui-glossary.md`.

## Do
1. **Fix each finding in your batch** as its row's Fix column says. If a fix turns out wrong or impossible as written, do the closest correct thing and say why in your decisions file. **Stay inside your batch's owned files.** Another batch runs at the same time on the others. If you need a file you don't own, tell the runner first.
2. **Coarse-pointer copies:** if your batch is listed as deleting per-feature `pointer: coarse` copies, delete them only once FX-2 (the global rule) has merged on `main`. Otherwise leave them and say so in the PR; the runner hands off the clean-up.
3. **Strings:** only FX-4 edits `web/messages/*.json`. Other batches reuse existing keys or add strings to their feature's own `messages/{en,zh}.json`, the repo's normal pattern, which isn't `web/messages/`. en **and** zh for anything new.
4. **Baselines:** if your fix changes screenshots on purpose, use the `web/e2e/.update-snapshots` marker. Check that only your surfaces' images changed (`git diff --diff-filter=M origin/main...HEAD -- '*.png'`), then remove the marker in the next push.
5. **Sizes:** never raise a limit. Report before → after for every entry that moves. If one would cross its limit, stop and tell the runner.
6. **Decisions file:** `docs/decisions/<date>-FX-n.md` (short): each finding ID, what changed, and any deviation from the report's Fix.

## Done when
These pass in `web/`:
- `npm run check`
- `npm run e2e:typecheck`
- `npx vitest run`
- `npm run build`, then `npx size-limit` (twice; identical)
- `npx playwright test` for the specs your batch row names, plus core

For FX-6's Opus part (I9, the query printer): `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, plus a round-trip property test.

Chromium is at /opt/pw-browsers; never run `playwright install`. If the pinned headless shell is missing, point `PLAYWRIGHT_BROWSERS_PATH` at a scratch folder of symlinks to the installed one. Don't background long jobs with `&`. Check each commit's file list (`git show --stat`) for stray formatter passes before pushing.

Open the PR titled "FX-n: …", follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head, the findings fixed and the size table. Use the `send_message` tool of the Claude Code Remote MCP server. Never merge. Work frugally. Push once per round of fixes.
