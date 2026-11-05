# X-sp4-followups: the SP-4 pre-hiding guard and a tighter set index
**Model:** Opus, with `/security-review` (content filter, attacker-chosen input, on-disk set). The runner reviews with a fresh Opus reviewer. Runner brief, 2026-11-05T23:40Z. Launch once RP-5 (#510) merges.

## Why
RP-5 (#510) built SP-4: when SP-2 or SP-3 hides or demotes a stored torrent, its payload signature (a hash of file sizes) joins a capped set, and later torrents with the same payload inherit the action. Its decisions file names two follow-ups, which the runner decided:
1. **The pre-hiding threat.** A payload signature is only file sizes. Someone who publishes a keyword-stuffed torrent with the exact sizes of a popular release gets it hidden by SP-2, and SP-4 then hides later copies of that real release. §20.6 gives SP-4 no guard.
2. **Memory.** The set takes about 45 MB in memory at its 1M cap: 8 B per entry plus a std `HashMap` index. A tighter index, using a `u32` location or sorted runs, can win back about half.

## Do
1. **The guard (the runner's call, flagged to the owner):** don't add a signature to the set when the store already holds a torrent with that payload that isn't hidden or demoted by a spam rule. Such a payload is already known as a legitimate release.
   - It must be decided at the writer's commit, in commit order, with no store transaction held while waiting on the set's lock (RP-5's lock order).
   - It needs a payload-to-torrent lookup. Find out whether the store or search index already has one (SB-6's `payload_sig` field, CS-8a's `PayloadSig`). If it needs a **new index or migration**, report the design, its disk and its ingest cost to the runner before building it. That is an on-disk format change.
   - Its cost on a crawl commit must stay within RP-5's 5% writer gate (`sp4_writer_cost`). Measure it.
   - Tests: a planted original whose payload matches an unhidden stored release is never added; a genuinely new spam payload still is; a later copy of the real release passes; and the order of arrival within one batch.
   - The decisions file states what the guard costs in recall: spam that copies a real release's sizes isn't cloned.
2. **The tighter index:** cut the set's memory at the cap to about 25 MB or less, without changing the on-disk rows, the load rules (fail closed per row, last entry wins, cap re-applied) or any SP-4 behaviour.
   - The hash-flooding defence must hold: random-keyed hashing, or an ordered structure that isn't keyed by attacker input.
   - Measure memory at the cap and lookup time before and after.
   - RP-5's tests, including the reload property test, pass unchanged.
3. **Decisions file:** `docs/decisions/2026-11-06-X-sp4-followups.md`, with the numbers.

## Own
- `crates/filter/src/spam_sigs.rs`, `crates/filter/tests/sp4.rs`;
- `crates/pipeline/src/writer.rs`, only the SP-4 add path;
- the store lookup the guard needs, in `crates/store/src/filter.rs` (report before any migration);
- the decisions file;
- your own `runner/slices.yaml` entry.

Don't edit `crates/filter/tests/props.rs` or `packs_gate.rs`: RP-G owns them.

## Done when
These pass:
- `cargo fmt --check`;
- `cargo clippy --workspace --all-targets -- -D warnings`;
- `cargo test --workspace` (per crate if the disk is short; say so);
- the release-mode `sp4_writer_cost` within the 5% gate;
- `cd tools/status-connector && npm test`.

Branch `slice/X-sp4-followups`. Open the PR titled "X-sp4-followups: SP-4 pre-hiding guard and a tighter set index". Follow `.claude/skills/steward/SKILL.md`. Never merge. Push once per round. No fixed sleeps. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the `send_message` tool of the Claude Code Remote MCP server.
