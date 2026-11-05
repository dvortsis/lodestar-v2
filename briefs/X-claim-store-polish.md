# X-claim-store-polish: the four polish items from #512's review
**Model:** Opus. This is small, but it tests a security guard's path. The runner reviews the result. Runner brief, 2026-11-05T23:25Z.

## Why
#512 (X-claim-store) merged at 01d956f3. It stores claimed trackers in the guard's canonical form. Its fresh Opus review gave MERGE, with four polish items. The runner deferred them so CI wouldn't restart; they're done here.

## Do
1. **P1, a pipeline-level property test** in `crates/pipeline/tests/claim_trackers.rs` (proptest), for `unverified::tracker_url` and `stored_trackers`:
   - every kept form `k` satisfies `tracker_url(k) == Some(k)` and is at most 2,048 bytes;
   - `stored_trackers` returns at most 64 entries, with total bytes within `room` and no duplicates;
   - every entry is a fixed point, and the order is the claim's order.
   - Mix the inputs: scheme case, `%`, `[]`, IDNA hosts (`ü`, `ⓛ`, `。`), ports 0–70000, paths with `@ ? # \ " ^` and spaces; 0–120 colliding spellings; `room` from 0 to 300.
2. **P2:** the comment in `fuzz/fuzz_targets/verified_claims.rs:226-232` says "so the pipeline stores it unchanged", but the target never calls the pipeline. Reword it to state what the target checks (the guard's fixed point) and point to the P1 test for the pipeline.
3. **P3:** add a clause to `docs/decisions/2026-11-05-X-claim-store.md` saying that first-fit can trade one tracker for another in a near-full pack. Example: `[http://t.example, udp://u.example:1/a]` with exactly 35 bytes of room. It is deterministic and within the clamp's slack.
4. **P4:** fix the doc comment's early line break at `crates/pipeline/src/unverified.rs:196-197`.
5. If the new proptest finds a real defect, stop and message the runner before fixing it.

## Own
- `crates/pipeline/tests/claim_trackers.rs`;
- `crates/pipeline/src/unverified.rs` (doc comment only);
- `fuzz/fuzz_targets/verified_claims.rs` (comment only);
- the X-claim-store decisions file;
- your own `runner/slices.yaml` entry and `dashboard.yaml` placement.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test -p lodestar-pipeline` all pass, plus `cd tools/status-connector && npm test`.

Branch `slice/X-claim-store-polish`. Open the PR titled "X-claim-store-polish: Property test and doc fixes from #512's review". Follow `.claude/skills/steward/SKILL.md`. Never merge. Push once per round. No fixed sleeps. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the `send_message` tool of the Claude Code Remote MCP server.
