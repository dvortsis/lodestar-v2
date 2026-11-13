# X-lease-reconcile-every-session

Runner brief, 2026-11-13. Opus, core: workers and cluster. Track Q (the 2026-11-12 code review). `/security-review` and a concurrency review before ready.

## Scope (as on main, `runner/slices.yaml`)
- **CLUSTER-01 follow-up after #779 (WK-11b):** lease reconciliation runs on **every new session**, not only after a spool removal. Today the worker sends its list only when `Spool::take_reconcile()` is set, which happens after a replay drains (`crates/lodestar/src/worker/runtime.rs` `reconcile`, `crates/lodestar/src/worker/spool.rs` `take_reconcile`). A worker that comes back inside the lease TTL with an empty spool must reconcile too.
- **CLUSTER-02: the lease cap.**
- **CLUSTER-05: the local-stop drain.**
- The runner sends the finding detail for CLUSTER-02 and CLUSTER-05 **by message only**. Never write that detail into a commit, the PR body, a code comment or a decisions file. Name findings by id in all of those.

## Owns
- `crates/lodestar/src/worker/runtime.rs`, `crates/lodestar/src/worker/session.rs`, `crates/lodestar/src/worker/spool.rs`
- `crates/cluster/src/leases.rs`, `crates/cluster/src/server.rs`
- `crates/lodestar/tests/cluster_chaos/**`
- `docs/decisions/2026-11-13-X-lease-reconcile-every-session.md`
- A `docs/metrics.md` row only if you add a metric. Ask the runner first.

## Spec
`docs/architecture.md` §19.3, §19.6 and §19.8; `docs/decisions/2026-11-12-WK-11b.md`; `runner/standards.md`.

## Must have
1. Reconciliation on every new session. Keep #779's spool-drain trigger working; it becomes a special case of the same path.
2. A chaos scenario in `crates/lodestar/tests/cluster_chaos/`: kill a worker and restart it **inside the lease TTL** with an empty spool. Assert that the coordinator re-leases or reclaims every lease the worker lost, and that none is stranded until TTL expiry.
3. Tests for the cap and the drain at unit level, plus one integration path each where the harness allows.
4. No fixed sleeps. Wait on conditions with deadlines of at least 10 s (owner §2.24). Never use tokio `test-util` or `testing` on `lodestar`.
5. Wire compatibility: an older worker talking to a newer coordinator, and the reverse, behave as today or are refused with the existing version error. State which in the decisions file.
6. A decisions file naming CLUSTER-01, -02 and -05 by id only, with the chosen numbers (any cap value) and why. A new owner-visible number is flagged to the runner (§2.19).

## Not in scope
WK-G, the installer (WK-18w), and anything in `crates/pipeline/**` or `crates/filter/**` (other sessions own those).

## Process
Branch `slice/X-lease-reconcile-every-session`; PR title `X-lease-reconcile-every-session: Worker lease reconciliation on every new session; the lease cap and the local-stop drain`. Follow `.claude/skills/steward/SKILL.md`. Before pushing, run `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and the touched crates' tests (`-p lodestar -p lodestar-cluster`, plus the chaos tests). Disk is tight: use `CARGO_INCREMENTAL=0`. When CI is green, message the runner `ready <PR> <full sha>`. No cloud or AWS calls.
