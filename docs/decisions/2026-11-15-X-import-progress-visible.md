# X-import-progress-visible decisions

A follow-up to #908 (X-import-status-lock), which the runner logged: while an import runs, `lodestar import status` couldn't show anything, because the import holds the store to itself. This adds a live view without loosening that lock.

## What changed

- **The import writes its progress to a side file.** `import-progress.json` in the data folder (`crates/lodestar/src/cli/import/progress.rs`): the job, its source, when this run started and when the file was last written, and the job's counters, the same ones the full status reports.
  - Written when the run starts, after saved batches (at most once a second; a batch held back goes out with the next write), and on a heartbeat every 15 s, so a slow source doesn't make a live import look gone.
  - Each write is a new temporary file (`create_new`, never through whatever sits at its name), then a rename, so a reader never sees half of one.
  - Removed when the run ends, however it ends (a guard's `Drop`, after the heartbeat is stopped so it can't write it back), and on the second Ctrl-C before the process exits, where no drop runs. Only a crash or a kill leaves it, and then it goes stale.
  - Best effort: a failed write costs only the live view, never the import.
- **`status` shows it while the store is held.** When the store can't be read because another command holds it, `status` reads the file (at most 64 KiB). If it was written in the last 2 minutes (a minute's leeway for a clock that stepped back) and, when a job was named, it's that job, `status` prints the live counters and says they're live, how long ago they were written, and that the full status and any other jobs show once the import pauses or finishes.
  - `--json` lists that one job with the same keys as the full status, `null` for what only the store knows (`reason`, `waiting_candidates`, `created_at`, `finished_at`), `"phase": "reading"`, `"control": "running"`, plus `"live": true` and `"run_started_at"`.
  - A stale, missing, oversized or unreadable file, or one for another job, gives #908's plain refusal, unchanged.
  - When the store can be read, the store is the answer and the file is ignored.
- **The lock is exactly as #908 left it.** `open_exclusive` and every writing verb are unchanged; `status` still only reads.

## UX choices (`docs/owner-decisions.md` §2.19, the runner's call)

- **A side file, not a reader-friendly lock.** Letting readers into the store during an import would weaken the lock that keeps the server and a second writer out. A side file gives the user the numbers without touching it.
- **Once a second at most, and a 15 s heartbeat.** Fresh enough to watch and cheap on a hard drive. The heartbeat makes the file's age mean "is the import alive", not "when did the last batch land".
- **Two minutes to stale** (eight missed heartbeats). A crash's leftover file stops posing as a running import soon after, and a stale file falls back to the plain refusal, which is always true.
- **Only the running job.** It's the only one known while the store is held; the text says the others show afterwards, and `--json` marks the entry `live`.
- **The same words and keys** as the full status, so the two read alike and scripts see one shape.

## Known gap

- **A crash or a kill** (not the second Ctrl-C) leaves the file for up to 2 minutes. If another lodestar command holds the store in that window, `status` shows the dead import as running until the file goes stale.

## Docs

- The `status` help text and `docs/operations.md` say `status` shows a running import's progress, only that job, and what happens after a force-quit.
