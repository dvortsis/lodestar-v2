# X-fixture-server-kill-on-drop decisions

Tests only. `Server` in `crates/lodestar/tests/fixture_server/mod.rs` now has a `Drop`.

- **Drop kills and reaps.** If the child hasn't been waited for, `Drop` sends SIGKILL (`Child::kill`) and waits, ignoring both errors, so it never panics, even during an unwind. `stop()` still sends SIGTERM, asserts a clean exit and returns the log; it sets a `reaped` flag so `Drop` doesn't kill again.
- **`log` is an `Option`,** because a type with `Drop` can't be moved out of; `stop()` takes it. `Drop` doesn't join the log thread: the pipe closes when the child dies, and a join could only hang a failing test.
- **The test is a new file,** `tests/fixture_server_drop.rs`, not `fixture.rs` (outside this slice's owns, and the helper's tests would otherwise run in both binaries). It spawns a `--fixture` server in a temp dir, panics inside `catch_unwind`, then polls `kill -0` on the pid with a 10 s deadline and no fixed sleep. It doesn't wait for `ready()`: a spawned child is enough.
- **Mutation check (by hand, not committed):** with the kill and wait removed from `Drop`, the new test fails ("server … outlived its panicking test").
- **Pid reuse** between the reap and the check is possible in principle; it would make the test fail loudly, never pass falsely.
