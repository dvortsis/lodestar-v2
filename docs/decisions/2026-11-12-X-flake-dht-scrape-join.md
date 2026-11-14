# X-flake-dht-scrape-join decisions

- **The abort was tokio 1.54.0's timer use-after-free, not our code.**
  - **Where it failed:** run 43132875604 (the ubuntu-latest test job) aborted the `crates/dht/tests/scrape.rs` binary with SIGABRT. `Node::shutdown` (`crates/dht/src/node/mod.rs:1171`) was awaiting the DHT actor's `JoinHandle` when tokio's `can_read_output` debug assertion `snapshot.is_join_interested()` failed (`runtime/task/harness.rs:426`). Dropping the same handle during the unwind hit the same assertion again (`state.rs:387`). That's a panic in a destructor during cleanup, so the process aborted.
  - **Why it isn't ours:** in tokio 1.54.0 only a `JoinHandle`'s own drop clears `JOIN_INTEREST`, and this handle was alive. So the task's state word was corrupted, or its memory reused. Our code can't do that: the workspace forbids `unsafe`, and `crates/dht` has no `JoinHandle::abort`, `JoinSet` or `LocalSet`. `Node::shutdown` takes `self`, so the handle is awaited exactly once.
  - **The tokio bug:** tokio-rs/tokio#8695, "time: `Interval` drops an elapsed timer without synchronizing with the time driver". PR #8458 (tokio 1.54.0) removed `TimerEntry`'s `Drop`, so `Interval::poll_tick` and `reset_without_timer` can discard an entry the time driver is still writing to.
    - The trigger is `select!` over `interval.tick()` and another branch.
    - The DHT actor's main loop is exactly that (`crates/dht/src/node/actor.rs:491-503`).
  - **The fix upstream:** tokio-rs/tokio#8692 in tokio 1.54.1 (2026-10-21) reverts #8458. **crates.io has yanked tokio 1.54.0:** a fresh `tokio = "=1.54.0"` no longer resolves.
- **The fix here is the dependency, with no change to `crates/dht` code or tests.** These are shared edits, approved by the runner (§2.33, §2.24):
  - `Cargo.lock`: tokio 1.54.0 → 1.54.1 (`cargo update -p tokio --precise 1.54.1`). Only tokio's version and checksum change.
  - `fuzz/Cargo.lock`: the same bump. That lock was already stale on `main` (`cargo metadata --locked` failed there, 18 packages short), so applying the bump also brings it in line with the workspace's crates.
  - Every `tokio = "1.54.0"` requirement is now `"1.54.1"`, so a lock refresh can't land on the broken release again. That's 14 crates plus `bench/`. `crates/companions` keeps its exact pin, now `=1.54.1`.
- **Every crate on 1.54.0 had the race, not only the DHT.** About 20 crates use `tokio::time::interval` or a timer reset (pipeline, cluster, companions, the lodestar worker and update paths, and more). The release-branch backports are the runner's, tracked separately.
- **Reproduction:** none locally; the race needs CI timing. The `scrape` debug binary passed 300 of 300 runs before the fix and 300 of 300 after. The evidence is the tokio source, the upstream issue and fix, and the yank.
