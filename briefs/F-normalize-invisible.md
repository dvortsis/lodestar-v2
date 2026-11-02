# F-normalize-invisible: the filter ignores every Unicode Cf character when matching
**Model:** Opus (content safety, untrusted names). `/security-review` and a fresh Opus reviewer follow. Runner brief, 2026-11-02. From X-strip-format-chars (#373) and its decisions file.

## Why
`crates/filter/src/normalize.rs` `invisible()` ignores only a hand-picked subset of format characters. Any Cf character it misses can split a word inside a name that reaches the filter without the feed cleaning (DHT names, imports), and a split word can slip past a term rule. These are the gaps `docs/decisions/2026-11-02-X-strip-format-chars.md` lists:
- U+2028/2029;
- the tag characters U+E0000–E007F;
- U+0600–0605, U+06DD, U+070F, U+0890–0891, U+08E2;
- U+FFF9–FFFB, U+110BD, U+110CD, U+13430–1343F, U+1BCA0–1BCA3, U+1D173–1D17A.

This only tightens: more names match the rules they were meant to match.

## Read first
- `docs/decisions/2026-11-02-X-strip-format-chars.md`.
- `crates/metainfo/src/display.rs`: `format_char` and `hidden_char`, with their reference test against Unicode 17.0 Cf.
- `docs/architecture.md` §20, especially normalization and term matching. Read the neutralize property tests and the SP-1/SP-2 notes in `crates/filter`.
- `docs/decisions/*F-filters*` (A3-1..3) and the filter golden and property tests.

## Do
1. Make `invisible()` ignore every Cf character by reusing `lodestar_metainfo::format_char`, or a shared table if a dependency from filter to metainfo is wrong; check the crate graph. Keep the non-Cf fillers it already ignores.
2. **Stored rules:**
   - Normalization also applies to rule text. Check whether any stored artefact depends on the normalized form: rule hashes, compiled caches, `filter.stand_ins` keys, printed text.
   - If one does, handle it so an upgrade can't drop or loosen a rule, and say how.
   - Rules that parsed before must still parse.
3. **Tests:**
   - a property test: inserting any Cf character inside a word never changes whether a term rule matches;
   - golden cases for each newly covered range;
   - the existing neutralize and NEAR/phrase tests still pass;
   - the §20.8 filter budget still holds (`filter budget (§20.8)` in CI; report local numbers if your CPU is calibrated, otherwise rely on CI).
4. **Search tokenizer:** leave it alone. It's the separate follow-up X-tokenizer-cf, which may need an index rebuild. Say so in the decisions file.
5. **Decisions file:** `docs/decisions/2026-11-02-F-normalize-invisible.md`, short.

## Own
`crates/filter/src/normalize.rs`, its tests, `crates/filter/Cargo.toml` (only if a dependency is needed) and your decisions file. **Don't edit** the store (SRC-1 is running), `docs/soak/**` (CR-COL-3), `crates/sources/src/egress.rs` or `crates/integrations/src/net.rs` (SRC-5).

## Size
≲ 300 changed lines.

## Done when
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass;
- a short fuzz run of the filter targets finds nothing;
- `/security-review` passes.

Branch `slice/F-normalize-invisible`. Open the PR titled "F-normalize-invisible: the filter ignores every Unicode Cf character". Follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the `send_message` tool of the Claude Code Remote MCP server. Never merge. Push once per round. Work frugally. Don't background long jobs with `&`.
