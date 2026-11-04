# RP-0: pack manifest (versions, changelogs, pending changes, SC-2)
**Model:** Opus. Core: content safety and filter state. `/security-review`. The runner reviews with a fresh Opus reviewer. Runner brief, 2026-11-04.

## The brief
The full brief is **Appendix A of `docs/plan/2026-11-04-v06-rulepacks.md`** on `main` (merged in #412). Follow it, and §4.3's RP-0 section, exactly.

**Also apply every point in `notes/rulepacks-brief-nits.md` on the runner-state branch** (`git show origin/runner-state:notes/rulepacks-brief-nits.md`). They're folded into the RP-0 `notes` in `runner/slices.yaml`. In short:
- seed SC-2 at version 1, and copy SC-1's `filter_exceptions` in the seeding transaction;
- an unreadable row counts as state;
- when the two rows differ, take the stronger version per rule;
- never seed when applied rows exist without the marker;
- report seeding on a non-empty `rule_set`;
- a rewrite merges per rule and keeps unknown newer versions;
- define shadow disagreement for both `widens` and `action-heavier`;
- own `crates/store/src/rules.rs` and `crates/api/src/filters.rs`.

## Governing rules
owner-decisions §1.8.7 and §1.8.8 (no silent weakening; strengthening needs no password), architecture §20.10 and §20.11. Any doubt counts as weakening.

## Own
RP-0's `owns` in `runner/slices.yaml` on `main`, and your decisions file `docs/decisions/2026-11-04-RP-0.md`.

## Done when
- The "Done when" in Appendix A passes, including every gate test listed there and in the nits note.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass.
- `/security-review` and a fresh self-review are done.

Branch `slice/RP-0`. The PR is titled "RP-0: Pack manifest: versions, changelogs, pending changes, SC-2". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with `send_message`. Never merge. Push once per round.
