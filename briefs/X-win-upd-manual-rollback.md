# Brief: X-win-upd-manual-rollback (Opus; owner's extra-session window)

**Runner:** session_01EYhgh4JX3AmHzKXPjYCoUg. **Branch:** `slice/X-win-upd-manual-rollback` from `origin/main`. **PR title:** `X-win-upd-manual-rollback: Windows manual Roll back (not_built_yet today)`.

**Why now:** the owner opened up to 10 extra sessions until **2026-11-12T23:30Z** (2026-11-12, §2.63). Your slice is its `runner/slices.yaml` entry: read it first (`grep -n "id: X-win-upd-manual-rollback," runner/slices.yaml`).

## Hard time box
- Check the time with `date -u` as you go.
- **By 23:20Z:** push what you have and open the PR. If it isn't finished, open it as a **draft** with a checklist of what's left.
- **By 23:25Z:** message the runner (send_message to session_01EYhgh4JX3AmHzKXPjYCoUg) one of:
  - "ready <pr> <full sha>" if CI is green;
  - "pushed <pr> <full sha>" with CI still running;
  - "draft <pr> <sha>: <what's left>".
- **Then stop work.** The runner resumes the slice later in a normal slot. Don't start anything after 23:25Z.
- Prefer a small, correct, tested PR over a big unfinished one. If the slice is bigger than fits, do the smallest self-contained part well, and say what's left.

## Scope
- **Owned paths:** "crates/lodestar/src/update/windows/rollback.rs", plus `docs/decisions/2026-11-12-X-win-upd-manual-rollback.md` for your calls. Ask the runner before any other file.
- **Notes from the plan:** /security-review; found by PK-guide-3
- Windows-only code: build and test what you can locally (`cargo check --target` is not available, so rely on `cfg` unit tests that run on Linux where the code allows it), and let CI's Windows job prove the rest.
- Read the architecture section the entry points to, `runner/standards.md` and CLAUDE.md. Run `/security-review` on your diff before the PR; untrusted input stays bounded, with no panics.
- Tests ship with the code. Parsers get property tests or fuzz targets as CLAUDE.md says. No fixed sleeps: condition waits with ≥ 10 s deadlines.

## Checks before the push
The CLAUDE.md "Before pushing" list for what you touched (Rust: fmt, clippy `-D warnings`, the affected crates' tests; web: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`).

## Done
Follow `.claude/skills/steward/SKILL.md`. Push once per round. Never merge, never push to main, never force-push.
