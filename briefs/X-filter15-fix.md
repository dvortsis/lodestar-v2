# X-filter15-fix: store write hold path (FILTER-15)

Runner brief, 2026-11-14. Opus, core (store and pipeline writer), /security-review. Held finding: detail by message only (docs/security/v1/README.md §5). Nothing in this brief, the PR, code comments, tests or the decisions file describes how the old code could be exploited.

Starts after #857 merges, in the same session (slot 32). Branch `slice/X-filter15-fix` from current main. PR title `X-filter15-fix: store write hold path`. Target about 1,000 changed lines.

## Owns
- `crates/store/src/lib.rs` (the write path) and store tests
- `crates/pipeline/src/writer.rs`, `crates/pipeline/src/lib.rs` (start-up queries) and pipeline tests
- `docs/decisions/2026-11-14-X-filter15-fix.md` (new)

Not: `crates/store/src/filter.rs` (slot 30 changes it next; ask the runner if you need it), `crates/store/src/import.rs` (goes to X-filter03-fix), `crates/filter/**`.

## Items
1. FILTER-15 (blocking): fix as sent by message, with the tests listed there.
2. A one-time start-up sweep behind a marker, as sent by message.

## Rules
- Tests assert invariants in general terms; no reproducer input in tests or seeds.
- Workspace fmt, clippy `-D warnings`, `cargo test --workspace`. Condition waits of at least 10 s, no fixed sleeps.
- Follow `.claude/skills/steward/SKILL.md`. `/security-review` before ready. Message the runner "ready <PR> <sha>". The runner merges.
