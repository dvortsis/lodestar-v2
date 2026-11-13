# Ingest pipeline review
Reviewed at 55e155a9.

## Coverage
- Read in full (non-test source): `crates/pipeline/src/lib.rs`, `dht.rs`, `stages.rs`, `worker.rs`, `coordinator.rs`, `writer.rs` (lines 1-2687), `remote.rs`, `have.rs`, `gate.rs`, `torrent_cache.rs`, `local_torrent.rs`, `spool.rs`, `egress.rs`, `rollups.rs`, `health/service.rs`; and, because the fetch pool lives there, `crates/sources/src/pool.rs` (lines 1-2696) and `crates/sources/src/pool/admission.rs`.
- Read in part: `unverified.rs` (ingest, the refile sweep, the verify cursor, the claims budget; the judging helpers skimmed), `control.rs` (`Control::start`, pause and sync, purge; the settings and reachability code only outlined), `requires.rs` (`run`), `adult_enrich.rs` (the run loop).
- Followed into other crates to confirm findings: `crates/peer-wire/src/fetch.rs` (hash check, timeouts), `crates/dht/src/node/actor.rs` (announce harvest, reply rate), `crates/sources/src/plugin.rs` (`Emitter`, `Shutdown`), `crates/sources/src/claims.rs` (limits), `crates/store/src/sources.rs` (`save_fetch_queue`), `crates/lodestar/src/services.rs` (start and stop order).
- Decisions read: `2026-11-02-R-mem-triage.md`, `2026-11-06-X-fetch-pool-bytes.md`, `2026-11-01-X-pipeline-1101.md`, `2026-11-07-WK-9.md` (parts), `2026-11-02-SRC-2.md` (parts), `2026-11-12-UP-0.md` (parts).
- Not read: `wire/*` (codecs with their own property tests; only the `results.rs` header), `identify.rs`, `enrich.rs`, `source_adult.rs`, `marginal.rs`, `setup_hold.rs`, `log_events.rs`, `health/{lanes,schedule,client,trackers,aggregate}.rs`, the test modules in `src/` and everything under `tests/` except a grep for sleeps and ignored tests. Reason: time; they are furthest from the concurrency focus.
- Nothing was built or run. Every finding is from reading.

## Findings

### PIPE-01 Any DHT peer can pin the fetch pool: keys whose only peer is one busy IP stay `Ready`, are never evicted, and are re-scanned on every loop pass
- Severity: blocking
- Kind: security (resource exhaustion from untrusted input)
- Confidence: likely (each step traced by reading; not exercised)
- Where:
  - `crates/dht/src/node/actor.rs:904-946` (`harvest_hash`: "announces and lookup results always pass"), `1376-1389` (an `announce_peer` with a valid token harvests the hash with the announcer as its peer); `1269-1272` is the only inbound limit, a global reply bucket, not per IP
  - `crates/pipeline/src/dht.rs:105-113, 221-228` (harvest goes on with its peers)
  - `crates/sources/src/pool.rs:1345-1356` (a discovery with peers becomes `Ready`), `2184-2289` (`dispatch`), `2313-2354` (`next_ready`), `2485-2507` (`evict_one` skips `Ready` and `Fetching`), `1309-1315` (a new key is lost as `Evicted` when nothing can be evicted), `2147` (no timer ever moves a `Ready` key)
- What: three properties of the pool combine.
  1. A key in `Ready` leaves that state only when `dispatch` gives it a socket. `dispatch` skips a key whose untried peers are all on IPs at the per-IP cap (2) and puts it back (`deferred`). Nothing bounds how long that lasts.
  2. `evict_one` never evicts `Ready` keys. With the pool at `max_pending` (200,000) and the 64 oldest admitted keys all `Ready`, it returns `false` and the arriving key is lost as `Evicted`.
  3. While sockets are free, `dispatch` pops every ready key, defers each blocked one, and pushes them all back afterwards. That is O(blocked keys) on every pass of the pool loop (each 25 ms tick, each joined fetch, each discovery batch), and each deferred key still spends its source's DRR deficit.
