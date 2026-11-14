# X-floor-renew decisions

A follow-up to `2026-11-14-X-sec-S6-02-03-r3.md`, on what happens after a worker's floor report lapses. This file says only what the code now does.

## Worker floor reports (WK-10d; architecture §19.3)

- **Expiry says whose rows it deleted.** `Store::expire_worker_floor` now returns each hash with the worker of its `worker-floor:<id>` row (`None` for an id that doesn't parse). Nothing else in the store changed. This is the runner's call (2026-11-14).
- **The floor writer remembers each lapsed key's worker.** `FloorReports` keeps the keys whose report lapsed, each with the worker whose report it was.
  - It holds at most `LAPSED_CAP` (65,536) keys, about 4 MB. Past that the oldest is forgotten, and that key is treated as before this change. This is the runner's call (2026-11-14).
  - A key that lapses again moves to the newest place under its new worker.
  - A revoke clears that worker's keys, along with its reports. A revoked worker's rows that lapse later (after a failed purge) aren't added.
  - The worker comes from the store's row, so a report written before a restart and lapsing after it is remembered (the writer expires at start). The map itself lives in memory: a restart forgets keys that lapsed before it, which are then treated as before this change.
- **A lapsed key isn't given back to the worker whose report lapsed.**
  - The store check gains `StoredKeys::excludes(worker, key)`. Its default excludes nothing, so other implementations are unchanged. It's called under the lease authority's lock, so it must be a quick in-memory answer.
  - **A claim** of an excluded key by that worker is denied as `Stored`, as a held key is. The worker drops the key, as it does for any `Stored` denial: no count of stored torrents changes and nothing is kept. This is the runner's call over `Busy` (2026-11-14). No protocol change.
  - **A re-grant** never picks that worker for an excluded key (`LeaseAuthority::regrant_pending_except`). The key goes to another target, even one holding more leases, or waits for one. It still counts against the worker that held it last, never against the excluded one, and isn't dropped or reported lost. A key that becomes excluded while its re-grant's store check runs waits again at the back of the queue.
  - Any other worker, and the built-in worker always, may have the key. The main node judges the result in full, as before.
- **That worker's own new report of a lapsed key is refused.** Nothing is held and no budget slot is used (`Reported::Lapsed`). It counts in `cluster_floor_reports_total` only. Another worker's report of the key is taken as usual.
