# X-snapshot-followups (Opus, store; same session as #980)

Runner brief, 2026-11-20. Follows #980 (merged b36eb8f2). Its delta review said MERGE and left these items, plus one older retention bug that review found.

## 1. Retention bug, predating #976 (the main item)
After `settle_conn` folds a left-over WAL, an early `?` return skips putting the snapshot's modification time back:
- in `purge_claims_only`, from ~1464 on (e.g. the WAL switch ~1504);
- in `purge_one`, from ~1101 on.

The fold has already written to the file, so its mtime becomes "now" and the snapshot can live up to 90 days longer than it should. Claims are still purged every hour, so this affects retention only, not safety.
- Fix: restore the mtime on every exit path after the fold, with a guard or a single exit.
- Add a test that fails without the fix: force an error after the fold, then assert the mtime is unchanged.

## 2. Two-sighting wait on a monotonic clock (runner's call, safety)
The one-hour confirmation (snapshots.rs ~988) uses wall-clock `now`, so a forward NTP step between two torn reads can delete a snapshot in under an hour of real time. The mark lives in memory only, so measure the wait with a monotonic `Instant` (kept beside the first wall-clock time for the log), not with wall time. Keep the existing backward-step guard's behaviour. Test it with the existing injectable clock pattern, or a small seam, and no sleeps.

## 3. Test gaps (from the #980 delta review)
- a. The reset after a retried error: a lasting sighting, then busy (a `BEGIN EXCLUSIVE` holder, as in the busy test), then lasting again an hour later. The snapshot must be kept. Today, putting `lasting` back into `marks.2` in the newer arm's transient branch passes every test.
- b. The stuck record clears in the older-schema Skipped arm (~865) and the newer `Ok(Some)` arm (~830). Add a short assertion for each: stuck once, clean pass, stuck again is reported at once.

## 4. Stuck report swallowed when the kind changes (UX)
`marks.1` is shared across error kinds. A "busy" stuck report followed within 24 h by an uncheckable sighting gives no `pipeline.snapshot_stuck` warning before the later `pipeline.snapshot_unchecked` deletion. Keep the kind in `marks.1`, and report at once when it changes.

## Owns
- crates/store/src/snapshots.rs and crates/store/src/snapshots/**
- docs/decisions/2026-11-20-X-snapshot-followups.md

No log-event, catalogue or web copy changes unless item 4 truly needs a new field. Ask the runner first.

## Size and checks
≲ 350 lines. cargo fmt --check; cargo clippy --workspace --all-targets -- -D warnings; cargo test -p lodestar-store and the pipeline snapshot tests. No fixed sleeps. Mutation-check each new test by hand: it must fail without its behaviour.