- Failure scenario: one host does a `get_peers` to obtain its token, then sends `announce_peer` for random info-hashes (passive harvest is on by default, `crates/dht/src/node/mod.rs:100`). Each becomes a pool key with one peer, that host. The host accepts TCP and stalls, so two fetches hold its IP for the 8 s fetch timeout and the backlog drains at about 0.25 keys/s. At even 50 announces/s the pool reaches 200,000 un-evictable `Ready` keys in about an hour. From then on legitimate keys in `Lookup`/`Backoff` are the only ones eviction can take, new discoveries from every source are mostly lost as `Evicted`, and the pool task re-scans up to 200,000 keys per pass. Keeping it full costs the attacker under one packet a second.
- Suggested fix: bound keys per peer IP at admission (or keep keys blocked on a busy IP in a per-IP wait list that eviction may take from and that is woken when the IP frees), cap keys examined per `dispatch`, and consider a per-source-IP limit on announce harvests in the DHT.
- Tracked?: not found. `2026-11-02-R-mem-triage.md`'s churn harness covers eviction of keys with no peers only.

### PIPE-02 A panic in one stage is not contained: ingest stops silently, and the next shutdown wipes the saved fetch queue
- Severity: important
- Kind: correctness / data-loss
- Confidence: confirmed (mechanism, by reading); it needs a panic in a stage to trigger
- Where:
  - `crates/pipeline/src/worker.rs:469-483` (`spawn_blocking(...).await.expect("prepare task")` around `prepare_one`, which runs `Info::parse`, `Preparer::prepare`, enrichment and the filter on fetched bytes)
  - `crates/pipeline/src/writer.rs:492-505, 764-853` and every other blocking step (`.expect(...)`, with the `Store` moved into the closure)
  - `crates/pipeline/src/torrent_cache.rs:605-619` (the cache judge task just returns)
  - `crates/pipeline/src/lib.rs:930-956` (a plugin that panics never clears `running` or sets `last_error`), `1313-1324` (its `JoinError` falls into `Ok(_) => {}`), `1297-1300, 1342-1355` (`?` on the health, intake, pool and prepare joins before `finish.send`)
  - `crates/pipeline/src/writer.rs:447-455` (`io.finish.await.unwrap_or_default()` then `save_queue`), `crates/store/src/sources.rs:520-522` (`DELETE FROM fetch_queue` first)
- What:
  - (a) A panic while preparing one info dict ends the prepare task and drops its `Preparer`. The pool then finds its output closed and starts no more fetches for the rest of the run (as `2026-11-06-X-fetch-pool-bytes.md` says it should with the receiver gone). Nothing restarts the stage and nothing reports it: discoveries still flow, so the writer's flush keeps `last_commit` fresh. The newest paths guard against exactly this (`writer.rs:1263` and `local_torrent.rs:415`, both with comments saying why); the main untrusted path does not.
  - (b) At shutdown, `Pipeline::shutdown` returns on the dead task's `JoinError` before it sends the final queue, although it already holds the pool's queue. The writer, still running, reads the dropped `finish` as an empty queue and saves it, which deletes the last periodic snapshot. The caller gets `Err`, so the store is not returned, the hot-column snapshot is not saved and the indexer's clean stop is skipped (`crates/lodestar/src/services.rs:1019-1049`).
  - (c) A plugin that panics is listed as running, with no error, until the next start.
