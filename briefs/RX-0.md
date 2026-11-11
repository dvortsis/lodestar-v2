# RX-0 (Opus, /security-review): regex parser and trigram plan, bounded and fuzzed

Runner brief, 2026-11-11 (UTC). Slice entry: `runner/slices.yaml` RX-0. Plan: `docs/plan/2026-11-05-v07-m7.md` (RX section) and `docs/decisions/2026-11-05-P-v07.md`. Spec: architecture §8 (query language) and §3 (budgets). Use the steward skill; Auto mode.

## Do
- A **pure** module `crates/search/src/regex.rs`: parse a user regex (untrusted input) into a bounded AST and a trigram plan (the AND/OR of trigrams an index could use to pre-filter), with no index access. Follow the plan doc for the syntax subset, flags and limits; where it's silent, choose the conservative option and record it.
- **Bounds, all enforced before any work grows:** pattern length, AST node count, nesting depth, repetition counts, alternation width, character-class size, and the plan's size; each refusal is a typed error with a stable reason code. No backtracking engine is built here; if the plan names one, defer it and say so.
- No new dependency without the runner's approval (standards §2). If you want `regex-syntax`, ask first with the reason; it's the likely right answer.
- **Tests:** unit tests per rule and per refusal; `crates/search/tests/regex_plan.rs` golden plans; a property test (plan soundness: every string the pattern matches contains the plan's required trigrams) using the crate's existing proptest setup; and `fuzz/fuzz_targets/regex_plan.rs` (no panic, bounded time and memory) with its `fuzz/Cargo.toml` `[[bin]]` and the CI fuzz matrix line in `.github/workflows/ci.yml` (both shared one-line appends).
- Decisions file `docs/decisions/2026-11-11-RX-0.md`.

## Owns
`crates/search/src/regex.rs`, `crates/search/tests/regex_plan.rs`, `fuzz/fuzz_targets/regex_plan.rs`, the one `mod` line in `crates/search/src/lib.rs`, the `fuzz/Cargo.toml` `[[bin]]` entry, the fuzz matrix line in ci.yml, and the decisions file. Nothing that wires regex into queries (that's later RX slices).

## Bar
- runner/standards.md; fmt, clippy `-D warnings`, `cargo test -p lodestar-search` (or the crate's name), fuzz target builds and runs 60 s locally clean.
- Branch `slice/RX-0`, PR `RX-0: <title>`. About 600–1,000 lines.
- Self-review including a /security-review pass; push once per round; reply "ready" to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with PR number and full head SHA when ci-ok is green.
