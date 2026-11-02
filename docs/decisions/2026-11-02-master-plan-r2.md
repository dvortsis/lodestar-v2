# Master-plan update r2 decisions (2026-11-02)

The 2026-10-30 master plan (`docs/build-plan.md` §0, `docs/decisions/2026-10-30-master-plan.md`) finished its lanes by 11-01: L, F, S and D are done, the crawler candidates are built behind switches, IMP-0..5 are merged, M5 exited (#352) and v0.5.1 is at rc.2. This update replaces §0 with the plan from here to the v0.6 tag and beyond, adds `docs/handoff.md` so any agent can continue when the current sessions stop, and brings `runner/slices.yaml` up to date. Docs only.

## Owner answers in this session (2026-11-02)
- **The v0.6 crawler gate stays at ≥ 100 verified torrents/min, with a decision checkpoint.** Measured today: 12.3/min (CR-0 24 h final). The owner chose to keep the gate and add Checkpoint 1 over decoupling the tag or setting an interim bar. Checkpoint 1 comes after the CR-ADM and CR-4 live A/B results and the bitmagnet comparator. If the projection falls short, the runner reports the ceiling, the dominant limiter and the options before CR-5, and the owner re-decides. The runner never loosens the gate (build-plan §0.5).
- **The handoff serves both a restarted Claude runner and a non-Claude agent** (`docs/handoff.md` §4, §5).
- **Scope:** the build plan, the slice graph, the handoff and this file, in one docs PR.

## Runner refinements (flagged under `docs/owner-decisions.md` §2.19 and §2.26)
None of these reverses or loosens an owner decision, weakens a security or content-safety decision, or changes an owner-set number.
- **The crawler order follows the evidence.** Of CR-0's 389,644 fetch failures, 74 % timed out, 15 % were unreachable and 11 % refused, and the DHT actor refused 95.6M peer requests for capacity. That points at H3 (transport and reachability) and H2 (admission) before H1. So the live A/B order is CR-ADM, then CR-4 (uTP, then route backoff), then the H1 candidates (CR-1a-S, CR-1a-H, CR-1b, CR-1c). CR-ADM is also the only candidate CR-SIM rated Better.
- **The bitmagnet comparator runs early,** before tuning, on the same host and IP. It shows what 100/min means on that connection, so Checkpoint 1 compares against a measured reference rather than a guess. The 2026-10-30 rules for it are unchanged: its own day, fresh data, version pinned.
- **SB-6: one search schema bump for M7, before the 10M pilot.** The one-bump-per-milestone rule (build-plan §1.3) already sends SRC-3's fields to the M7 bump. SB-6 makes that bump a slice of its own that fixes every M7 field at once: `verified:`/`source:`/`network:`, `payload_sig` and grouping, the trigram field, file category and `pieces_root`. Landing it before the 10M import means its blue/green rebuild is measured at 10M and the 50M corpus is indexed once, instead of rebuilt at 50M with about 2× the index disk. SRC-3 now depends on SB-6, replacing `ext:m7-schema-bump`.
- **V14 goes to SRC-1 now.** P-v06-sources-plan already accepted this (Q12). The cut it waited for exists (`release/v0.5.1` at `c3f6341a`), so `ext:release-branch-cut` is satisfied, and SRC-1 waits only on the runner assigning the slot. Fixes for v0.5.1 are cherry-picked onto the release branch, so a new migration on `main` doesn't affect that release. CR-3 takes the next free number after the 50M gate. The migration queue is in build-plan §0.7.
- **X-search-files-3 lands before the 10M pilot.** `docs/decisions/2026-10-31-X-search-files-2.md` projects cold file-search p95 over 150 ms at 10M. Indexing `size` in the file index is the proposed fix. Measuring the pilot without it would only confirm the projection.
- **v0.5.2 is recommended soon after v0.5.1.** `main` carries fixes that v0.5.1 lacks (B-update-5..6, filter fail-closed, the M5 exit fixes) and is moving fast. A second cut from `main` is safer than long cherry-pick chains. The gate keeps the 2026-10-30 delta rule: a full 24 h Unraid soak for crawler lifecycle, sampler or endpoint-policy changes; otherwise a 6 h Windows service run and a short Unraid smoke. Crawler switches stay off unless they've passed acceptance.
- **The r3 soak's old stop rule.** A stop caused only by the working-set rule is checked against `anon` before it counts as a failure, per `docs/decisions/2026-10-31-X-soak-docker-memrule.md`. This tightens nothing and loosens nothing: the binding memory gate was already defined on `anon`.
- **Path B merges are the owner's.** An agent working without the runner opens PRs but doesn't merge. Branch protection already requires `ci-ok`; this keeps §2.22's "only the runner merges" intact when there is no runner.
- **No `nightly.yml`.** Build-plan §1.3 once planned it, but the scheduled jobs live inside `ci.yml` and `e2e.yml`. The plan now says so instead of carrying the file as a gap.
- **Epics held for the soak verdict get it as a dependency.** E-M6, E-v06-workers and E-v06-companions were "held" only in `state.json`. They now depend on `ext:v051-soak-verdict` in `runner/slices.yaml`, so the graph says what the state says.
- **New slices and externals in `runner/slices.yaml`.**
  - Track C: `CR-4` (recording #365), `CR-AB-COMP`, `CR-AB-ADM`, `CR-AB-4`, `CR-CP1`, `CR-AB-1`, `CR-1c`, `CR-5`, `CR-COMB`, `G-v06-crawler`.
  - Track I: `X-search-files-3`, `G-import-smoke`, `G-import-1M`, `G-import-10M`, `G-import-50M`, `CR-3`.
  - Track S: `P-v06-rulepacks`.
  - Track M7: `SB-6`.
  - Track R: `D-v051-main`, `P-v052-cut`.
  - Track B: `F-filter-standin-undo`, `X-strip-format-chars`, `openapi-example-size`, `i9-stage2`, `X-settings-overview`, `X-overflow-links`, `X-polish-carry-1102`.
  - Recorded as merged: `B-subscriptions-api`, `CR-1b`. `S-blocklists` now depends on `B-subscriptions-api` instead of the superseded `S-settings-1`, and the four superseded entries say what replaced them.
  - Externals: `v051-soak-verdict`, `v051-tagged`, `experiment-host`, `owner-checkpoint-1`, `import-host`, `owner-50m-approval`, `owner-upload-exception`, `owner-import-server-surface`, `release-after-parser`.

## Left for the owner
The owner queue is in build-plan §0.3. `docs/owner-decisions.md` §3 ("Points still open") predates the v0.6 Sources questions. It's the owner's file, so this update doesn't edit it.
