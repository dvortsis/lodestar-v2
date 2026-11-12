# X-upd-stop-early decisions

The updates engine hears the shutdown when it starts, not after the serving drain. From #714's review (rv-714, the residual in `2026-11-11-X-upd-stop-races.md`) and rv-709's nit. These are the slice's decisions under §2.33. None loosens an owner decision or a security decision.

## The stop, when the signal fires
- **Before:** `Services::run` drained serving for up to `SHUTDOWN_GRACE` (10 s) and only then called `Handles::shutdown`, which sent the updates stop. An install that reached its switch during the drain still switched. Under the supervisor the forwarded stop won, but the version was dequeued and the staged overlay was left behind.
- **Now:** the shutdown signal passed to axum is `on_shutdown(signal, updates_stop, rest)`. It sends the updates stop the moment the signal resolves, before it closes the event hub, the jobs and the log streams and starts the drain's clock (`rest`). The engine's look before its switch then sees it during the drain.
- **Nothing else moved.** `Handles::shutdown` still calls `stop_updates` in the same place, with the same `UPDATES_STOP_WAIT`:
  - it sends the stop again, which is harmless;
  - it waits on Windows;
  - it aborts the task.

  Every other handle's order is unchanged.
- **The sender is a clone** of the one `Handles` keeps (`watch::Sender` is `Clone`), because axum's shutdown future must be `'static`.

## A stopped install is Info, not Warn (rv-714 Important)
- **New event `update.install_stopped`** (Info, System, no fields). It's logged where a stopped install used to log `update.install_failed` with `error = stopping`. Nothing failed: the version stays queued for the next start.
- **It has no fields.** The catalogue's `Label` is a static code, and a version would need a dynamic one. Which version stayed queued is in the Updates screen at the next start.
- **Shared edits (approved in the brief):**
  - the web event name in `logging/event-names/system.ts`, with `en`/`zh` messages;
  - the logs-catalog golden (`LODESTAR_BLESS=1`).

## Nits
- **rv-714:** a Window-queued variant of the stop test, which the "Install now" test couldn't observe because Install now isn't queued.
- **rv-714:** #714's decisions now say "no staged overlay is left" holds for a poll that reaches the look.
- **rv-709:** `services.rs` reads Prowlarr's setting as `prowlarr::ID`, not a bare `ID`.

## Tests
- **`the_updates_stop_is_sent_before_serving_drains`** (services.rs): a serve future ends its accept on the signal, then holds a drain open until the test lets it go.
  - It asserts that the engine's receiver sees the stop while the drain is still held, then that the drain ends and `bounded` returns it.
  - **Mutant (the old order):** `on_shutdown` without its send. The test fails: the engine never hears the stop before the drain ends.
- **`a_stop_before_the_switch_leaves_a_window_install_queued`** (engine.rs): a Window-queued install, once due, stopped from the verification hook. It asserts:
  - no switch, no overlay, not held back;
  - still queued and due;
  - exactly one more `update.install_stopped` line, at Info;
  - no `install_failed` line with `stopping` anywhere.

  The two stop tests take turns (a test-only mutex), since the log capture is shared by every test.
- **Waits:** condition waits have deadlines of 10 s (`Env::until`) and 20 s; there are no fixed sleeps and no tokio `test-util`.
