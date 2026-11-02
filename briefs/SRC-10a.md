# SRC-10a: Nyaa RSS plugin (Nyaa only)
**Model:** Sonnet. It follows SRC-7's plugin pattern and reuses SRC-8's audited RSS parser. The runner reviews on Opus. Runner brief, 2026-11-02.

## Why
This is the second v0.6 source plugin and the first public feed. It proves the SRC-7 pattern (`crates/sources-plugins/README.md`) works for a non-account source.

## Read first
- `crates/sources-plugins/README.md`, and `src/prowlarr.rs` with `tests/prowlarr.rs` as the pattern to copy: fixture contract tests, no network.
- SRC-8's parser: `crates/sources-feeds`, the RSS 2.0/Atom parser with the `nyaa:` namespace, and its decisions file.
- `docs/architecture.md` §18.1, §18.2 (the Nyaa entry) and §18.7.
- `docs/owner-decisions.md` §2.28. Under **Q2**, record the ToS/robots check for nyaa.si's RSS in the decisions file; if it's a grey case, stop and tell the runner. **Q7:** Sukebei is a separate variant, **not** in this slice.
- The decisions files `docs/decisions/2026-11-02-SRC-{4,5,6,7,8}.md`.

## Do
1. **`src/nyaa.rs`:**
   - **Manifest:**
     - an External consent class (per SRC-4; confirm it in the decisions file);
     - fixed manifest endpoints: `nyaa.si` only, so `Endpoints::manifest`, and the DNS goes through the proxy when a proxy is set;
     - no credentials;
     - a polite poll interval: at least what §18 says, and no faster than Nyaa's RSS update cadence.
   - **Parsing:** use SRC-8's parser and its limits only. Write no new parsing of untrusted text beyond mapping its typed output.
   - **Infohash:** from `nyaa:infoHash` or the magnet only.
   - **Claims:**
     - title, size, category, seeders and leechers, all with Low confidence and unverified, through SRC-2;
     - the category maps to the content type;
     - Nyaa's "remake" and "trusted" flags become claims only if §18 says so.
   - **Never fetch** the `.torrent` link or the view page.
2. **Fixtures:** scrubbed Nyaa RSS samples in `fixtures/nyaa/`. If they're hand-written, say so; real recordings are a follow-up. Cover:
   - a normal page;
   - an empty feed;
   - malformed or oversized input;
   - a missing infohash;
   - a duplicate item;
   - 429 and 5xx answers.
3. **Tests** in `tests/nyaa.rs`, following the Prowlarr tests: no network and no fixed sleeps. If SRC-8 has a fuzz target, extend it rather than adding a new one.
4. **Registration:** the same state as SRC-7. Don't wire it into `services.rs` or the pipeline (another slice owns them). Add only the `pub mod` line to `crates/sources-plugins/src/lib.rs`.
5. **Decisions file:** `docs/decisions/2026-11-02-SRC-10a.md`: the Q2 check, the consent class, the poll interval and the category map.

## Own
`crates/sources-plugins/src/nyaa.rs`, `fixtures/nyaa/**`, `tests/nyaa.rs`, the one `pub mod` line in `crates/sources-plugins/src/lib.rs`, and your decisions file.

**Don't edit:** `crates/sources/**`, `crates/pipeline/**`, `crates/lodestar/**` (other slices), or `crates/sources-feeds/**` (SRC-8's audited parser). If the parser lacks something, message the runner first.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test -p lodestar-sources-plugins` pass (CI runs the workspace). A fresh self-review is done.

Branch `slice/SRC-10a`. Open the PR titled "SRC-10a: Nyaa RSS plugin". Follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the `send_message` tool of the Claude Code Remote MCP server. Never merge. Push once per round. Work frugally. Don't background long jobs with `&`.
