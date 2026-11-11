# X-upd-stop-races (Opus): close two stop races in the update engine

Runner brief, 2026-11-11 (UTC). Follows #706 (X-win-upd-stop-wait, merged 4b63499c), whose self-review found both. Same session continues.

## Do
1. **Unix: no `switch` after a stop.** On the multi-thread runtime, `abort()` doesn't stop a poll that is already running. If the stop lands just as `install_steps`' second reverify completes, the poll can run on through `switch` (Switch intent plus a restart request), and a `docker stop` comes back up on the new version.
   - In `crates/lodestar/src/update/engine.rs`, check the stop signal immediately before `switch`, and after any await that comes before it in the same step. Leave cleanly with nothing switched and the cache kept.
   - Log it with the existing log-event pattern (`log_events.rs`), only if a fitting event exists or a new one is cheap. Add a new event's row wherever the repo documents log events.
2. **Windows: a stop wins over a due check.** In `crates/lodestar/src/update/windows/check.rs`, `Notifier::spawn`'s check loop: make the selects `biased;` with the stop arm first, so a stop that arrives as a scheduled check falls due never starts that check.
3. **Tests.**
   - Unix: a deterministic test in which the stop is set between reverify and `switch` (use the engine's existing seams or fakes; no fixed sleeps), so no Switch intent or restart request is written. Show it fails without the check (state the mutant in the PR).
   - Windows: a test that a stop and a due check arriving together never run the check. Show that removing `biased;` (or putting the stop arm second) fails it, with paused time or a controlled clock in test code only.
   - Never enable tokio `test-util` or a `testing` feature on the `lodestar` package.
4. **Decisions file** `docs/decisions/2026-11-11-X-upd-stop-races.md`.

## Owns
`crates/lodestar/src/update/engine.rs`, `crates/lodestar/src/update/windows/check.rs`, tests next to them, and the decisions file. Anything else: ask the runner first.

## Bar
- runner/standards.md; fmt, clippy `-D warnings`, `cargo test -p lodestar update::` and services tests on Linux; Windows-gated tests pass on windows-latest in CI.
- About 150–300 lines. Branch `slice/X-upd-stop-races`, PR titled `X-upd-stop-races: <title>`.
- Steward skill; self-review before review; push once per round. Reply "ready" to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with PR number and full head SHA when ci-ok, web-ok and e2e-ok are green.
