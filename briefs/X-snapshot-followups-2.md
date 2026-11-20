# X-snapshot-followups-2 (Opus; runner brief 2026-11-20)

These are the optional findings from #987's fresh review, taken because they're cheap and the code is fresh. Low priority, so keep the PR small.

1. **A failed time restore says so.** `KeepTime`'s Drop ignores `set_modified`'s error (`snapshots.rs` ~1171). If a restore fails, the snapshot can outlive its 90 days with no warning. Record the failure, for example in a `Cell` the pass reads, and add one line to `PurgeReport::warnings` that names the snapshot. Wording is plain and UX-first. A file that's already gone (NotFound) is not a warning.
2. **Remove leftovers from the old restore paths.**
   - `apply_purges_only`'s manual restore (~1722-1725) and its `taken` local.
   - `purge_one`'s `restore_time = drop::<Connection>`.
   - `purge_claims_only`'s `restore(conn, _wrote)`.
   Plain drop order (the guard declared before `conn`) already closes `conn` first. Keep a one-line comment where the order matters.
3. **Skip a write that changes nothing.** Don't call `set_modified` when the file's time already equals the time it should get back. Each hourly pass then stops opening every kept snapshot for writing.
4. **Fix the wording.** A pass that returns early (`Live::read` fails, or nothing is left to purge) keeps `marks.2`. It's still two real sightings at least an hour apart. Fix the decisions or doc wording that says "the last pass found the same", or make an early return clear the sighting. Pick whichever fails toward keeping, and record your choice.

## Tests
- A restore failure shows in the warnings. Use a seam to inject the failure rather than real permissions if that's simpler.
- A pass where nothing changed leaves the time untouched without writing.
- The existing mutation-caught tests still pass.

## Owns
`crates/store/src/snapshots.rs`, `crates/store/src/snapshots/**`, `docs/decisions/2026-11-20-X-snapshot-followups-2.md`, plus this slice's own `slices.yaml`/`dashboard.yaml` entries.

## Done
CI green. Self-review per the steward skill. Then send "ready <pr> <full sha>".
