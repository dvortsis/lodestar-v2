# Brief: X-banner-live-region (Sonnet; owner's extra-session window)

**Runner:** session_01EYhgh4JX3AmHzKXPjYCoUg. **Branch:** `slice/X-banner-live-region` from `origin/main`. **PR title:** `X-banner-live-region: Banner: no nested live regions (StorageStep wraps a role=status Banner)`.

**Why now:** the owner opened up to 10 extra sessions until **2026-11-12T23:30Z** (2026-11-12, §2.63). Your slice is its `runner/slices.yaml` entry: read it first (`grep -n "id: X-banner-live-region," runner/slices.yaml`).

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
- **Owned paths:** "web/src/lib/ui/feedback/Banner.svelte", "web/src/lib/ui/feedback/feedback.test.ts", plus `docs/decisions/2026-11-12-X-banner-live-region.md` for your calls. Ask the runner before any other file.
- **Notes from the plan:** from #726: give Banner a prop to drop its role, or a live opt-in; audit other callers
- `web/` size budgets are tight: the Wizard steps chunk has about 10 B of headroom. Run `npx size-limit` and keep every budget green. Don't raise a budget.
- Read the architecture section the entry points to, `runner/standards.md` and CLAUDE.md. 
- Tests ship with the code. Parsers get property tests or fuzz targets as CLAUDE.md says. No fixed sleeps: condition waits with ≥ 10 s deadlines.

## Checks before the push
The CLAUDE.md "Before pushing" list for what you touched (Rust: fmt, clippy `-D warnings`, the affected crates' tests; web: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`).

## Done
Follow `.claude/skills/steward/SKILL.md`. Push once per round. Never merge, never push to main, never force-push.
