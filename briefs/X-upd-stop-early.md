# X-upd-stop-early (Opus): send the updates stop when shutdown starts, not after the drain

Runner brief, 2026-11-12 (UTC). From #714's review (`reviews/rv-714-593ace9d.md`, residual at decisions:17) and rv-709's nit. Branch `slice/X-upd-stop-early` from current `main`. PR title `X-upd-stop-early: <title>`.

## The bug
`crates/lodestar/src/services.rs` runs `bounded(serve, stopping, SHUTDOWN_GRACE)` (about :838) to drain serving for up to 10 s, and only then calls `handles.shutdown()`, which sends `stop_updates`. An install that reaches `switch` during the drain still switches: under the supervisor the forwarded stop wins, but the version is dequeued and the staged overlay is left behind.

## Do
1. Send the updates stop as soon as the shutdown signal fires, before (or concurrently with) the serving drain. Keep every other handle's shutdown order unchanged unless a test shows it must move; state any move in the decisions file.
2. rv-714 Important: a stopped install logs `update.install_failed` (Warn, `error=stopping`) at `crates/lodestar/src/update/engine.rs` (about :1286). Add an Info `update.install_stopped` and use it there. Shared edits approved: `crates/lodestar/src/update/log_events.rs` (or wherever update events live), the web event-name chunk for the Updates component and `messages/{en,zh}.json`, and the logs-catalog golden (`LODESTAR_BLESS=1`).
3. rv-714 nits: a Window-queued variant of the "the version stays queued" test (engine.rs about :2299), and the decisions wording that "no staged overlay is left" holds only for a poll that reaches the look.
4. rv-709 nit: `services.rs:347` bare `ID` -> `prowlarr::ID`.

## Tests
- A deterministic test that a shutdown signal reaches the update engine before the serving drain finishes: hold serving open (a held request or a test seam), fire shutdown, assert the engine sees the stop while serving is still draining. Show it fails with the old order (state the mutant in the PR).
- The `install_stopped` event: exact count, Info level, no `install_failed` on a stop.
- Condition waits with deadlines of at least 10 s; no fixed sleeps. Never enable tokio `test-util` or a `testing` feature on the `lodestar` package.

## Decisions
`docs/decisions/2026-11-12-X-upd-stop-early.md`, plus a one-line pointer in `docs/decisions/2026-11-11-X-upd-stop-races.md` (or #714's file) that the residual is closed.

## Checks
`cargo fmt --check`, `cargo +1.99.0 clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`; in `web/` `npm run check` and `npx vitest run` for the event-name chunk. Steward skill; message the runner "ready <PR> <sha>" when ci-ok, web-ok and e2e-ok are green.
