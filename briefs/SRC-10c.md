# SRC-10c: Internet Archive plugin
**Model:** Sonnet. Glue on SRC-7's plugin pattern and SRC-9's Internet Archive JSON parser (`crates/sources-feeds/src/archive_org.rs`). The runner reviews on Opus. Runner brief, 2026-11-04.

## Why
Internet Archive is §18.2 rank 6 and documents its APIs. It's on the v0.6 sources critical path (SRC-10* → SRC-G).

## Read first
- `crates/sources-plugins/README.md`. Copy the pattern of `src/prowlarr.rs` and `src/nyaa.rs` (SRC-10a) with their tests: fixture contract tests, no network.
- SRC-9's Internet Archive JSON parser (`crates/sources-feeds/src/archive_org.rs`) and its decisions file.
- `docs/architecture.md` §18.1, §18.2 (this source's entry) and §18.7.
- `docs/plan/2026-11-01-v06-sources.md` §4.2 SRC-10a–e and §5 Q2/Q4.
- `docs/owner-decisions.md` §2.28 Q2: record the ToS and robots check for each endpoint in the decisions file. If any is a grey case, stop and tell the runner.

## Do
1. **`src/archive_org.rs`:**
   - **Manifest:**
     - the consent class per SRC-4 and §18.7 (confirm it in the decisions file);
     - fixed manifest endpoints only;
     - no credentials;
     - polling at ≤ 1 request/s, with `since` cursors so each poll fetches only what's new (Q4).
   - **Parsing:** use only the audited parser and its limits. Write no new parsing of untrusted text beyond mapping its typed output.
   - **Infohash:** from the parser's typed output only.
   - **Claims:** title, size, category and files where the source gives them. Each is Low confidence and unverified, through SRC-2.
   - **Never fetch** `.torrent` files or view pages.
   - **Cursors:** persist the `since` cursor through the plugin state (SRC-0's `PluginState`), so a restart resumes without refetching.
2. **Fixtures** in `fixtures/archive_org/`, scrubbed (say if they're hand-written). Cover:
   - a normal page;
   - an empty answer;
   - malformed or oversized input;
   - a missing infohash;
   - a duplicate;
   - 429 and 5xx answers.
3. **Tests** in `tests/archive_org.rs`: no network, no fixed sleeps.
4. **Registration:** add only the `pub mod` line to `crates/sources-plugins/src/lib.rs`. Don't wire it into `services.rs` or the pipeline.
5. **Decisions file:** `docs/decisions/2026-11-04-SRC-10c.md`, with the Q2 checks, the consent class, the rate limit and the claims map.

## Own
`crates/sources-plugins/src/archive_org.rs`, `fixtures/archive_org/**`, `tests/archive_org.rs`, the one `pub mod` line, and your decisions file.

**Don't edit** `crates/sources/**`, `crates/pipeline/**`, `crates/lodestar/**` or `crates/sources-feeds/**`. If the parser lacks something, message the runner first.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test -p lodestar-sources-plugins` pass, and a fresh self-review is done.

Branch `slice/SRC-10c`. Open the PR titled "SRC-10c: Internet Archive plugin". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the `send_message` tool of the Claude Code Remote MCP server. Never merge. Push once per round. Work frugally. Don't background long jobs with `&`.
