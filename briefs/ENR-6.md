# ENR-6: Enrichment match as a strong adult signal in classify
**Model:** Opus, with `/security-review`: its output feeds the Adult-only core checks. The runner reviews with a fresh Opus reviewer. Runner brief, 2026-11-04; auto-approved under owner §2.33.

## Spec
- **The plan:** `docs/plan/2026-11-04-v06-rulepacks.md`: the ENR-6 bullet (around line 159), and Q13 for the confidence, with its recommendation. Q6–Q16 are the runner's, and the plan's recommendation stands.
- **Architecture:** §20.9 and §20.7 ("Filtering ignores corrections, except to be stricter"). Also V8/V9 for correction precedence.

## Do
1. **A new `classify` entry point** beside the existing one in `crates/classify/src/lib.rs`, so today's callers don't change. It takes an optional enrichment match: the kind the service reports (scene, movie, VR) and its confidence.
2. **`adult.rs` treats a match as a strong signal.** It pins the reported kind at Q13's confidence, below a user correction or a classification rule, which still win.
3. **Unmatched torrents classify exactly as before.** `CLASSIFY_VERSION` doesn't change and nothing is re-derived.
4. **Filtering still judges detection's own result, stricter only.** A match can make an item more adult or more specific, and never less. Assert that a match can't turn an Adult item non-adult, or loosen anything a filter relies on.
5. **No pipeline wiring:** ENR-1 owns the callers.

## Tests
- The classifier golden corpus is unchanged, bit for bit.
- One test per kind.
- Correction and rule precedence over the match.
- The match never weakens.
- A property test: for any input with no match, the new entry point gives the same result as the old.

## Own
`crates/classify/src/adult.rs`, `crates/classify/src/lib.rs` (the new entry point), `crates/classify/tests/adult_enrichment.rs`, and the decisions file `docs/decisions/2026-11-04-ENR-6.md`.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass, `/security-review` is clean, and a fresh self-review is done.

Branch `slice/ENR-6`. The PR is titled "ENR-6: Enrichment match as a strong adult signal in classify". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with `send_message`. Never merge. Push once per round. Times in docs are UTC (§2.35).
