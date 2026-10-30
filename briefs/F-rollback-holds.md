# F-rollback-holds: a rollback keeps the safety holds recorded after the snapshot
**Model:** Opus. This is content safety and on-disk state. Runner brief, 2026-10-30. **Launch after #241 (F-rollback-journal) merges**: build on its `purge_for_restore` / `purge_staged` transaction. High priority, from the #241 review.

## Why
#241 makes a rollback apply the live deletion journal to the staged snapshot. But the **hold rows** recorded after the snapshot aren't carried into the restored store:
- user-reported **CS-1** hashes (`cs_hashes`, not imported);
- **tombstones**;
- **quarantine**, which is sealed.

The held torrents themselves are removed from the copy, yet the restored store no longer refuses them. A later crawl can rediscover, fetch and index content the safety core blocked after the snapshot. This isn't a regression (before #241 the content itself came back), but it is a content-safety gap.

## Do
1. **Carry the holds.** In the same `begin_write` transaction as #241's purge, merge the live store's **non-imported** `cs_hashes`, `tombstones` and `quarantine` rows into the copy, wherever the copy's schema has those tables (V4+; check each table's introduction version).
   - Imported or bundled list entries come back through re-import or the floor reseed. Confirm that in code, and say so in the decisions file. If they don't, carry them too.
   - Conflicts keep the stronger or later hold. Quarantine stays sealed. Nothing loosens.
2. **The whole journal (the #241 review, O1).** For `deleted` journal rows, apply the **whole live journal** to a rollback copy (`since = i64::MIN`), not the snapshot-mtime window. A snapshot copied without `-p` has a late mtime, and the window would let a deleted torrent back.
   - The cost: torrents legitimately re-fetched since then are removed, and they can be fetched again.
   - Record this as a refinement of BB-1 for rollback.
3. **Tests:**
   - with holds recorded after a snapshot, a rollback keeps every hold, and a rediscovered held hash is refused after the restore (through the real ingest or filter path where practical);
   - a late-mtime snapshot still loses a deleted torrent;
   - the O3 suggestion: assert the merged journal row in `purge_for_restore_from_an_older_store` for live ≥ V12;
   - mutations: dropping each table's merge fails a test.
4. **Docs:** in the decisions file, include O2 as well: a pre-V12 live store records no deletions, so rolling forward can bring back what it deleted. It's inherent, so state it. Flag the change to the owner (§2.19) as content safety.

## Own
`crates/lodestar/src/cli/rollback.rs`, `crates/store/src/snapshots.rs` (and a shared store helper if needed), their tests and the decisions file.

## Done when
- fmt, workspace clippy `-D warnings`, and `cargo test -p lodestar-store -p lodestar` (including `tests/cli.rs`) pass;
- a fresh Opus self-review with its blocking findings fixed;
- `/security-review`.

Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head. Never merge. Work frugally.
