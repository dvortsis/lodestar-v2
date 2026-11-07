# P-v052-cut: cut release/v0.5.2 from main, map the delta, build the rc
**Model:** Opus, with `/security-review` on any code it touches. A fresh Opus reviewer reads each PR.

Runner brief, 2026-11-07T14:40Z. Slot 1. Track R step R5 (`docs/build-plan.md` §0.2 rung 1, §0.4 "v0.5.2").

## Read first
- `docs/plan/2026-11-06-v052-feature-manifest.md` (the freeze list) and `docs/decisions/2026-11-06-P-v052-manifest.md`.
- `docs/build-plan.md` §0.4 (v0.5.2 paragraph) and Track R.
- `docs/releases/v0.5.2.md` (draft, D-v052-notes) and `.claude/skills/user-docs/SKILL.md` (material-composer, 450-word cap).
- How v0.5.1 was cut and gated: `docs/decisions/2026-11-07-v051-release-verdict.md` and the rc delta reports under `docs/releases/`.

## Runner decisions already made (don't re-ask)
- **Cut point (manifest Q1):** `main` at **`045d4a385ef540e3b95ab7197075b8463a4444bf`** (#580 WK-9c merge, 2026-11-07). Later merges reach v0.5.2 only as reviewed backports. If your delta report finds something at or before that SHA that must not ship and can't be shown inactive, stop and tell the runner before cutting; the runner may pick an earlier SHA.
- **Q2:** close G1–G4 (manifest §3.3) in this slice, on `main`, before the cut is final, and backport the tests to the release branch.
- **Q4, Q5:** as the manifest records (torrent cache stays off; webhooks advanced, API only; soak plan per §6).
- **Collector and `409 notice_required` (Q5):** the collector fails loudly on a 409 instead of recording gaps. `docs/soak/tools/linux/collect.py` is yours for that change only; X-soak-screen-polish (not running) also owns it, so keep the hunk small.
- Crawler switches stay off. No new migration: confirm no V15 landed in `63a2c6df..045d4a38`.

## Do, in order (split into PRs; each ≲ 1,500 lines)
1. **PR A on `main`** (`P-v052-cut: Delta report and default-off tests for the v0.5.2 cut`, branch `slice/P-v052-cut`):
   - **Delta report:** classify every first-parent merge in `63a2c6df..045d4a38` (about 66) into the manifest's four classes, as an addendum section in the manifest (decision D2). Anything default-on, a migration, a format bump or a delta-rule re-soak trigger gets its own row with acceptance. Pay attention to: WK-0..WK-9c (remote workers: show every path is inactive or unreachable with no workers configured), CMP-0 (companions), SRC-15*, ENR-9*, LEGAL-2/3a, X-refetch-private (#576, a privacy fix: enabled), X-fetch-pool-bytes (#554, fetch path: re-soak trigger?), WK-1/WK-9 Insights rollup rows, GS-gate.
   - **G1–G4 tests** (manifest §3.3), each failing without the property it guards. G4 includes the `build-msi.ps1` example fix and a tarball/Windows fixture check in `release.yml` if that's the right home; ask the runner before touching `release.yml`.
   - **Collector 409** loud failure, with a test in `docs/soak/tools/linux/test_collect.py`.
   - Decisions file `docs/decisions/2026-11-07-P-v052-cut.md`: cut point, classification summary, what re-soaks the delta rule requires, anything you decided.
2. **After PR A merges and the runner replies "cut approved":** create `release/v0.5.2` at `045d4a38` (`git push origin 045d4a385ef540e3b95ab7197075b8463a4444bf:refs/heads/release/v0.5.2`). If the push is refused, stop and tell the runner; don't route around it.
3. **PR B into `release/v0.5.2`** (`P-v052-cut: v0.5.2-rc.1`, branch `release/v052-rc1`):
   - backport PR A's tests (and any code fix they force);
   - version `0.5.2-rc.1` wherever v0.5.1's rc PRs set it;
   - `docs/releases/v0.5.2.md`: remove every `pending` marker against the build, apply manifest §7's corrections, trim to ≤ 450 words, check every claim against the release branch;
   - the "in-app updates under the supervisor" claim: verify the release image runs under its supervisor (that's the pending item on line 30), or drop the claim;
   - an rc delta report like v0.5.1's.
4. **Dispatch** every workflow on the release branch head (CI, E2E, Release dry run, as v0.5.1's rcs did) and report the results. No regenerated baselines.
5. **Owner steps** in your message to the runner (the runner relays them to the owner; don't commit a steps file): the tag command, the V12 → V14 upgrade smoke and rollback check (manifest §5.2, §5.3, Docker and Windows), the 24 h Docker soak on an upgraded folder with every pack on, the ≥ 6 h Windows run on a fresh folder, each clock starting after setup and the notice. Plain, numbered, for the owner.

## Owns
- `docs/plan/2026-11-06-v052-feature-manifest.md` (addendum), `docs/decisions/2026-11-07-P-v052-cut.md`, `docs/releases/v0.5.2*.md`.
- New tests for G1–G3 (`crates/lodestar/tests/webhooks_e2e.rs` assert, a pipeline config test); `packaging/windows/build-msi.ps1` (example line).
- `docs/soak/tools/linux/collect.py` and `test_collect.py`: the 409 change only.
- Everything on `release/v0.5.2` through PR B.
- **Ask the runner first** for anything else, naming the file. **Don't touch:** `crates/sources/src/pool.rs`, `crates/peer-wire/**` (X-fetch-meta-reserve, #579), `.github/workflows/ci.yml` size-budget job and `bench/src/bin/size-report.rs` (X-gs-gate-polish, #581), `crates/pipeline/src/marginal.rs`, `crates/api/src/insights_workers.rs` (WK-15).

## Done when
- PR A merged on `main`; `release/v0.5.2` exists at the cut SHA; PR B green with all dispatched workflows, a fresh Opus review, merged by the runner.
- The runner has the rc.1 head SHA, the workflow results and the owner steps. The owner tags `v0.5.2-rc.1`; you never tag.
- No fixed sleeps; waits on conditions with deadlines ≥ 10 s. No tokio `test-util` or `testing` features on the `lodestar` package.
- Checks: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (per-package runs are fine on limited disk; say so), and the soak-tools Python tests.
- Process: follow `.claude/skills/steward/SKILL.md`, push once per round, never merge, never push to `main`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with each PR number and head SHA once CI is green, and before step 2.
