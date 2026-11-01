# X-e2e-fixture-ci decisions

Closes the "fixture CI job" follow-up of `2026-11-01-S-e2e-2.md`. No `web/src` or `crates` change.

- **Nightly and manual only.** Two jobs in `e2e.yml`, with the `all-themes` condition (`schedule`, or `workflow_dispatch` without `update_snapshots`). Neither runs on pull requests; neither is in `ci-ok`.
- **Binary as an artifact.** `fixture-build` (`ubuntu-latest`) builds `web/` then `cargo build -p lodestar --release` with the repo's rust-cache, since the binary embeds `web/build`. The Playwright container downloads it, so it needs no Rust toolchain.
- **Start and wait.** The server binds `127.0.0.1:8291`; the job polls `/healthz` for up to 120 s and fails early, printing the log, if the process exits. No fixed sleep.
- **Password.** A throwaway password in the workflow env: the server is loopback-only, in an ephemeral runner.
- **Two fixes the first runs needed.** The workers only see the project through `E2E_FIXTURE=1`, and the embedded UI is built with `VITE_DEV_ROUTES=1` for `/dev/shortcuts`.
- **Spec rot fixed, server untouched.** The login limit (a burst of 5 per address, `crates/api/src/auth.rs`) is a safety feature, and the five fixture files sign in five times. `shortcuts.fixture.test.ts` now signs in once per file, as the other files do. A sixth file will exceed the burst; a shared `storageState` from a global setup is the durable fix then. The lantern spec waits (up to 10 s) for the suggestions popup to close before clicking the chip.
