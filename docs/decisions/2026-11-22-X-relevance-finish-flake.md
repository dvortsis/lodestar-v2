# X-relevance-finish-flake decisions (#1042)

`bench/tests/relevance_sets.rs`, `refused_while_a_server_holds_it`, failed once in #1029's CI (`test (ubuntu-latest)`, head 2f0f3237). It passed on a re-run.

## Which check failed
- **It was line 346 at that head:** the success check right after `drop(held)`, the server's `index/.lock`. It wasn't the check after `writer.finish()`, as the brief and #1029's comment said.
- **Both success checks are exposed** to the same mechanism.
- **Reproduced on `main` (73bb8b68):** the test binary run 300 times with `--test-threads 16` failed 14 times. With its output printed, the tool said "in use: a running Lodestar holds …/index/.lock" right after the test dropped that lock. The same test alone (`--exact`) failed 0 of 300 times.

## The mechanism (Linux; macOS by the same reasoning)
- **Both locks are `flock` locks:**
  - the tool's check (`File::try_lock_shared`, `bench/src/bin/relevance/data_dir.rs` `hold_locks`);
  - the lock tantivy takes (`MmapDirectory::acquire_lock`, fs4 `try_lock_exclusive`).
- **A `flock` lock belongs to the open file description.** It's released only when every descriptor for it is closed.
- **Every test in this binary runs the tool through `Command`, in parallel threads.** Only threads of the same process share its descriptor table, so only this binary's own tests can interfere with each other.
- **A spawn copies that whole table into the child:** glibc's `posix_spawn` (`clone` with `CLONE_VFORK`), or fork then exec. The copy includes other tests' lock-file descriptors, until exec closes them as close-on-exec.
- **So a lock a test drops while another test's child is between clone and that close stays held a moment longer.** The tool the test runs next is refused.
- **tantivy's own release is synchronous.** The writer lock lives in the `IndexWriter`, which `wait_merging_threads` consumes. A `DirectoryLock` closes its file on drop.
- **Reproduced on Linux.** macOS follows from the same fork and exec reasoning, but it wasn't observed there.
- **On Windows this mechanism doesn't apply:** Rust opens handles non-inheritable, and `CreateProcess` copies only inheritable ones.
- **A different, unobserved risk remains on Windows** (the self-review).
  - The locks there are `LockFileEx`, released by closing the handle or by the process exiting, not by `UnlockFileEx`. Microsoft's documentation says the time that release takes "depends upon available system resources".
  - So on a loaded runner, a drop or a tool's exit could leave a lock held briefly: a success check would see "in use", or `index.writer` would get `LockBusy`.
  - It hasn't been seen in CI. If it is, the fix is a condition wait of at least 10 s on the real state (re-run only while the output says "in use"), as the brief allows, not a sleep.

## Why `spawn` returning isn't enough
- **The first fix held a mutex around `Command::spawn` only, and dropped the locks under it.** The reasoning: std's `spawn` waits for the exec, through `posix_spawn`'s vfork semantics or its close-on-exec error pipe.
- **It still failed, 4 of 300 runs.**
- **The reason:** Linux lets the parent go (vfork completion, or the error pipe's EOF) before it closes the child's other close-on-exec descriptors. In `begin_new_exec`, `exec_mmap` releases the vfork parent, and `do_close_on_exec` runs later.
- **So "spawn returned" doesn't mean "the child holds no copies".** Only "the child exited" does.

## The fix (test only; the tool is unchanged)
- **A `RwLock`, `CHILDREN`, in the test file:**
  - every run of the tool holds it for reading, from spawn to exit (`Command::output`);
  - a test holds it for writing while it drops a lock it then expects free: `held`, and `writer.finish(); drop(index)`.
- **So no child exists while such a lock is dropped, and any child spawned later can't copy it.** The release is deterministic. There's no wait and no sleep, and every assertion stays.
- **Tool runs still overlap each other.** Only the two drops wait for the runs in flight. The comment at `CHILDREN` names the mechanism, so nobody "simplifies" it away.
- **A poisoned guard is taken with `into_inner`,** so one failing test doesn't fail the others.
- **Every assertion on the tool's exit status now prints its stdout and stderr.** That includes the success checks (`succeeded`), the refusals, the failed gates and the exit-code-2 checks.

## Results
| Run | Before (main 73bb8b68) | After |
|---|---|---|
| `cargo test -p lodestar-bench --test relevance_sets`, 20 times in a loop | 0 of 20 failed | 0 of 20 failed |
| The test binary, `--test-threads 16`, 300 times | 14 of 300 failed | 0 of 300 failed (and 0 of 600 in a second run) |
| The mutex around `spawn` only (first fix), 16 threads | — | 4 of 300 failed |
| Without the guard on `drop(held)` (mutation check), 16 threads | — | 7 of 300 failed |

The default `cargo test` loop rarely shows the race on this machine, so the 16-thread loop is the one that tells.
