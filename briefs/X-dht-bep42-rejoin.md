# X-dht-bep42-rejoin: look up the new id's neighbourhood after a BEP 42 id change

**Runner brief, 2026-11-12 (UTC).** Opus, core (the DHT actor). Small: about 150 lines with tests.
- **Branch:** `slice/X-dht-bep42-rejoin` from fresh `origin/main`.
- **PR title:** `X-dht-bep42-rejoin: <title>`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.

## Why
This comes from rv-756 N-3 (runner-state `reviews/rv-756-3f950dff.md`), and it predates #756. `Actor::on_external_ip` (`crates/dht/src/node/actor.rs`, ~1604) takes new BEP 42 ids when a quorum reports a new external address, and rebuilds the routing tables around the new `ids[0]`. A self-lookup that is still running (or already done) targeted the old id. So on a typical public start, the levels near the new id wait for the 15-minute stale refresh, and `join_targets` counts levels against the new id using nodes found near the old one.

## Goal
When `on_external_ip` changes the ids, queue exactly one self-lookup for the new `ids[0]` in each family whose table was rebuilt. Keep the join and bootstrap accounting right (the parked or `join_planned` logic from X-dht-lookup-converge, #756), so it neither double-counts nor ends the bootstrap early.

## Owns
- `crates/dht/src/node/actor.rs` and its in-file tests.
- `docs/decisions/2026-11-12-X-dht-bep42-rejoin.md`.

Tell the runner **before** pushing if anything else needs to change (for example `crates/dht/src/node/*.rs` beyond actor.rs, or crawler-sim).

## Requirements
1. **Once per change.** Repeated votes for the same address don't queue more lookups, and nothing changes when the id is exempt or already valid (the early returns stay).
2. **Bounded.** The lookup goes through the existing lookup paths and limits, with no new unbounded queue. If a self-lookup for the old id is still running, say in the decisions file whether it's cancelled or left to finish, and why.
3. **Shards.** With a shard set (WK-4), the new ids stay inside the shard's slots, as today.
4. **No behaviour change** when no id change happens: existing actor tests pass unchanged.

## Tests
- An actor test where the external-IP quorum changes the id mid-bootstrap, then a self-lookup for the new `ids[0]` is issued (observed through the outgoing `find_node` targets or the lookup state). One extra lookup only, after a repeated vote.
- An id change after bootstrap has finished also issues it.
- An exempt address issues none.

No fixed sleeps: use the actor's clock or condition waits with deadlines of at least 10 s. No tokio `test-util` or `testing` feature on the `lodestar` package.

## Done
- `cargo fmt --check`;
- `cargo +1.99.0 clippy --workspace --all-targets -- -D warnings`;
- `cargo test -p lodestar-dht`, plus crawler-sim's tests if it depends on the actor.

Send the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) `ready <n> <full sha>` once ci-ok, web-ok and e2e-ok are green. A fresh Opus review follows.
