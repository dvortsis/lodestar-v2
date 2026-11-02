# SRC-7: Prowlarr/Jackett plugin #1, and the recorded-fixture contract-test pattern
**Model:** Opus (it parses untrusted indexer responses and handles credentials). `/security-review` and a fresh Opus reviewer follow. Runner brief, 2026-11-02, written after SRC-2, SRC-4, SRC-5, SRC-6, SRC-8 and SRC-9 merged.

## Why
v0.6 Sources needs its first real plugin, and a pattern every later plugin copies (SRC-10a–e, SRC-11–13): recorded fixtures plus contract tests that never touch the network. Prowlarr (and Jackett, which speaks the same Torznab) is the user's own indexer manager, so it's the safest first plugin.

## Read first
- `docs/architecture.md` §18.1, §18.2, §18.5, §18.6, §18.7 and §18.9.
- `docs/plan/2026-11-01-v06-sources.md`, especially §5.
- `docs/owner-decisions.md` §2.28:
  - **Q2:** a plugin ships only if its recorded check finds automated API access allowed;
  - **Q15:** `private_source` items get claims only by default. The `.torrent` is fetched only on the user's explicit interest, rate-limited and with a snatch warning.
- The decisions files of SRC-2, SRC-4, SRC-5, SRC-6, SRC-8 and SRC-9 (`docs/decisions/2026-11-0*-SRC-*.md`).
- `crates/sources/src/{plugin,manifest,claims,http}.rs` and `crates/sources/src/testing.rs`.
- `crates/sources-feeds` (SRC-8 and SRC-9) as the nearest parser pattern: bounded, fuzzed, with a golden corpus.

## Do
1. **Create the new crate `crates/sources-plugins`, with `src/prowlarr.rs`.**
   - **Manifest:**
     - consent class per SRC-4 (Account, since it uses an API key);
     - endpoints are the user's configured base URL only;
     - the credentials field is `api_key`, sealed by SRC-4 and never logged.
   - **Uses SRC-5's per-plugin egress through the `Kit`.** Never build your own HTTP client.
2. **Torznab parsing** (Prowlarr's `/api/v1/search` JSON, or the Torznab XML `/api`; pick one and say why):
   - bounded by size, count and depth;
   - no external entities;
   - infohash taken from `infohash` or magnet `xt` only, never from a guessed link;
   - sizes and dates validated;
   - every field treated as untrusted.

   Add a `cargo fuzz` target and property tests.
3. **Claims, not verified metadata.** Every item becomes an unverified claim through SRC-2's pipeline. Map Prowlarr's indexer privacy flag to `private_source`. Private items are claims only (Q15): never fetch a `.torrent` automatically, and never expose them to Torznab, Stremio or exports.
4. **Q2 record:** in the decisions file, record the ToS/API check for Prowlarr and Jackett (the user's own software, documented API) and say how indexer ToS stays the user's choice.
5. **The fixture contract pattern:** `crates/sources-plugins/fixtures/prowlarr/**` holds recorded, scrubbed responses (no real API keys, hostnames or personal data). `tests/prowlarr.rs` runs the plugin against a local fixture server. Document the pattern in `crates/sources-plugins/README.md` so SRC-10+ can copy it.
6. **Registration:** register the plugin where SRC-6 or SRC-14a expects plugins. If that needs a file another slice owns (SRC-14a owns `crates/pipeline/src/lib.rs`, `control.rs` and `crates/sources/src/http.rs`), **message the runner first**; don't edit it.
7. **Decisions file:** `docs/decisions/2026-11-02-SRC-7.md`.

## Own
The paths `runner/slices.yaml` gives SRC-7:
- `crates/sources-plugins/Cargo.toml`, `src/lib.rs`, `src/prowlarr.rs`, `fixtures/prowlarr/**`, `tests/prowlarr.rs` and `README.md`.

Plus the workspace `Cargo.toml` and `Cargo.lock` entries for the crate, the new fuzz target, and your decisions file.

**Don't edit:** SRC-14a's files (above, plus `crates/api/src/ops.rs`, `crates/store/src/sources.rs` and `crates/lodestar/src/{ops,metrics}.rs`), or SRC-2b's (`crates/lodestar/src/percolator.rs`, `crates/store/src/saved.rs`).

## Size
≲ 1,500 changed lines, fixtures excluded. Split it if it's bigger, and tell the runner first.

## Done when
- fmt, clippy `-D warnings` and `cargo test --workspace` pass;
- a short fuzz run finds nothing;
- `/security-review` passes;
- no fixed sleeps in tests.

Branch `slice/SRC-7`. Open the PR titled "SRC-7: Prowlarr/Jackett plugin and fixture contract tests". Follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the `send_message` tool of the Claude Code Remote MCP server. Never merge. Push once per round. Work frugally. Don't background long jobs with `&`.
