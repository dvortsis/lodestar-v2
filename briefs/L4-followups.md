# L4-followups: rollback journal, file-cache budget at scale, cancel tokens
**Model:** Opus. Core: search, CLI and concurrency. The runner reviews with a fresh Opus reviewer. Runner brief, 2026-11-04.

## Why
The 2026-10-30 landing review of L4 raised three items outside its scope (commit 4531e3d9). Later slices may have done some of them, so **audit first**.

## Read first
- `docs/decisions/2026-10-30-F-rollback-journal.md` and `2026-10-30-F-rollback-holds.md`: what F-rollback-* already did.
- `crates/lodestar/src/cli/rollback.rs`, `crates/search/src/files.rs`, `crates/search/src/suggest/**`, `crates/search/src/percolate.rs`, and how cancel tokens are threaded through the main search path (look for an existing cancel or deadline type).
- `docs/decisions/2026-10-31-X-search-files-2.md` and `docs/architecture.md` §3 (the file-search budgets).
- SB-6 has just merged (#415): the file index now has `size_bucket`, `category` and `pieces_root`.

## Do
1. **Audit.** For each item below, say whether `main` already does it, with file:line. Message the runner with the audit before coding if any item is already done or turns out larger than ~600 lines.
2. **Rollback applies the live deletion journal.** A rollback must apply deletions recorded since the snapshot, so torrents deleted (or held) after it don't come back. Test it: delete after the snapshot, roll back, and the torrent stays gone.
3. **File-cache budget at scale.** Measure the file-clause cache with broad file clauses (for example `file:*.mkv`, a short common term) on the bench fixture at the largest size the bench supports. Report memory against the budget and enforce a cap if there isn't one. Use the existing bench harness; give the numbers in the PR.
4. **Cancel tokens.** Suggest, compare, filters and percolate's file clauses honour the same cancellation or deadline as the main search path, so an abandoned request stops work. Test with a condition wait (≥ 10 s deadline) that a cancelled request stops; no fixed sleeps.
5. **Decisions file:** `docs/decisions/2026-11-04-L4-followups.md`.

## Own
`crates/lodestar/src/cli/rollback.rs`, `crates/search/src/files.rs`, `crates/search/src/suggest/**`, `crates/search/src/percolate.rs`, their tests, and your decisions file.

**Don't edit** `crates/search/src/{index,rebuild}.rs`: SB-6b owns them right now. Message the runner if you need them.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass, the search latency CI job stays green, and a fresh self-review is done.

Branch `slice/L4-followups`. The PR is titled "L4-followups: Rollback journal, file-cache budget, cancel tokens". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with `send_message`. Never merge. Push once per round.
