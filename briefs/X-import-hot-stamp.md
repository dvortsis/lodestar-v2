# X-import-hot-stamp: an offline import makes the next start rebuild the hot columns

**From:** the runner, 2026-11-27T22:11Z. **Model:** Opus. **Branch:** `slice/X-import-hot-stamp`. **PR title:** `X-import-hot-stamp: An offline import makes the next start rebuild the hot columns`.

**Why (found by X-import-drain-memory, step 0):** a clean shutdown snapshots the hot columns and records the snapshot's stamp in `settings.hot_stamp` (`save_hot`, `crates/pipeline/src/lib.rs` around 1746–1786). The next start loads that snapshot instead of rebuilding from SQLite (`open_hot`). An offline `lodestar import` (`crates/lodestar/src/cli/import.rs`, server stopped) writes torrent rows but leaves the stamp alone. So the next start loads a snapshot that is missing every imported torrent, and the hot columns disagree with the store until something forces a rebuild. That is a correctness bug in what search and the counts see.

**The change**
1. **Invariant:** whenever anything other than the running pipeline writes rows the hot columns are built from, the recorded stamp is 0 (stale) before its first such write. Then a crash part-way through still forces a rebuild.
2. Make `lodestar import` (start and resume) clear the stamp before its first write. Prefer one small public helper next to `open_hot`/`save_hot` (for example `invalidate_hot(store)`), not a second copy of the key. Keep `HOT_STAMP` private.
3. **Audit every other offline writer** in `crates/lodestar/src/cli/` (rollback, settings, lock, companion, anything else that opens the store with the server stopped) and in `crates/sources-importers/src/driver.rs`'s offline path.
   - For each, decide whether it changes an input of the hot columns (read `HotColumns` and `Store::load_hot` to see which rows and columns those are).
   - Clear the stamp where it does. List each writer and your verdict in the decisions file.
   - **Rollback in particular:** a restored DB brings back its own `hot_stamp`. Show, in a test or from the load code, that it can't match the current snapshot directory, or clear it on restore.
4. **Tests:**
   - save_hot, then an offline import of at least one new torrent through the real CLI path, then open_hot. The result is `from_snapshot == false` and the imported torrent is in the hot columns.
   - The same with a resume.
   - A test that an import which fails before its first write may leave the stamp. Or, if you clear it up front, state that it is cleared up front and test that.
   - A mutation that removes the clear must fail a test.
   - Use condition waits with deadlines of at least 10 s, never fixed sleeps. Never use tokio test-util on the lodestar crate.
5. **Decisions file:** `docs/decisions/2026-11-27-X-import-hot-stamp.md`. Record the invariant, the writer audit and any refinement.

**Owns:** `crates/pipeline/src/lib.rs`, only the hot-stamp section near `open_hot`/`save_hot` and its exports; `crates/pipeline/tests/health.rs` (the hot tests); `crates/lodestar/src/cli/import.rs` and `crates/lodestar/src/cli/import/tests.rs`; any other `crates/lodestar/src/cli/*.rs` the audit shows needs the clear (tell the runner which first); `docs/decisions/*-X-import-hot-stamp.md`.
- Leave `runner/` alone; the runner adds your slices.yaml entry.
- Leave `crates/search/` alone; X-import-drain-memory owns it now.
- X-healthz-cold-start follows you in `crates/pipeline/src/lib.rs`, so keep your edit there small and local.
- Ask the runner before touching any other file.

**Checks before `ready`:** cargo fmt --check; cargo clippy --workspace --all-targets -- -D warnings; `cargo test -p lodestar-pipeline -p lodestar` plus whatever else you touch; the workspace tests CI runs. Follow `.claude/skills/steward/SKILL.md`. Send `ready <PR> <sha>` to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) when CI is green.

**Size:** small, about 150–300 lines. Target: ready within a few hours. A fresh Opus review follows.
