# X-ci-no-fail-fast decisions

## Calls

- **`cargo test` steps that run more than one test binary keep going past a failing binary** (runner). They gain `--no-fail-fast`:
  - **`test` (every OS):** `cargo test --workspace`.
  - **the `dht and sources tests without test seams` step (Linux, in `test`):** `cargo test -p lodestar-dht -p lodestar-sources`.
  - **`bitmagnet importer (PostgreSQL)`:** `cargo test -p lodestar-sources-importers`, which runs the crate's unit tests and its `bitmagnet_check` test.
  - **Why:** without it, cargo stops at the first test binary that fails. On macOS that hid a second failure behind the first, so #896 and #899 each needed the other's fix before either went green. With it, one run shows every failing binary.
- **Steps left alone:**
  - the `--list` and `--no-run` probes, which run no tests;
  - the single-binary steps (the filter budget, logging bench, mmap retention, Windows GC stress and plugin safety tests), where there is nothing later to hide.
- **Nothing else changes:** no permissions, triggers, concurrency or required-check names.

## Why a failing test still fails the job

`--no-fail-fast` changes only when cargo stops, not what it reports. Cargo still runs every test binary in the step, records each one that fails, and at the end prints the failed targets and exits with an error (code 101). These steps run the command directly, with nothing after it in a pipe and no `|| true`. So the shell gets cargo's exit code, the step fails (on Windows too: Actions' `pwsh` wrapper exits with `$LASTEXITCODE`), and so do the job and `ci-ok`, just as before. The only difference is that the step runs longer when a test fails, because it finishes the binaries left after the first failure.

Checked locally: in a scratch workspace with two crates whose tests fail, `cargo test --workspace --no-fail-fast` ran both crates' tests, reported `2 targets failed`, and exited with 101.

## Ride-along

- `crates/lodestar/src/metrics.rs` (from #902's review): the "CR-1a-S's streaming families on the DHT's side" doc comment had been left sitting above `render_reply_ratio`. It's back on `render_stream_dht`. `render_reply_ratio` keeps its own one-line doc.
