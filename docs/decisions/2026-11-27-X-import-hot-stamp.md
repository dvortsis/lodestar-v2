# X-import-hot-stamp decisions

- **Invariant: any writer but the running pipeline that changes a hot-column input marks the snapshot stale before its first such write.** The inputs are what `Store::load_hot` reads: `swarm` (seeders, leechers, trackers reporting, DHT seeds and peers, flags, whether it was ever answered) and `torrents` (id, `last_seen_at`). Marked first, so a crash part-way still forces the rebuild. Before this, an offline `lodestar import` left the stamp a clean shutdown recorded, and the next start loaded a snapshot missing every imported torrent (architecture §7: SQLite stays authoritative).
- **One helper, `lodestar_pipeline::invalidate_hot(conn)`, beside `open_hot`/`save_hot`.** It deletes the `hot_stamp` row, which reads as 0 (stale) once absent. It takes a SQLite connection rather than a `Store`, so the rollback can clear its staged raw copy without opening it as a store (which could migrate it). The key stays private to that section.
- **`lodestar import`:** `start`, `resume` and the legacy `import-bitmagnet` all run through `run_job`, which clears the stamp before its first write. `remove` clears it after the confirmation and before the first removal step, since removal deletes torrents and their swarm rows. A run refused before then (source missing or refused, no room, job conflict) has written no torrent and keeps the stamp; a missing source for `start` and `resume`, and a job conflict, are tested. `pause`, `cancel`, `inspect` and `status` change only job rows, or nothing, and keep it.
- **The writer audit** (`crates/lodestar/src/cli/`, with the server stopped):

  | Writer | Changes a hot input? | Verdict |
  |---|---|---|
  | `import start` / `resume` / legacy | Adds torrents (last-seen) | Cleared in `run_job` |
  | `import remove` | Deletes torrents and swarm rows | Cleared before the first step |
  | `import pause` / `cancel` | Job rows only | Kept (tested for cancel) |
  | `rollback` | Restores a copy with its own stamp, then purges torrents deleted since | Cleared in the staged copy, before the purge and the rename (tested) |
  | `settings set` / `reset` | `settings.*` rows and the `system.keep_or_revert.*` records only, never `hot_stamp` | Kept |
  | `rollup repair` | Rollup tables only | Kept |
  | `vacuum` | Same rows and ids (`VACUUM INTO`, then a rename) | Kept |
  | `reindex` | Moves the index folder only | Kept |
  | `companion` | Companion rows only | Kept |
  | `backup` | Writes a copy, not the store | Kept |
  | `lock`, `stats`, `worker`, `update-helper` | No store rows | Kept |

  `sources-importers`' driver writes only through the CLI import, so `run_job` covers it.
- **Rollback, in detail:** a pre-migration snapshot is taken when the store opens, before `open_hot` clears the stamp, so it holds the stamp of the last clean shutdown. After a crash since then, the hot folder still holds that stamp's files, and without the clear the restored store would load them even though the purge has removed torrents. Clearing is cheap, so the restore always clears.
- **Outside `cli/`, not changed:** the update journal's and the Windows portable update's restore of a pre-migration snapshot rename it into place without a purge. That snapshot's stamp matches the hot folder only when no clean shutdown has run since it was taken, and every offline writer now clears the stamp first. One writer is not covered: the running writer's hourly `purge_snapshots` (`crates/pipeline/src/writer.rs`) deletes torrents inside pre-migration snapshots and leaves their stamp. So a restored snapshot can load hot entries for ids it no longer holds. That is harmless: those ids are gone from the store and the index, `AUTOINCREMENT` never reuses an id, and nothing outside tests walks the columns by id (`HotColumns::for_each`). It is left as it is. A backup restored by hand is the same, without the purge.
- **Pinning the order:** an import killed right after its first batch commits must still leave the stamp cleared, so a test stops the run there (a test-only hook in `run_job`'s per-batch progress callback) and checks that the next start rebuilds. Moving the clear after the driver's run fails it.