- Failure scenario: one crafted or unlucky dict trips a panic in a parser, the classifier or a rule. From then on the crawler discovers keys but stores nothing, with no log line beyond the panic message. On the next restart the pending queue (up to 200,000 keys, and the remote leases saved with it) is gone.
- Suggested fix: `catch_unwind` per item in prepare (count it `invalid`, end its lease) and in the cache judge; in `shutdown`, collect join errors and still send the queue; in the writer, skip `save_queue` when `finish` was dropped; mark a panicked plugin stopped with an error; show a dead stage on the status page.
- Tracked?: not found. rv-753 N3 (`docs/decisions/2026-11-12-UP-1.md`) fixed it for uploads only.

### PIPE-03 A failed store write drops the whole ingest batch with its drained counters, and nothing slows fetching
- Severity: important
- Kind: data-loss
- Confidence: confirmed (behaviour). Keeping the leases is a recorded decision; the rest is not.
- Where: `crates/pipeline/src/writer.rs:726-729` (funnel and rollups drained before the write), `880-897` (batch dropped), `2475-2504` (same after promotions committed)
- What: on a `Store::write` error the batch's fetched torrents (up to `batch_max` 256), rediscoveries, scrape updates, events and the funnel and rollup counters drained for it are discarded. `2026-11-02-R-mem-triage.md` line 87 records that the leases are kept on purpose and calls it "bounded by store failures". But the sweeps got a retry with backoff for the same error class (`2026-11-01-X-pipeline-1101.md`, naming `SQLITE_BUSY` past the busy timeout), and the ingest path has neither. Several other connections write to the same database (`Control`'s events and purge steps, the API, the indexer), each behind a 5 s busy timeout (`crates/store/src/schema.rs:926`).
- Failure scenario:
  - Transient: a purge step or another long write holds the lock past 5 s. One batch of verified metadata is thrown away, its keys are not fetched again this run, and that flush's harvest and loss counts never reach the cohort tables, so the funnel no longer reconciles for those hours.
  - Persistent (disk full): every commit fails; the pool keeps opening up to 400 peer connections and discarding the results, and the lease table grows by up to 256 entries per failed batch until restart.
- Suggested fix: on a write error put the drained cohorts and counters back, retry the batch after a short wait, and pause the pool while writes keep failing.
- Tracked?: partly (lease retention, R-mem-triage). The lost counters and the missing backoff: not found.

### PIPE-04 The emitter-to-intake channel is bounded by count only, so the claims byte budget leaves up to 16,384 discoveries of up to 1 MiB unbudgeted
- Severity: important
- Kind: resource
- Confidence: confirmed (the bound, by reading); how much real feeds fill it was not measured
- Where: `crates/pipeline/src/lib.rs:806` (`mpsc::channel(config.intake_capacity)`, 16,384), `1567-1634` (intake takes a discovery, then waits for the budget), `crates/pipeline/src/unverified.rs:83-95` (`CLAIMS_BUDGET_KIB`, 64 MiB), `crates/sources/src/claims.rs:73` (`MAX_TOTAL` 1 MiB per discovery)
- What: `2026-11-02-SRC-2.md` bounds the intake-to-writer claims channel in bytes because "one discovery's claims run to a megabyte and feeds are untrusted, so a count alone would allow gigabytes in flight". The channel one step upstream carries the same discoveries and is bounded by count alone. When the writer is behind, intake blocks on the budget and the emit channel fills behind it.
- Failure scenario: several feed or indexer plugins backfill at once (a first poll of a large library, every indexer through one aggregator) with file lists and descriptions. The writer judges 8 MiB of claims per step, so the backlog sits in the emit channel: 16,384 entries at 20 KiB each is about 320 MiB, and at the 1 MiB clamp it is 16 GiB, against a 1 GB process budget.
- Suggested fix: charge the same budget at the emitter (acquire in `emit`, drop and count in `try_emit`), or give claims-carrying discoveries their own small channel.
- Tracked?: not found.

