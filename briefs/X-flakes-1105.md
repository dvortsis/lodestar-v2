# X-flakes-1105: two timing flakes, fixed once
**Model:** Opus (DHT core test; importer concurrency). A fresh Opus reviewer follows. Runner brief, 2026-11-05T06:20Z.

## Why
Each of these tests failed once on 2026-11-05 and passed on re-run. Per CLAUDE.md, a flake gets fixed once, not re-run onto the next PR. Neither test is owned by a running slice.

1. **`lodestar-dht` `node::actor::restart_wire::no_restored_peer_is_sampled_before_its_deadline`** (`crates/dht/src/node/actor.rs:3585`).
   - Failed on Windows in #454's `test (windows-latest)` job: the `samples_received(..).all(n == 0)` assert. It passed on Linux.
   - It's timing-sensitive around a deadline.
2. **`lodestar-sources-importers` `magnetico::tests::file_less_parents_stay_within_the_byte_budget`** (`crates/sources-importers/src/magnetico/tests.rs:1000`).
   - Failed with `open: NotQuiescent` in a review's local run.

## Do (branch `slice/X-flakes-1105` from `origin/main`)
For each test:
1. Find the root cause by reading the test and the code it drives. Name the race: clock granularity on Windows, a paused-time vs real-time mix, a background task not yet quiescent, or an ordering assumption.
2. Fix the test, or the code if the race is real there, so the test asserts the same property deterministically.
   - Wait on conditions with a deadline of 10 s or more.
   - Use tokio's paused time where the code allows it.
   - No fixed sleeps. Never skip, retry, ignore or loosen what the test proves.
   - If the race is a real product bug, fix it, and say so.
3. Prove it.
   - Run each test 200 times: `cargo test -p lodestar-dht --lib <name> -- --exact` in a loop, or `cargo nextest` if present. Report the counts.
   - For the DHT test, also run it under load, with `taskset -c 0` or `stress` if available, to widen the window. State what you ran.
4. Write `docs/decisions/2026-11-05-X-flakes-1105.md` (short): each cause and fix.

## Own
- the two test functions and any test helper they use;
- the minimal code fix, if the race is in product code: the smallest change, in `crates/dht/src/node/actor.rs` or `crates/sources-importers/src/magnetico/**`; justify it in the decisions file;
- the decisions file.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and the two crates' tests pass. Open the PR against `main` with the title "X-flakes-1105: fix the restart_wire deadline and magnetico NotQuiescent flakes". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the repeat counts, using the Claude Code Remote MCP `send_message` tool. Never merge. Push once per round.
