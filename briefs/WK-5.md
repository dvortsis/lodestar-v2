# WK-5: Assign v1 and placement: versioned payload, v1 planner (DHT by external IP, fetch and prepare everywhere, plugins refused)
**Model:** Opus, with `/security-review`. It adds a decoder of network bytes and the politeness split. A fresh Opus reviewer follows. Runner brief, 2026-11-07T00:25Z. It launches into the slot X-win-log-held-open freed (#555 merged), within owner §2.42's four slots.

## Spec
- **The plan:** `docs/plan/2026-11-06-v06-workers.md`. Read:
  - the WK-5 row (about line 67) and its slices entry (about line 98);
  - the shared-file list in §4.2 (about lines 121–144);
  - **the WK-5 paragraph in §4.3 (about line 151): it is your spec and your gates;**
  - Q5 (the worker pool cap, 50,000 keys) and Q9 (v1 placement scope and same-IP politeness), about lines 186 and 190.
- **Architecture:** §19.5 and §19.2.
- **What has merged:**
  - WK-0 (#538): it defines the v1 messages and the chunked transfer in `crates/cluster/src/{server,proto}.rs`, which you don't edit;
  - WK-1 (#544): `docs/decisions/2026-11-06-WK-1.md`;
  - WK-4 (#536): the DHT `Shard`; read its decisions file;
  - WK-6 and WK-7.
- **Running beside you:**
  - WK-2 owns `crates/lodestar/src/worker/**`, `cli/*` and `main.rs`. Don't touch them.
  - X-fetch-meta-reserve owns the fetch pool. You only carry its cap as a number in `Assign`.
- **`runner/slices.yaml`:** the `WK-5` entry and its notes.

## Do
1. **The `Assign` payload, v1,** in `crates/cluster/src/assign.rs`:
   - the fields: revision, the DHT shard, budget and intensity, the worker pool cap (Q5), and the dictionary and filter-snapshot versions;
   - a versioned, bounded decode: unknown versions refused, lengths checked before allocation, nothing half-used;
   - a fuzz target `fuzz/fuzz_targets/cluster_assign.rs`, with one `[[bin]]` line in `fuzz/Cargo.toml` (a shared one-line edit).
2. **The v1 planner,** in `crates/cluster/src/placement.rs`, exactly as §4.3 says:
   - **workers sharing an external IP** split that IP's 8 id slots and the 256 target prefixes. The coordinator's own DHT counts as one member of its IP;
   - **workers on different IPs** each get all id slots and a share of the prefixes;
   - **fetch and prepare** run everywhere;
   - **every source plugin is refused** for workers (Q9);
   - **same-IP politeness:** members of one IP divide that IP's per-remote limits (connections per peer IP, and BEP 51 sampling intervals per remote node). A remote peer never sees more from one address than a single install would;
   - **re-plan** on join, leave and pin, each time with a new revision.
3. **The coordinator side,** in `crates/lodestar/src/cluster/placement.rs`: the planner wired to the coordinator's membership. `crates/lodestar/src/cluster/mod.rs` gets one `mod` line, and `crates/cluster/src/lib.rs` gets its `mod` lines (shared one-line edits).
4. **Single-machine parity (§19.8):** with no worker joined, nothing changes and no cluster CA is opened.
5. **The decisions file** `docs/decisions/2026-11-07-WK-5.md`, with notes for WK-8, WK-10 and WK-14.

## Done when
- **The gates in §4.3** pass:
  - a planner property test: no overlap, full cover, and per-remote limits summed over same-IP members ≤ one install's;
  - a plugin with any `Distribution` other than the DHT's is never assigned;
  - the decoder fuzz target, run for a short smoke in CI's fuzz job if one exists.
- **No fixed sleeps.** Wait on conditions with a deadline of at least 10 s.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass.
- **No tokio `test-util` and no `testing` features on the `lodestar` package.** A CI guard refuses them.
- **Size and owns:** about 650 lines. Edit outside the `owns` and the shared one-line edits only after asking the runner.
- **Process:** follow `.claude/skills/steward/SKILL.md`, push once per round, and never merge. Title the PR `WK-5: Assign v1 and placement`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head SHA when CI is green, using the claude-code-remote `send_message` tool.
