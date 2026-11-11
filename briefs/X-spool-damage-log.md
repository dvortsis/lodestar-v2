# X-spool-damage-log: log spool damage by segment name, and three spool fixes from #699's last review
**Model:** Opus (on-disk format, untrusted bytes read back from disk). A fresh Opus reviewer follows; `/security-review` applies.

Runner brief, 2026-11-11T23:5xZ. Same session as WK-11 (#699, merged as c1b411c1). Slot 3. Epic E-v06-workers. Deps: WK-11 (merged).

## Read first
- `crates/pipeline/src/spool.rs` on main (your WK-11 code) and `docs/decisions/2026-11-11-WK-11.md`.
- The X-spool-damage-log row in `runner/slices.yaml` (its notes are the requirements).
- `docs/owner-decisions.md` §2.17 (secure deletion).

## Do
1. **Log damage by segment name (rv-699 I3).** When a segment's header doesn't decode, a record is torn, or corruption stops a segment, log one event naming the segment file (name only, never a path outside the spool folder, never record bytes) and the reason class. Use an existing event or add one through the usual event registry, with its en and zh message rows if the registry needs them (shared edits, named in the PR). Rate-limit or dedupe so a damaged spool can't flood the log.
2. **Set `next_seq` before `write_atomic` in `start_segment` (rv-699 r3 I1).** A dir-sync failure (EMFILE, EIO) or a file planted at `<next_seq>.lsp` must not wedge the spool until restart. Add a test that plants a name at `next_seq` after open and shows later appends succeed (the review's probe made 3 of 3 fail before the fix).
3. **Reword the `secure_remove` doc comment and the decisions note (rv-699 r3 I2).** On Windows `shared()` is always false, so hard-linked files still have their target zeroed. Say exactly what each platform does. Add the correction to your new decisions file, `docs/decisions/2026-11-11-X-spool-damage-log.md`, and reference WK-11's file rather than rewriting it (a one-line pointer in WK-11's file is fine).
4. **Reject `shared(&opened)` in `start_segment`'s post-open check (rv-699 r3 N1).** Safe because `next_seq` has already moved on.
5. **The optional `claim()` test at `crates/lodestar/src/worker/runtime.rs`** belongs to X-wk-own-lease-lost (not started; it also waits for WK-20). Leave it out of this slice.

## Owns
- `crates/pipeline/src/spool.rs`, `crates/pipeline/tests/spool.rs`, your decisions file.
- **Shared edits (approved):** one event constant and its en/zh rows if needed; a one-line pointer in `docs/decisions/2026-11-11-WK-11.md`.
- **Ask the runner first** for anything else. Don't touch `crates/lodestar/src/worker/runtime.rs`.

## Done when
- The checks in CLAUDE.md pass, with `cargo +1.99.0 clippy --workspace --all-targets -- -D warnings`.
- No fixed sleeps; any wait is a condition with a deadline of at least 10 s.
- PR titled `X-spool-damage-log: Log spool damage by segment name`, from branch `slice/X-spool-damage-log`. Follow `.claude/skills/steward/SKILL.md`; tell the runner "ready" with the head. The runner merges.