### PIPE-05 A purged key's in-flight fetch result is applied to a later entry for the same key, which can leak a lookup slot
- Severity: polish
- Kind: correctness
- Confidence: likely
- Where: `crates/sources/src/pool.rs:997-999` (`task_keys` holds key and peer, no generation), `2356-2465` (`on_result` looks the entry up by key), `2510-2535` (`drop_source` loses entries in any state), `1767-1782` (`poll_request` drops a request whose entry moved on, without `lookups -= 1`)
- What: `drop_source` removes a `Fetching` entry and leaves its fetch running. If another source finds the key again before that fetch ends, the old fetch's failure is handled as the new entry's: an entry in `Lookup` is moved by `make_ready` or `back_off`, which leave its request in `requests` and `lookups` counted. `poll_request` then removes the request and returns early.
- Failure scenario: a source is purged while its keys are fetching; the DHT reports one of them again within the 8 s fetch timeout; the old fetch fails. One of the 256 lookup slots is gone until restart, each time.
- Suggested fix: store the entry generation in `task_keys` and drop a result whose generation is not the entry's.
- Tracked?: not found.

### PIPE-06 An upload is answered `Accepted` once queued, not once committed
- Severity: polish
- Kind: correctness
- Confidence: confirmed (by reading); the windows are small
- Where: `crates/pipeline/src/local_torrent.rs:380-382, 427-433` (the doc says "once the writer has taken the commit"; the code answers after `out.send`, which only queues), `crates/pipeline/src/writer.rs:429-446` (claims and scrape rounds are drained after the loop; the cache channel is not), `880-896` (a store error drops lease-free files too)
- What: the user gets `Accepted` while the commit is still one of up to 16 queued items. A store error on that batch, or the writer's loop ending with the item still queued, drops it with only a counter.
- Suggested fix: carry the reply to the writer and answer after the commit, or drain `io.cache` after the loop and reword the doc.
- Tracked?: not found.

### PIPE-07 Shutdown waits for plugins one after another, 15 s each
- Severity: polish
- Kind: correctness (shutdown ordering)
- Confidence: confirmed
- Where: `crates/pipeline/src/lib.rs:1310-1341` (plugin tasks, then egress tasks, each awaited in turn with `PLUGIN_STOP_TIMEOUT`); `crates/sources/src/pool.rs:194` adds up to 5 s
- What: every plugin gets the stop signal at once, but each one that ignores it costs a full 15 s in sequence. Four stuck plugins use the whole 60 s stop grace the packaging sets, and the container is killed before the writer's final queue save and checkpoint.
- Suggested fix: await all plugin tasks under one shared deadline, then abort the rest.
- Tracked?: not found.

### PIPE-08 Two health tests assert "nothing happened" after a fixed 300 to 400 ms sleep
- Severity: polish
- Kind: test
- Confidence: confirmed
- Where: `crates/pipeline/tests/health.rs:222-223, 383-384`
- What: the scheduler ticks every 200 ms and launches at the tracker interval. On a slow runner a regression that does rescrape, or does start a round while banned, would not show inside the window, so these assertions can pass when the behaviour is broken. They cannot fail spuriously, so this is weak coverage rather than a flake.
- Suggested fix: drive them on paused time, or assert on a counter that the skipped path increments.
- Tracked?: not found (PR 778 is about flakes, not these).

