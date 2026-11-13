# X-filter01-rejudge

**Runner brief, 2026-11-13. Opus, core (filter, store, on-disk marker). Same session as #843 and #855.** Detail stays at component level here; the runner sends the specifics by message (docs/security/v1/README.md §5).

Branch `slice/X-filter01-rejudge` from current main. PR title `X-filter01-rejudge: re-judge stored torrents after a core change`. Target ≲ 1,000 changed lines.

## Owns
- `crates/filter/**`, `crates/store/src/{filter,snapshots,lib}.rs` and a migration if the marker needs one, `crates/pipeline/**` for the re-judge task
- `crates/api/src/routes/filters.rs` and `standin.rs` only if the re-judge surfaces there
- `docs/architecture.md` §20 (one line per changed rule), `docs/decisions/2026-11-13-X-filter01-rejudge.md` (new), its placement line in `runner/dashboard.yaml`

## Items
1. **The re-judge.** A one-time core re-judge of stored torrents behind a core-version marker, as sent by message. Resumable, bounded per batch, no fixed sleeps, and fail-closed: an item that can't be judged is held, never shown. Tests for restart mid-run, an already-current store, and a downgrade.
2. **Store hold precedence** (component: store, quarantine writes and snapshot merge). Details by message.
3. **#855 review nits** (by message): the meter's upper bound, the corpus test's hold check and counts, a test comment, edge-case strings in the kept-hold agreement test, test-literal consistency, the stand-in release count, decisions headings with their PR numbers, and an en dash in a core.rs comment.

## Rules
- Workspace fmt, clippy `-D warnings`, `cargo test --workspace`, the ignored `budget` run. Condition waits ≥ 10 s, no fixed sleeps.
- `/security-review` before ready. Follow `.claude/skills/steward/SKILL.md`. Message the runner "ready <PR> <sha>".
