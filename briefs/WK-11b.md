# WK-11b: Lease reconciliation after a worker reconnects: replayed batches against the coordinator's leases
**Model:** Opus, with `/security-review` (network protocol message, concurrency). A fresh Opus reviewer follows.

Runner brief, 2026-11-12T18:51Z. Epic E-v06-workers, on the path to WK-G. Dep WK-11 merged (#699); X-wk-fetched-lease and X-wk-own-lease-lost have merged too.

## Read first
- `docs/decisions/2026-11-11-WK-11.md` §"Lease counts after a replay" (~85–87): WK-11 detects drift (`worker_spool_lease_drift_total`, `HeartbeatAck.leases`, the `Work` hooks `reconcile`/`reconciled`); WK-11b corrects it.
- `docs/decisions/2026-11-12-X-wk-own-lease-lost.md` (~23–24) and `docs/decisions/2026-11-12-X-wk-fetched-lease.md`: the held set, `Fetched` marks and why per-session state is rebuilt on reconnect.
- `docs/architecture.md` §19.3 and §19.6; `docs/decisions/2026-11-06-WK-0.md` (one lease authority, byte-capped queues) and `docs/decisions/2026-11-07-WK-9.md` (fencing, late results, Standing).
- WK-11b's `notes` in `runner/slices.yaml`.

## Do
1. **Protocol** (`crates/cluster/src/proto.rs`): a versioned worker→coordinator message listing the lease tokens the worker still holds, sent when WK-11's drift check finds a difference after a replay drains (and only then). Bounded: a cap on the count (no more than the worker's lease budget allows) checked before allocating; chunk it if a full list could pass the frame cap. Older coordinators and workers keep working (negotiated, or ignored safely); say how in the decisions file. Add the decoder to the `cluster_frame` fuzz target's coverage or a new target if the shape needs one.
2. **Coordinator** (`crates/cluster/src/server.rs`, `crates/cluster/src/leases.rs`): for that worker only, leases it holds that the list omits go back for re-grant (the same path as an expiry, cohort kept, no loss counted twice); tokens in the list the coordinator doesn't hold are counted, never created. Holder-checked: a worker can only return its own leases, never touch another's. Fencing and epoch rules from WK-9 apply; a quarantined, revoked or purged worker's list is refused. Count the drift (`returned`, `unknown`) in a metric row in `docs/metrics.md`.
3. **Races:** a result in flight for a lease while the list is built must not be lost or double counted (the list is a snapshot; a lease committed after it stays committed; a lease returned and then reported is a late result under WK-9). A `Fetched`-marked lease (X-wk-fetched-lease) is not returned. Tests for each, waiting on conditions, never sleeps.
4. **Worker side:** the minimal hook where WK-11's `reconcile` fires (`crates/lodestar/src/worker/spool.rs` or the session), sending the list and recording the reply. Approved as a shared edit; keep it small.
5. **Decisions file** `docs/decisions/<date>-WK-11b.md`: the message, the caps, compatibility, the race rules, and any refinement flagged (§2.19).

## Owns
- `crates/cluster/src/proto.rs`, `crates/cluster/src/server.rs`, `crates/cluster/src/leases.rs`, your decisions file.
- **Shared edits (approved):** tests in `crates/cluster/src/tests.rs` and `crates/cluster/tests/`; the client send in `crates/cluster/src/client.rs`; the worker hook in `crates/lodestar/src/worker/spool.rs` or `worker/session.rs`; the fuzz target and its `[[bin]]`; rows in `docs/metrics.md`.
- **Ask the runner first** for anything else (`crates/pipeline/**`, `crates/lodestar/src/cluster/**`).
- **Don't touch:** `crates/lodestar/src/main.rs`, `crates/lodestar/src/cli/**`, `crates/lodestar/src/update/**`, `crates/companions/**`, `web/**`.

## Done when
- **Gates:** after a replay with drift, the counts agree within one upkeep; a worker can't return or touch another worker's lease; a refused worker's list is refused; no lease is lost or committed twice under the races above; an older peer still works.
- **Tests:** waits on conditions with deadlines of at least 10 s, never fixed sleeps. No tokio `test-util` or `testing` feature on the `lodestar` package.
- **Checks:** `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` (stable is now 1.100; #776 fixed the one new lint, so bring `main` in once if your first CI is red only on clippy), `cargo test --workspace` (per-package is fine on limited disk; say so).
- **Size:** about 500 lines. If it heads past ~1,500, tell the runner first.
- **Process:** follow `.claude/skills/steward/SKILL.md`, push once per round, never merge. Branch `slice/WK-11b`; PR title `WK-11b: Lease reconciliation after a worker reconnects`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with "ready <pr> <full sha>" once CI is green, using the Claude Code Remote MCP `send_message` tool.
