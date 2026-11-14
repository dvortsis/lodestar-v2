# X-sec-S8-01: data folder file modes (S8-01)

Runner brief, 2026-11-14. Opus, core (data folder and store files), /security-review. A held finding: the detail and the chosen fix come by message only (docs/security/v1/README.md §5). Nothing in this brief, the PR, code comments, tests or the decisions file describes how the old setup could be exploited.

Branch `slice/X-sec-S8-01` from current main (fetch first). PR title `X-sec-S8-01: data folder file modes (S8-01)`. Target ≲ 600 changed lines.

## Owns
- `crates/lodestar/src/services.rs`, `crates/lodestar/src/cli/backup.rs`, and a new module under `crates/lodestar/src/` if the fix needs one
- the user docs page that covers the data folder (one section), per `.claude/skills/user-docs/SKILL.md`
- `docs/decisions/2026-11-14-X-sec-S8-01.md` (new)

Not `crates/store/**` (#857 and the store fix slices own it). If the fix as sent can't be done without the store crate, stop and message the runner.

## Items
1. S8-01 (important): fix as sent by message.

## Rules
- Docker, Unraid and Windows keep working: check the image's and the Unraid template's user and folder setup before choosing modes, and report what you found in the decisions file.
- Tests assert the general invariant, never a specific triggering setup (README §5 step 5).
- Workspace fmt, clippy `-D warnings`, `cargo test --workspace`. Condition waits ≥ 10 s, no fixed sleeps. No tokio test-util on `lodestar`.
- Follow `.claude/skills/steward/SKILL.md`. `/security-review` before ready. Message the runner "ready <PR> <sha>". The runner merges.
- No model identifiers; UTC only.
