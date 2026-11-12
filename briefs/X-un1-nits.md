# X-un1-nits: usenet poll timing, a hold counter, a defensive refusal, the switch hidden until UN-3

**Runner brief, 2026-11-12 (UTC).** Opus, core (pipeline and an untrusted-input plugin). Small: about 200 lines with tests. Run by the UN-1 session, after #762 merged (d6acfead).
- **Branch:** `slice/X-un1-nits` from fresh `origin/main`.
- **PR title:** `X-un1-nits: <title>`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.

These are the nits from the fresh review of #762 (runner-state `reviews/rv-762-885c7717.md`).

## Do
1. **Poll timing** (`crates/sources-plugins/src/prowlarr.rs`, `poll` and `usenet_due`):
   - Mark a Usenet indexer polled only after a 200 whose results were all emitted.
   - A failed, refused or oversized page retries on the normal 15-minute cadence, not after an hour.
   - Results not sent because the poll's 4,096 budget ran out mid-page come back on the next poll, as the `USENET_POLL_ITEMS` doc says. Keep the hourly limit (24 hits a day per indexer) for successful polls: a retry after a failure must still respect a bounded count per day. State the bound in the decisions file.
2. **Hold counter** (`crates/pipeline/src/unverified.rs`, `UnverifiedStats::held`): make the doc and the counting agree for usenet holds. Prefer matching the BitTorrent path's meaning (a non-core hit counted the same way on both networks). Say which in the decisions file.
3. **Defensive refusal** (`crates/pipeline/src/lib.rs` intake, the `retried` branch): refuse a usenet find there (`is_usenet()`), so its key prefix can never reach the coordinator or peer finding as an info hash. Add a unit test.
4. **The `usenet` switch** (runner UX call, §2.26): hide it from the settings form (`CONFIG_SCHEMA`) until UN-3 makes items sendable. Keep the config key working and its default off. Users shouldn't spend their indexers' daily API quota on items they can't see or send. No new user-facing text. UN-3 un-hides it.
5. **Name in the PR** that `crates/pipeline/tests/claim_trackers.rs` was a shared edit in #762.

## Not in this slice
Switching usenet off leaves stored items until a source purge. That goes to UN-4's notes; the runner records it.

## Owns
- `crates/sources-plugins/src/prowlarr.rs` and `crates/sources-plugins/tests/prowlarr_usenet.rs`
- `crates/pipeline/src/unverified.rs`, `crates/pipeline/src/lib.rs` (the intake branch only)
- The file holding Prowlarr's `CONFIG_SCHEMA`, if it's not prowlarr.rs. If item 4 needs web or openapi changes, tell the runner before you push.
- `docs/decisions/2026-11-12-X-un1-nits.md`

## Done
- `cargo fmt --check`;
- `cargo +1.99.0 clippy --workspace --all-targets -- -D warnings`;
- `cargo test -p lodestar-sources-plugins -p lodestar-pipeline`.
- No fixed sleeps. No tokio `test-util` or `testing` feature on the `lodestar` package.

Send the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) `ready <n> <full sha>` once ci-ok, web-ok and e2e-ok are green. A fresh Opus review follows.
