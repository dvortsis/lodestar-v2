# SRC-14b: purge as a job, the snapshot scrub, and egress hardening
**Model:** Opus (on-disk data, credentials, concurrency). `/security-review` and a fresh Opus reviewer follow. Runner brief, 2026-11-02, written after SRC-14a (#394) merged.

## Why
SRC-14a shipped the Sources API and egress wiring. This finishes SRC-14:
- purging a source becomes a tracked job that leaves no trace, including in rollback snapshots;
- the egress gaps the #380 and #394 reviews left open get closed.

## Read first
- The briefs: `git show origin/runner-state:briefs/SRC-14.md` (must 5 is yours) and this file.
- The decisions files `docs/decisions/2026-11-02-SRC-{4,5,7,14a}.md`, plus the SRC-1 and SRC-2 files.
- `docs/architecture.md` §18.7 and §18.8 (purge), §9 (`/jobs`) and §7 (snapshots and rollback).
- The code: `crates/store/src/{sources,snapshots}.rs`, `crates/api/src/ops.rs`, `crates/lodestar/src/ops.rs`, `crates/pipeline/src/{control,lib}.rs`, and `crates/sources/src/{http,egress}.rs`.

## Do
1. **Purge as a job:**
   - `POST /sources/{id}/purge` runs over `/jobs`, with progress, cancel and a truthful partial state. It takes the password where SRC-4 and §18.7 require it.
   - It first stops the source and drops its in-memory pool keys.
   - The final Purged write drops egress, schedule, agents, credentials and the consent ack in one transaction. SRC-14a's decisions file says that egress, schedule, agents and credentials can still be written on a Purged source until then.
   - Test a crash in the middle: what was committed is reported truthfully, and the next run resumes or finishes.
2. **The snapshot scrub (blocking).** The pre-migration snapshots (`VACUUM INTO`, kept 90 days) must not bring back a purged source's sealed credentials or consent ack through `lodestar rollback`, because `secrets.key` stays in the data directory.
   - Either scrub `sources.secret.<plugin>.*` and `sources.consent.<plugin>` from every snapshot as part of the purge job, or record the purge and refuse a rollback across it. Say which, and why.
   - Test it: purge, roll back, and check that no credentials or ack come back.
3. **Egress hardening:**
   - **IPv4-mapped addresses** (`[::ffff:0.0.0.0]`, `[::ffff:224.0.0.1]`) currently pass `usable_proxy` and `parse_proxy`. Convert with `to_ipv4_mapped()` before the check, in all three places (the API parser, the stored value and the factory). Add tests.
   - **A route-level delay after `EgressDown`** in `PluginEgress`, so a plugin's retry loop can't spin while the kill switch is down. Make it bounded, with a test.
   - **Scope the `http.rs` 429/Retry-After block and the 5xx back-off to the poll unit,** not the whole host. Today one Prowlarr indexer that rate-limits us blocks the whole manager for up to 24 h. Keep host-level protection for host-wide failures (connect errors), and test both.
4. **SRC-14a's decisions file, lines 89 and 102:** correct "keeps its default". That's true only for priority, schedule and agents, not egress.
5. **Don't register Prowlarr or Nyaa yet.** Prowlarr waits for real recordings; Nyaa waits for the owner's Q2 check. Note in your decisions file that the scoped back-off is a precondition for Prowlarr.
6. **Decisions file:** `docs/decisions/2026-11-02-SRC-14b.md`.

## Own
- `crates/store/src/{sources,snapshots}.rs`, `crates/api/src/{ops,dto/ops}.rs` and `crates/lodestar/src/ops.rs`;
- `crates/pipeline/src/{control,lib}.rs` (purge and egress only);
- `crates/sources/src/{http,egress}.rs`;
- the generated API files, and your tests and decisions file.

**Don't edit** R-mem-triage's area, a release blocker that's running now: `crates/sources/src/pool.rs`, `crates/pipeline/src/worker.rs`, `crates/cluster/**`, `crates/log/**` and `crates/dht/**`. If you need one of them, message the runner first.

## Size
≲ 1,500 changed lines. If it's bigger, split it (for example, 14b-1 purge and scrub, 14b-2 egress hardening) and tell the runner first.

## Done when
- fmt, clippy `-D warnings` and `cargo test --workspace` pass;
- the web checks pass if the generated types changed;
- `/security-review` passes;
- no fixed sleeps in tests.

Branch `slice/SRC-14b`. Open the PR titled "SRC-14b: purge job, snapshot scrub, egress hardening". Follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the `send_message` tool of the Claude Code Remote MCP server. Never merge. Push once per round. Work frugally: the owner is near the weekly usage limit. Don't background long jobs with `&`.
