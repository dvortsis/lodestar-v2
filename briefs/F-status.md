# F-status: an honest status pill (v0.5.1 release blocker)
**Model:** Sonnet (Svelte UI, following the existing pattern). **No Rust and no API changes.** If you need either, stop and tell the runner.

## Base and PR timing
- Branch `slice/F-status` from `origin/land/L6`. L6 holds the owner's shared status presentation (`web/src/lib/ui/status.ts`) and lands after L3–L5.
- Your PR goes to `main` after L6 merges; the runner tells you when. Until then: build, test, self-review, push, and message the runner "ready, waiting for L6". Then merge `origin/main` once and open the PR.
- `main` was rewritten on 2026-10-30. Never use an older clone.

## Why
The v0.5.0 soak showed the crawler can starve while every health signal says "healthy" (S05 in `docs/soak/2026-10-24-v0.5.0-metadata-throughput.md`). An Unraid user then sees green while nothing new arrives. The master plan (build-plan §0.4, F-status) requires progress-aware status before v0.5.1. The Docker `HEALTHCHECK` stays a liveness check; don't touch packaging.

## Do
1. **Status pill** (`web/src/lib/shell/StatusPill.svelte`, `status.svelte.ts`, `status.ts`):
   - Show progress, not just liveness, using the **existing API fields only**: `fetched_per_hour` from the ops DTO (`crates/api/src/dto/ops.rs`), `ingest_per_hour` from Insights (`crates/api/src/dto/insights.rs`), and the existing stage states `writer_stalled`, `cohort_stalled` and `index_lag` (dto/insights.rs ~175, 331, 341).
   - Rates come from the API. **The client never computes rates** (`runner/standards.md` §4).
   - Add a **stalled-pipeline warning state**: the pill uses the warning tone with a short label, and a tooltip or popover explains it and links to Insights.
   - Apply it when the API reports a stall reason, or when the stored rate is 0 for the window the API reports, while the crawler is running and not paused and outside the documented 6 h startup sampling suppression.
   - Read how the suppression is exposed. If nothing in the API tells you, show "Starting: sampling resumes after up to 6 h" while uptime is under 6 h. Uptime must come from the API.
   - Use `web/src/lib/ui/status.ts` (shared icon, ink and border maps).
2. **Insights overview:** show the same stalled warning at the top, using the existing notice component pattern. Keep it small.
3. **uTP setting:** wherever `transport.utp` is shown to the user (the Settings registry or form labels in `web/`), label it **"not active yet"**. uTP has no runtime effect until CR-4 (S04).
   - If the label or help text comes from the Rust settings schema, don't edit Rust. Note it in the PR as a shared-file change the runner must make.
4. **Strings:** add every new string to the feature's `messages/en.json` and a `zh.json` entry, or use the documented fallback.
5. **UI rules** (CLAUDE.md):
   - tokens only, no hex values or arbitrary values;
   - the §10.2 spacing ramp;
   - accessible: the state is announced and not conveyed by colour alone.
6. **Tests:**
   - vitest for each pill state: healthy, starting/suppressed, stalled, paused, offline;
   - a Playwright spec using the existing API mock fixtures for the stalled state at 390 and 1366 px;
   - no fixed sleeps: `expect.poll` or `expect` with a deadline of at least 10 s.

## Own
`web/src/lib/shell/**`, `web/src/lib/ui/status.ts`, `web/src/lib/features/insights/overview/**`, `web/src/lib/features/insights/messages/**`, the settings messages (`web/src/lib/features/settings/messages/**`) and any settings registry label file in `web/` for `transport.utp`, and `web/e2e/**` (new spec only).

## Done when
In `web/`, these pass: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit` (never raise a limit) and `npx playwright test` for your spec (Chromium is at /opt/pw-browsers; never run `playwright install`). The fresh-reviewer self-review (steward §1) is done.

## Standing instructions
- Read CLAUDE.md, `runner/standards.md` and `.claude/skills/steward/SKILL.md` first, and follow steward from your first push.
- Work only on `slice/F-status` and edit only the paths you own. Name any shared-file change you need in the PR description.
- The PR title is `F-status: <title>`. The description covers what changed, the checks, the self-review, any shared-file changes and any deviations.
- Message the runner (`send_message` to session_01EYhgh4JX3AmHzKXPjYCoUg) when ready, after fix rounds, and when blocked.
- **Do not merge.** The runner merges.
- No model identifiers in commits, PRs or code.
