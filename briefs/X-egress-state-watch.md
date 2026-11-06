# X-egress-state-watch: the egress's source-on check must not depend on the API's store lock
**Model:** Opus. The runner reviews the result with a fresh Opus reviewer. Runner brief, 2026-11-06T04:10Z, from the fresh review of #525 (X-tcache-recheck, merged at 50519837). This is an availability fix that keeps #525's fail-closed rule; it's the runner's call.

## Finding
- **Where:** `PluginEgress::source_on` (`crates/sources/src/egress.rs`, around line 903) asks `EgressCredentials::source_enabled` before every plugin HTTP request, and refuses when there's no answer within `STATE_WAIT` (2 s). The store-backed `SourceCredentials::source_enabled` (`crates/sources/src/consent.rs`, around line 445) runs `store.lock()` and then `sources()` on the **shared `Mutex<Store>`** that the API and Control also use. It's wired in `crates/lodestar/src/services.rs`, around line 325.
- **Scenario:** a long API or Control operation holds that lock for more than 2 s, for example a large import step, a purge, or a slow disk. Every HTTP plugin request is then refused for that time. It fails closed, which is correct, but it costs availability and fills `policy_refusals` with noise.

## Do
1. Make the source-on check independent of the shared lock, without letting it go stale. **Preferred design (a):** read the source's state on a dedicated read-only store connection, such as the one the torrent cache's re-check already uses, or a new one opened beside it. A committed write by *any* writer is then seen at once, whether that's the toggle, a credential lapse, a purge or the startup disables in `crates/pipeline/src/lib.rs`, and there's no second copy to keep in step.
   - **Alternative (b), only if (a) can't work:** an in-memory `watch` of source states, published by every writer of `SourceState`. On disable, publish before the store write; on enable, publish after the commit. A test must cover each writer. If you choose (b), say in the decisions file why (a) wasn't possible.
2. **Keep fail-closed exactly as #525 has it:** no answer within `STATE_WAIT`, a read error, or a missing row all mean refuse.
3. **Fold in an API doc fix:** the `restart_required` doc in `crates/api/src/dto/ops.rs` still says a source "starts, stops or picks up its new settings at the next restart". Since #525, a stop is immediate. Fix the doc, then regenerate `web/src/lib/api/openapi.json` and `web/src/lib/api/schema.d.ts` with the repo's own tooling, never by hand.
4. **Tests**, with no fixed sleeps and every wait a condition with a deadline of 10 s or more:
   - with the shared store mutex held by the test for longer than `STATE_WAIT`, an enabled source's request still goes out;
   - a source turned off is refused on the next request, at the transport;
   - a read error, or a missing row, refuses;
   - the existing #525 egress and M2 tests still pass unchanged.
5. **Decisions file:** `docs/decisions/2026-11-06-X-egress-state-watch.md`, naming the design chosen and its consistency argument.

## Own
- `crates/sources/src/consent.rs`: `SourceCredentials::source_enabled` and whatever constructs it;
- `crates/sources/src/egress.rs`: only `source_on`, `STATE_WAIT` and `EgressCredentials` if the trait has to change;
- `crates/lodestar/src/services.rs`: the wiring lines only;
- a reader-connection helper in `crates/store`, if you need one. Keep it small, and add no schema change;
- `crates/api/src/dto/ops.rs` (the doc line), `web/src/lib/api/openapi.json` and `web/src/lib/api/schema.d.ts` (regenerated);
- their tests, the decisions file, and your own `runner/slices.yaml` entry. Use the one-line flow style with `deps: []`, under the X-tcache-recheck follow-ups. Add a `runner/dashboard.yaml` placement only if no rule covers it.

Don't touch `crates/pipeline/src/control.rs`: X-consent-gate-052 owns it next. If design (b) needs it, stop and message the runner.

## Done when
These pass: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test` (per crate if the disk is short; say so), the web `npm run check` (because `schema.d.ts` changes), and `cd tools/status-connector && npm test`. Also run `/security-review`, since this check guards egress.

Branch `slice/X-egress-state-watch`. Open the PR titled "X-egress-state-watch: Check a source is on without the shared store lock". Follow `.claude/skills/steward/SKILL.md`. Never merge. Push once per round. No fixed sleeps. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the `send_message` tool of the Claude Code Remote MCP server.
