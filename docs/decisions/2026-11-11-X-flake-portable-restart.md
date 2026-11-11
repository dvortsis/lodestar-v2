# X-flake-portable-restart decisions

The flake in `a_portable_restart_serves_again_on_the_same_port` on #655 (Windows CI, at `ca0fbf6d`): the relaunched copy started 10 ms after the hand-over and failed with `serve: sqlite: disk I/O error`. Brief: runner, 2026-11-11. Supersedes the "then for end of file, which comes when this process exits and Windows has released its port, data lock and files" claim in `crates/lodestar/src/windows/mod.rs` (PK-win-restart, X-win-restart-followups).

## The root cause
- **The end of file isn't the exit.** The old process keeps the pipe's write end until it exits (`HANDED`), so the pipe closes as Windows closes its handles. Microsoft's "Terminating a Process" lists the order: objects are closed, then "the process code is removed from memory", and the process object is signalled last. So the old process's mapped files are still mapped when the successor reads end of file.
- **The old process exits with the store open.** `serve_as` calls `reexec` inside `block_on`, and `hand_over` ends with `std::process::exit(80)`. Store connections still held by tasks are never closed: the process's exit closes them.
- **SQLite's first opener truncates `-shm`.** With the old process's handles closed, its lock on the `-shm` DMS byte is gone, so the successor takes it as the first opener and truncates the file (`winLockSharedMemory`). The old process's view of `-shm` is still mapped, so `SetEndOfFile` fails. That's `SQLITE_IOERR_TRUNCATE`, "disk I/O error", which SQLite doesn't retry.
- **Why it's rare.** The gap is the time Windows takes to unmap a large address space (the index's maps, the heaps). On a loaded runner it was long enough.
- **Unix isn't affected.** `exec` replaces the image and its maps at once, and the supervisor waits for its child's exit.

## The fix: wait for the process, not the pipe
- **The old process's id is already in the hand-over.** `relaunch::token()` is `lodestar-<pid>-<nanos>`, made by the old process. `restart::predecessor_pid` reads it back. `spawn()` and `hand_over` are unchanged.
- **A watch, started first thing.** `wait_if_successor` starts a Windows PowerShell with no window (`powershell_command`) before it waits for the token, so its start overlaps the old process's shutdown. The script (`restart::predecessor_script`) waits on the process object (`WaitForExit`) and ends with 0 once the process has exited or isn't running.
  - A process with that id that started after the copy took the id later, so it isn't waited on. The old process always starts before its copy.
  - Any error ends it non-zero.
- **Then, after the hand-over, the copy waits for the watch** (`restart::await_predecessor`), polling every 50 ms for up to **30 s** (`PREDECESSOR_WAIT`). The floor was 10 s; 30 s covers a cold PowerShell start on a loaded runner when `hand_over` had to start a copy itself.
  - **Still running then:** the copy prints "Lodestar didn't start again after Restart now: the copy it replaces was still running 30 s after it shut down, so this one stopped before opening the data folder. Close Lodestar and start it again." and exits with a failure. That's before `hook` returns, so before any config load or store open. In `Start Lodestar.cmd`'s window, `hold_if_relaunch_failed` keeps it readable.
  - **Nothing to wait on** (`Predecessor::Unknown`): a token that names no process, a PowerShell that can't start (blocked by policy), or a watch that fails. The copy goes ahead as before this fix, which is right far more often than refusing a restart on a machine without PowerShell.
  - The 60 s `EXIT_LIMIT` for the end of file is unchanged. Before, it went ahead after it; now it's followed by the process wait.
- **Why not unsafe FFI:** `OpenProcess` and `WaitForSingleObject` need `unsafe`, which the workspace forbids. No crate already in `lodestar`'s dependencies waits on another process, and the brief allows none new. PowerShell is how this crate already reaches Windows APIs (`acl.rs`, `priority.rs`).
- **Why not close the store before the hand-over:** that's `main.rs` (`reexec` after the runtime's shutdown), a shared file. It also can't be guaranteed: a blocking task past `RUNTIME_GRACE` keeps its handles, and the index's maps would still be up to the exit. The process wait covers everything the old process held: the port, the index lock, the companions lock, the maps.
- **No second layer here.** A bounded retry of the store's open on `SQLITE_IOERR_TRUNCATE` would be in `services.rs` or `lodestar-store`, outside this brief's files. It's left for the runner to decide.

## Tests
- **Every OS** (`restart.rs`, a fake clock and a fake probe, no waiting):
  - `a_relaunched_copy_waits_until_the_old_process_has_exited`: running, running, exited goes on at the third probe; nothing to wait on goes on at once.
  - `a_process_still_running_past_the_wait_is_refused`: probed until the deadline, then refused; the wait is at least 10 s; the message.
  - `only_a_watch_that_ended_well_says_exited`.
  - `the_token_names_the_process_it_came_from`: `token()` round-trips to this process's id; malformed tokens name none; the script names both ids.
- **Windows** (`windows_service.rs`): `a_relaunched_copy_waits_for_the_process_it_replaces`. A copy whose token names the test process, which keeps running, refuses after at least 30 s, before reading its broken config. That checks the real PowerShell watch. The two `portable_restart` tests check its happy path with a real old process.
- **Mutation-checked:** with `await_predecessor` returning at the first probe, the first two unit tests fail.
- **Not run here:** this session has no Windows. The Windows code was checked with `cargo clippy --target x86_64-pc-windows-gnu`, and the integration tests run on Windows CI.

## Shared edits
- `crates/lodestar/src/windows/mod.rs`: `wait_if_successor` and a new `watch_predecessor` in `relaunch::os`, one `use` line, and the `relaunch` module's doc. `spawn()`, `hand_over` and `hook` are untouched. PK-win-portable-upd is editing `spawn()` at the same time.
