# X-import-pilot-fixes decisions

Fixes for what the cloud tester's real-dump Bitmagnet import lane found on `main` a553fd1b (2026-11-13). No schema change, no API change, no new metric or log event. Nothing here loosens a security or content-safety decision.

## The free-space check follows `--limit`
- **`import start` sizes its check by the torrents this run will import:** the source's rows, capped by `--limit` plus 499, because the driver checks the limit only between batches of up to 500, so a run can commit up to a batch less one past it. Before, it sized by the whole source, so a 200k or 1M pilot was refused wherever the full import wouldn't fit.
- **`import resume` sizes by what's left of the source, capped the same way by its own `--limit`.** `--limit` on `resume` counts "this many more" (`driver::Options::limit` is per run), and a job doesn't store the limit it started with, so the run's own limit is the cap.
- **`import inspect` still sizes the whole source.** It has no `--limit`, and it answers "would the whole source fit", which is still the right question before a full import. When it doesn't fit, its refusal now adds that a pilot with `--limit` may still fit, so a reader stopped there isn't turned away from the pilot. `docs/operations.md` ("the import is refused") could say the same; this slice doesn't own it, so the PR description names it for the runner.
- **With an unknown row count** (PostgreSQL hasn't analysed the table) and a limit, the check now sizes by the limit, where before it checked nothing.

## `DEFAULT_RECORD_BYTES` is 2048 (runner, §2.26)
- **Was 4096:** about 8× the 470 B the 100k measurement gave, to cover the index, the WAL and longer file lists (IMP-4b). The tester's 1M-torrent Bitmagnet pilot measured **1,313 B per torrent, store and index together**.
- **2048 keeps about 1.5× over that measurement,** before the 25 % headroom the check adds. The runner decided it under `docs/owner-decisions.md` §2.26 (a performance number).
- **The measured path is unchanged:** once a store holds 10,000 torrents, its own size per torrent is used instead.

## A Unix-socket `--host` names `--allow-plaintext-loopback`
- **The CLI refuses a socket `--host` without the flag before connecting,** with a message that names the flag and says why it's safe: PostgreSQL has no TLS on a Unix socket, and the socket is on the same machine, so nothing is sent over a network. It keeps the `bad_settings` label.
- **On Windows,** which has no Unix sockets for the importer, the message says to connect to `127.0.0.1` instead, flag or not.
- **When the flag is required doesn't change.** The library still refuses a socket with `Tls::Verify` as `Refusal::BadSettings`, whose message stays generic: refusal messages are bounded and shared with logs and metrics, and the CLI is where the flag's name means something.

## `bitmagnet_check` samples within the key range the database holds
- **The cause of 88 samples on a 2M-torrent slice:** the random starting keys were drawn from the whole 160-bit hash space. A slice restored by key range covers only part of it (2M of 45.6M torrents is about 4.4 %), so about 95 % of the 2,000 starts fell past the highest key, wrapped round to the lowest, and collapsed onto the same few rows. 2,000 × 2M / 45.6M ≈ 88, the number the tester saw.
- **Fix:** the checker reads the table's lowest and highest key (`ORDER BY … LIMIT 1` each way, from the primary key index; bytea has no `min`/`max`), cuts each to 20 bytes so a hostile database can't inflate the starts, and draws the starts inside that range. A full database spans nearly the whole space, so its sample doesn't change in kind.
- **And it says so when a sample is thin:** a table bigger than the sample that still gives fewer rows than asked adds a warning naming the column and both numbers.

## The findings counters are per run, by definition
- **Not a bug.** The driver's `Report::findings` was always "this run's per-reason counts", counted when each batch commits, and they're not stored with the job. The job's counters (`read`, `new`, `invalid`, …) are stored and cumulative.
- **So a pause-and-resume pilot's findings add up only leg by leg:** each leg prints its own, and `import status` shows none. They're also counted **per occurrence**, not per torrent: one torrent can raise several (a lossy name and a lossy path in each of its files), so they don't match `invalid` or any other counter.
- **Two edges where legs don't add up to one run:** a batch replayed after a crash (committed, but its leg never printed a report) is counted in neither leg, and `prepare_rejected` counts torrents the store couldn't prepare, which the adapter's own findings don't include.
- **What changed:** the text report says it ("Found this run (each occurrence; not kept with the job)"). The JSON is unchanged: `findings` sits next to `this_run`, not inside `counters`. Storing findings with the job would need a schema change, which isn't this slice's to make.

## smoke.py accepts the Legal Notice as the UI does
- **`login()` fetches `GET /api/v1/legal/notice` and sends its `version` as `notice_version` with `POST /api/v1/setup`,** as `SetupForm.svelte` does. Without it, setup answers `409 notice_required` (§2.38).
- **On a plain login,** it reads `GET /api/v1/auth` and, if `notice_required`, accepts through `POST /api/v1/legal/accept`, so a data folder set up before a newer notice still works.

## The runbook
- **Scale and duration, from one measured run:** a full restore of a 45.6M-torrent, 281.6M-file dump took about 190 GB for the cluster, plus 8 GB of WAL, and about 25 h; in that run it was limited by one PostgreSQL backend on one core. The runbook says it's one run.
- **The dump checks stream from the ZIP:** step 3's checks only read the dump from start to end, so they run on `unzip -p backup.zip backup.sql`, and the two slow ones share one pass. The restore itself still reads the unpacked file (`restore.sh` takes a path); streaming it would change `restore.sh`, which this slice doesn't (no new tools).
- **Not here:** the "importer tables only" restore route and how Lodestar gets the reader password. The runner tracks both.
