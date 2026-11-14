# X-rejudge-stall-followups decisions

The runner's findings from the fresh review of #888 (X-rejudge-stall), in `crates/pipeline/src/core_rejudge.rs` and around it.

## A batch the breaker stops still holds what did read (the runner's IMPORTANT finding)
- **Before,** a batch the breaker stopped held nothing. A torrent in it that read, and that today's core holds, stayed shown until its unreadable neighbours read again, which on a damaged store may be never.
- **Now** the batch writes today's core's holds on the torrents that did read (`Batch::read_holds`): their hold is written and they're deleted, as in any batch, with their hashes marked for intake first. This applies in both phases and when a stalled range is judged again and the breaker stops it again.
- **Only holds are written.** Torrents that read and that the core keeps get no new marks yet; they're judged again with their range, as before. Torrents that won't read get no hold, so the breaker still keeps a damaged store from holding everything in it.
- **The range is kept whole,** as before. A torrent held now is no longer stored when the range is judged again, so it isn't judged or counted twice.
- **The pass's totals count what was held:** found and held go up by the torrents held, and unreadable doesn't change (those torrents are counted when their range is judged).
- **The rules changed during the batch** (the write is rejected): nothing is written and the batch runs again from the same place, as for any rejected batch. The range is noted on that run.
- **Log and help text** no longer say nothing was held: `pipeline.core_rejudge_stalled`'s doc and its English and Chinese help, the writer's comment, and the `docs/metrics.md` gauge row.

## Smaller findings (the runner's nits)
- **A restarted batch that reaches further is one range.** When a new range starts where a kept range of its phase starts, the kept range now reaches the further of the two, and nothing more is logged. Before, a restarted batch that read further than the first time (each batch stops after `BATCH_TIME`) added an overlapping range and a second Warn. The overlaps line in #888's decisions file is updated to match.
- **`stalled()`** is for tests only (`#[doc(hidden)]`), and its doc says why: it reads the pass through the store's write path. It stays `pub` because the crate's integration tests use it. The writer reports the same numbers as gauges.
- **The writer's retry-wait test** takes the time before the retry, so its bound is exact instead of five seconds loose.
- **The gauges' render** is checked in `a_serving_index_commit_reaches_the_gate`, the existing test that renders `/metrics` from a running pipeline.
- **The Chinese help text** uses 已存储的种子, as the other re-judge events do, for both stall events.
