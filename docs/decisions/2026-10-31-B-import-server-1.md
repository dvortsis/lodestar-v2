# B-import-server-1 decisions: imported jobs finish on the server, and one filter loader

Lane I, from IMP-4b's open list (`2026-10-31-IMP-4b.md`). No schema change, no API change.

## One filter loader
- **The filter is built the same way at the server's start and in `lodestar import`** (#257 fresh review, important):
  - `crates/lodestar/src/filter_load.rs`, `sync_sources`: the config's subscriptions synced, the config's blocklist files synced into the floor, the subscribed lists applied. These steps need the binary's config types, so they live in the binary, not in `pipeline`.
  - `lodestar_pipeline::StoreFilter::read` and `lodestar_pipeline::load_filter` (now public): the store's CS-1 entries, CS-8 signatures and switches, then packs, your rules and classification rules. `Pipeline::start` calls it with what it read at start; the CLI calls it right after `sync_sources`.
  - `filter_load::read_seal_key` reads `seal.key` for both. The server creates the key when there's none; the CLI never does, and a key it can't read or that isn't 32 bytes is a warning (quarantines keep no name), as before.
- **Each caller keeps its behaviour:** rules that don't parse are skipped by both; the server logs each one, the CLI prints how many.
- **A parity test** (`cli::import::tests::the_cli_and_the_server_build_the_same_filter`) runs the server's path through the real `Pipeline::start_with` and the CLI's `filter`, on one store and config, and compares the verdicts for a hash in a config blocklist file, a CS-1 entry, a CS-8 floor signature, a name a drop rule matches, and a torrent nothing matches.
  - The brief's "floor term" is taken as the CS-8 signature: the floor (CS-1a, CS-1b, CS-8a) is hashes and signatures, and a shipped term list entry would put a real term in a test.

## Jobs leave `indexing`
- **The insights recorder completes them** (`crates/lodestar/src/insights.rs`, `finish_imports`). It already follows the serving index's commits and raises `index_watermark_hour`, so no new task or loop is added. It calls `Store::complete_indexed_import_jobs`:
  - at its start, after recording what the index committed before the hook was attached: jobs found in `indexing` (an offline import);
  - after each commit that raises the watermark;
  - on its 30 s tick, for a job that needs no watermark move: one with no new rows, or one paused at `indexing` and resumed.
- **The watermark is the table, not the index's in-memory counter:** a job completes only once `index_watermark_hour` reaches its `max_new_id`, which the recorder writes after the index commits those ids. The index commits in id order (§7).
- **`Store::complete_indexed_import_jobs`** (`crates/store/src/import.rs`) finds the running `indexing` jobs whose `max_new_id` the watermark has reached and moves each through `set_import_phase(.., Complete, ..)`, IMP-1's guarded transition: the store still picks `complete_with_issues`. A job changed since it was read (paused, cancelled, removed) is skipped by the revision guard. With nothing due, it's one read of `import_jobs`, a row per job.
- **A job with no new rows (`max_new_id` 0) completes at once.**
- **A paused or blocked `indexing` job waits** until it runs again: `set_import_phase` takes only a running job (IMP-1), and this slice doesn't loosen that.
- **With search off the watermark doesn't move,** so a job with new rows stays in `indexing`: its torrents aren't searchable. That's the truthful state.
- **No `import_finished` annotation is written at completion.** IMP-5 writes it when the job reaches `indexing` (it stopped writing records then); completing writes nothing more.
- **A failed completion write** is logged as `insights.step_failed` with `step = "imports"` and retried at the next commit or tick. No new log event.
- **Shutdown:** the recorder stops as before; a job still waiting stays in `indexing` and completes after the next start.
- **`status` while the server runs** reads through its own read-only connection, so it shows the new phase without a restart.

## CLI wording
- `lodestar import status` says of an `indexing` job: "Waiting for the search index: the job completes once the running server has indexed these torrents." It used to say the server indexes them after it starts.

## Tests
- Store: `indexed_jobs_complete_when_the_watermark_passes_them` (not before the watermark, `complete_with_issues` with a waiting candidate, an empty job at once, a paused job waits, nothing twice).
- Recorder: `the_recorder_completes_indexed_imports` (an empty job found at start, a job completing at the commit that reaches its last id and not at the one before, a reader opened earlier seeing each change, a job still waiting at shutdown staying in `indexing`). Condition waits with a 10 s deadline.
- Parity: above.

## Left
- Importing through the running server's writer and `origin="import"` in the gate: B-import-server-2.
