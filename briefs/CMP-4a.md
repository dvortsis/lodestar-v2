# CMP-4a: Companion supervisor (minimal-env spawn, restart with backoff, redacted log ring, health loop, ordered shutdown, orphan cleanup)

**Model:** Opus. **Review:** a fresh Opus reviewer plus `/security-review`; concurrency-heavy. **Branch:** `slice/CMP-4a`, cut from `origin/main` (CMP-1 is in at 52e1ac1d; CMP-2 #600 is about to land). **PR title:** `CMP-4a: <title>`. **Steward:** follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.

## Read first
- `docs/plan/2026-11-06-v06-companions.md`:
  - §4's CMP-4a bullet, including the gated orphans design point. It's the spec.
  - §5 Q3, decided as (b) by `docs/owner-decisions.md` §2.47: same account, sandboxed. The sandbox itself (Landlock, Windows restricted token or Job Object) is **CMP-4b's**. CMP-4a never runs as root or an administrator, uses a clean environment and closes stdin. Leave a hook point so CMP-4b can apply limits right after spawn.
  - §5 Q7 (runner, §2.26): backoff 1 s doubling to 5 min; after 5 crashes in 10 min, stop with "Needs attention"; health every 30 s, and 3 failures in a row restart it; a 10 s shutdown deadline, then kill; a 1 MB log ring per companion, with credentials redacted before they enter it.
- `docs/architecture.md` §11, §12.2 and §18.10.
- `crates/companions/` on main:
  - `adapter.rs`: CMP-0's `Health` trait (CMP-8a implements it; use a fake here);
  - `catalogue.rs`: the templates, env vars and executable path;
  - `credentials.rs` and `config_files.rs`: CMP-3's secrets. Your redaction must cover them;
  - `fetch.rs`: CMP-1's style for typed errors, fs discipline and tests.
- `crates/lodestar/src/update/supervisor.rs`: the init process, which is a subreaper. Read it to understand reaping, so yours never races the init's.
- `runner/standards.md` is the bar.

## Owns
- `crates/companions/src/supervise/mod.rs`
- `crates/companions/src/supervise/process.rs`
- `crates/companions/src/supervise/logs.rs`
- `crates/companions/src/supervise/pidfile.rs`
- `crates/companions/tests/supervise.rs`
- `crates/lodestar/src/update/supervisor.rs`, **the orphan-kill hook only**. Keep that diff small and say what it does in the PR.

**Shared edits the runner allows:**
- one `mod` line in `crates/companions/src/lib.rs`, plus `pub use` if needed;
- `crates/companions/Cargo.toml` and `Cargo.lock`, for workspace dependencies already in the lockfile (`tokio` with `process`, `rustix`, and so on). A crate outside the lockfile needs the runner first, with a licence and maintenance check (Q13). No first-party `unsafe`, so no `pre_exec`;
- new events in `crates/companions/src/log_events.rs`, if needed. Events carry the companion id and codes, never env values, argv secrets or log lines. If an event needs web message keys, use none and return codes (as CMP-1 and CMP-2 did);
- one `docs/decisions/2026-11-08-CMP-4a.md`.

CMP-2 (#600) also adds `lib.rs` and `Cargo.toml` lines. Whichever merges later brings `main` in; that's a trivial conflict.

## What to build
1. **Spawn** (`process.rs`):
   - **Environment:**
     - Linux: `env_clear()` plus the template's variables only.
     - Windows: the base a process needs, plus the template's variables. That base is `SystemRoot`, `TEMP`/`TMP`, `PATH` limited to the system directories, and `USERPROFILE`/`APPDATA` pointed at the companion's folder.
   - **Working directory:** under `<data>/companions/<id>/`, 0700 on Unix, created without following links.
   - **Streams:** stdin is null; stdout and stderr go into the log ring.
   - **Executable:** the path comes only from CMP-2's layout (`Install::executable`) and is checked as a regular file. Never search `PATH`.
   - **Privilege:** never elevate. Refuse to spawn if Lodestar itself runs as root or an administrator; log the reason as a code.
   - **CMP-4b hook:** a callback or trait taking the child's pid/handle right after spawn.
2. **Log ring** (`logs.rs`):
   - 1 MB per companion, bounded in bytes. Line length is bounded too, so a line without a newline can't grow without limit.
   - Credentials are redacted **before** they enter the ring: every secret CMP-3 generated or the user pasted for that companion, plus common shapes (`password=`, `apikey`, `Authorization:`). Test it with a secret split across two reads.
   - Non-UTF-8 output is handled lossily, never panics.
3. **Supervision** (`mod.rs`):
   - **Restart:** with Q7's backoff. The crash-loop rule (5 in 10 min) sets "Needs attention" and stops restarting until the user acts.
   - **Health:** a loop through the `Health` trait every 30 s; 3 failures in a row restart the companion.
   - **State:** observable (for CMP-5 and CMP-6): starting, running, unhealthy, backing off, needs attention, stopped.
   - **Shutdown:** in reverse dependency order, each with a 10 s deadline, then kill. Bounded overall.
   - **Concurrency:** no lock is held across an await that waits on the child. Cancellation-safe. No task leaks after stop.
4. **Orphans** (`pidfile.rs` plus the `update/supervisor.rs` hook), the plan's gated design point:
   - **Records:** each running companion is recorded in `<data>/companions/run/` by pid, process start time, executable path, and a pidfd where the kernel has one. Records are created new, 0600, without following links.
   - **At start-up:** a recorded process whose pid, start time **and** executable path all still match is killed before anything starts. A partial match is never killed, because the pid was reused.
   - **When the work child dies:** the init kills the companions it adopts.
   - **Reaping:** the companion supervisor runs in the work child, and Unix reaping must not race the init's reaper.
   - **Windows:** the Job Object is CMP-4b's. Here, pidfile cleanup at start works the same way.
5. **Errors** are typed codes and carry no secrets, no env values and no unbounded child output. No `unwrap` or `expect` outside tests.

## Tests and gates
- **A fake companion:** a tiny test binary built as an `[[example]]`, or `tests/` helper bin built by cargo. Avoid shell scripts that won't run on Windows. It can print, crash, hang, ignore SIGTERM and report its env.
- **Cases:**
  - the env is exactly the allowed set;
  - stdin is closed;
  - the working directory is right;
  - backoff doubles to its cap;
  - the crash-loop rule;
  - health failures restart it;
  - shutdown order and its deadline, then kill;
  - the ring cap and line cap;
  - redaction, including a secret split across reads;
  - a stale pidfile with a reused pid is not killed;
  - a matching orphan is killed at start;
  - **Linux:** SIGKILL the work child, then assert no companion survives the restart. That's the plan's required test.
- **No fixed sleeps.** Waits are conditions with ≥ 10 s deadlines. Use tokio's paused time only if it doesn't need the `test-util` feature on the `lodestar` package; that's never allowed there. In `crates/companions`, dev-only `test-util` is acceptable if you say so in the PR. Tests run on Windows CI too.
- **Before pushing:**
  - `cargo fmt --check`;
  - `cargo clippy --workspace --all-targets -- -D warnings`;
  - `cargo test --workspace`, at minimum `-p lodestar-companions -p lodestar`.
- **Size:** about 900 lines per the plan; stay under 1,500, or state why.

## Decisions you may make (§2.33; record each in the decisions file)
- the state model and the API shape for CMP-5 and CMP-6;
- the redaction patterns beyond the known secrets;
- how the fake companion is built;
- the pidfile format.

Anything that loosens Q3 or Q7, changes an owner number, or touches `packaging/systemd/lodestar.service` (that's CMP-4b's, and only per Q20) goes to the runner.

When CI is green, Windows included, message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head SHA, using the Claude Code Remote MCP `send_message` tool.
