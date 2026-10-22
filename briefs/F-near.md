# F-near: NEAR with multi-token words in filter rules
**Model:** Opus (core: the filter matcher, content safety). **Branch:** `slice/F-near`.

**Spec:**
- `runner/standards.md`;
- architecture §20 (rule language, NEAR, the matcher and narrowing, §20.8 budget and verification);
- `docs/owner-decisions.md` §1.8 (the floor, core rules, the 50 µs budget is the owner's);
- the code: `crates/filter/src/**` (the rule parser, compiler and matcher), `crates/filter/tests/**`.

**The bug** (pre-existing, from #42): a NEAR operand that the tokenizer splits into several tokens (for example `1080p` or `x264`) can never match inside NEAR. Find the exact cause and fix it.

## Own
- The matcher and compiler files in `crates/filter/src/` that implement NEAR, and their unit tests.
- A new `crates/filter/tests/near.rs`.
- A §16 entry.

Don't edit these, because other open PRs are working in them: `crates/filter/src/reclassify.rs`, the engine's judge entry points (#70, the filter session), and `crates/filter/tests/props.rs` (F1b #66). If the fix needs one of them, say so in the PR and keep the change minimal.

## Build
- **The fix:** a multi-token operand inside NEAR matches as a phrase (its tokens adjacent and in order). Its position for the distance check is the span's first token, or the nearest end of the span (choose one, justify it in §16 and keep it consistent with the search query language's NEAR in `crates/search`, if that already defines it).
- **Edge cases:** both operands multi-token; an operand that is itself part of a compound token; repeats; window edges; negated NEAR, if the language allows it.
- **Safety:**
  - No core rule (CS-*) may get weaker. Run every core rule's intent fixtures, and add fixtures where a multi-token NEAR in a core rule now matches.
  - Say in the PR whether any core or pack rule uses such operands today, and what changes for it.
- **Budget:** measure the matcher cost before and after with the existing bench or props harness, without editing props.rs. Report the numbers.

## Tests
- Unit and property tests:
  - multi-token NEAR matches exactly when a brute-force reference does, over random token streams;
  - single-token behaviour is unchanged.

## Done when
- After merging `origin/main`, these pass: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- The PR description gives the root cause, the chosen position rule, the rules affected and the budget numbers.
