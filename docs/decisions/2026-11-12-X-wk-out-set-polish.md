# X-wk-out-set-polish decisions (#737): the out set's cost, and tests that race

Follow-up to X-wk-own-lease-lost (#727), from its review (`reviews/rv-727-ebd93dd4.md` on runner-state). Brief: runner, 2026-11-12.

## What the out set keeps, and for how long
The out set is the tokens of a worker's results on their way out, in `WorkerLeases` (`crates/pipeline/src/worker.rs`).
- **While a result is out:** its entry is kept until the horizon, a lease TTL plus `seal_wait`, 1,200 s at defaults.
- **Once it's committed** (`committed`): the entry is finished and kept for `FINISHED_GRACE` (60 s) only.
  - That's long enough for the holder's own lease to come back on the claim path after the commit was seen on the results path. The two run milliseconds apart in the runtime.
  - #727 kept every finished entry until the horizon.
- **Once it's released or lost** (`done`): the entry is finished but kept until the horizon, as while out.
  - The coordinator keeps that lease until the loss report lands. The report waits for a session, so with the session down for minutes it can arrive well after a reconnected claim.
  - Until then the coordinator answers the holder's claims with that lease, so the entry must still answer `Out` (rv727, and this PR's self-review B1).
  - Losses are a small share of results, so this barely changes the memory figures.
- **At most `MAX_OUT` (50,000) entries,** the worker pool's key cap (plan Q5).
  - The cap is enforced on every insert and every move out, so it's hard.
  - Past it, finished entries go first, the one due to expire first (so a committed entry goes before a lost one), then the entry due to expire first.
- **Each entry has its own expiry,** indexed in a `BTreeMap`, so all the memory belongs to live entries. #727's queue held every entry for the full horizon, finished or not.
  - The prune takes the first index entry, then drops the map entry only if it matches, so it always progresses.
- **The map gives back a peak's capacity:** when its capacity is over 1,024 and more than 4× its length, it shrinks to 2× its length.
- **A forgotten entry** (past its expiry, or over the cap) makes the holder's own lease, if it comes back, count as new again. That's the behaviour before #727, and the coordinator still fences any stale result.
- **A worker restart forgets every out entry:** they're in memory only. A result replayed from the spool after a restart is handled as before #727.

## The memory cost (worker RSS budget: 256 MB, owner §1.6.5)
- **An entry costs about 190 B:**
  - **The hash map's slot:** a 20-byte key and a 40-byte value, padded to 64 B, plus a control byte, at its load factor. About 85 B at 50,000 entries: 65,536 buckets × 65 B, about 4.3 MB.
  - **The expiry index:** about 100 B. Keys arrive in rising order, so B-tree leaves split about half full, and there are internal nodes on top.
  - **The finished index:** a little more for finished entries.
- **Steady state:** what's out (seconds' worth) plus 60 s of committed entries:
  - about 1.2 MB at 100 results/s;
  - about 3.5 MB at 250 results/s.
- **Worst case: results aren't being committed.** That happens when the coordinator keeps granting leases but refuses the batches (backpressure), or the session drops while the worker still fetches the leases it holds. Then every result stays out until the horizon.
  - That's capped at 50,000 entries, about 10 MB. Past the cap, finished entries go first, then the one due to expire first (above).
  - An unreachable coordinator grants nothing new, so there the set is bounded by the leases already held, at most the pool cap.
- **For WK-G:** include the out set in the worker RSS gate. Expect at most about 10 MB, and about 1–4 MB in steady state at defaults.

## The horizon's TTL
- **It's the protocol's default** (`ServerConfig::default().lease_ttl_secs`), not the TTL the coordinator states in `Welcome`.
  - Only the session sees `Welcome` (`WorkerSession::welcome`). The runtime gets a `SessionHandle` in `Work::connected`, and builds its leases when it starts, before any session.
  - The TTL actually enforced is the coordinator's lease authority's (`ClusterServer::with_hooks` takes it from there), built from the pipeline's `LEASE_TTL_SECS`. A test pins it to the protocol's default, so a drift fails CI instead of silently shortening the horizon.
- **Reading the stated TTL** would need `Welcome`'s TTL passed to the runtime (a change to the `Work` trait or the cluster client). It isn't worth it while the number can't differ. If it ever does, a too-short horizon only brings back the old behaviour past it. A comment at `out_horizon` says so.

## Tests that race
- **`the_move_out_happens_under_one_lock`:** a test-only probe (`GapHook`, compiled only in `cfg(test)`) runs inside `send_out` at the instant the token has left *held*. From another thread it checks that the lock is still held there.
  - A change that lets the lock go at that point fails it, every run.
  - A split placed elsewhere (before the probe), or a removed probe, isn't caught by this test. A removed probe fails it ("the probe never ran"), and the race test below covers the rest.
- **`a_grant_racing_the_move_out_or_the_commit_is_never_held_twice`:**
  - both threads start from one spinning flag;
  - the side given a head start alternates;
  - half the rounds also commit.

  Rounds go on until both orders (`Held` and `Out`) have come up, with at least 2,000 rounds; the head start doubles every 200 rounds while one order is missing, up to a 30 s deadline. It asserts the key is never held again.
  - #727's `Barrier` version came out `Out` in 1,000 of 1,000 rounds.
  - With `send_out` split into two lock acquisitions, this test now fails too, alongside the probe test.
- **New unit tests:**
  - the commit grace (from the commit, and an entry sent out again keeping its new expiry);
  - a lost entry kept until the horizon;
  - the cap (finished first, then the earliest to expire);
  - the shrink, with entries still live.
- **The two TTLs pinned:** `the_protocols_lease_ttl_is_the_one_enforced` asserts that the protocol's default lease TTL, which the runtime's horizon reads, equals the pipeline's `LEASE_TTL_SECS`. The coordinator's lease authority enforces the latter: `ClusterServer::with_hooks` takes the TTL from the authority, not from `ServerConfig`.

## A flake fixed on the way (the runner, shared edit)
- **The flake:** `remote_leases_survive_a_restart_and_parked_ones_go_to_the_built_in_worker` (`crates/pipeline/tests/remote_intake.rs`) failed 1 run in 3 in a reviewer's local runs.
  - `hand_parked` makes the built-in worker the holder before it counts the key in `stats.parked`.
  - The test waited for the holder only, then read `parked`, so it could read it in between.
- **The fix, in the test only:** one wait, with the test's 15 s deadline, for both conditions: the built-in worker holds the key and `parked == 1`.
- **The production order stays:** `parked` counts a key only once the pool has taken it. A key the pool has no room for goes back to wait and isn't counted, so counting it earlier would be wrong.
- **Checked:** 30 of 30 runs pass.
