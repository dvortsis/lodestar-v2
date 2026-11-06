# WK-2: lodestar worker: join from a token file or stdin, credentials, connect, reconnect, renewal, reseal, drain and stop
**Model:** Opus, with `/security-review`. This slice handles credentials, the token and the network session. A fresh Opus reviewer follows. Runner brief, 2026-11-06T23:12Z. It launches into the slot ENR-9b freed (#551 merged), within owner §2.42's four slots.

## Spec
- **The plan:** `docs/plan/2026-11-06-v06-workers.md`. Read the WK-2 row (about line 64), the WK-2 paragraph (about line 148), Q2 (about line 183), and the shared-file list (about lines 136–137).
- **Architecture:** §19.6 and §13. Also read `docs/decisions/2026-10-23-V6-cluster2.md`, which describes the cluster library this slice builds on.
- **What has merged:** WK-0 (#538), WK-1 (#544; read `docs/decisions/2026-11-06-WK-1.md`, which covers the coordinator side, `ClusterHandle`, the PKI and reseal), WK-4, WK-6 (the wire codec) and WK-7 (#549, the worker stages).
- **`runner/slices.yaml`:** the `WK-2` entry and its notes.

## Do
1. **`lodestar worker --join <url> --token-file <path|->`** joins and saves the credentials. After that, **`lodestar worker --data <dir>`** runs from the saved credentials.
   - The token comes from a file or from stdin (`-`), never from argv.
   - **Plan Q2 is answered (owner §2.47, `owner-wkq2` holds; corrected by the runner at 23:15Z):** also accept `LODESTAR_TOKEN` from the environment, exactly as the plan's Q2 row gives it.
     - Only for the first join, while no credentials exist.
     - Ignore it afterwards, logging a warning to remove it.
     - Never log its value.
     - The token is single-use, name-bound and expires in 15 minutes; the coordinator side already enforces this, so check it.
     - The plan row is the decision. Read `docs/plan/2026-11-06-v06-workers.md` §5 Q2 and owner §2.47.
   - The token and the credentials never reach logs, error text, a panic message or argv. A test greps the captured logs and errors for the token. Another test shows `LODESTAR_TOKEN` is ignored, with the warning, once credentials exist.
2. **Connect** over the cluster library's mTLS session.
   - Reconnect with capped exponential back-off and jitter, and make the waits testable with paused time.
   - Renew the certificate at half-life.
3. **Drain and stop.**
   - On `Drain`: finish the current leases and report them, then stop.
   - On `Stop(Revoked)` or `Expired`: stop at once and print one plain message saying to join again.
   - A reused or expired token fails with a single message.
4. **Reseal.** Call `Credentials::reseal` during the worker's secrets-key rotation, per V6-cluster2. Test it: after reseal then retire, the credentials still open.
5. **Idle runtime.** The worker connects and stays up but runs no DHT or fetch work. That is WK-10's job. Leave clear seams for WK-3 (`worker/have.rs`) and WK-10 (`worker/runtime.rs`, `worker/budget.rs`), one `mod` line each.
6. **Gates (plan):**
   - join;
   - restart;
   - renew;
   - reseal then retire;
   - a revoked worker stops within one heartbeat;
   - a reused or expired token fails with one message;
   - the token never appears in logs or argv; `LODESTAR_TOKEN` works for the first join only (Q2, §2.47).

   Test these in-process against a real `ClusterServer` from WK-1, plus one real-binary test if it's cheap. Real-binary tests are `#[cfg(unix)]`, like `tests/cluster_serve.rs`.
7. **Decisions file:** `docs/decisions/2026-11-06-WK-2.md`, with notes for WK-3, WK-10 and WK-18.

## Own
- Everything in the entry's `owns`, plus your decisions file.
- **One shared line:** the `Command` variant in `crates/lodestar/src/cli/mod.rs`. If you need more than that, or anything else (`crates/cluster/**`, `config.rs`, `Cargo.toml`), message the runner first.
- **Running beside you:**
  - SRC-15b (`web/`);
  - X-consent-gate-polish (#553: `crates/pipeline/src/control.rs`, `lib.rs`, `crates/lodestar/src/ops/tests.rs`, logging);
  - X-fetch-pool-bytes (#554: `crates/sources/src/pool.rs`, `crates/lodestar/src/metrics.rs`, `docs/metrics.md`).

  If you add metrics, ask the runner, because `metrics.rs` is in #554.

## Done when
- These pass:
  - `cargo fmt --check`;
  - `cargo clippy --workspace --all-targets -- -D warnings`;
  - `cargo test -p lodestar-cluster -p lodestar` (one crate at a time if the disk is short);
  - `cd tools/status-connector && npm test`;
  - `/security-review`.
- No fixed sleeps. Waits are conditions with deadlines of at least 10 s, or paused time.
- **The PR:**
  - branch `slice/WK-2`, titled "WK-2: <short imperative title>";
  - follow `.claude/skills/steward/SKILL.md`, never merge, and push once per round;
  - message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head SHA through the claude-code-remote `send_message` tool.
