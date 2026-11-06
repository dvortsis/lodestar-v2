# X-win-log-held-open: lodestar.log rotation retries after a delay when a reader holds the file
**Model:** Opus. Small, but it is a concurrency and file-handle change on Windows. A fresh Opus reviewer follows.

Runner brief, 2026-11-06T23:28Z. It launches into the slot SRC-15b freed (#552 merged), within owner §2.42's four slots. Target: v0.5.2.

## Background
- **What it does now:** `crates/lodestar/src/windows/logfile.rs` (`LogFile::write_record`, about lines 104–125) rotates `lodestar.log` at its cap. It closes the file, then renames it to the rotated path.
- **What goes wrong:** on Windows the rename fails while another program holds the file without `FILE_SHARE_DELETE`, for example Notepad or a log viewer. The code ignores that error (`let _ = fs::rename(...)`).
- **Why that matters:** after a failed rename it presumably retries at every following record, so it keeps reopening and paying the cost each time. The file can also grow past the cap.
- **The fix already exists elsewhere:** `crates/lodestar/src/logging/file.rs` solved the same problem with `ROTATION_RETRY` (60 s) and `retry_at` (about lines 35 and 648). Read that code first and match its behaviour.
- **Context:** the Windows service log work around the MSI check, in `docs/decisions/2026-11-06-*` (search for logfile and X-win), and the `X-win-log-held-open` notes in `runner/slices.yaml`.

## Do
1. **Retry the rename on a timer.**
   - After a failed rename at the cap, keep appending to the current file and don't try the rename again for 60 s. Use the same constant value and semantics as `logging/file.rs`.
   - Once the reader lets go, the next rename succeeds.
   - Every line still lands whole, in order. None are lost because of the retry.
2. **Bound the growth while it waits.** State the bound in a doc comment: at the cap, plus what 60 s of writes adds.
   - If `logging/file.rs` has a hard ceiling, mirror it.
   - If it doesn't, propose one in the decisions file. The runner decides under §2.26.
3. **Tests.**
   - A `cfg(windows)` test holds the file open without `FILE_SHARE_DELETE`, using `std::os::windows::fs::OpenOptionsExt::share_mode`. While the handle is held: the rename fails, every line lands, and no second rename is attempted inside the delay. After the handle is dropped and the delay has passed: rotation happens.
   - Make the delay injectable or use a test clock. **No fixed sleeps and no real 60 s wait.**
   - Keep the existing tests passing. Add a platform-neutral unit test of the retry timing if the logic can be separated out.
4. **Decisions file:** `docs/decisions/2026-11-06-X-win-log-held-open.md`.

## Own
- `crates/lodestar/src/windows/logfile.rs` and your decisions file.
- Ask the runner before touching anything else, `logging/file.rs` included.

**Running alongside you:**
- WK-2: `crates/lodestar/src/cli/worker.rs`, `worker/**`, plus one line each in `cli/mod.rs` and `main.rs`.
- X-consent-gate-polish (#553).
- X-fetch-pool-bytes (#554): `crates/lodestar/src/metrics.rs`.

## Done when
- These pass:
  - `cargo fmt --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test -p lodestar` (windows tests run on CI's `test (windows-latest)`; check that the job runs yours)
  - `cd tools/status-connector && npm test`
- **The PR:**
  - branch `slice/X-win-log-held-open`, PR titled "X-win-log-held-open: <short imperative title>";
  - follow `.claude/skills/steward/SKILL.md`; never merge; push once per round;
  - message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head SHA through the claude-code-remote `send_message` tool.
