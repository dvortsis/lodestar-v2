# CMP-5: Companion manager (install flow, pinned updates keeping the previous version, uninstall, state rows, companion CLI)

**Model:** Opus. **Review:** a fresh Opus reviewer plus `/security-review`. It runs downloaded programs. **Branch:** `slice/CMP-5`, cut from `origin/main`: CMP-1, CMP-2, CMP-3, CMP-4a, CMP-4b (#608, c748dba9), CMP-7 and CMP-8a are in. **PR title:** `CMP-5: <title>`. **Steward:** follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.

## Read first
- **`docs/plan/2026-11-06-v06-companions.md`:**
  - §3 row;
  - §4.2 shared files;
  - §4.3's **CMP-5** bullet, which is the spec;
  - §5 **Q6, Q10, Q17, Q18** (and Q7 for health cadence, Q8/Q9 for routes).
- **`docs/architecture.md`:** §11 and §18.10.
- **`crates/companions/src/` as built:**
  - `catalogue.rs` and `adapter.rs` (CMP-0);
  - `fetch*` (CMP-1);
  - `extract*` (CMP-2);
  - `credentials.rs` and `config_files.rs` (CMP-3);
  - `supervise/` (CMP-4a, CMP-4b: `launcher_main()`, the `egress` gate, limits);
  - `compose.rs` and `unraid.rs` (CMP-7);
  - `detect/` (CMP-8a).
- **The decisions files** for CMP-0 through CMP-4b in `docs/decisions/`, especially the residuals.
- **`runner/standards.md`** is the bar.

## Owns
- `crates/companions/src/manager.rs`
- `crates/companions/src/state.rs`
- `crates/companions/tests/manager.rs`
- `crates/lodestar/src/companions.rs` (you create it; CMP-5g adds one wiring line later)
- `crates/lodestar/src/cli/companion.rs`

**Shared edits allowed (one-line or minimal):**
- `crates/lodestar/Cargo.toml`;
- `crates/lodestar/src/services.rs`;
- `crates/lodestar/src/cli/mod.rs`;
- `crates/lodestar/src/main.rs`, for the launcher hook below;
- `crates/companions/src/lib.rs`, mod lines;
- `crates/companions/src/log_events.rs`, new events with their registrations (`crates/lodestar` logging catalogue and golden, `event-names.ts`, en/zh messages). Events carry the companion id, version and codes only;
- one `docs/decisions/2026-11-08-CMP-5.md` (or the date you open it).

**State storage:**
- If the journal and state rows need a store table or a migration, stop and ask the runner first. The plan says "no migration".
- Prefer files under `<data>/companions/` written atomically, with the formats CMP-3 already uses.

## What to build
1. **The install flow as one state machine with a journal.**
   - A crash at any step leaves either the old version or nothing, never a half-install.
   - Steps: confirm (an explicit confirmation token naming entry, version and SHA-256; CMP-6 will carry it from the API) → fetch → verify → extract → credentials (CMP-3) → config files → start (CMP-4a/4b) → health → configure through the API (CMP-8b's `Configure`, behind a trait object so this works before CMP-8b merges) → done.
   - Each step is resumable or reversible from the journal on startup.
2. **Updates keep the previous version until the new one passes health,** then remove it. A failed update rolls back to the previous version and restarts it.
3. **Uninstall:**
   - stops the companion, removes its folders under `<data>/companions/<id>/` and its credentials row;
   - says exactly what it deletes;
   - never touches anything outside that folder.
4. **Refusals, typed:**
   - native installs inside a container (Q17);
   - unsupported platforms;
   - no Windows qBittorrent entry (CMP-11 owns that).
5. **CLI:** `lodestar companion list|install|remove|compose` (compose prints CMP-7's output).
   - Install from the CLI needs the same explicit confirmation: the user types or passes the shown SHA-256, never a bare `--yes`.
6. **The launcher hook (from #608):**
   - `lodestar_companions::supervise::launcher_main()` (or its exact name) runs **first** in `crates/lodestar/src/main.rs`, before anything else reads args, logging or config.
   - Add a test that the sentinel argv reaches it.
   - On Windows it must be a no-op or absent, per CMP-4b.
7. **The companion snapshot for CMP-10:** publish a `watch` channel of installed or detected companions with their health.
   - If CMP-10 has merged when you open the PR, wire it as `PipelineConfig::companions: Some(rx)` in `crates/lodestar` with a test. The snapshot also carries whether any `integrations.clients` entry exists, for `download_clients` (runner decision).
   - Otherwise expose the channel and leave a TODO naming CMP-5g. Tell the runner which case applies.

## Carried follow-ups (you must handle or explicitly hand off in the decisions file)
- **From #608:**
  - **Route-state freshness.** If the egress monitor dies while `PluginEgress` stays alive, a stale `Up` keeps the companion released. Make the gate's input expire, or treat a monitor exit as down.
  - **`setsid` grandchildren** survive a hold on Linux, because stop signals the process group only. Note it, and stop by Job/cgroup where available, or document it for CMP-G.
  - **Re-assert qBittorrent's bind and Prowlarr's proxy on every start,** by running CMP-8b's plan or apply after each start for installed companions.
  - **Config writers must not follow symlinked parents** inside the companion's writable home (`O_NOFOLLOW`/`openat2` `RESOLVE_NO_SYMLINKS`, as CMP-4b does).
- **From #611 (CMP-8b):**
  - Store the new credential row durably **before** CMP-8b changes a password (`PasswordChange` doc).
  - For installed companions the origin is `Installed` via CMP-8b's `InstallReceipt`. Only this crate can create it, so build it in the manager after a successful install.
- **From #610 (CMP-8a):** never log `ProbeResponse` with `{:?}`.

## Tests and gates
- **A crash at each step:** use an injected fault point per step, then restart. You end with the old version or nothing, and the journal is clean.
- **Update:**
  - with health pass, the previous version is removed;
  - with health fail, it rolls back and the previous version runs.
- **Uninstall** removes exactly the companion's folder and credentials.
- **Refusals:** container plus native, an unsupported platform, a missing or wrong confirmation token.
- **The launcher hook test,** and the CMP-10 wiring test if applicable.
- **No fixed sleeps.** Waits are conditions with ≥ 10 s deadlines. Tests run on Windows CI.
- **Before pushing:**
  - `cargo fmt --check`;
  - `cargo clippy --workspace --all-targets -- -D warnings`;
  - `cargo test -p lodestar-companions -p lodestar`.
- **Size:** about 1,000 lines per the plan. Stay under 1,500, or state why.

## Decisions you may make (§2.33; record each)
- the journal format and location;
- the state machine's shape;
- the confirmation-token shape (CMP-6 consumes it);
- the CLI flags;
- the snapshot type.

**These go to the runner:**
- a store migration;
- any install without explicit confirmation;
- anything that runs a program not verified by CMP-1;
- a change to `packaging/systemd/lodestar.service` (owner Q20);
- widening CMP-4b's sandbox or grants.

## Coordination
These run in parallel:
- **CMP-8b** (#611): `configure/`;
- **CMP-10:** `requires.rs`, pipeline;
- **X-cmp4b-mounts:** `supervise/limits*`;
- **WK-10e:** cluster.

Don't edit their files. `crates/companions/src/lib.rs` mod lines may conflict trivially; bring `main` in only when needed.

When CI is green, Windows included, message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and the full head SHA, using the Claude Code Remote MCP `send_message` tool.
