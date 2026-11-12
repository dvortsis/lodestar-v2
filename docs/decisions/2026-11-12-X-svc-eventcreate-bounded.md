# X-svc-eventcreate-bounded decisions

The service's own `eventcreate` gets the same bound as the update helper's (rv-702's N2, left open by X-event-log-followups, #721). This is the runner's call under §2.33. It loosens no owner decision or security decision.

## Why it needed a bound
The service has already reported Running to the SCM when it refuses to start (the data folder isn't private) or waits for a rollback. Its Application log note ran `eventcreate` with `.status()`, which has no deadline.
- **A hung `eventcreate`** (the Event Log service stuck, say) kept the process Running for good. The SCM doesn't end a Running service, and a stop request times out (event 7011) without killing it.
- **On the rollback path** the hang came before the idle wait, so the stop signal was never polled.

## One bounded run, shared
- **`crates/lodestar/src/windows/process.rs`** now holds `Started`, `supervise`, `run_bounded` and `EVENT_LOG_WAIT` (10 s), moved from `update/windows/helper.rs` unchanged in behaviour. It isn't duplicated.
  - The update helper's `msiexec` (bounded by `MSIEXEC_WAIT`), its `eventcreate`, and the service's `eventcreate` all go through it.
  - It sits under `windows/` with the service, and `update::windows` already uses `crate::windows::service`.
  - **It writes nothing itself,** so it isn't an Event Log writer. `tests/legal_facts.rs` holds the writers to exactly `update/windows/helper.rs` and `windows/service.rs`, the two Legal Notice section 5 names, by scanning sources for writer names. So `process.rs` keeps those names out of its text, comments included. The first CI run caught a comment that named one.
- **`supervise` takes a `what` label** for its error text ("msiexec ran past its deadline", "eventcreate ran past its deadline"). Before, the helper's `eventcreate` errors named msiexec (#721's N3). The kinds are unchanged: `TimedOut` at the deadline, `Interrupted` for no code or a failed wait. So the helper's P-4 rule (nothing after the spawn is "not started") holds as before.
- **The service calls `write_event_note`,** which runs the command through `run_bounded` for at most `EVENT_LOG_WAIT`. Past it the child is killed and reaped, and the refusal goes on whatever happened: the temp log and the exit code already carry it.
  - The command is the same as before: `System32\eventcreate.exe`, `CREATE_NO_WINDOW`, no input or output.

## Tests
- **`windows::process::tests::a_hung_child_is_killed_at_its_bound`** (moved from the helper's tests):
  - a real child that never exits (`sleep 600`, or `Start-Sleep` on Windows) is killed at a 300 ms bound, well inside the test's 10 s deadline;
  - its error names it;
  - a child that ends answers its code.
- **`windows::service::tests::a_hung_event_note_never_holds_up_the_refusal`:** the service's note with a child that never exits returns within 10 s, answering `TimedOut`. `write_event_note` returns the run's result (the service discards it), so a spawn error, which also returns at once, can't pass for the bound (self-review).
- **The helper's `supervise` tests** (`a_started_msiexec_is_never_classed_as_not_started`, over a fake child and clock) run unchanged through the moved function.
- **No fixed sleeps, and no tokio `test-util`.** `supervise` polls with a deadline.

## Checks
- `cargo clippy --workspace --all-targets -- -D warnings` passes on Linux.
- **A Windows-target pass** (`--target x86_64-pc-windows-gnu`, with MinGW for `ring`) finds nothing in the touched files. It caught one import that only the tests use on Windows, which this PR moves into the test module.
- **That pass also shows warnings already on `main`, outside this slice:**
  - `lodestar-companions`: an unused `File` import in `config_files.rs`, and needless `mut`s in `extract.rs`, `fetch.rs`, `layout.rs` and `manager.rs`;
  - `lodestar/src/main.rs:301`: a collapsible `if`.

  Linux CI doesn't lint `cfg(windows)` code, so they don't fail it. They went to the runner, which queued them as X-win-clippy.

## Self-review (fresh Opus reviewer)
No blocking or important findings, and 2 polish items, both fixed: a comment that sat on the wrong import, and the service test now telling "killed at the bound" from an early spawn error.
