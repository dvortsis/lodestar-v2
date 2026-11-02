# SRC-14: Sources API v0.6 (manifest, health, yield; egress, schedule and agents editable; purge as a job)
**Model:** Opus (API surface over credentials, egress and consent). `/security-review` and a fresh Opus reviewer follow. Runner brief, 2026-11-02, written after SRC-2 (#383), SRC-4 (#388) and SRC-5 (#380) merged.

## Why
The Settings → Sources UI (SRC-15a and SRC-15b) needs one API that:
- lists each source with its manifest, consent state, health and yield;
- edits egress, schedule and agents;
- purges a source as a tracked job.

SRC-4 built consent and credentials, and SRC-5 built per-plugin egress and the kill switch. Neither is reported or editable as a whole yet.

## Read first
- `docs/architecture.md` §18.4, §18.6, §18.7, §18.8 and §9 (the API and jobs). Also §10.10.7 and §10.10.8 (the Sources screen this feeds).
- The decisions files `docs/decisions/2026-11-02-SRC-{2,4,5}.md` and the SRC-1 file. They hold the runner's rulings you must keep.
- The current code: `crates/api/src/ops.rs`, `crates/api/src/dto/ops.rs`, `crates/lodestar/src/ops.rs`, `crates/store/src/sources.rs`, `crates/sources/src/{consent,egress}.rs`, and `crates/store/src/snapshots.rs`.

## Must (carried from earlier reviews; each is blocking)
1. **Egress status and metrics:**
   - render the `source_egress_*` metrics and status in the API, and spawn `PluginEgress::monitor`;
   - add an `HttpError` variant so a kill-switch refusal doesn't back off the host;
   - warn when `HTTPS_PROXY` or `ALL_PROXY` is set while a plugin's egress is Direct.
2. **Don't expose the network-interface egress route.** Its DNS goes through the system resolver outside the interface (the owner question q-egress-dns-leak is open; the runner recommends keeping it hidden). The API refuses it with a clear error until a bound resolver exists.
3. **Help text:** name the socks5h LAN caveat, and default to `SocksDns::Local` when the proxy is on the LAN.
4. **Before the verification switch can be turned on:** add a per-source pending cap, or a Low priority class. Dead swarms can hold about 30k of the 200k pool, and `evict_one` ignores the source. If this is too large for the slice, the switch stays off and the API refuses to enable it; say so.
5. **Purge scrubs pre-migration snapshots too.** `crates/store/src/snapshots.rs` keeps `VACUUM INTO` snapshots for 90 days, and `secrets.key` stays in the data directory, so `lodestar rollback` would bring back a purged source's sealed credentials and consent ack.
   - Either drop `sources.secret.<plugin>.*` and `sources.consent.<plugin>` from every snapshot as part of the purge job, or record the purge and refuse a rollback across it.
   - Test it: purge, roll back, and check that no credentials or ack come back.
6. **Consent is never bypassed.** Every enable goes through `enable_source`, with SRC-4's checks and password rules; `set_source_state` refuses `Enabled`. Editing endpoints or egress re-checks consent where SRC-4 says it must.
7. **Secrets never leave the server:** the API reports "set" or "not set" for credentials, and Debug output and logs redact them.

## Do
- **Endpoints:** list, get, PATCH (egress, schedule, agents, priority) and purge (a job over `/jobs`, with progress, cancel and a truthful partial state). Each takes the password where §18.7 or SRC-4 asks for it.
- Regenerate `openapi.json` and `schema.d.ts` with the repo's tooling, never by hand.
- Add metrics and log events to `docs/metrics.md` and the logs catalogue.
- **Tests:** an API test for each must above, plus a crash-in-the-middle-of-purge test.
- **Decisions file:** `docs/decisions/2026-11-02-SRC-14.md`.

## Own
The paths `runner/slices.yaml` gives SRC-14:
- `crates/api/src/ops.rs` and `crates/api/src/dto/ops.rs`;
- `crates/lodestar/src/ops.rs`;
- `crates/store/src/sources.rs`;
- the generated API files.

Plus `crates/store/src/snapshots.rs` (purge scrub only), the minimal hooks in `crates/sources/src/egress.rs` the musts need, and their tests.

**Don't edit:** `crates/lodestar/src/percolator.rs` or `crates/store/src/saved.rs` (SRC-2b is running), `web/src/lib/shell/**` (X-overflow-links), or `web/src/routes/settings/**` (X-settings-overview).

## Size
≲ 1,500 changed lines. If it's bigger, split it into SRC-14a (API and egress) and SRC-14b (purge job and snapshot scrub), and tell the runner first.

## Done when
- fmt, clippy `-D warnings` and `cargo test --workspace` pass;
- the web checks pass, if the generated types changed;
- `/security-review` passes;
- no fixed sleeps in tests.

Branch `slice/SRC-14`. Open the PR titled "SRC-14: Sources API v0.6". Follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the `send_message` tool of the Claude Code Remote MCP server. Never merge. Push once per round. Work frugally. Don't background long jobs with `&`.
