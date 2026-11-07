# WK-3: Have-filter replication (keyed SipHash, key over mTLS, chunked snapshot and deltas, fresh snapshot on rebuild)
**Model:** Opus, with `/security-review`. This slice adds a decoder of network bytes, and its key stops peers choosing colliding hashes. A fresh Opus reviewer follows. Runner brief, 2026-11-07T03:00Z. It takes the slot WK-5 freed (#560 merged), within owner §2.42's four slots.

## Spec
- **The plan:** `docs/plan/2026-11-06-v06-workers.md`. Read:
  - the WK-3 row and its slices entry;
  - the §4.2 shared files and the cross-track rule (Track W never waits for Track C; CR-5 narrows its owns around `have.rs`);
  - **the WK-3 paragraph in §4.3, which is your spec and gates**;
  - **Q6**, about line 187.
- **Q6 is answered.** Owner §2.46 delegated it to the runner; `ext:owner-wkq3` holds. The plan's recommendation is the answer: a **separate worker filter** that the coordinator builds for workers.
  - It's sized to the stored count plus 25 % headroom, not 2×.
  - It uses 10 bits per hash up to 30M stored and 8 above.
  - The coordinator rebuilds it when it fills.
  - Above 64 MB the worker swaps it in place instead of holding two copies.
  - The snapshot header carries capacity and bits per hash, so the format fits either design. Build the worker filter as Q6 says.
- **Architecture:** §19.3, §7 and §19.8.
- **What has merged:**
  - WK-0 (#538): the chunked messages in `crates/cluster/src/{server,proto}.rs`, which you don't edit;
  - WK-1 (#544);
  - WK-2 (#556): `crates/lodestar/src/worker/**`. You may add one `mod` line to `worker/mod.rs`.
  - WK-5 (#560): `Assign` carries the filter-snapshot version. Read `docs/decisions/2026-11-07-WK-5.md`.
- **`runner/slices.yaml`:** the `WK-3` entry and its notes. `siphasher` is a new direct dependency (already in `Cargo.lock`).

## Do
Follow §4.3 and Q6:
1. **The keyed PRF.** Replace `RandomState` in `crates/pipeline/src/have.rs` with SipHash-1-3 under a random 128-bit key per filter.
   - The false-positive rate is unchanged; a property test shows it.
   - The key travels only in the snapshot header over mTLS. It never appears in logs, `Debug` output or error text.
2. **Snapshot and deltas** in `crates/cluster/src/have.rs`:
   - **the header:** version, key, bits, hashes, capacity, count and generation;
   - **chunks:** at most 512 KiB each, with a sequence number and total;
   - **deltas:** batched inserts tagged with the filter generation.
   - **A rebuild** bumps the generation and sends a fresh snapshot. The worker keeps its old filter until the new one is complete, except above 64 MB, where it swaps in place.
   - **Decoding is bounded:** lengths and counts are checked before allocating, unknown versions are refused, out-of-order and missing chunks are refused, and nothing half-built is used.
   - **A fuzz target**, `fuzz/fuzz_targets/cluster_have.rs`, plus one `[[bin]]` line in `fuzz/Cargo.toml`.
3. **The coordinator side** (`crates/lodestar/src/cluster/have.rs`):
   - build and maintain the worker filter (Q6);
   - send the snapshot and deltas through WK-0's chunked messages;
   - add one `mod` line to `cluster/mod.rs`.
   - **Insert and rebuild notifications go through `have.rs` itself, never `writer.rs`.**
4. **The worker side** (`crates/lodestar/src/worker/have.rs`): receive, assemble, swap, and answer "might be stored". It's wired into the runtime later by WK-10; keep it testable alone.
5. **Single-machine parity (§19.8).** With no worker joined, the local have-filter behaves as today and no worker filter is built. Measure it against the parity bench and report the result.
6. **The decisions file:** `docs/decisions/2026-11-07-WK-3.md`, with notes for WK-10 and WK-G. Include memory at 10M and 50M stored under Q6.

## Done when
- **§4.3's gates pass:**
  - a worker never reports "not stored" for a key the coordinator stored before the snapshot it holds;
  - out-of-order and missing chunks are refused;
  - the fuzz target;
  - the key is absent from logs and `Debug`;
  - the memory figures are reported.
- **No fixed sleeps**, and no tokio `test-util` or `testing` features on the `lodestar` package.
- **Checks:** `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`.
- **Size and owns:** about 800 lines. Ask the runner before editing outside your owns and the shared one-line edits.
- **Process:** follow `.claude/skills/steward/SKILL.md`, push once per round, and never merge. Title the PR `WK-3: Have-filter replication`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head SHA once CI is green.