## Checked and found sound
- Hash verification: peer-wire checks the assembled dict against the requested key (SHA-1 or truncated SHA-256) before returning it (`crates/peer-wire/src/fetch.rs:536, 584-596`); a torrent-cache file is checked against its key again (`torrent_cache.rs:364-371`); an upload's key is derived from its own dict (`local_torrent.rs:133`). A remote worker's record is not re-hashed, since no dict travels: recorded, with `REMOTE_UNVERIFIED` and WK-13's sampling (`2026-11-07-WK-9.md`).
- Every network step of a fetch is under a deadline: 4 s connect, 8 s overall, and the reservation wait sits inside it (`fetch.rs:167-168, 261`, `pool.rs:2375-2416`).
- Fencing: a result commits only under a current lease held by the worker that sent it (`writer.rs:697-714`, `coordinator.rs:186-205`).
- Ordering at commit: hashes to hold are marked for intake before the write and settled from the store after it; the have-filter learns a key before its lease ends, so a rediscovery in between is held, not fetched again (`writer.rs:745-777, 846-849, 907-913`).
- Writer loop (`writer.rs:351-428`): work runs in arm bodies, so no `select!` arm cancels a commit in flight; every arm's future is a cancel-safe `recv`.
- Intake (`lib.rs:1565-1694`): a shutdown during a send drops only the item in hand; a refused key is claimed and lost at once so its harvest is counted once.
- Pool byte budget (`pool.rs:226-549`): a claim is released on every ending through `Drop`; a cancelled wait leaves its queue and wakes the next waiter.
- Pool shutdown (`pool.rs:1117-1167`): aborted fetches keep their entries and are saved; undelivered results go back to the queue; restore drops keys already stored (`lib.rs:1454`).
- Remote intake (`remote.rs`): per-batch decode budgets, the lease's sources and privacy used instead of the worker's, a late result taken once (`Prior::remove` is the check), `try_send` backpressure to the worker's spool, and no new grants above the writer's high-water mark.
- Dropping a `Pipeline` without `shutdown()` (a failed start in `services.rs:408-470, 694`) does not reach the writer's empty save: the detached heartbeat keeps the remote intake's sender alive, so the writer loop never ends.
- Default-off switches: verification fetches go out only with `Verify::on` (`writer.rs:1662-1665`, `unverified.rs:1503`); coordinated admission and streaming peers are off in `PoolConfig::default`; the setup hold keeps plugins, the scrape scheduler and the pool idle until setup (`lib.rs:931-943, 976-977`, `health/service.rs:310-319`, `control.rs:888-901`).
- Spool (`spool.rs`): lengths checked before allocation, torn and damaged tails told apart, segments created by link so nothing is replaced, no-follow removal, a failed sync closes the segment.
- `SourceId` is below 64 (`crates/store/src/sources.rs:18-24`), so the `1u64 << source` shifts cannot overflow.

## Area-level concerns
- **Tasks are assumed never to panic.** The pool, the writer, prepare, the scheduler and the plugins have no supervisor, and `Pipeline` exposes no way to learn that a stage has ended. PIPE-02 is the sharpest case; the health scheduler (`.expect("health store task")`) and the DHT plugin (a dead node only disables the harvest arm, `dht.rs:221`) have the same shape.
- **Wall-clock time drives leases and expiry.** `unix_now()` feeds the lease TTL and expiry (`lib.rs:1048-1053`), the epoch in tokens (`started_at as u32`), quarantine expiry and the hourly purge (`writer.rs:423, 619-622`), and `first_run_at`, which is written once (`lib.rs:1441-1447`). A forward step of 600 s expires every remote lease at once (duplicate work, not corruption). A first start before the clock is set (a board with no RTC) fixes `first_run_at` near 1970 for good, so the `BACKFILL` window never applies, and anything quarantined before the clock is corrected expires at the next purge.
- **`requeue_cancelled` has no back-off** (`pool.rs:2035-2055`). It relies on finders dropping accepted work only when paused or stopping. I did not read the DHT actor far enough to confirm that holds under load; if it does not, the pool re-asks every 25 ms.
- **The fetch queue is rewritten whole every 5 minutes** on the writer (`lib.rs:1060-1078`, `save_fetch_queue`): up to 200,000 rows deleted and inserted in one transaction, during which the writer commits nothing and other connections wait on the lock.
- **Test gaps for the findings above:** nothing exercises a panic in prepare, a store failure's effect on the funnel counters, a large per-IP-blocked ready backlog, or a full emit channel of claims.
- **Not covered here:** `wire/*`, `identify.rs`, `enrich.rs`, the health lanes and tracker clients, and the test suites, as listed under Coverage.
