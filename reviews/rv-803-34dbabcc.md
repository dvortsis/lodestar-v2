# rv-803 at 34dbabcc: X-percolate-promoted-hold

VERDICT: MERGE

Checks run on the head: `cargo fmt --check` passed. `cargo +1.99.0 clippy -p lodestar-store -p lodestar-search -p lodestar --all-targets -- -D warnings` is clean. `cargo test -p lodestar-store --lib saved` passed 15 of 15, the two new tests included. `cargo test -p lodestar --bin lodestar percolator` passed 29, with 1 ignored (the bench), and all 5 new hold tests pass. The 12-digit grep finds nothing. There are no model ids, local times or fixed sleeps. The diff touches no Cargo.toml, so no `test-util` or `testing` feature was added to `lodestar`. The only paths touched are percolator.rs, the approved saved.rs edit and the decisions file. There's no schema change or migration.

## What I checked and found correct
- **`record_promoted_rows` is atomic** (saved.rs:607-627). It uses `begin_write` (IMMEDIATE). The per-key `DELETE` checks `!= 1`, and `seq <= 0` is refused. An early `return` drops the `Transaction`, which rolls it back. So an unknown, repeated or zero seq leaves no rows deleted and no alerts written, and the `[4, 2, 9]` case pins rolling back deletes already made. Deletes are primary-key lookups and the read is a range scan on `settings.key`, so nothing is O(n²).
- **A held row is never deleted.** Only rows in `done` go into `seqs`, and `done` takes only `Some(empty)` results.
- **Hits are paired per search, not per seq**, and that's correct. It's explained in the doc comment (saved.rs:599-606) and in the decisions file. A replay hits `alerts`' uniqueness, and `insert_promoted` keeps the paused and edited checks.
- **The cursor** (percolator.rs:372-380) never skips a row:
  - A stopped batch resumes at the stopped row. Row 0 can't stop, because it runs on the fresh `first` token.
  - When the index breaks a batch or the queue ends, the cursor goes back to 0.
  - The cursor and `seen.held` change only after the commit (402-410).
  - An all-held batch returns before any write (385-389) and doesn't set `more`. The `snapshot_max < torrent_id` guard means a row whose torrent is gone can't be held for ever.
- **Crash between the read and the record.** The state is in memory only. After a restart every due search reruns, and alert uniqueness stops duplicates (the pruned-alert limit was accepted).
- **Races.** A held search paused or deleted mid-wait drops out of `parsed`, so on the retry with `only` the row is done (correct). An edited search is checked by `insert_promoted`. The percolator is the only writer to the queue, and `record_promoted` is now used only in tests.
- **The held inference.** A search is left out of `matched` only when it's held or its range is empty (percolate.rs:188-197). The empty-range case is covered by the `snapshot_max` guard, so the inference is exact. `the_held_set_is_what_percolate_holds` pins it.

## Blocking
None.

## Important
- **I1: `Seen::held` has no cap** (percolator.rs:155, extended at 387 and 411). Its size follows the number of held rows. During a long `payload_sig` backfill, every promotion due for a `sig:` search adds an entry with one id per held search, and nothing caps it. The brief asks for the held list to be bounded.
  - Fix: insert only while `seen.held.len() < HELD_CAP` (say `16 * PROMOTED_BATCH`). A row missing from the map simply reruns every due search, which the alert-uniqueness rule makes safe.
  - Note the cap in the decisions file and add a unit test that fills past the cap.

## Optional
- **O1: a large backlog of held rows delays unrelated promotions.** All-held full batches don't set `more` (percolator.rs:384). A row queued behind H held rows waits about H / 256 runs, each one commit or a 10 s idle poll away. Setting `more` there would spin, so the trade-off is right, but name it under "Bounded and live" in the decisions file.
- **O2: the "all-held under a spent budget" case runs only once** (percolator.rs:1777). Loop it 3 times and assert `(0, 1, false)` with `alerts == 0` each time. Also assert "no write" directly in the loop at 1648, for example that `PRAGMA data_version` or the queue's value doesn't change.
- **O3: a doc comment now sits on the wrong test** (saved.rs:773-777). The new test was inserted under the existing doc comment of `record_promoted_skips_a_search_paused_or_edited_meanwhile`, so that comment now documents `promoted_rows_can_be_stepped_past_and_dropped_one_by_one`. Move both new tests above that comment.
