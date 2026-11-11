MERGE

# Review: #692 X-cmp-handover-followups, delta ead0d88d..cfc09faa (fresh Opus reviewer)

The delta closes B1 and the five non-blocking items from `rv-692-ead0d88d.md`. No main merge is in the range: all six touched files are the PR's own (`manager.rs`, `tests/manager.rs`, `companion_wiring.rs`, `companion_wiring_guided_tests.rs`, `companions.rs`, the decisions file). Every claim checks out. Nothing blocks. Three small non-blocking items follow.

## Blocking
None.

## Non-blocking

**N1 (Important). Three of the four `logged_at_bound` sites are untested.** `crates/companions/src/manager.rs:1587` (the undo's release), `:1779` (the pending release), `:1959` (restart/regenerate).
- **What I checked:** I removed each guard separately. All three mutations survive the full `lodestar-companions` suite. Only the install's own site (`:1368`) is killed, by `a_hand_over_past_its_bound_is_waited_for_then_undone`'s count at `tests/manager.rs:2093`.
- **What's at stake:** only a duplicated `companions.install_failed` line. It isn't a metric (not in `docs/metrics.md`), and the decisions file only claims the hand-over's "logged once" as mutation-checked. That's why it isn't blocking.
- **Cheap fix for the pending-release site:** add `assert_eq!(count, seen + 1)` after `pending` finishes in `a_release_past_its_deadline_is_waited_for_and_keeps_its_marker` (`tests/manager.rs:2153`).

**N2. A restart or regenerate whose hand-over hangs now leaves the companion stopped indefinitely.** `crates/companions/src/manager.rs:1923-1938`; decisions file line 39-40.
- **Why:** `live()` halts the companion, then awaits `hand_over_now`, then starts it. Before the delta, a hung store kept it down for at most 15 min. Now it stays down until the store answers or shutdown.
- **What still holds:**
  - The slot is held, which is correct: a removal can't release while a late save is in flight.
  - The request isn't blocked: `restart`/`regenerate` drop `Pending` (`crates/lodestar/src/companions.rs:724,734`).
- **What's missing:** the decisions file says only "holds the slot the same way". Add half a sentence that the companion stays stopped meanwhile. Optionally, a later slice could start it in parallel with the hand-over.

**N3 (nit). N6's fake change has no test that observes it.** `crates/companions/tests/manager.rs:357-359`. Removing the early return survives. `only_a_companion_that_is_handed_over_is_marked` (`:2168`) could assert `w.api.sink.passwords().is_empty()`. It's harmless as it stands.

## Focus questions

**1. N1's wait after the bound** (`manager.rs:2005-2035`):
- **The wait:** it pins the future, runs `timeout(3*deadline, as_mut())`, logs once on expiry, then awaits the same future, discarding its result, and returns `timed_out`. That's the same correct pattern as `release_now`: it doesn't lose the wakeup and doesn't drop the save.
- **Deadlock:** none.
  - `Sink::download_client` (`companion_wiring.rs:605-621`) takes only the registry in `spawn_blocking`, never the guided lock or a slot.
  - `claim` is try-only (`manager.rs:741`).
  - Every other taker of the guided lock either uses `try_lock_owned`, which answers `busy` (install, guided steps), or waits while holding nothing (serve's `release_pending` task, `companions.rs:247-251`).
- **Shutdown:** `axum::serve` is bounded by `SHUTDOWN_GRACE` (10 s, `services.rs:834`), and the runtime by `RUNTIME_GRACE`. So an indefinitely waiting install's request or task never holds the process up. It's dropped with the runtime.
  - **On a stop mid-wait:** the journal is before `configured`, and the marker exists for a fresh install. Recovery undoes it and queues the release for `wire_sink`. Nothing is lost.
- **Request handlers:** the install handler awaits the install (`companions.rs:1285`), as it did before (the undo's `release_now` was already unbounded). A closed tab drops only the `JoinHandle`, not the task. Restart and regenerate don't await.
- **Ordering:** marker, then hand-over, which settles, then `Err(timed_out)`. `handed` stays true, so `undo` → `roll_back` gives `Ok(None)` for a first install, then `release_now`, then `unmark` only on `Ok`. The release always follows the settled save, so no late save can land after it.
  - **If the release itself times out:** it's logged at its own bound, waited for, the marker kept, and the next start releases it.
  - **No lost or double release:** at most one release per undo, and it's idempotent if repeated at a later start.
  - **Safe to release:** the release can only remove this install's entry, because `can_hand_over` refused any existing entry under the same guided lock the install still holds.
  - The test asserts `releases == 1` and that no marker is left.

**2. B1's default of true.** The implementers are `Wiring` (binary), `Api` (companions, `manager.rs:2208`), `Unwired` (CLI, `companions.rs:139`) and the `FakeApi` (which overrides).
- `Api` and `Unwired` keep the default `release` (`Ok(())`), so a marker they don't need does nothing.
- `Unwired`'s `configure` fails `not_wired` before the marker step anyway.
- `Wiring::release` is a no-op except for qBittorrent, whose hand-over really saves.
- **The only new cost:** an fsync'd marker per first install of Prowlarr or FlareSolverr, and a marker-write failure now fails those installs (fail-closed).
- Nothing must avoid marking.

**3. Tests and mutations** (each reverted after):

| Mutation | Result |
|---|---|
| Trait default `false` | `every_first_install_is_marked` fails (`companions.rs:1531`) |
| Drop `let _ = handed.await;` (old give-up behaviour) | `a_hand_over_past_its_bound_is_waited_for_then_undone` fails (`tests/manager.rs:2068`) |
| Install's `!logged_at_bound` guard → `true` | the same test fails on its log count (`:2093`) |
| Undo, pending and live guards | survive (N1 above) |
| Remove `added.send_replace` in the tail | `a_dropped_confirmation_keeps_the_lock_until_its_save_lands` fails at the new assert (`companion_wiring_guided_tests.rs:946`), plus two others |
| N6 fake early return | survives (N3 above) |

- The watch is sent before `_one` drops, so it's observed under the lock's release.
- **Waits:** both are `yield_now` loops under `WAIT` = 10 s (`tests/manager.rs:1281`), plus `timeout(WAIT, install)`. There are no fixed sleeps.
  - The deadline is 200 ms, the same as the existing release test.
  - The only other deadline-bounded step on that path is the fake's immediate release, so the flake risk is negligible.
- `crates/lodestar/Cargo.toml` is unchanged: no tokio `test-util`, no `testing` feature.

**4. Scope:** only owned files and the decisions file.

**5. The other claims:**
- **N2:** the tail now writes the origin row, reads `records`, and sends `added` itself. That's equivalent to the removed `self.refresh()` (`companion_wiring.rs:1174`). The store guard is a statement temporary, dropped before `records` re-locks, so there's no self-deadlock.
- **N3:** the comment at `manager.rs:1778` is corrected.
- **N4:** the decisions file and the `finish` doc state the unbounded waits.

## What I ran (worktree at cfc09faa)
- `cargo fmt --check`: OK.
- `cargo clippy -p lodestar-companions -p lodestar --all-targets -- -D warnings`: exit 0. The only notice is the repo-wide unknown-lint one for `clippy::chunks_exact_to_as_chunks`.
- `cargo test -p lodestar-companions`: all pass (`tests/manager.rs` 46/46).
- `cargo test -p lodestar --bin lodestar companion`: 70/70.
- `cargo clippy --target x86_64-pc-windows-gnu -p lodestar-companions --all-targets`:
  - It compiles, so the reparse-point branch builds.
  - With `-D warnings` there are 5 errors: `config_files.rs:26` unused `File`, and unneeded `mut` at `extract.rs:780`, `fetch.rs:584`, `layout.rs:693` and `manager.rs:530`.
  - All five are present at the merge base `c818b432` and outside the PR's diff, so they aren't this PR's.
- No network, nothing pushed, posted or messaged.
