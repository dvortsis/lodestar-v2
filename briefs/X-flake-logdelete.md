# X-flake-logdelete: `delete_while_writers_run_and_a_compression_is_in_flight` deleted nothing
**Model:** Opus (concurrency in the logging file sink). Runner brief, 2026-10-30.

## Base
Branch `fix/flake-logdelete` from `origin/main`. Open a PR to `main` and follow `.claude/skills/steward/SKILL.md`. The runner merges.

## The failure
`crates/lodestar/src/logging/source_tests.rs::delete_while_writers_run_and_a_compression_is_in_flight` failed once under full-workspace load: `assert!(done.deleted >= 1, "{done:?}")` saw `LogFilesDeleted { deleted: 0, … }`. It passed 3/3 alone and in two full runs since. IMP-1's session reported it. No fix PR is open.

## Do
1. **Root-cause it; "flake" isn't a cause.**
   - The test waits until the writer has stored n ≥ 600 lines, which means `rotate()` returned twice. It then lists and deletes with `LogFilesWhich::All`.
   - Find how a delete can then remove zero files. Look at:
     - `source.rs::delete` (its listing, then `rt.delete_logs(...)`);
     - `file.rs::delete` (it removes `name` and `name.compressed()`);
     - `rotate_then` and the compression worker: a rename to `.gz`, a temp name not parsed by `LogName::parse`, or the index;
     - `delete_logs` ordering behind an in-flight compression;
     - `rotated_name` collisions under the same `Stamp`.
   - Reproduce it under load: run the test in a loop alongside a CPU hog, or add a `#[cfg(test)]` delay seam at the suspected point.
2. **Fix the real race.**
   - If it's a product bug (a delete that silently skips a closed file mid-compression), fix the product: that's a user-visible "Delete logs" that deletes nothing.
   - If it's a test-only assumption, make the test wait on the actual precondition, with a deadline of at least 10 s. For example, wait until `src.files()` lists a non-current file.
   - Replace the test's fixed `sleep(100 ms)` after the delete with a condition: wait until the writer has written k more lines.
3. Add a regression test that fails before the fix, through the seam if needed.

## Own
`crates/lodestar/src/logging/**`. Name anything else in the PR.

## Done when
- fmt, workspace clippy `-D warnings` and `cargo test --workspace` pass;
- the test passes 50 runs in a loop under a CPU hog;
- the steward self-review is done;
- you've messaged the runner (session_01EYhgh4JX3AmHzKXPjYCoUg).

No model identifiers in commits, PRs or code. Never merge.
