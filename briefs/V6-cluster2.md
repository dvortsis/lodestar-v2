# V6-cluster2: Cluster transport, protocol, heartbeats and lease fencing
**Model:** Opus (network protocol, concurrency, untrusted input, security). **Branch:** `slice/V6-cluster2`.

**Spec:**
- `runner/standards.md`; `docs/owner-decisions.md`; CLAUDE.md (untrusted-input parsers get property or fuzz tests).
- Architecture §19 in full, especially §19.2 (topology, the message table, the in-process local worker), §19.3 (leases, fencing tokens, TTL renewal in heartbeats, clock offset), §19.6 (joining and security), §19.8 (budgets and verification: revocation closes connections within one heartbeat).
- `docs/build-plan.md` P4 "Remote workers v1", step 2.
- `docs/decisions/2026-10-22-V6-cluster1.md` and the merged `crates/cluster` (#96): `tls.rs`, `join.rs`, `coordinator.rs`, `worker.rs`, `x509.rs`.

## Own
- `crates/cluster/**`, `fuzz/fuzz_targets/cluster_frame.rs` (new), the `fuzz/Cargo.toml` entry for it, `docs/decisions/<date>-V6-cluster2.md`.
- `Cargo.toml`/`Cargo.lock` only for crates this needs (e.g. `postcard`, a WebSocket crate such as `tokio-tungstenite` already compatible with the workspace's rustls): justify each in the PR; prefer what's already in the lockfile.
- **Don't edit** `crates/lodestar` wiring, `crates/api`, `crates/store` (use the settings key-value store or files under `<data>/cluster/` as cluster1 does; if a store helper is truly needed, e.g. `compare_and_set`, name it in the PR for the runner), the pipeline, or the DHT. No have-filter replication (cluster3), sharded DHT (cluster4) or fetch/prepare (cluster5).

## Build
1. **Transport:** WebSocket over the cluster1 mTLS config on the cluster port; frames in `postcard`, batches zstd-compressed; a strict frame-size cap; protocol version in `Hello`, refusing unknown major versions.
2. **Messages:** the §19.2 types needed for the lifecycle now: `Hello`, `Assign` (shape only, payload opaque/minimal), `Heartbeat` (stats, clock offset, lease renewals), `Claim`/`Grant`/`Deny` with **fencing tokens and TTLs**, `Outcome` (idempotent, stale-token reports ignored), `Drain`/`Stop`. Types for the later ones may be stubbed but not wired.
3. **Coordinator side:** an accept loop with, **before authentication**, a handshake timeout, a global and per-IP connection cap, and bounded buffers; after the handshake, `identify()` must succeed before any message is read. One task per connection, bounded channels, no lock held across an `.await` (the coordinator mutex guards synchronous SQLite work only: use `spawn_blocking` or restructure).
4. **Leases and fencing:** an in-memory in-flight table with TTLs renewed by heartbeats; expiry re-grants; a late report with a stale fencing token is ignored and counted. Persistence of outstanding leases can be a decisions-file "next slice" item if it needs the store, but the fencing logic must be complete and tested.
5. **Heartbeats and clock offset:** every 5 s; the coordinator estimates each worker's offset and flags > 60 s.
6. **Revocation closes connections** within one heartbeat (§19.8), and the PKI state is shared, not per process: either a single owner of the PKI (CLI commands go through the running server, document how) or the server reloads issued/revoked state from the store on each heartbeat tick. Pick one and justify it.
7. **Carry-forwards from #96's reviews:** rate-limit `renew` (each adds a 72 h `cluster.cert.*` row); run `maintain` hourly from the coordinator loop (the coordinator's own leaf expires at 72 h otherwise); re-seal cluster keys when `SecretStore` rotates; take the join token from stdin or a file rather than `--token`/`LODESTAR_TOKEN` in argv/env (§13): wire only within `crates/cluster` and name the CLI hook for the runner.
8. **The local worker** (§19.2): the same interfaces over in-process channels, so a single machine uses one code path. A trait or enum both transports implement; tests run the lifecycle over both.

## Tests
- Property or fuzz tests for frame decoding (never panics, never allocates past the cap); a `cargo fuzz` target `cluster_frame`.
- End to end over real TLS on loopback: join (cluster1) → connect → Hello → heartbeats → claim/grant → outcome; a stale-token outcome ignored; lease expiry re-grants; revocation closes within one heartbeat; a pre-auth slowloris is cut by the timeout; connection caps refuse the excess.
- The same lifecycle over the in-process transport.
- Deterministic: a controllable clock, no wall-clock budgets beyond hang guards.

## Done when
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` pass after merging `origin/main`; the new fuzz target builds and runs 60 s clean.
- A PR titled `V6-cluster2: Cluster transport, protocol, heartbeats and fencing`, with a threat model update (pre-auth exposure, DoS limits, fencing), the message table as implemented, new dependencies and why, and anything deferred to cluster3–6.
