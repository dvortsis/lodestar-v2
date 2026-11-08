# PK-win-restart: Windows Restart now (service exits for the SCM's recovery action, portable relaunch); memory and free space on Windows

**Model:** Opus. **Review:** a fresh Opus reviewer plus `/security-review`. **Branch:** `slice/PK-win-restart`, cut from `origin/main` (X-win-refusal-ux #597 is in at f90344f0 and freed `service.rs` and `main.wxs`). **PR title:** `PK-win-restart: <title>`. **Steward:** follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.

## Read first
- `docs/plan/2026-11-06-m6-gap-audit.md`:
  - §4's **PK-win-restart** bullet. It's the spec.
  - §5 **Q10**, the runner's call (flagged to the owner): keep the approved design, plus a conservative count Lodestar keeps itself.
  - **Q9:** the workspace forbids `unsafe`. Windows memory comes from a safe crate already in the lock, or stays unknown.
  - §2's table rows for Restart now and `/system` on Windows.
- `docs/architecture.md` §10.10.6 (Restart now) and §11 (Windows).
- `docs/decisions/2026-10-23-B2c.md`, line 49 and around it: the hooks it left to M6.
- `docs/decisions/2026-11-07-X-win-refusal-ux.md`: what #597 just changed in `service.rs` (the customer bit, `scm_code`) and `main.wxs`.
- Code on main:
  - `crates/lodestar/src/system/restart.rs`: `Restart`, `RestartMethod::Manual` on Windows (`:151`, `:189`), the hint, `requested`, the supervised `EXIT_RESTART` path you mirror;
  - `crates/lodestar/src/windows/service.rs`: `scm_code`, `CUSTOMER`, the `ServiceExitCode::ServiceSpecific` exit (`:439`), and the stop path;
  - `packaging/windows/main.wxs:112-117`: `FailureActionsWhen="failedToStopOrReturnedError"`, two restarts at 60 s, then none, reset after one day;
  - `crates/lodestar/src/system/info.rs:110-125,244-258`: memory and free space are `None` off Unix;
  - `crates/api/src/dto/system.rs:176`: `RestartMethod`, `RestartInfo`.
- `runner/standards.md` is the bar.

## Owns
- `crates/lodestar/src/windows/service.rs`
- `crates/lodestar/src/windows/mod.rs`
- `crates/lodestar/src/system/restart.rs`
- `crates/lodestar/src/system/info.rs`
- `crates/lodestar/tests/windows_service.rs`
- `packaging/windows/main.wxs`
- `packaging/windows/smoke.ps1`
- `crates/lodestar/Cargo.toml` and `Cargo.lock`: lockfile crates only (`fs4` is already there through tantivy). A crate outside the lockfile needs the runner first.

**Shared edits the runner allows:**
- `crates/api/src/dto/system.rs`, only if `RestartMethod` gains a value or `RestartInfo` a field. Then regenerate `web/src/lib/api/openapi.json` and `schema.d.ts` with the repo's tooling, never by hand.
- New events in the lodestar log catalogue, plus their golden, if needed. Events carry codes, never paths or arguments.
- One `docs/decisions/2026-11-08-PK-win-restart.md`.

**Not yours:** web components (the restart dialog reads `RestartInfo`'s estimate and hint; if you think it needs a string change, tell the runner first), `docs/user/**` (PK-guide-3's), and `update/**` (PK-win-upd-*).

## What to build
1. **Service** (`service.rs`, `restart.rs`):
   - Restart now runs the same graceful shutdown as a stop.
   - Then it exits with a **dedicated service-specific code**, distinct from 1 (error) and 3 (refused), through `scm_code`, so the SCM's recovery action starts it again after 60 s.
   - **The guard (Q10):** Lodestar keeps its own count in the data folder of every Restart now and every start after an unclean stop in the last 24 h. Restart now is offered (`available`) only while that count is 0, so a crash always has a recovery left. Otherwise the hint says to restart the service from Services.
   - **The count file is untrusted input.** Parse it defensively: bounded size, a missing or corrupt file counts as "a recovery may have been used", so it fails closed to Manual. Create it without following links, inside the already private data folder. No `unwrap`.
   - **The estimate:** `RestartInfo` says about a minute (the 60 s delay plus the measured start).
   - Check that `main.wxs`'s `FailureActionsWhen` really triggers on a service-specific exit after a clean `SERVICE_STOPPED` report. If it needs a `main.wxs` change, keep it minimal and explain it in the PR.
2. **Portable** (`restart.rs`, `windows/mod.rs`):
   - Start a new copy of the same executable with the same arguments (`std::env::current_exe`, `args_os`) once this process has released the HTTP port, then exit.
   - Never through a shell. No arguments are added.
   - **Race:** wait (a condition with a deadline, no fixed sleep) until the listener is closed before spawning, or have the new copy retry binding with a bounded deadline. Say which in the decisions file.
   - If the spawn fails, log a code and keep this process running; never exit without a successor.
3. **`/system`** reports `available` and the method on the service and portable builds. On a service run with the guard tripped, it reports Manual with the hint.
4. **Free space and memory on Windows** (`info.rs`): free space through `fs4`. Memory per Q9: a safe API already in the lock, or `None` with the reason in the decisions file.
5. **smoke.ps1** gains a Restart now check on the installed service: press it through the API, see the service stop, then come back (the SCM restart), and `/healthz` return. Every wait has a deadline (≥ 90 s here because of the 60 s delay), no fixed sleeps.

## Tests and gates
- **`windows_service.rs`** (runs on the Windows CI job):
  - the restart request path exits with the dedicated code;
  - the 24 h count: 0 offers it; a recent Restart now or an unclean start withdraws it; entries older than 24 h expire; a corrupt or oversized file fails closed.
- **Portable relaunch:** the new copy starts with the same arguments, and a failed spawn keeps the old one running.
- **Unix:** behaviour unchanged (existing restart tests stay green).
- **No fixed sleeps.** Waits are conditions with ≥ 10 s deadlines. Never the tokio `test-util` feature on the `lodestar` package.
- **Before pushing:**
  - `cargo fmt --check`;
  - `cargo clippy --workspace --all-targets -- -D warnings`;
  - `cargo test --workspace`, at minimum `-p lodestar -p lodestar-api`.
  - If openapi changed: the web checks from CLAUDE.md.
- **Size:** about 650 lines per the plan; stay under 1,500, or state why.

## Decisions you may make (§2.33; record each in the decisions file)
- the dedicated exit code's value;
- the count file's format and name;
- the portable race strategy;
- whether `RestartMethod` grows a value (for example `Service`) or reuses an existing one.

Anything that adds a privilege, changes the MSI's recovery numbers (60 s, two restarts, one day), or needs a new grant goes to the runner. Plan Q13's helper is PK-win-upd-2's, not yours.

When CI is green, Windows included, message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head SHA, using the Claude Code Remote MCP `send_message` tool.
