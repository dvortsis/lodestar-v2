# B-log1b decisions: the stalled-download shutdown test (fix/shutdown-stall)

The runner's decision of 2026-10-30, after `tests/shutdown.rs`'s `a_stalled_download_cannot_hold_the_shutdown` failed on `main`'s CI in about 0.35 s (run 41018406044). This refines part 2b's test (`2026-10-23-B-log1b-r3.md`) and #165's fix. No server behaviour changes.

- **Why it failed:** the forced path needs hyper to have stopped reading the response's body, which it does only once its own write buffer is full. If the buffer still has room when the signal comes, `logs.close()` ends the body and the connection closes by itself, so the graceful shutdown finishes without the bound. The kernel's queues (the client's receive queue still, the server's send queue holding data) don't show hyper's buffer, so the test can't tell the two apart.
- **The test now holds the property every run, then names the path.**
  - **The property:** the process exits 0 within the bound (10 s) plus a margin (under 25 s), and the state is saved.
  - **The path:** either `app.shutdown_forced`, before `app.stop`, after at least 9 s; or no `app.shutdown_forced` and under 9 s, with the download ended by the close. A run that fits neither fails and says which it looked like.
- **The forced path is covered every run by unit tests** (`services::bounded_tests`). The race moves into `bounded(serve, stopping, grace)`, with the same behaviour. Three tests cover it:
  - a serve future that never ends is dropped after the bound;
  - one that ends returns its result;
  - before the signal the bound never runs, even at zero.

  They use real timers of 50 ms or less with outcomes that don't depend on timing, and a 10 s deadline only so a broken case can't hang. tokio's paused time would need its `test-util` feature in the binary's manifest, which isn't this slice's.

- **Not done:** option (b), letting an in-flight download keep streaming after `logs.close()` until the bound. It changes the server's behaviour, and the runner decided against it.
