# X-win-upd-history (Opus): Windows install history and the rollback notice in Updates

Runner brief, 2026-11-11 (UTC). The X-upd-stop-races session continues (#714 merged as cd2b7eb4). G-m6 depends on this slice.

## Why
On Windows, `UpdatesApi::status` returns `history: vec![]` (`crates/lodestar/src/update/windows/check.rs`, about line 1106), and `clear_history` is a no-op. The Unix engine keeps `history::History` in `history.json` and publishes `rolled_back` once from it (`engine.rs`, `history()` and the `rolled_back` announcement). So Settings → Updates on Windows never shows past installs or a rollback. PK-guide-3 found this. rv-708 I5 adds that the Updates notice after a rollback must not tell users to reinstall the build that failed.

## Do
1. **Record outcomes.** Whatever finishes a Windows install or rollback (the helper's `Outcome` in `update/windows/helper.rs`: installed, install_failed, rolled_back, rollback_halted, install_interrupted, …) appends an entry the same shape as Unix's `HistoryEntry`. Reuse `update/history.rs`'s types and file format if you can. A new `update/windows/history.rs` is fine for the Windows side. The writer must be crash-safe (atomic write, as the existing journals) and bounded (same cap as Unix).
   - Decide who writes: the elevated helper or the service on its next look at the journal. Prefer the service reading the helper's finished journal, so the helper gains no new file and no new privilege (§2.47 Q13). If the helper must write, say why in the decisions file.
   - The file lives in the data folder's update area with the same ACLs as the other update files. No account names, paths or config values in entries: version, result code, time, a reason code.
2. **Serve it.** `status` returns the entries, newest last, like Unix. `clear_history` clears them, with the same rules as Unix. Publish `rolled_back` once per new rollback entry, as Unix does.
3. **The notice after a rollback.** `settings_updates_rolled_back_next` and anything else shown after a rollback must not tell users to reinstall the failed build. Point them at the history and at waiting for a newer version (or skipping that one). Check the zh file gets the key too if the merge script needs it; ask before editing other features' messages.
4. **Tests.** Unit tests for the history writer (append, cap, corrupt file read as empty with a log, clear). An integration test that a finished helper journal for each outcome turns into exactly one entry, and a second look adds none. A test that `rolled_back` publishes once. Windows-gated where needed; they run on windows-latest in CI. No fixed sleeps; condition waits with ≥ 10 s deadlines. Never enable tokio `test-util` or `testing` on the `lodestar` package.
5. **Decisions file** `docs/decisions/2026-11-12-X-win-upd-history.md`, with the writer choice and its privilege reasoning.

## Owns
`crates/lodestar/src/update/windows/check.rs`, `crates/lodestar/src/update/windows/history.rs` (new), `web/src/lib/features/settings/updates/messages/en.json`, tests next to them, and the decisions file. A shared read-only use of `update/history.rs` types is fine; editing it, `helper.rs` or `rollback.rs` needs the runner first (X-win-upd-first-rollback and X-win-upd-manual-rollback own those next).

## Also, small, from rv-714 (same PR is fine only if it stays in owned files; otherwise tell me)
- The #714 decisions file heading lacks `(#714)`; leave it unless you're in that file anyway.

## Bar
- runner/standards.md; `cargo fmt --check`, `cargo +1.99.0 clippy --workspace --all-targets -- -D warnings` (install 1.99.0 with rustup first; the container's stable lags CI), `cargo test -p lodestar update::`; web check, vitest and build if en.json changes.
- About 300–500 lines. Branch `slice/X-win-upd-history` from fresh main, PR titled `X-win-upd-history: <title>`. /security-review applies (privileged file, untrusted journal input).
- Steward skill; self-review before review; push once per round. Reply "ready <PR> <full sha>" to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) when ci-ok, web-ok and e2e-ok are green.
