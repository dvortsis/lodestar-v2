# October 27 CR-0 interruption

**Interrupted; acceptance not achieved.** The original run is preserved at
`C:/src/Github/lodestar-v2/target/soak/cr0-2026-10-25-43d054df`.

The last valid sample was October 27 at 06:25:19 MDT. At approximately 06:26:20
MDT, writing the next metrics snapshot failed because C: had no free space.
Collector reporting then failed with a file-in-use error. Neither `status.json`
nor `result.json` exists; `status.json.tmp` and `final.prom` are empty. The
application logged a SQLite shutdown failure. No clean exit status was recorded.
The native crawler and collector were no longer running when investigated.

The measured phase has approximately 16.1 hours of usable adjacent-interval
coverage: 11,603 newly stored torrents (approximately 720/hour), 11,635 fetch
successes, 276,576 fetch attempts, 75,564 actor admissions, 64,203,205 actor
rejections, and 2,197,148 pool-loss events. Peak sampled RSS was approximately
408 MiB. No sampled ownership-cap violation or pre-terminal writer error was
observed. These are partial observations, not searchable yield or soak acceptance.

## Preservation and recovery

`preserve_interrupted.py` initially inventoried 1,565 files but could not enumerate
`data/logs` in the sandbox. An approved access retry hashed all 1,567 files with
zero enumeration/read errors and no observed changes while hashing. The complete
private manifest and recovery fixture are under this worktree's ignored
`target/incident-2026-10-27-complete/`; the initial limited manifest is retained.

The stopped database, WAL and SHM were copied and their hashes compared before
SQLite opened the disposable copy. `PRAGMA integrity_check` returned `ok` and
`PRAGMA foreign_key_check` returned zero violations. This establishes structural
consistency of the recovered copy, not successful final application persistence,
complete indexing, or correctness of every record. The original database was
never opened through SQLite by this recovery operation.

## Disk attribution and restart

At implementation start C: had approximately 141 GiB free. Earlier inventory
found substantial generated build output (about 39 GiB root debug and 29 GiB
support worktree), while readable soak evidence occupied approximately 0.16 GiB.
The approved retry found two application log files totaling 951,122 bytes. They
do not explain the exhausted disk. The Docker error identifies
the write that failed; it does not establish which process consumed the space.
No cleanup, volume deletion, Docker restart, or crawler restart was performed.

Use a new run directory and fresh data for the next baseline. Keep the old Docker
crawler stopped. Its earlier binary lacks CR-0 startup suppression; any future
handover must use actual stop/send evidence and the conservative cooldown policy,
not the abandoned scheduled completion time.
