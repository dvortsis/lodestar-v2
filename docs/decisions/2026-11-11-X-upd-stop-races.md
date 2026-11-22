# X-upd-stop-races decisions: two stop races in the update engines

Follow-up to #706 (X-win-upd-stop-wait), whose self-review found both. Brief: `runner-state:briefs/X-upd-stop-races.md`. The code is in `crates/lodestar/src/update/engine.rs` (Unix) and `crates/lodestar/src/update/windows/check.rs`. Calls here are the runner's under §2.19; the owner can veto them.

## Unix: no switch after the service's stop
- **The engine keeps the service's stop** (the receiver `spawn` is given), and an install looks at it just before `switch`. That's after the second reverify, the last await before the switch. If the stop has been sent, the install ends with `stopping`: no Switch intent and no restart request.
  - **What it closes:** the production runtime is multi-threaded. `Handles::shutdown` sends the stop and then aborts the updates task, but an abort doesn't stop a poll already running on another worker. A poll that had just finished the reverify used to run on through `switch`.
  - **Defence in depth under the supervisor:** a supervisor already drops a switch request that meets a forwarded stop (`2026-10-23-B-update-r2.md`, "A stop that meets a request"), so a supervised `docker stop` never came back up on the new version. What this adds there: the version stays queued and isn't marked failed, and no staged overlay is left behind, for a poll that reaches the look (rv-714). Before, a switch at a stop dequeued the version, held it back for the rest of the run, and left its overlay.
  - **Install now too:** it runs in a task of its own, which shutdown never aborts, so the same look covers it. An Install now asked for after the stop is refused as `busy` before it downloads anything.
- **What a stopped install leaves:**
  - **The staged folder** is removed, as on every other refusal after staging.
  - **The version** isn't held back. It stays queued for the window if it was, and isn't marked failed, so the next start can install it.
  - **Nothing is published,** because the service is going away. It was logged as `update.install_failed` with `error = stopping`, a Warn. Since X-upd-stop-early it's an Info `update.install_stopped`.
- **Residual: the drain before the stop. Closed by X-upd-stop-early** (`2026-11-12-X-upd-stop-early.md`): the stop is now sent when the shutdown signal fires. What follows is the record. The engine's stop was sent by `Handles::shutdown`, which runs only after serving has drained (up to `SHUTDOWN_GRACE`, 10 s). An install that reaches the switch during that drain still switches, as before. Under the supervisor, the stop then still wins (B-update-r2), but the version is dequeued and the overlay left. Sending the updates stop when the shutdown signal fires would close it. That's in `services.rs`, outside this slice, so it's left to the runner. Beyond that, a stop sent during the few synchronous steps between the look and `switch` can still lose the race, a window of microseconds.

## Windows: a stop wins over a due check
- **Both selects in `Notifier::spawn`'s check loop are `biased;` with the stop first.** So a stop that comes as a scheduled check falls due, or as the check lock comes free, never starts that check. A check started there would cost the service's stop its whole 2 s wait (X-win-upd-stop-wait).
- **The lock select decides it.** If the first select takes the due sleep over the stop, the loop goes straight to the lock select, where the stop wins. The first select's bias is kept for clarity, since a stop should win wherever it's ready.

## Tests
- **`a_stop_before_the_switch_switches_nothing`** (engine.rs): the stop is sent from the supervisor-verification hook, after staging and before the last reverify. It asserts:
  - the hook ran;
  - no switch was asked for;
  - no Installing state was published;
  - no overlay is left;
  - the version isn't held back;
  - the check lock is free and no switch is claimed;
  - an Install now after the stop is refused as `busy` and downloads nothing.
- The stop is sent from the hook before the last reverify, not between it and the switch: nothing there reads the stop, so the look sees it either way.
- **`a_stop_as_the_check_lock_comes_free_never_runs_the_check`** (check.rs): the test holds the check lock until the loop is waiting for it, seen through a test-only counter, then sends the stop and releases the lock, so both are ready at once. It runs 40 times, because an unbiased select picks at random.
- No paused time, and no tokio `test-util`: the waits are condition loops with deadlines of 10 s (`Env::until`) and 20 s.
- **Mutation-checked, all killed:**
  - no look before the switch;
  - the stop not kept in `spawn`;
  - a stopped install treated as a failure (caught by the version being held back; Install now isn't queued, so dequeuing isn't observed);
  - Install now not refused after the stop;
  - a stopped install keeping the check lock as a switch would (self-review);
  - the lock select unbiased;
  - the lock select with the stop second.
- **One mutant survives, and it's equivalent:** removing the first select's bias. The lock select's bias still stops the check, as above.
