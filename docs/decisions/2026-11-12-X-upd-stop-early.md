# X-upd-stop-early decisions

The updates engine hears the shutdown when it starts, not after the serving drain. From #714's review (rv-714, the residual in `2026-11-11-X-upd-stop-races.md`) and rv-709's nit. These are the slice's decisions under §2.33. None loosens an owner decision or a security decision.

## The stop, when the signal fires
- **Before:** `Services::run` drained serving for up to `SHUTDOWN_GRACE` (10 s) and only then called `Handles::shutdown`, which sent the updates stop. An install that reached its switch during the drain still switched. Under the supervisor the forwarded stop won, but the version was dequeued and the staged overlay was left behind.
- **Now (Unix):** the shutdown signal passed to axum is `on_shutdown(signal, updates_stop, rest)`. It sends the updates stop the moment the signal resolves, before it closes the event hub, the jobs and the log streams and starts the drain's clock (`rest`). The engine's look before its switch then sees it during the drain.
- **Unix only** (`early_updates_stop`; self-review I1). On Windows the updates handle is the `Notifier`, which has no in-process switch to race.
  - Its settings watcher takes one last look at the stop, then ends for good.
  - That look must stay after the drain. Then a switch to Off saved by a request still being served withdraws a waiting Install now request before the helper could replay it at boot (X-win-upd-stop-wait, owner §2.13).
  - So Windows keeps the old order exactly.
- **Nothing else moved.** `Handles::shutdown` still calls `stop_updates` in the same place, with the same `UPDATES_STOP_WAIT`. On Unix it sends the stop again, which is harmless, and aborts a task that has usually already ended. Every other handle's order is unchanged.
- **On Unix, an install request served during the drain is now refused as `busy`** (self-review P3). That covers both Install now and "install in the window", because `install_request` looks at the stop first. Before, a Window queue made in the drain was saved for the next start. A request that arrives while the service is going away can be made again after the start.
- **The sender is a clone** of the one `Handles` keeps (`watch::Sender` is `Clone`), because axum's shutdown future must be `'static`.

## A stopped install is Info, not Warn (rv-714 Important)
- **New event `update.install_stopped`** (Info, System, no fields). It's logged where a stopped install used to log `update.install_failed` with `error = stopping`. Nothing failed and nothing changed: the version isn't held back, and one queued for the window stays queued. An Install now was never queued, so there's nothing to keep (self-review I2).
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
  - **What it doesn't cover:** the one line in `Services::run` that wires `early_updates_stop(&handles)` in. A `Handles` can't be built in a unit test without starting every service. That line and the function's two branches are short enough to review by eye (self-review P2).
- **`a_stop_before_the_switch_leaves_a_window_install_queued`** (engine.rs): a Window-queued install, once due, stopped from the verification hook. It asserts:
  - no switch, no overlay, not held back;
  - still queued and due;
  - exactly one more `update.install_stopped` line, at Info;
  - no `install_failed` line with `stopping` anywhere.

  The two stop tests take turns (a test-only `tokio::sync::Mutex`), since the log capture is shared by every test. The event is logged before `installing` clears, so a test that waits on that has seen its line.
- **Waits:** condition waits have deadlines of 10 s (`Env::until`) and 20 s; there are no fixed sleeps and no tokio `test-util`.
