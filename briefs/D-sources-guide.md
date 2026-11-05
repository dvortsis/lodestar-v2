# D-sources-guide: the user guide for Settings → Sources
**Model:** Sonnet (docs, following the `user-docs` skill). A fresh Opus reviewer follows. Runner brief, 2026-11-05T06:20Z.

## Why
SRC-15a (#455) shipped the Settings → Sources screen: grouped rows, status pills, on/off toggles, priority and the yield bar. Its review noted that the user guide has no page for it.

## Read first
- `.claude/skills/user-docs/SKILL.md`, then the owner's `github-docs` skill it names.
- `docs/user/05-use-lodestar.md` and `docs/user/appendix-b-settings-reference.md`.
- The screen's code: `web/src/lib/features/sources/**`.
- The consent rules in `docs/decisions/2026-11-0*-SRC-4*.md`.
- The sources plan `docs/plan/2026-11-01-v06-sources.md` (§18.8), for what each status means.

## Do (branch `slice/D-sources-guide` from `origin/main`)
1. **Add a task section to `docs/user/05-use-lodestar.md`: "Choose where Lodestar finds torrents (Settings → Sources)".** Cover:
   - what a source is;
   - the groups, and each status pill's meaning;
   - turning a source on or off, and what "needs setup" means;
   - priority, and what the yield bar shows;
   - the safe default: leave external and account sources off unless you need them;
   - that changes apply at once, with no Undo, unlike other settings (the runner's §2.19 refinement for SRC-15a).

   Write for the guide's reader, as the skill says: define terms and recommend the safe choice.
2. **Add the Sources rows to appendix B.**
3. **Screenshots:** if the docs shot harness (`web/e2e/docs/shots.docs.ts`) can capture the Sources screen, add its shots per the skill and the shot list. If it can't, say so in the PR and add a `SHOTLIST.md` entry instead.
4. **Housekeeping:** update `docs/user/index.md` (the task finder) and `docs/user/revisions.md`, and run the docs gates: `lint_docs.py --strict` and `docs_tool.py sync --check` (find them as LEGAL-1r's delta report names them).
5. **Write only what the screen does today.** Consent dialogs, credentials and purge come with SRC-15b. Say they're coming; don't describe them.

## Own
- the new section in `docs/user/05-use-lodestar.md`;
- the Sources rows in `docs/user/appendix-b-settings-reference.md`;
- `docs/user/index.md` and `docs/user/revisions.md` entries (LEGAL-1 also edits `revisions.md`, so whichever PR merges later brings `main` in);
- the shot list and any new images.

**Don't touch:** `docs/user/02-*`, which LEGAL-1 owns.

## Done when
The docs gates pass, and `npx playwright test` passes for any docs shot spec you touch (`PW_CHROMIUM_PATH=/opt/pw-browsers/chromium-1243/chrome-linux64/chrome`). Open the PR against `main` titled "D-sources-guide: user guide for Settings → Sources". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the Claude Code Remote MCP `send_message` tool. Never merge. Push once per round.
