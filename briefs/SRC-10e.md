# SRC-10e: Sukebei variant of the Nyaa plugin (off, External consent, Adult forced)
**Model:** Sonnet (glue following SRC-10a's pattern). A fresh Opus reviewer follows. Runner brief, 2026-11-05.

## Spec
Owner-decisions (Q7, written): "Allowed as a variant of the Nyaa plugin: off by default, External consent, every item forced into the Adult category, hidden unless "Show adult content" is on." `docs/plan/2026-11-01-v06-sources.md` §4 "SRC-10a–e" (glue only: manifest, config schema, mapping to claims, fixtures, SRC-7 contract tests; Nyaa polls at most every 5 minutes) and §5 Q2 (a recorded ToS/robots check before it may ship), Q7; architecture §18.2, §18.7, §20.9. Copy SRC-10a's Nyaa plugin (`crates/sources-plugins/src/nyaa.rs`) and the README's contract-test pattern.

## Do
- A second manifest in the Nyaa plugin (or a thin variant) for Sukebei's RSS: its own endpoint list, External consent, off by default, poll ≥ 5 min.
- **Every item is forced into the Adult category** regardless of the feed's own category, at mapping time, with a test; adult items stay hidden unless Show adult content is on (existing filter path; add a test that they don't surface with it off).
- The full filter pipeline still applies before storage (child-safety core and floor), with a test using a blocklisted fixture.
- Fixtures: hand-written, synthetic, clean names only (no real adult titles), passing the README's scrub grep.
- Q2: record the ToS/robots status in the decisions file. This container can't reach the site, so write a bold HOLD line at the top of the decisions file ("nothing may register or enable this plugin until the Q2 check is recorded"), as SRC-10c did.

## Own
`crates/sources-plugins/src/nyaa.rs` (the variant), `crates/sources-plugins/fixtures/sukebei/**`, `crates/sources-plugins/tests/sukebei.rs`, `docs/decisions/2026-11-05-SRC-10e.md`.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. PR "SRC-10e: …" against `main` from `slice/SRC-10e`. Follow `.claude/skills/steward/SKILL.md`; message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) via the claude-code-remote `send_message` tool. Never merge. Push once per round. No fixed sleeps.
