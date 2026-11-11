# X-upd-stop-races decisions: two stop races in the update engines

Follow-up to #706 (X-win-upd-stop-wait), whose self-review found both. Brief: `runner-state:briefs/X-upd-stop-races.md`. The code is in `crates/lodestar/src/update/engine.rs` (Unix) and `crates/lodestar/src/update/windows/check.rs`. Calls here are the runner's under §2.19; the owner can veto them.

## Unix: no switch after the service's stop
- **The engine keeps the service's stop** (the receiver `spawn` is given), and an install looks at it just before `switch`. That's after the second reverify, the last await before the switch. If the stop has been sent, the install ends with `stopping`: no Switch intent, no restart request, so a `docker stop` never comes back up on the new version.
  - **Why it's needed:** the production runtime is multi-threaded. `Handles::shutdown` sends the stop and then aborts the updates task, but an abort doesn't stop a poll already running on another worker. A poll that had just finished the reverify used to run on through `switch`.
  - **Install now too:** it runs in a task of its own, which shutdown never aborts, so the same look covers it.
- **What a stopped install leaves:**
  - **The staged folder** is removed, as on every other refusal after staging.
  - **The version** isn't held back. It stays queued for the window if it was, and isn't marked failed, so the next start can install it.
  - **Nothing is published,** because the service is going away. The existing `update.install_failed` event is logged with `error = stopping`: "an install failed before the switch; the running version carries on".
- **Residual:** nothing awaits between the look and `switch`, but a stop sent on another thread during those few synchronous steps can still lose the race. That window is microseconds, down from the length of a whole reverify. Closing it fully would need the stop and the switch to share a lock, which is more than this race is worth.

## Windows: a stop wins over a due check
- **Both selects in `Notifier::spawn`'s check loop are `biased;` with the stop first.** So a stop that comes as a scheduled check falls due, or as the check lock comes free, never starts that check. A check started there would cost the service's stop its whole 2 s wait (X-win-upd-stop-wait).
- **The lock select decides it.** If the first select takes the due sleep over the stop, the loop goes straight to the lock select, where the stop wins. The first select's bias is kept for clarity, since a stop should win wherever it's ready.

## Tests
- **`a_stop_before_the_switch_switches_nothing`** (engine.rs): the stop is sent from the supervisor-verification hook, after staging and before the last reverify. It asserts:
  - the hook ran;
  - no switch was asked for;
  - no Installing state was published;
  - no overlay is left;
  - the version isn't held back.
- **`a_stop_as_the_check_lock_comes_free_never_runs_the_check`** (check.rs): the test holds the check lock until the loop is waiting for it, seen through a test-only counter, then sends the stop and releases the lock, so both are ready at once. It runs 40 times, because an unbiased select picks at random.
- No paused time, and no tokio `test-util`: the waits are condition loops with 20 s deadlines.
- **Mutation-checked, all killed:**
  - no look before the switch;
  - the stop not kept in `spawn`;
  - a stopped install treated as a failure (held back and dequeued);
  - the lock select unbiased;
  - the lock select with the stop second.
- **One mutant survives, and it's equivalent:** removing the first select's bias. The lock select's bias still stops the check, as above.
