# rv-734 @adb01dbe (X-upd-stop-early): MERGE

VERDICT: MERGE

## Blocking
None.

## Important (fold in if there is another round; otherwise a follow-up)
- **services.rs:894: the platform choice that protects owner §2.13 has no test.** `early_updates_stop` takes `&Handles`, which a unit test can't build, so nothing checks that Windows gets `None`. If a later edit turned the early stop on for Windows, it would bring back the X-win-upd-stop-wait regression: a switch to Off saved during the drain would no longer withdraw a waiting Install now. Nothing would fail. The fix is about 10 lines in an owned file:
  - change the signature to `fn early_updates_stop(updates: Option<&watch::Sender<bool>>) -> Option<watch::Sender<bool>>`;
  - call it as `early_updates_stop(handles.updates.as_ref().map(|(s, _)| s))`;
  - add a test that asserts `is_none()` under `cfg(windows)` and `is_some()` otherwise, with a `None` input giving `None`. Windows CI then guards §2.13.

  The one wiring line in `Services::run` stays checked by eye, which is acceptable: it's a single call next to the `on_shutdown` it feeds.

## Nits
- **engine.rs:1208: a Window queue is now refused during the drain on Unix, which isn't needed.**
  - Queueing writes only `updater.json` and never reaches a switch, so it can't race the stop. Before this PR, such a request was saved and kept for the next start.
  - The refusal could be limited to Install now: check `self.stopping()` inside the `InstallWhen::Now` arm, or in the guard as `matches!(when, InstallWhen::Now)`. That keeps the old Window behaviour.
  - Otherwise, the decisions file (decisions:13) already states and justifies it. The window is narrow, because axum's graceful shutdown serves only requests already in flight.
  - It loses no queue that already exists. The refusal returns before `save_updater`, and a queue saved earlier is untouched.
- **The `busy` refusal says the wrong thing.** The UI shows "Another update is already running." (`web/src/lib/features/settings/updates/messages/en.json:190`) for a refusal that really means "the service is stopping". This predates the PR (#714 added it for Install now) and the page is about to lose its connection, so it's cosmetic. A `stopping` reason code would be accurate later.
- **services.rs:852: the comment says `Handles::shutdown` "still waits for, then aborts" the task.** On Unix `UPDATES_STOP_WAIT` is zero, so it aborts at once. Suggested wording: "still stops its task in the same place as before".
- **decisions:1: the heading has no PR number.** `docs/decisions/README.md:8` asks for `# X-upd-stop-early decisions (#734)`. The same nit was raised on rv-714.

## Verified
- **Unix early stop (services.rs:849-863, :903-914):**
  - `on_shutdown` sends on a clone of the `Handles` sender right after `signal` resolves. That is before `rest()` closes the hub, jobs and logs and sets `stopping_tx`, which starts `bounded`'s clock.
  - axum spawns the graceful-shutdown future in a task of its own. So the send isn't delayed by the accept loop.
  - Nothing is held across an await. The send is synchronous, and `stopping()` takes a std mutex only briefly.
- **Sent twice is harmless:**
  - `Handles::shutdown` → `stop_updates` sends `true` again, which is idempotent on a `watch`, then aborts the task with no wait.
  - The engine loop has usually already returned. Its `select!` takes `stop.changed()`, and the `switching()` branch's `stop.changed().await` also resolves, because the loop's own receiver hasn't seen the value. So nothing deadlocks.
- **No path where the stop is never sent:**
  - If `serve` ends without the signal firing, the future is dropped unsent, as before. `Handles::shutdown` still sends.
  - An install already running inline in the loop runs on to the look (engine.rs:1376). There it sees the stop and returns `STOPPING`, or the post-drain abort ends it, as before.
  - Install now runs in its own task, which is never aborted, and the same look covers it.
- **Windows:**
  - `cfg!(windows)` returns `None`, and the `updates` field isn't cfg-gated, so it compiles on both platforms.
  - With `None`, `on_shutdown` behaves exactly like the old `async move { signal.await; … }`, and `Handles::shutdown` keeps its 2 s `UPDATES_STOP_WAIT`. So the Notifier's last look at the settings stays after the drain, and the X-win-upd-stop-wait / §2.13 behaviour is preserved.
  - Other non-Unix targets have `task: None` anyway.
- **The behaviour change (an install request in the drain is `busy` on Unix):**
  - It's safe: refused before `may_install`, the download and `save_updater`.
  - It's user-visible only as the `busy` toast above, and it's recorded in decisions:13.
  - It can't lose a window install that already exists: a queued version is dequeued only on a real failure, never on `STOPPING`. The Window test asserts that.
- **The new event:**
  - It's Info, System, with no fields, logged before `installing` clears (engine.rs:1285).
  - `install_failed` is no longer logged for a stop.
  - The en/zh texts are accurate: they no longer claim an Install now stays queued.
- **Tests (no fixed sleeps):** the deadlines are 20 s in services and 10 s and 20 s in engine, through `Env::until`. No tokio `test-util` or `testing` feature is added: Cargo.toml is untouched.
- **The `STOPS` `tokio::sync::Mutex` is sound.**
  - It is runtime-agnostic and doesn't poison, so it's safe across each `#[tokio::test]`'s own runtime.
  - The only other test that sends a stop (engine.rs:2877) does so after its switch, so it never logs `install_stopped`.
  - It only serialises counting in the shared capture buffer. It hides no production ordering problem.
- **Docs:** the decisions file and the pointer in #714's file (`2026-11-11-X-upd-stop-races.md`) are accurate, including the "for a poll that reaches the look" clause.
- **Scope:**
  - The diff touches only the brief's owned files (services.rs, engine.rs, log_events.rs, both decisions files), plus the accepted shared edits (system.ts, en/zh messages, logs-catalog golden).
  - Commit subjects are imperative.
  - No model identifiers appear outside the harness trailers.

## What I ran (worktree at adb01dbe, own target dir)
- `cargo fmt --check`: clean.
- `cargo +1.99.0 clippy -p lodestar --all-targets -- -D warnings`: clean (1.99.0).
- `cargo +1.99.0 test -p lodestar --bin lodestar -- update::`: 317 passed. `-- services:: settings:: logging::`: 222 passed, 4 ignored, which covers the logs-catalog golden.
- **Flakiness:** the 3 new or changed tests passed 30/30, and 10/10 pinned to one CPU (`taskset -c 0`).
- **Mutants (all reverted; worktree clean):**
  - The send removed from `on_shutdown`: `the_updates_stop_is_sent_before_serving_drains` fails at services.rs:1424. The sender is dropped, so `wait_for` errors. With a sender kept alive, the 20 s timeout would catch it.
  - `install_failed error=stopping` logged in place of `install_stopped`: the Window test fails (engine.rs:2411).
  - The `STOPPING` early return disabled, so it falls through to the failure path: both stop tests fail (engine.rs:2341, :2406).
- `git merge-tree --write-tree origin/main adb01dbe` (main at 6fed62c0d): clean, tree dd1febf41.
