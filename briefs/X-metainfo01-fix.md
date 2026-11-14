# X-metainfo01-fix

**Runner brief, 2026-11-14Z. Opus, core; it parses untrusted network input.** It fixes Track Q item METAINFO-01 (blocking; component: metainfo parser). Detail stays at component level here. The runner sends the specifics by message (`docs/security/v1/README.md` §5). The PR text, commits, comments and decisions file describe the fix in general terms, as "a combined resource bound on parsed file trees", and don't spell out an attack recipe.

Branch `slice/X-metainfo01-fix` from fresh `origin/main`. PR title `X-metainfo01-fix: bound the combined cost of a parsed file tree`. Target ≲ 600 changed lines.

## Owns
- `crates/metainfo/**`: source, tests and golden files.
- `fuzz/seeds/metainfo/` and `fuzz/fuzz_targets/` entries for metainfo only, plus a metainfo dictionary if the fuzz layout supports one (see `fuzz/README.md`).
- `docs/decisions/2026-11-14-X-metainfo01-fix.md` (new).
- Its own placement line in `runner/dashboard.yaml`.
- **Keep the public API of `lodestar_metainfo::Info` unchanged.** If the fix needs a change that touches callers in other crates, stop and ask the runner first.

## Items
1. The fix, as sent by message. It must be a bound that holds whatever the input. A test must assert memory and time at the limits.
2. Seeds, and a test of the parse's bound, as sent by message.
3. Error behaviour: an over-bound input returns the crate's existing limit error. Never a panic, and never a partial `Info`.

## Rules
- Workspace fmt, clippy `-D warnings`, `cargo test --workspace`, or the touched crates and their dependents if the disk is tight; say which.
- No fixed sleeps.
- Run `/security-review` before ready, then a fresh Opus self-review.
- Follow `.claude/skills/steward/SKILL.md`. Push once, then message the runner "ready <PR> <sha>".
