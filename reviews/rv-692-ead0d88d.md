Verdict: CHANGES

# Review: #692 X-cmp-handover-followups, delta bf1c204c..ead0d88d (fresh Opus reviewer)

The delta fixes the previous review's three Important items, and they hold. One new guard ships untested (B1). It's a one-line fix.

## Blocking

**B1. `Wiring::hands_over` is untested, and its trait default turns the crash marker off.** `crates/lodestar/src/companions.rs:559`, `crates/companions/src/manager.rs:362`, `:1469`.
- **What changed:** the delta makes the #679 I2 crash marker opt-in. `marks = fresh && m.p.api.hands_over(id)`, and `ApiConfigure::hands_over` defaults to `false`. The shipped binary writes the marker only because `Wiring` overrides it.
- **What I checked:** I replaced `Wiring::hands_over`'s body with `false`, the trait default, and ran `cargo test -p lodestar --bin lodestar companion`. All 69 tests pass, so the mutation survives. The only test of the new gate (`only_a_companion_that_is_handed_over_is_marked`) uses the `FakeApi` in `crates/companions/tests/manager.rs`, never `Wiring`.
- **Failure scenario:** a refactor drops or breaks the override. For example, X-prowlarr-register adds Prowlarr's hand-over in `Wiring::hand_over` but not in `hands_over`. The binary then silently writes no marker. A crash between the hand-over and `configured` again leaves an orphaned `lodestar-qbittorrent` entry, which is the residual I2 closed. Nothing fails.
- **Why it blocks:** the brief's bar is "Mutation-check each new guard", and the standards treat a missing test for new behaviour as blocking.
- **Fix (either):**
  - a unit test in `companions.rs`' tests: `Wiring::hands_over(qbittorrent)` is true and `hands_over(prowlarr)` is false;
  - or make the trait default `true`, failing safe: a marker for a companion that saves nothing is harmless, since its release is a no-op.

## Non-blocking

**N1 (Important). A hand-over past its 3×deadline bound still lets the guided lock go while its save may land.** `crates/companions/src/manager.rs:1474-1481`, `:2004`; decisions "Hardening", 3rd bullet.
- **The cause:** `Sink::download_client` saves in `spawn_blocking` (`companion_wiring.rs:604-621`). Dropping the timed-out future doesn't stop that save.
- **Failure scenario:**
  1. The install returns `handover/timed_out`. Its undo releases nothing, the install's task ends, and the guided lock is free.
  2. You then confirm qBittorrent through the guided steps. `clear_handover` removes the marker, and your entry is saved.
  3. The native save lands late. `save_clients` re-reads on `Stale` and retries, so it overwrites `MANAGED_CLIENT` with the rolled-back install's target and credentials.
  4. The marker is gone, so no start releases it.
- **The cost:** your entry is silently replaced.
- **This is Important 3's hazard, fixed for release but not for the hand-over.** The decisions file's reasoning ("the next start releases what it left") doesn't hold once a confirmation runs in the same process.
- **How likely:** the registry serialises saves, so this needs the blocking save stalled for more than 15 min outside the section lock. That's pathological, which is why it's not blocking.
- **Suggested fix:** treat it as `release_now` does. Log `timed_out` at the bound, then keep awaiting the pinned future under the slot and guided lock. Or record the residual in the decisions file.

**N2. A dropped confirmation never refreshes the watch.** `crates/lodestar/src/companion_wiring.rs:1262`.
- The record now lands after the request is gone, because the tail runs on. `self.refresh()` runs only in the caller, so `Wired::added` stays stale until the next guided change or restart.
- `added` drives the pipeline's companion gate (`present`), so the managed entry is counted as `external_client` meanwhile.
- **Fix:** send the refresh from the tail (clone `added`, `store`, `catalogue`, `runtime`). It's cheap.

**N3. Logging.**
- A first install's undo whose release passes the deadline logs twice: `release/timed_out` from `release_now`, then `release/rollback_failed` (`manager.rs:1582-1592`).
- The comment at `manager.rs:1776`, "Logged already, once it settled", is wrong: it's logged at the deadline, before it settles.

**N4. Unbounded waits now exist, as documented.** `release_now` (`manager.rs:1964-1979`) and the guided tail's sink hand-over (`companion_wiring.rs:1231`) both wait indefinitely on a store that never answers. They hold the slot and the guided lock until shutdown.
- Only serve's pending-release task is aborted at shutdown. An install's or a confirmation's spawned task is cancelled by runtime drop, which is fine.
- The decisions file states this for release only. Add one line for the confirmation's tail, which by design has no bound.

