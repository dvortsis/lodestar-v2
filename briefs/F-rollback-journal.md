# F-rollback-journal: a rollback never brings back a deleted torrent
**Model:** Opus. This is on-disk format, the deletion guarantee and the store. Runner brief, 2026-10-30. It's I3 from the L4 review (state.json `L4-followups`). The rest of L4-followups (I4, P2, P3: performance) stays queued.

## Why
`lodestar rollback` (`crates/lodestar/src/cli/rollback.rs` `restore`, about lines 260–365) copies a snapshot over the store.
- **The guarantee:** the V12 deletion journal (`crates/store/src/schema.rs` about line 179, `crates/store/src/snapshots.rs`) records every deleted torrent's hash, so snapshots lose it too. That's BB-1, the owner's deletion guarantee (§2.16/2.17/2.21 refinement).
- **The gap:** the snapshot purge runs at runtime and can lag or be interrupted. A restore that copies a snapshot without first applying the **current store's** journal can resurrect a torrent the user deleted after the snapshot was taken, including a takedown or safety deletion.

## Do
1. **Confirm the gap with a failing test first:** store a torrent, take a snapshot, delete the torrent (journalled), skip the purge, roll back, and see that the torrent is back. Then fix it.
2. **Fix it.** Before the staged copy is renamed into place, apply every journal row from the current store to the staged copy, inside one transaction, using the same matching rules as the purge (`at` bounds, hash forms).
   - Carry the journal itself into the restored store, so later snapshots keep losing those hashes.
   - The kept pre-rollback copy (`*.before-rollback-*.db`) isn't purged. It's the user's undo, and the CLI says so. If the brief's author decides it must be purged too, raise that with the runner first.
3. **Crash safety:** a crash at any step leaves either the old store or a fully purged restored one, never a restored one without the purge. Test the interrupted path (the staged copy exists, the rename hasn't happened).
4. **The confirmation text** says deleted torrents stay deleted. Keep it short.
5. **Decisions:** add a file that refines BB-1 for rollback.

## Own
`crates/lodestar/src/cli/rollback.rs`, `crates/store/src/snapshots.rs` (only if a shared helper is needed), their tests and the decisions file.

## Done when
- fmt, workspace clippy `-D warnings`, `cargo test -p lodestar-store` and `cargo test -p lodestar` (including `tests/cli.rs`) pass;
- there's a fresh Opus self-review with its blocking findings fixed;
- `/security-review`, because this is the deletion guarantee.

Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head. Never merge. Work frugally.
