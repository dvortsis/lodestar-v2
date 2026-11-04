# RP-1: Pack corpora and pack precision corpus, run in CI
**Model:** Opus, with `/security-review`. `corpus.rs` is runtime code over compiled-in data, and RP-3 runs it in production for `GET /filters/fixtures`. The runner reviews with a fresh Opus reviewer. Runner brief, 2026-11-04; auto-approved under owner §2.33.

## Spec
- **The plan:** `docs/plan/2026-11-04-v06-rulepacks.md`, the RP-1 bullet (around line 140) and its table row. Read the RP-0 bullet before it for the versions and directions you check.
- **Architecture:** §20.6 (packs and the guard column), §20.7 (plain-text pack fixtures are allowed) and §20.8 (gates).
- **What RP-0 built** (merged as #424, 75fd512f): `crates/filter/src/packs/{mod,manifest,upgrade}.rs`, and its decisions file `docs/decisions/2026-11-04-RP-0.md`. The per-rule `GOLDEN` in `manifest.rs` now pins each rule's action. Your corpus takes over from it as the guard against silent matching changes, as the plan says. Keep GOLDEN unless the plan says to retire it, and say which you chose.
- **Brief nits:** `git show origin/runner-state:notes/rulepacks-brief-nits.md`, the RP-1 items if any.

## Do
1. **A corpus per pack** in `crates/filter/data/packs/<pack>/corpus.txt`: must-match and must-not-match examples (name, files with sizes, type), in a plain-text format you define and document at the top of `corpus.rs`.
   - Parse it with a small bounded parser: a line-length cap, an item cap, no panics, and errors naming the file and line.
   - It's compiled in (`include_str!`), so it's trusted data. It still mustn't be able to panic in production, because RP-3 serves its results.
2. **A pack precision corpus**, `crates/filter/data/packs/precision.txt`: the legitimate items each guard exists for (§20.6's guard column). For example disc PC players, VST/sample packs, screensavers, scene RAR sets, sample/trailer, short-episode packs, discographies, offline cracks and single-player trainers. Use the plan's list as the minimum.
3. **`corpus.rs`** loads both and runs each item through `Engine::judge` with only that pack on. SC-2 is covered by its own id. It also checks each version's recorded direction (RP-0's `widens` / `narrows` / `action-heavier` / `action-lighter` / `mixed`) against what the corpus shows. Give it an API that RP-3 can call from `GET /filters/fixtures`: results per pack, per rule and per item, with no I/O.
4. **Tests** in `crates/filter/tests/pack_corpus.rs`:
   - every built stateless rule has at least 3 must-match and 3 must-not-match examples (computed, so a new rule without examples fails);
   - the whole corpus passes;
   - a deliberately broken guard fails it, done by weakening a guard in a test-only engine build or fixture, never by editing shipped rules;
   - the parser rejects malformed lines with useful errors;
   - SP-3 and SP-4 are exempt (stateful, covered by RP-4 and RP-5), and the test states that.
5. **Keep content safety in mind:** corpus entries are names and file lists only. Never real infohashes, links or anything that points to real illegal content. Use obviously synthetic names for the must-match examples of harmful classes (MW, FK, CSAM-adjacent rules if any). Say in the decisions file how the examples were made.

## Own
- `crates/filter/src/corpus.rs`, `crates/filter/data/packs/*/corpus.txt`, `crates/filter/data/packs/precision.txt`, `crates/filter/tests/pack_corpus.rs`.
- The decisions file `docs/decisions/2026-11-04-RP-1.md`.
- Shared edit, to be named in the PR: one `pub mod corpus;` line in `crates/filter/src/lib.rs`.
- **Don't edit** `engine.rs`, `load.rs`, `packs/**` or `store/src/rules.rs`. SRC-3 (#426) is editing `load.rs` now, and RP-2 owns the others next. If you need a hook, say so in the PR.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass, `/security-review` is clean, and a fresh self-review is done.

Branch `slice/RP-1`. The PR is titled "RP-1: Pack corpora and pack precision corpus, run in CI". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with `send_message`. Never merge. Push once per round.
