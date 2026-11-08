# WK-10e: Apply Politeness: per-peer-IP dials, BEP 51 interval scale, the coordinator's own share
**Model:** Opus, with `/security-review` (it changes how many connections one public address opens to a remote). A fresh Opus reviewer follows.

Runner brief, 2026-11-08T13:45Z. Slot 4. Epic E-v06-workers, after WK-10c and WK-10d (#607, ed25b460). WK-G depends on it.

**Do WK-10d-polish first** (runner decision, below), then this slice.

## Read first
- `docs/plan/2026-11-06-v06-workers.md` Q9 (same-IP members divide per-remote limits, so a remote never sees more than one install's worth from one public address; WK-G measures the fleet-wide sum).
- `crates/cluster/src/assign.rs`: `Politeness { peer_ip_groups, peer_ip_group, per_peer_ip, bep51_interval_scale }`, `Politeness::ALONE`, `connections_to(peer)`, `valid()`, `peer_ip_bucket` (FNV-1a, IPv4-mapped as IPv4), `INSTALL_PER_PEER_IP`.
- `crates/cluster/src/placement.rs` (~160–190): how shares are computed; `MAX_DHT_PER_IP = 8`.
- `crates/sources/src/pool.rs`: `per_ip` (~131, default 2 at ~551) and the dial pick at ~2155 and the release at ~2287.
- `crates/dht/src/node/actor.rs` ~1450–1465: the BEP 51 not-before interval taken from the peer's `interval`.
- The WK-10a, WK-10b, WK-10c and WK-10d decisions files (`docs/decisions/2026-11-0[78]-WK-10*.md`).

## Do
1. **Per-peer-IP dials in the fetch pool.** The pool asks a politeness policy for each candidate peer: the cap is `connections_to(peer.ip())`, and **0 means never dial that peer** from this install (another member of the same public address owns that bucket). With `Politeness::ALONE` behaviour is exactly today's (`per_ip = 2`). Keep the existing per-IP accounting; don't add a second map.
2. **BEP 51 interval scale in the DHT.** The not-before time after a `sample_infohashes` answer is the peer's interval (or `SAMPLE_INTERVAL`) **× `bep51_interval_scale`**, saturating, so N same-IP DHT nodes together sample a remote no faster than one. Scale 1 is today's behaviour.
3. **Wiring.**
   - The worker runtime applies its `Assignment.politeness` to its pool and DHT node, and a live `Assign` that changes politeness applies without a process restart. A lowered per-IP cap stops new dials above it; existing connections finish (don't drop in-flight fetches). A changed scale applies from the next answer.
   - The coordinator applies its own share (`Placement::coordinator().politeness`) through `serve.rs` and the `services.rs` wiring, on the same sync tick WK-10 added.
   - Reject an invalid `Politeness` (`!valid()`) with a typed error and keep the previous value; never fall back to unlimited.
4. **Decisions file** `docs/decisions/2026-11-08-WK-10e.md`: the policy shape, how a live change applies, and the numbers' sources.

## Numbers
None are owner-set. `INSTALL_PER_PEER_IP = 2` is the pool's existing `per_ip` (§6); the scale comes from WK-5's planner, capped by `MAX_DHT_PER_IP`. Changing either is a runner call: ask first.

## Owns
- `crates/sources/src/pool.rs` (the politeness hook and its tests only);
- `crates/dht/src/node/actor.rs` (the not-before scaling and its tests only), plus a config field where the node takes its settings;
- `crates/lodestar/src/worker/runtime.rs`, `crates/lodestar/src/cluster/serve.rs`, and the coordinator wiring in `crates/lodestar/src/services.rs`;
- tests in `crates/sources/tests/`, `crates/dht/tests/` or the files' own test modules, and `crates/lodestar/tests/worker_runtime.rs`;
- one decisions file.
- **Ask the runner first** for anything else, especially `crates/cluster/**` (read-only here) and `crates/pipeline/src/lib.rs`.

## Done when
- **Gates:**
  - a property test: for any set of same-IP members with valid placement shares, the summed `connections_to(remote)` over the members is ≤ `INSTALL_PER_PEER_IP` for every remote, and the summed sampling rate is ≤ one node's;
  - a pool test: a peer whose bucket isn't ours is never dialled; ALONE behaves as today;
  - a DHT test: scale N spaces repeat samples of one peer by N × the interval;
  - a live `Assign` politeness change applies without a restart and without dropping in-flight connections;
  - the coordinator applies its own share.
- **Tests:** waits on conditions with ≥ 10 s deadlines, never fixed sleeps. No tokio `test-util` or `testing` feature on the `lodestar` package. Paused time only where it's already a dev-dependency (the dht crate's tests may already use it; check).
- **Checks:** `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and the touched packages' tests (`-p lodestar-sources -p lodestar-dht -p lodestar -p lodestar-cluster`); say so if you skip the full workspace for disk.
- **Size:** about 400 lines. Past ~1,000, tell the runner first.
- **Process:** `.claude/skills/steward/SKILL.md`; push once per round; never merge. Branch `slice/WK-10e` from `origin/main` (after WK-10d-polish merges, or bring `main` in then). PR title `WK-10e: <title>`. The runner adds the `slices.yaml` entry. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and full head SHA once CI is green.

## WK-10d-polish (first, its own PR)
Branch `slice/WK-10d-polish` from `origin/main`, title `WK-10d-polish: <title>`. The #607 review's items:
1. bump `check_failures` only when the answer is `None` (no double count);
2. `if server.pause(worker) { evict }`;
3. the `save_floor_pause` race with a resume or revoke (test it);
4. a short `held_among` result maps to Unknown (test it);
5. `assert_eq!(grants.len(), 2)`;
6. optional: requeue Unknown at the back; deny Unknown as Busy;
7. a decisions line (append to `docs/decisions/2026-11-08-WK-10d.md`) about the dropped honest duplicate.

Files: `crates/lodestar/src/cluster/serve.rs`, `crates/cluster/src/server.rs`, `crates/lodestar/src/cluster/ops.rs`, `crates/cluster/tests/transport.rs` and that decisions file. About 40–80 lines. No `/security-review` needed; a fresh Opus reviewer follows.
