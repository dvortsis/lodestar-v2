# ENR-9: Claim encoding v3: alternative titles, canonical title, genres, year, source category, posted time, newsgroups; fuzzed
**Model:** Opus, with `/security-review` (the notes ask for it). The runner reviews the result with a fresh Opus reviewer. This is the runner's brief, 2026-11-06T16:55Z. The slice launches into the slot WK-4 freed (#536 merged), within owner §2.42's four slots.

## What to build
- **The plan:** `docs/plan/2026-11-05-v07-m7.md`. It's the spec: the ENR-9 row (about line 89), the inventory row (about line 32), and the "may land before the cut, inactive" rule (about line 57: ENR-9 decodes v3, and nothing writes it until a plugin is enabled). Read the whole plan for context. The architecture sections it cites are §6, §18.1 and §18.5.
- **Its `runner/slices.yaml` entry (`ENR-9`), including its notes:**
  - `/security-review`;
  - this is an on-disk format: v1 and v2 blobs keep decoding;
  - IDs stay in v1's `ids`;
  - no personal fields.
- **Owner decisions that bind it:** `docs/owner-decisions.md` §2.46 and §2.47 (the planning-wave answers, v0.7 Q5 in particular: only the parsed title, year and type go out to enrichers), and §2.37. Where a plan row decided a question, the plan row is the decision.
- **The decoder reads bytes from the store, which other versions wrote:** bounded lengths and counts, no panics, canonical encoding only, and unknown versions rejected. Run the existing fuzz target `fuzz/fuzz_targets/claims.rs` for 10 minutes and extend it to v3. Add golden v1, v2 and v3 blobs as fixtures.

## Own
- `crates/store/src/claims.rs`
- `crates/sources/src/claims.rs`
- `fuzz/fuzz_targets/claims.rs`
- its tests, fixtures and fuzz seeds;
- a decisions file, `docs/decisions/2026-11-06-ENR-9.md`;
- its own `runner/slices.yaml` entry (`owns` only, if it changes).

**Running beside it:** three other slices, WK-0, WK-6 and X-consent-gate-052, with disjoint `owns`. WK-6 edits `fuzz/Cargo.toml`. If you need `fuzz/Cargo.toml` or any other file outside your `owns`, message the runner before touching it.

## Done when
- **These pass:**
  - `cargo fmt --check`;
  - `cargo clippy --workspace --all-targets -- -D warnings`;
  - `cargo test` for the crates you touch and their dependants (per crate if the disk is short; say so);
  - the claims fuzz target for 10 minutes;
  - `cd tools/status-connector && npm test`;
  - `/security-review`.
- **No fixed sleeps:** wait on a condition with a deadline of at least 10 s.

Use branch `slice/ENR-9`. Title the PR "ENR-9: <a short imperative title, under 60 characters>". Follow `.claude/skills/steward/SKILL.md`. Never merge. Push once per round. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the `send_message` tool of the Claude Code Remote MCP server.
