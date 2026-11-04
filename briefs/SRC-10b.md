# SRC-10b: AnimeTosho plugin
**Model:** Sonnet. Glue on SRC-7's plugin pattern and SRC-6's audited Torznab parser. The runner reviews on Opus. Runner brief, 2026-11-04.

## Why
This plugin is on the v0.6 sources critical path (SRC-10* → SRC-G). AnimeTosho publishes a Torznab-style feed (§18.2 rank 13).

## Read first
- `crates/sources-plugins/README.md`. Copy the pattern of `src/prowlarr.rs` with `tests/prowlarr.rs`, and of `src/nyaa.rs` with `tests/nyaa.rs` (SRC-10a): fixture contract tests, no network.
- SRC-6's parser in `crates/sources-feeds/src/torznab.rs` and its decisions file.
- `docs/architecture.md` §18.1, §18.2 (the AnimeTosho entry) and §18.7.
- `docs/plan/2026-11-01-v06-sources.md` §4.2 SRC-10a–e and §5 Q2/Q4 (AnimeTosho polls no faster than every 5 min).
- `docs/owner-decisions.md` §2.28 Q2: record the ToS and robots check for AnimeTosho's feed in the decisions file. If it's a grey case, stop and tell the runner.

## Do
1. **`src/animetosho.rs`:**
   - **Manifest:**
     - External consent;
     - fixed endpoints (`feed.animetosho.org` only), so `Endpoints::manifest`;
     - no credentials;
     - polls no faster than every 5 min.
   - **Parsing:** use only SRC-6's parser and its limits. Write no new parsing of untrusted text.
   - **Infohash:** from `torznab:attr infohash` or the magnet only.
   - **Claims:** title, size, category, seeders and leechers, plus the **file list and media info**. Each is Low confidence and unverified, through SRC-2.
   - **Never fetch** the `.torrent`, NZB or view page.
2. **Fixtures** in `fixtures/animetosho/`, scrubbed (say if they're hand-written). Cover:
   - a normal page;
   - an empty feed;
   - malformed or oversized input;
   - a missing infohash;
   - a duplicate item;
   - 429 and 5xx answers.
3. **Tests** in `tests/animetosho.rs`, following the Nyaa tests: no network, no fixed sleeps.
4. **Registration:** add only the `pub mod` line to `crates/sources-plugins/src/lib.rs`. Don't wire it into `services.rs` or the pipeline.
5. **Decisions file:** `docs/decisions/2026-11-04-SRC-10b.md`, with the Q2 check, the consent class, the poll interval and the category and claims map.

## Own
`crates/sources-plugins/src/animetosho.rs`, `fixtures/animetosho/**`, `tests/animetosho.rs`, the one `pub mod` line, and your decisions file.

**Don't edit** `crates/sources/**`, `crates/pipeline/**`, `crates/lodestar/**` or `crates/sources-feeds/**`. If the parser lacks something, message the runner first.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test -p lodestar-sources-plugins` pass, and a fresh self-review is done.

Branch `slice/SRC-10b`. Open the PR titled "SRC-10b: AnimeTosho plugin". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the `send_message` tool of the Claude Code Remote MCP server. Never merge. Push once per round. Work frugally. Don't background long jobs with `&`.

## Runner decisions (2026-11-04, after the first session was blocked)
- The owner has added `animetosho.org` and `feed.animetosho.org` to the environment's allowed domains. **Verify the feed host and the Q2 terms and robots check yourself** before fixing the endpoint in the manifest. If the check is grey, stop and tell the runner.
- **Claims:** title, size, category, ids and the file count. Drop seeders and leechers, as SRC-10a does. The file list and media info wait for a follow-up parser slice (SRC-6b), because SRC-6's `Item` has only a file count. Name the deferral in the decisions file.
- Fixtures may be scrubbed recordings of the real feed (preferred) or hand-written in its exact shape. Say which.
