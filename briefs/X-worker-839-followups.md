# X-worker-839-followups

**Runner brief, 2026-11-13. Opus, core (concurrency). Same session as #839 (X-lease-reconcile-every-session) and #853.**

Branch `slice/X-worker-839-followups` from current main. PR title `X-worker-839-followups: worker lease and session follow-ups from #839`. Target ≲ 600 changed lines.

## Owns
- `crates/cluster/src/leases.rs`, `crates/cluster/src/server.rs`
- `crates/lodestar/src/worker/{runtime,session}.rs`
- `crates/lodestar/src/metrics.rs` (the one new series only), `docs/metrics.md` (its row)
- `crates/companions/tests/detect.rs`, `docs/decisions/2026-11-13-X-flake-companions-loopback.md` (item 6 only)
- `docs/decisions/2026-11-13-X-worker-839-followups.md` (new), its `runner/dashboard.yaml` placement line

## Items
1. **Drained reads events.** While the worker session's `drained()` waits for its reply, keep reading the event stream, so a full event queue can't push the stop onto the slow path. Pre-existing, from the #839 review. A test that fills the event queue during a drain and shows the stop still takes the fast path.
2. **Flush on `Changed`.** `runtime.rs`: the flush on `Changed` answers done without sending on the new session, while the comment says otherwise. Either loop until `Done` on the new session within the existing deadline, or correct the comment; say which and why in the decisions file. Optional: a connect + flush in the same wake test. Fix the `leases.rs` (~2558) panic text "granted".
3. **`busy_capped` on /metrics.** Export it (a counter or gauge, following the existing worker series' naming), add a `docs/metrics.md` row, and log when the cap is hit, rate-limited.
4. **Per-worker lease index.** `LeaseTable` reconcile/evict scans every lease under both locks on each handshake. Add a per-worker key index (`HashMap<WorkerId, HashSet<InfoHash>>`, kept in step on every insert/remove path) so reconcile is O(that worker's leases). Keep lock order unchanged. A test that the index matches a full scan after grant, renew, release, expiry, evict and reconcile, plus a property test over random op sequences if it fits.
5. **Re-grant room (investigate).** Re-grants never check the worker pool's room; a full pool evicts and reports lost, then re-harvests. It's now bounded by the 2× pool-cap lease cap. Propose a room-aware re-grant in the decisions file; implement it only if it's small and local, else leave it as a named follow-up.
6. **#853 nits (two lines).** `detect.rs` ~455-458: assert `taken.kind() == io::ErrorKind::AddrInUse` exactly (the brief's ask; PermissionDenied isn't reachable there). The decisions file line ~30: drop "(or 127.0.0.1)", since only the ::1 bind retries. Optionally a comment that any `CI` value counts.

## Rules
- No fixed sleeps; condition waits with deadlines ≥ 10 s. Never tokio `test-util` on `lodestar`.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` before pushing. One push per round.
- Follow `.claude/skills/steward/SKILL.md`. Message the runner "ready <PR> <sha>" when green.
