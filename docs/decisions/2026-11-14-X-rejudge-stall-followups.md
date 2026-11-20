# X-rejudge-stall-followups decisions (#901)

The runner's findings from the fresh reviews of #888 (X-rejudge-stall) and of this PR, in the core re-judge (architecture §20.5, `crates/pipeline/src/core_rejudge.rs`).

**Supersedes** two bullets of `2026-11-14-X-rejudge-stall.md`:
- **"Nothing in the range is touched"** (its line 10): a batch the breaker stops now writes today's core's holds on the torrents in it that did read (below). Nothing else in the range is touched.
- **"Overlapping ranges aren't merged"** (its line 31): a range that starts inside or at the end of a kept range of its phase is now merged into it (below).

## A batch the breaker stops still holds what did read
- **Before,** a batch the breaker stopped held nothing. A torrent in it that read, and that today's core holds, stayed shown until its unreadable neighbours read again, which on a damaged store may be never.
- **Now** the batch writes today's core's holds on the torrents that did read (`Batch::read_holds`): their hold is written and they're deleted, as in any batch, with their hashes marked for intake first. This applies in both phases and when a stalled range is judged again and the breaker stops it again.
- **Only holds are written.** Torrents that read and that the core keeps get no new marks yet; they're judged again with their range, as before. Torrents that won't read get no hold, so the breaker still keeps a damaged store from holding everything in it.
- **The range is kept whole,** as before. Its rows leave out the torrents just held, which are no longer stored, so they aren't judged or counted twice.
- **The pass's totals count what was held:** found and held go up by the torrents held, and unreadable doesn't change (those torrents are counted when their range is judged).
- **The rules change during the batch** (the write is rejected): nothing is written, no range is noted or rotated, the pass stays where it was, and the batch runs again, as for any rejected batch. The run after it notes the range and holds the torrent. `a_stopped_batch_whose_rules_change_holds_nothing_and_runs_again` covers both phases and the retry.
- **Log and help text** no longer say nothing was held: `pipeline.core_rejudge_stalled`'s doc and its English and Chinese help, the writer's comment, and the `docs/metrics.md` gauge row.

## Kept ranges merge on any overlap at their start
- **The rule:** a new range that starts inside a kept range of its phase, or where it ends, is merged into it, reaching the further of the two. It isn't logged again.
  - This one rule covers the same range again, a range inside a kept one, a run of stopped batches that follows on, and a restarted batch.
  - A restarted batch repeats the batch it stopped at, which may reach further or less far (`BATCH_TIME`). It may also start inside a range the run has extended since: for example, [0,32] is kept, [32,64] extends it to [0,64], and after a crash the restart at 32 reaches 70. Before, that added a second, overlapping range and a second Warn.
- **Rows past the old end** are added in proportion to the ids the new range covers there; all of its rows when it follows on. Rows were already approximate (`2026-11-14-X-rejudge-stall.md`).

## Smaller findings (the runner's nits)
- **`stalled()`** is for tests only (`#[doc(hidden)]`), and its doc says why: it reads the pass through the store's write path. It stays `pub` because the crate's integration tests use it. The writer reports the same numbers as gauges.
- **The writer's retry-wait test** takes the time before the retry, so its bound is exact instead of five seconds loose.
- **The gauges' render** has its own function, `render_core_rejudge`. `core_rejudge_stalled_gauges_render_their_values` checks it on stats no running writer touches.
- **The Chinese help text** uses 已存储的种子, as the other re-judge events do, for both stall events.
