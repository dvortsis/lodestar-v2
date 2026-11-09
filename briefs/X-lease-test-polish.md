# X-lease-test-polish: tighten the X-lease-report-atomic race test's wording and failure output

**Model:** Opus, in the session that built #647. **Review:** none beyond the runner's self-check, if the delta stays test and doc text only. **Branch:** `slice/X-lease-test-polish`, cut from `origin/main` (#647 merged at e009b89e). **PR title:** `X-lease-test-polish: <title>`. **Steward:** follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.

## Why
#647's delta review (5c27d313) passed the PR with three polish items. The runner merged it and moved them here instead of spending another CI cycle.

## Owns
- `crates/cluster/src/leases.rs`, the test module only.
- `docs/decisions/2026-11-08-X-lease-report-atomic.md`, the Tests section only.

## What to change
1. **The wording overclaims.** The doc comment at leases.rs:1568-1570 and the decisions file (line 29) say every round sees claims before, around and after the report. The reviewer instrumented the test: in 190-199 of 200 rounds every answer was `Committed`, because the claimer's first call came after `report`.
   - What the test guarantees, and asserts, is at least one claim after the report in every round. Rounds where the claimer is already spinning also see claims before and during it.
   - Reword both places to say that. The test's logic doesn't change.
2. **The failure message can be very large.** `panic!("… {answers:?}")` at leases.rs:1615 prints the whole answers Vec, which can hold tens of thousands of entries. Print the length, the index of the bad answer, and the answer itself.
3. **A redundant assert:** `assert!(refused >= 200, …)` at leases.rs:1622 always holds once each round's last answer is checked as `Committed`. Remove it, or turn it into a comment.

Keep the outside-the-lock mutant failing: re-run it once and say so in the PR. No fixed sleeps. Run `cargo fmt --check`, `cargo clippy -p lodestar-cluster --all-targets -- -D warnings` and `cargo test -p lodestar-cluster`. Push once, then message the runner "ready" with the head.
