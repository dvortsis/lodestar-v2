# R-rc3-backport: port #357 onto release/v0.5.1 for rc.3
**Model:** Opus (filter loading is content safety). `/security-review` and a fresh Opus reviewer follow. Runner brief, 2026-11-02. Track R step R3 in `docs/build-plan.md`; the owner's decision is `docs/owner-decisions.md` §2.29.

## Why
The owner chose to ship F-filter-load-fail-closed (#357, merge commit `736a3028` on `main`) in v0.5.1. rc.3 = rc.2 + #357, **exactly** (§2.29), with no other post-cut change. A trial `git cherry-pick -m 1 736a3028` onto `origin/release/v0.5.1` (`6be95525`, rc.2) conflicts in five files:
- `crates/api/src/filters.rs`;
- `docs/metrics.md`;
- `web/.size-limit.mjs`;
- `web/src/lib/features/insights/messages/{en,zh}.json`.

`main` moved on after the cut (re-queue work, held-set chunking, X-requeue-2 and others), and #357 was written against that.

## Read first
- `docs/owner-decisions.md` §2.29; `docs/build-plan.md` Track R (R3) and "The stand-in gap in rc.3"; §0.4 (the delta rule).
- `docs/decisions/2026-11-01-F-filter-load-fail-closed.md` and #357's diff (`git show -m --first-parent 736a3028`).
- `docs/architecture.md` §20 (filters) as it stands on the release branch.

## Do
1. **Branch** `release/rc3-357` from `origin/release/v0.5.1`, then cherry-pick `-m 1 736a3028`.
2. **Resolve each conflict to #357's behaviour on the release branch's code.**
   - Never pull in a post-cut feature to make it fit: no pending re-queue machinery, no held-set chunking, no new store APIs.
   - Where #357 calls a helper that only exists on `main` (e.g. `push_pending_requeue`, `requeue_text`), write the smallest equivalent on the release code, or drop the part that only exists because of that feature. Say which in the decisions file.
   - The fail-closed semantics must be identical: unreadable stored rule, `unless` and classification rows hold through stand-ins, they're reported in Insights and `/metrics`, and nothing loosens.
   - Keep #357's tests. Adapt only what the release code forces, and say why.
3. **Mechanical files:**
   - Regenerate `openapi.json` and `schema.d.ts` with the repo's tooling, never by hand.
   - `docs/metrics.md`: keep the release branch's text plus #357's rows.
   - `web/.size-limit.mjs`: keep the release branch's limits, change only what #357 changed, and report the measured sizes.
   - Insights messages: the release branch's keys plus #357's new key, in en and zh.
4. **The delta report** is `docs/releases/v0.5.1-rc3-delta.md` on this branch. It contains:
   - every changed file and line range from rc.2, each tied to a targeted test (filter load, stand-ins, the §20.8 filter budget, Insights, `/metrics`);
   - a statement that nothing touches the crawler lifecycle, the sampler or the endpoint policy, so the r3 soak evidence carries over (§0.4);
   - **the stand-in gap:** rc.3 holds what an unreadable rule's stand-in matches, and nothing in rc.3 undoes those holds or writes the `filter.stand_ins` record (#367 does that, on `main`);
   - the version bump to `0.5.1-rc.3` if the release branch carries rc versions in Cargo.toml and openapi `info.version` (check how rc.2 did it in #222).
5. **Decisions file:** `docs/decisions/2026-11-02-R-rc3-backport.md` (short). For every conflict, say how you resolved it and why it's equivalent.

## Own
The `release/rc3-357` branch, and only the files #357 touches, plus the delta report and decisions file. **Don't edit** `main` or any slice branch.

## Done when
On the branch:
- `cargo fmt --check`;
- `cargo clippy --workspace --all-targets -- -D warnings`;
- `cargo test --workspace`;
- in `web/`: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`.

Then run `/security-review` on the diff against `origin/release/v0.5.1`.

Open the PR **against `release/v0.5.1`** (not `main`), titled "R-rc3-backport: #357 onto release/v0.5.1 for rc.3". Add the `full-ci` label. Follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head, the conflicts resolved and the size table. Use the `send_message` tool of the Claude Code Remote MCP server. Never merge: the runner merges after the soak verdict (R1). Never tag. Work frugally. Push once per round of fixes. Don't background long jobs with `&`.
