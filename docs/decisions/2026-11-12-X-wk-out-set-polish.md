# X-wk-out-set-polish decisions: the out set's cost, and tests that race

Follow-up to X-wk-own-lease-lost (#727), from its review (`reviews/rv-727-ebd93dd4.md` on runner-state). Brief: runner, 2026-11-12.

## What the out set keeps, and for how long
The out set is the tokens of a worker's results on their way out, in `WorkerLeases` (`crates/pipeline/src/worker.rs`).
- **While a result is out:** its entry is kept until the horizon, a lease TTL plus `seal_wait`, 1,200 s at defaults.
- **Once it's committed, released or lost:** the entry is *finished* and kept for `FINISHED_GRACE` (60 s) only.
  - That is long enough for the holder's own lease, coming back on another path, to arrive after its commit was seen first. That gap is milliseconds.
  - #727 kept finished entries until the horizon.
- **At most `MAX_OUT` (50,000) entries,** the pool's key cap (plan Q5). Past that, the entry due to expire first goes.
- **Each entry has its own expiry,** indexed in a `BTreeMap`. The prune drops whatever has expired, so all the memory belongs to live entries; nothing waits in a queue for its old position. #727's queue held every entry for the full horizon, finished or not.
- **The map gives back a peak's capacity:** when its capacity is over 1,024 and more than 4× its length, it shrinks to 2× its length.
- **A forgotten entry** (past its expiry, or over the cap) makes the holder's own lease, if it comes back, count as new again. That's the behaviour before #727, and the coordinator still fences any stale result.
- **A worker restart forgets every out entry:** they're in memory only. A result replayed from the spool after a restart is handled as before #727.

## The memory cost (worker RSS budget: 256 MB, owner §1.6.5)
- **An entry costs about 150 B:** the hash map's slot (a 20-byte key and a 40-byte value, plus its control byte and load factor) and the expiry index's slot (a 24-byte key and 20-byte value in B-tree nodes).
- **Steady state:** what's out (seconds' worth) plus 60 s of finished entries:
  - about 0.9 MB at 100 results/s;
  - about 2.3 MB at 250 results/s.
- **Worst case: the coordinator unreachable for 20 minutes or more.** Nothing is committed, so every result stays out until the horizon. That's capped at 50,000 entries, about 7.5 MB, reached at about 42 results/s over 20 minutes. Past the cap, the oldest are forgotten (above).
- **For WK-G:** include the out set in the worker RSS gate. Expect at most about 7.5 MB, and about 1–2 MB in steady state at defaults.

## The horizon's TTL
- **It's the protocol's default** (`ServerConfig::default().lease_ttl_secs`), not the TTL the coordinator states in `Welcome`.
  - Only the session sees `Welcome` (`WorkerSession::welcome`). The runtime gets a `SessionHandle` in `Work::connected`, and builds its leases when it starts, before any session.
  - Today the two are the same, because `serve` keeps the protocol's TTL.
- **Reading the stated TTL** would need `Welcome`'s TTL passed to the runtime (a change to the `Work` trait or the cluster client). It isn't worth it while the number can't differ. If it ever does, a too-short horizon only brings back the old behaviour past it. A comment at `out_horizon` says so.

## Tests that race
- **`the_move_out_happens_under_one_lock`:** a test-only probe (`GapHook`, compiled only in `cfg(test)`) runs inside `send_out` at the instant the token has left *held*. From another thread it checks that the lock is still held there.
  - Splitting `send_out` into two lock acquisitions fails it deterministically: 5 of 5 runs, with the probe between them.
  - It replaces relying on a race to catch that.
- **`a_grant_racing_the_move_out_or_the_commit_is_never_held_twice`:**
  - both threads start from one spinning flag;
  - the side given a head start alternates, over 2,000 rounds;
  - half the rounds also commit.

  It asserts the key is never held again, and that both orders (`Held` and `Out`) came up. That last assertion is skipped on a single core, where a head start may not change who runs first. #727's `Barrier` version came out `Out` in 1,000 of 1,000 rounds.
- **New unit tests:** the grace (from the finish, and an entry sent out again keeping its new expiry), the cap and the shrink.
