# CR-0w: Remove the start wait

**Runner brief, 2026-11-27T07:29Z.** Core, Opus (DHT protocol behaviour, restart format, concurrency). Target ≤ 1,500 changed lines; if the work runs larger, split it with the runner before pushing (for example the CR-SIM golden re-record as a second PR). An Opus protocol review and a fresh Opus review gate the merge.
- **Branch:** `slice/CR-0w` from fresh `origin/main` (with #1131, X-sec-S8-27, merged as 9ac5ad66).
- **PR title:** `CR-0w: Remove the start wait: no sampling suppression after any start, no blanket gate across a restart, safe restart on by default`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.
- The `runner/slices.yaml` entry exists (r18). Don't add another; edit nothing else in that file or `runner/dashboard.yaml`.

## Why
Owner §2.77 (2026-11-15, `docs/decisions/2026-11-15-owner-no-start-wait.md`): no start wait. Today a first start, and every restart whose proof fails, suppresses sampling for up to about six hours, and a version-1 snapshot carries a blanket gate into its first version-2 run (#1118's upgrade path). The runner's audit of #1118 found §2.77 unmet for every user on first start and on every unproven restart.

## Read first
`docs/owner-decisions.md` §2.77, `docs/decisions/2026-11-15-owner-no-start-wait.md`, `docs/decisions/2026-10-30-CR-0d.md`, `docs/decisions/2026-11-24-X-split-resume-gate.md`, `docs/decisions/2026-11-27-X-sec-S8-27.md` (the restart snapshot as it now is: read-only restored sections, fresh keys, version 3), the slice's `runner/slices.yaml` notes (line with `id: CR-0w`), `crates/dht/src/node/proof.rs` and `proof/**`, `crates/dht/src/restart.rs`, `crates/lodestar/src/config.rs`, and `runner/standards.md`.

## Do
1. No sampling suppression after any start: first start, a failed or missing proof, an invalid snapshot, a version-1 snapshot. A failed proof samples at once and restores nothing. Only per-node restrictions carry over a restart.
2. Drop `sample_start_delay`'s coupling to safe restart. `dht.safe_restart` is on by default.
3. Drop the blanket gate a version-1 snapshot carries into its first version-2 (or version-3) run.
4. Decide, in `docs/decisions/2026-11-27-CR-0w.md`, the crash-loop guard and the `invalidate_failed` and `locked` cases. Keep the restart snapshot format as S8-27 left it: restored sections stay read-only, new inserts use fresh keys. Don't change the format's version numbers unless the decisions file says why.
5. `dht_sample_suppression_seconds` reads 0 or is retired; update `crates/lodestar/src/metrics.rs` help text and `docs/metrics.md` to match (docs/metrics.md:676-677 are already stale; fix them here).
6. Re-record the CR-SIM restart goldens in `crates/crawler-sim/**` and say in the PR what changed and why.
7. Ask the runner before any Legal Notice edit. User docs are D-start's and the first-hour UI is UX-start's; don't edit them. Note in the PR what each needs.

## Timing (the runner's call)
The path to 100 runs on the owner's box now (cr-ab-comp-2 from 2026-11-27T17:00Z, then CR-AB-ADM, CR-AB-4, CR-AB-LC). Build, review and get CI green as normal. **The runner times the merge** against the stage-1 images, so a green, reviewed PR may wait. That's expected; don't merge main in while waiting unless there's a conflict or the runner asks.

## Owns
As in the slices.yaml entry: `crates/dht/src/node/proof.rs`, `crates/dht/src/node/proof/**`, `crates/dht/src/node/mod.rs`, `crates/dht/src/node/actor.rs`, `crates/dht/src/restart.rs`, `crates/dht/src/restart/**`, `crates/dht/tests/**`, `crates/lodestar/src/config.rs`, `crates/lodestar/src/metrics.rs`, `docs/metrics.md`, `crates/crawler-sim/**`, `docs/decisions/*-CR-0w.md`. Not `crates/dht/src/node/sampler.rs` or `crates/dht/src/state.rs` (ask first if you need them), not `web/**`, not `docs/user/**`.

## Done
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (if the session's disk allowance can't hold a full workspace build, run the crates you touched plus `lodestar` and `crawler-sim`, and say so). No fixed sleeps; condition waits with deadlines of at least 10 s. Never tokio test-util on the lodestar crate. Report `ready <n> <full sha>` to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) once ci-ok, web-ok and e2e-ok are green. Push once per round of fixes.

No AWS or cloud calls, no new accounts, no spending, no force-push. Held security detail stays off every branch.
