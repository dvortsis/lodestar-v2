# CMP-4b: Companion resource limits, sandbox and egress (Job Object, rlimits, delegated cgroup v2 when present, Landlock; stopped while the route is down)

**Model:** Opus. **Review:** a fresh Opus reviewer plus `/security-review`; platform security and concurrency. **Branch:** `slice/CMP-4b`, cut from `origin/main` once CMP-4a (#603) is in. **PR title:** `CMP-4b: <title>`. **Steward:** follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.

## Read first
- `docs/plan/2026-11-06-v06-companions.md`:
  - §4's **CMP-4b** bullet (line ~159). It's the spec.
  - §5 **Q3**, decided as (b) by `docs/owner-decisions.md` §2.47 (the plan row is the decision): same account, **sandboxed**.
    - On Linux, a Landlock ruleset through the safe `landlock` crate limits the companion to its own folder and its downloads folder, with no access to Lodestar's data.
    - On Windows, a restricted token or AppContainer, if a safe wrapper crate exists.
    - Where the sandbox can't be applied (no Landlock in the kernel, or no safe Windows wrapper), the install is still offered, but its confirmation says plainly that the companion could read Lodestar's data. CMP-5 and CMP-6 show that; you report whether the sandbox applied.
  - **Q8:** a companion that can't follow its route doesn't start. Never start it on the direct route.
  - **Q9:** stop a companion when its route goes down, and start it again when the route is back.
  - **Q13:** new crates.
  - **Q18:** each companion has its own egress.
  - **Q20**, decided as "no relaxation" (§2.46, `owner-cq7`): **you don't edit `packaging/systemd/lodestar.service`.** If something needs it, tell the runner, and it goes to CMP-G's list for the owner.
- `docs/architecture.md` §11, §18.4 and §18.10; `docs/owner-decisions.md` §1b.5.
- **CMP-4a's code on main:** `crates/companions/src/supervise/*`, and `docs/decisions/2026-11-08-CMP-4a.md`.
  - Use the `AfterSpawn` hook. It's your entry point.
  - Read the process groups, and how `Supervisor` restarts and stops a companion.
- `crates/sources/src/egress.rs`: `EgressFactory`, `PluginEgress`, the route state watch and the probe.
- `packaging/systemd/lodestar.service`: `ProtectControlGroups=yes`, with no `Delegate=`.
- `runner/standards.md` is the bar.

## Owns
- `crates/companions/src/supervise/limits.rs`
- `crates/companions/src/supervise/limits_linux.rs`
- `crates/companions/src/supervise/limits_windows.rs`
- `crates/companions/src/supervise/egress.rs`
- `crates/companions/tests/limits.rs`

**Shared edits the runner allows:**
- `mod` lines in `crates/companions/src/supervise/mod.rs`. If wiring the route gate needs more than a few lines there (starting only while the route is up), keep it small and say so in the PR.
- `crates/companions/Cargo.toml` and `Cargo.lock`:
  - `rustix`'s `process` feature (for `prlimit`), and `lodestar-sources` as a dependency (no cycle: `crates/sources` must never depend on `crates/companions`; check it).
  - **New crates, pre-approved under plan Q13 on these conditions:** `landlock` (Linux-only target dependency) and one safe Windows Job Object wrapper (`win32job`, or similar).
    - Each must be MIT or Apache-2.0 and actively maintained, checked and recorded in the decisions file: licence, last release, maintainers, and `cargo tree` added lines.
    - A Windows wrapper that pulls in a large new tree (for example, the whole `windows` crate when only `windows-sys` is in the lock) goes to the runner first, with the line count.
    - A restricted-token or AppContainer wrapper only if a safe, maintained one exists. Otherwise, none, and Windows reports the sandbox as not applied.
  - **No first-party `unsafe`,** so no `pre_exec`.
- New events in `crates/companions/src/log_events.rs`. Events carry the companion id and codes, never paths, env values or routes' credentials.
- One `docs/decisions/2026-11-08-CMP-4b.md`.

**Not yours:**
- the `companions.<id>.egress` setting and its storage, the install confirmation text, and the wiring in `crates/lodestar` (all CMP-5's or CMP-6's). Take the route as an input, and expose what they call;
- `packaging/systemd/**` (Q20);
- the rest of `supervise/*` (CMP-4a's; one session per branch).

If a read-only accessor is missing in `crates/sources/src/egress.rs` (for example, subscribing to the route state), tell the runner before editing it.

## What to build
1. **Linux limits** (`limits_linux.rs`), through the `AfterSpawn` hook:
   - **rlimits:** apply `RLIMIT_NOFILE`, plus `RLIMIT_AS` when set, with `prlimit` on the child's pid right after spawn. The plan accepts the window of microseconds before they apply.
   - **cgroup v2 only when a delegated subtree is present.** Detect it; never require it. Under the shipped unit, rlimits are what applies. Report which applied.
   - **Landlock (Q3 b):** read and write only the companion's own folder and its downloads folder, plus read-only and execute for what the binary needs (its install folder, system libraries, `/etc` resolver files, `/dev/null`, `/dev/urandom`).
     - **No access** to Lodestar's data folder outside the companion's own, including `secrets.key`, the database, the auth file and `companions/run/` (CMP-4a's pidfiles; keep them out of the writable set).
     - **The ruleset has to apply in the child before it runs untrusted code.** Without `pre_exec` (no `unsafe`), work out how, and say which in the decisions file. One option is a re-exec of Lodestar's own binary as a tiny launcher that restricts itself and then `exec`s the companion. If no safe route exists, stop and tell the runner before building a workaround.
     - Use the best ABI the kernel offers, and report the result: fully, partly or not enforced.
2. **Windows limits** (`limits_windows.rs`):
   - every companion in one Job Object with kill-on-job-close, so companions die with Lodestar;
   - an optional memory cap;
   - the Q3 sandbox only if a safe wrapper exists, else report it as not applied.
   - Assign the process to the job before it can spawn children of its own, or say how the race is closed.
3. **A sandbox report** (`limits.rs`): a typed per-companion result (sandbox applied: full, partial or none, with a reason code; and the limits applied). CMP-5 and CMP-6 show it, and the confirmation uses it.
4. **The egress gate** (`egress.rs`, Q8, Q9, Q18):
   - The route comes in from `crates/lodestar` as a `PluginEgress` built by `EgressFactory`. `crates/companions` never depends on `crates/pipeline`.
   - A companion whose route isn't direct starts only while the route is up. It is stopped when the probe reports the route down, and started again when it's back.
   - A route kind the companion can't follow (per the catalogue: qBittorrent binds an interface or address; Prowlarr uses SOCKS5) means it never starts, with a typed reason for "Can't follow your VPN setting".
   - Configuring the companion's own proxy or bind settings is CMP-5's (through CMP-3's config writers). You expose the decision: which route kind, and allowed or refused.
   - The gate uses the supervisor's stop and start, so restarts while the route is down don't count as crashes towards "Needs attention".
5. **Errors** are typed codes. No `unwrap` or `expect` outside tests.

## Tests and gates
- **`tests/limits.rs`**, using CMP-4a's fake companion (the test binary in `FAKE_MODE`):
  - **Linux:** the fake reports its `RLIMIT_NOFILE` (and `RLIMIT_AS` when set).
  - **Landlock:** where the kernel enforces it, the fake can't read a file in the data folder outside its own, nor `companions/run/`, but can write its own folder. Where the kernel doesn't, the report says so, and the test asserts the report instead.
  - **Windows:** the fake is in the job, and closing the job kills it.
  - **Egress:** with a scripted route state, it doesn't start while down; it's stopped when the route goes down and restarted when it comes back. These restarts never reach "Needs attention", and a route kind the companion can't follow refuses to start.
- **No fixed sleeps.** Waits are conditions with ≥ 10 s deadlines. Dev-only tokio `test-util` is acceptable in `crates/companions` if you say so in the PR; never on the `lodestar` package. Tests run on Windows CI.
- **Before pushing:**
  - `cargo fmt --check`;
  - `cargo clippy --workspace --all-targets -- -D warnings`;
  - `cargo test --workspace`, at minimum `-p lodestar-companions -p lodestar-sources`.
- **Size:** about 750 lines per the plan; stay under 1,500, or state why.

## Decisions you may make (§2.33; record each in the decisions file)
- the crates chosen, within the conditions above;
- how Landlock is applied before the companion runs;
- the default rlimit values (stated, with reasons);
- the sandbox report's shape;
- the API that CMP-5 and CMP-6 call.

**These go to the runner:** anything that loosens Q3 or Q7, changes the systemd unit, adds a privilege or capability, starts a companion on the direct route when its route isn't direct, or needs `unsafe`.

When CI is green, Windows included, message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head SHA, using the Claude Code Remote MCP `send_message` tool.