**N5. The Windows reparse-point branch** (`manager.rs:2287-2309`) isn't compiled or tested here.
- The flags and attribute values are right: `FILE_FLAG_OPEN_REPARSE_POINT` is 0x00200000 and `FILE_ATTRIBUTE_REPARSE_POINT` is 0x400.
- It fails closed (`false` means no SOCKS launch).
- It also refuses other reparse files, such as OneDrive placeholders and dedup files. That's acceptable, since the install folder is Lodestar's own.
- CI's Windows job has to compile it.

**N6. Two fakes don't match their gates.** `FakeApi::hand_over` still saves to the sink when `no_hand_over` is set. It's harmless for the test, but it doesn't match what the gate means.

## The focus questions
1. **The tail's `OwnedMutexGuard`:**
   - **Bound and released correctly:** it's bound as `_one` (not `_`), at the top of the spawned block. It's held through `clear_handover`, the sink save and the origin row, and dropped at the block's end on success, on every `?` error, and on panic unwind.
   - **Survives the request being dropped:** a dropped `JoinHandle` doesn't abort the task.
   - **Cancellation during `apply`, before the spawn:** the guard drops with `finish`'s future. Only qBittorrent's own settings change then; nothing is written to the sink. That's the same as before.
   - **No deadlock:** inside the tail there are only std mutexes held briefly (`pending`, `store`), never across an await. Every other taker of `changing` uses `try_lock_owned` (install, guided steps) or waits without holding anything the tail needs (serve's `release_pending` task).
   - **Mutation-checked:** dropping the guard at the tail's start fails `a_dropped_confirmation_keeps_the_lock_until_its_save_lands` at `companion_wiring_guided_tests.rs:931`.
2. **The 3×deadline bound:** the hand-over is bounded at `3 * deadline()` (15 min shipped). On timeout, `handed = false`, so there's no release and the marker stays.
   - The test bound is 3 s against `WAIT` = 10 s.
   - The state is consistent within the manager, but see N1 for the lock being let go with the save in flight.
3. **`release_now` waiting for the late release:** it pins the future, runs `timeout(as_mut())`, then awaits the same future. That's correct.
   - No lost wakeup: the future is re-polled and its waker re-registered.
   - A release that finishes at the deadline wins, because `Timeout` polls the inner future first.
   - No hang when it's already done: it only awaits its own future.
   - The test's `Notify` permits are stored, so the order doesn't matter.
4. **The previous findings:**
   - Important 1 is fixed and tested, and the mutation is killed.
   - Important 2 is fixed: the bound is derived from the live deadline, and a hung hand-over keeps its marker and releases nothing.
   - Important 3 is fixed for release. It remains for the hand-over (N1).
   - The nits are handled: the marker is written only for a companion that's handed over (B1 above), the clear-before reasoning is documented (sound), the `managed_client_in_use` wording no longer names an origin, and the Windows branch is added.
   - No regressions found.
5. **Tests:**
   - Four new tests: a hung hand-over, a release past its deadline, the marker gate, and a dropped confirmation.
   - Every wait is a condition wait under a deadline of at least 10 s (`WAIT`, `Duration::from_secs(10)`). There are no fixed sleeps and nothing skipped or ignored in the new code.
   - Owned paths are respected. `companion_wiring_guided_tests.rs` is `companion_wiring.rs`' test module.
6. **Features:** no tokio `test-util` and no `lodestar-companions/testing` feature on the `lodestar` package (`crates/lodestar/Cargo.toml`). `set_live_deadline` is used only in `lodestar-companions`' own tests.

## What I ran (worktree at ead0d88d)
- `cargo fmt --check`: OK.
- `cargo clippy -p lodestar-companions -p lodestar-api -p lodestar --all-targets -- -D warnings`: exit 0. The only warning is the repo-wide `-A clippy::chunks_exact_to_as_chunks` unknown-lint notice, which isn't this PR's.
- `cargo test -p lodestar-companions -p lodestar-api -p lodestar`: all pass. The four new tests pass.
- Mutation 1: `Wiring::hands_over` set to `false`. It survives (69/69 companion tests pass). That's B1.
- Mutation 2: the tail drops the guided guard at its start. `a_dropped_confirmation...` fails, so the test is real.
- Not run: the Windows build, which is CI's job. No network calls; nothing pushed or posted.
