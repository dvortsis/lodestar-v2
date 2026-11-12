# X-m8-net-e2e-nits: follow-ups from the #763 review

**Runner brief, 2026-11-12 (UTC).** Opus, tests and docs. Small, about 100 lines. Run by the M8-NET-E2E session after #763 merged (423a91b1).
- **Branch:** `slice/X-m8-net-e2e-nits` from fresh `origin/main`.
- **PR title:** `X-m8-net-e2e-nits: <title>`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.
- The review: `origin/runner-state:reviews/rv-763-30223eca.md`.

## Items
1. **Correct the harness description** (review Important 1). Your #763 decisions file, line 18, says the client runs an iterative `get_peers` lookup. It doesn't: `Network::closest` picks the 8 closest from the test's own id list, and `Client::announce` sends `get_peers` and `announce_peer` to them directly. Don't edit the merged file. Write the correction in `docs/decisions/2026-11-12-X-m8-net-e2e-nits.md`, so M8-CHAOS-1 doesn't assume announces exercise lookup convergence. Also correct the "drop guard keeps the data dir" wording (Optional 5) there.
2. **The bar is exclusive.** `e2e_search.rs:317` fails on `> BAR`; make it `>=`, matching the owner's "< 5 s".
3. **Doc comment** at `net.rs:104-108`: move the `dropped_and_timeouts` comment onto its own function.
4. **Failed-run cleanup** (Optional 4). On a panic before `app.shutdown()`, the temp dir can stay behind on Windows. Make the teardown order robust, or leave it and say why, in the decisions file. No fixed sleeps.
5. **macOS time.** Read e2e_search's per-hash max from the macOS job on `main` at or after 423a91b1, and record it in the decisions file. Write no CI job ids into the repo. If macOS is over the bar, stop and tell the runner before changing anything.

## Owns
- `crates/crawler-sim/tests/e2e_search.rs`
- `crates/crawler-sim/tests/e2e/**`
- `docs/decisions/2026-11-12-X-m8-net-e2e-nits.md`

## Done
- `cargo fmt --check`;
- `cargo +1.99.0 clippy -p lodestar-crawler-sim --all-targets -- -D warnings`;
- `cargo test -p lodestar-crawler-sim --test e2e_search`.

Send the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) `ready <n> <full sha>` once ci-ok, web-ok and e2e-ok are green.
