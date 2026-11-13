# Cluster and remote workers review
Reviewed at 55e155a9.

## Coverage
- Read in full:
  - `crates/cluster/src/`: `lib.rs`, `coordinator.rs` (CA, issuance, tokens, renewal, revocation, rotation), `join.rs`, `tls.rs`, `x509.rs`, `worker.rs` (credentials), `proto.rs`, `link.rs`, `server.rs`, `client.rs` (non-test), `leases.rs` (non-test, lines 1-1053), `frame.rs` (encode/decode paths and the limit tests).
  - `crates/lodestar/src/cluster/`: `intake.rs`, `serve.rs` lines 1-1440 and 1510-1820 (floor reports, store check, sink, tick, open/close), `have.rs` (non-test), `filter.rs` (non-test), `ops.rs` lines 286-600.
  - `crates/lodestar/src/worker/`: `mod.rs` (non-test), `session.rs` (non-test), `spool.rs` (non-test), `runtime.rs` lines 1-430, 590-1150, 1200-1540, 1751-2300.
  - `crates/lodestar/src/cli/worker.rs` (non-test).
  - Outside the area, because the worker-injection questions end there: `crates/pipeline/src/remote.rs` (the coordinator's intake, all of it), `crates/pipeline/src/coordinator.rs` lines 40-135, `crates/pipeline/src/spool.rs` lines 186-445 and 913-1430 (record/segment format, open, append, read, remove, reseal).
  - `packaging/unraid/lodestar-worker.xml`, the runtime stage of `packaging/docker/Dockerfile`, `healthcheck` in `crates/lodestar/src/main.rs`, and the diffs of open PRs 797 and 779 (file list, decisions files, the worker-side patches).
- Skimmed: `crates/pipeline/src/wire/results.rs` (caps and decode entry only), `crates/pipeline/src/rollups.rs` (`Admit`), `crates/lodestar/src/worker/budget.rs` and `dht.rs` (how intensity and sockets are set), `crates/lodestar/tests/cluster_chaos/scenarios.rs` (first scenario, to judge coverage), `docs/architecture.md` §19, decisions `2026-11-11-WK-11.md`, `2026-11-08-WK-10d.md` (grep), `runner/slices.yaml` (WK entries).
- Not read: `crates/cluster/src/assign.rs`, `placement.rs`, `have.rs`, `clock.rs` (beyond `SystemClock`), `hex.rs`, `tests.rs`, `crates/cluster/tests/**`; `crates/lodestar/src/cluster/placement.rs`, `testing.rs`, `serve.rs` 1440-1510 and 1820-2217 (totals, metrics rendering, `Cluster::start`/`run`) and its tests; `crates/lodestar/src/worker/have.rs`, `runtime.rs` 430-590, 1150-1200, 1540-1750 and its tests; `crates/pipeline/src/wire/filter_snapshot.rs` (the seal and the snapshot codec); `crates/pipeline/src/spool.rs` 445-912 (sealer and file helpers). Reason: time. These carry coordinator-to-worker data (a trusted direction once mTLS holds) or planner logic; the worker-to-coordinator paths were prioritised. `crates/integrations` (`SecretStore`, which protects every cluster key at rest) is another reviewer's.
- Nothing was built or run. Every finding is from reading the code.

## Findings

### CLUSTER-01 A worker that comes back within the lease TTL strands every lease it lost; the fix under way is triggered only after a spool replay
- Severity: blocking (a §19.3 / §19.8 violation: leases of a vanished worker are meant to expire and be re-granted, and the funnel is meant to reconcile after worker kills)
- Kind: correctness
- Confidence: confirmed at `main` by reading; "likely" for the part about PR 779 (only its diff was read)
- Where:
  - `crates/cluster/src/server.rs:1249-1288` (a new session records a heartbeat for the worker and evicts nothing), `crates/cluster/src/server.rs:1533-1567` (every heartbeat renews all the worker's leases; `Heartbeat::leases`, "leases the worker believes it holds", is never read)
  - `crates/cluster/src/leases.rs:247-290` (only a worker silent past the TTL, or a lease marked fetched, expires)
  - `crates/lodestar/src/worker/session.rs:390` and `crates/lodestar/src/cli/worker.rs:361` (a local stop returns at once; nothing is reported), `crates/lodestar/src/worker/runtime.rs:821-841` (drop aborts every task), `crates/lodestar/src/worker/runtime.rs:426-427, 629-637` (`Runtime::shutdown` is test-only)
  - `crates/lodestar/src/worker/runtime.rs:1954-1961` with `crates/cluster/src/client.rs:748-773` (an outcome batch is dropped from the worker's queue once it is written to the socket; `Outcome` has no acknowledgement)
  - `crates/lodestar/src/worker/spool.rs:380-391` and `crates/lodestar/src/worker/session.rs:438-443` (the lease-count comparison is asked for only when a spool segment has just been removed and the spool is empty)
  - `docs/decisions/2026-11-11-WK-11.md:49`
- What: the coordinator keeps a lease for as long as its holder heartbeats. A worker forgets leases in three ordinary ways: (1) the process restarts (its pool and lease map are in memory), (2) an `Outcome` or a `Grants` message is lost with a connection, (3) it is stopped with SIGTERM, which sends neither outcomes nor `Drained`. If the worker is back within 10 minutes, its new session's heartbeats renew the forgotten leases for good. Other workers' claims for those keys are denied `HeldElsewhere`, no re-grant happens, and the funnel's "in flight" never drains. A key recovers only if the same worker sights it again (its own claim returns the lease), the worker stays away past the TTL, it is drained or revoked, or the coordinator restarts.
  The decisions file says a restarted worker's leases "then expire on the coordinator"; that holds only if it stays down past the TTL.
  At `main` the worker only counts the difference (`drift`). PR 779 (WK-11b) adds the `Holding` list that returns such leases, but its trigger is unchanged: `take_reconcile()` is true only after `after_removal` saw an empty spool. A worker that restarts with an empty spool (the usual case: it was connected, so nothing was spooled), or that loses an `Outcome` on a short reconnect with no batch in flight, never compares counts, so it would never send the list.
- Failure scenario: a worker holding 5,000 leases gets a new image; Docker stops it (SIGTERM) and starts it 20 s later. It reconnects with an empty spool and an empty lease map. The coordinator shows 5,000 leases for it indefinitely; the heartbeat ack even carries that number, and nothing acts on it. Those 5,000 info-hashes are refused to every other worker. `cluster_chaos` covers a kill followed by waiting out the TTL (`scenarios.rs:31-100`), not a kill followed by a quick restart.
- Suggested fix: compare lease counts on every new session (the first heartbeat ack already carries the coordinator's count) and whenever they differ, not only after a spool replay; or have the coordinator return, at `Hello`, the leases the worker held before a restart (a per-process nonce in `Hello` would tell a restart from a reconnect). On a local stop, report what is held (or send `Drained`) before closing. Add a chaos scenario for kill-and-restart inside the TTL. Correct the sentence in the WK-11 decisions file.
- Tracked?: partly. PR 779 (WK-11b) and gate slice WK-G. The trigger gap described above is not in PR 779's diff or decisions file.

### CLUSTER-02 No cap on the leases a worker may hold: an authenticated worker can grow the coordinator's lease table and fetch queue without bound
- Severity: important
- Kind: security / resource
- Confidence: confirmed (every step on the grant path was read; no per-worker or fleet cap exists)
- Where: `crates/cluster/src/server.rs:1576-1713` (`claim`), `crates/cluster/src/leases.rs:606-665` (`claim_batch`), `crates/pipeline/src/remote.rs:1080-1096` (`GrantAdmission::admit` returns `wanted` unless the writer queue is past its high-water mark), `crates/pipeline/src/lib.rs:1066-1073` (every remote lease is also written to the fetch queue on each save)
- What: a claim grants every key that is not stored, held or recently committed. The only brake is the writer's queue, which a worker that never sends results does not fill. The honest runtime limits itself to its pool's room (`runtime.rs:1788-1799`), but that is the worker's own check. Leases are renewed by the worker's heartbeats and never expire while it is connected.
- Failure scenario: a compromised or buggy worker sends `Claim` batches of 512 random 20-byte keys back to back. Each batch costs one store check and adds 512 leases (key plus about 70 bytes each, plus map overhead), 512 "harvested" counts in the funnel, and 512 fetch-queue rows at the next save. Tens of thousands of keys a second are plausible; the table reaches gigabytes in minutes. The per-heartbeat and per-tick full scans in CLUSTER-07 then stall the lock the built-in worker's own claims and commits take. Re-verification, the planned detector for a bad worker, would not see it (no results are sent).
- Suggested fix: cap leases per remote worker at grant time (the assignment's `pool_cap` for that worker is the natural number; `held_by` is already computed per heartbeat) and answer the rest `Busy`. Consider pausing a worker whose claims keep hitting the cap, as the floor-report budget does.
- Tracked?: not found. `docs/decisions/2026-11-08-WK-10d.md:60` notes that "the leases a worker holds don't bound the reports", which is the same missing bound, but only the floor reports got a budget.

### CLUSTER-03 The worker has no liveness check on the coordinator: a silent link stalls it for as long as the OS keeps retransmitting
- Severity: important
- Kind: correctness / resource
- Confidence: likely (the absence of a check is confirmed; the stall's length depends on the OS's TCP timers, not measured)
- Where: `crates/cluster/src/client.rs:636-670` (`Driver::run`), `crates/cluster/src/client.rs:696-701` (`send` with no timeout), `crates/cluster/src/client.rs:718-732` and `837-855` (heartbeats are sent, acks only update the clock echo), `crates/lodestar/src/worker/session.rs:388-423` (`drive` reacts only to events, upkeep and shutdown). No TCP keepalive or user timeout is set anywhere in `crates/cluster/src`, `crates/lodestar/src/worker` or `crates/lodestar/src/cluster` (grep).
- What: the coordinator closes a session after three silent heartbeats (15 s). The worker has no matching rule: a missing `HeartbeatAck` is never noticed, and neither a WebSocket ping nor a socket keepalive is used. When the path dies without a reset (the coordinator host loses power, a NAT or VPN mapping is dropped, a firewall starts discarding), the session ends only when the kernel gives up retransmitting the unacknowledged heartbeats, about 15 minutes with Linux defaults.
- Failure scenario: a worker's WireGuard tunnel to the coordinator drops silently. For about 15 minutes the worker believes it is connected: the pool is not paused (`refresh_pause` sees a live handle), claims time out one by one after 15 s and their keys are dropped, sealed results are "sent" into the socket buffer and held in memory until the 8 MiB outbound budget fills, and nothing goes to the spool (it is used only when there is no session). Meanwhile the coordinator dropped the session after 15 s and re-granted the worker's leases at 10 minutes, so most of what the worker fetched in that window is stale when it finally reconnects.
- Suggested fix: end the session when no `HeartbeatAck` arrives within a few heartbeat intervals (the interval comes in `Welcome`), put a timeout on `Driver::send`, and optionally set TCP keepalive on both sides.
- Tracked?: not found.

### CLUSTER-04 "Trust but verify" is not built: nothing a worker says about a torrent's name or files is ever checked, and there is no quarantine or purge
- Severity: important
- Kind: gap
- Confidence: confirmed
- Where: `crates/lodestar/src/cluster/intake.rs:73` (`NoStanding`), `crates/pipeline/src/remote.rs:137-148`, `crates/lodestar/src/worker/runtime.rs:713-717` (a `Reverify` request is ignored), `crates/lodestar/src/cluster/serve.rs:1027-1031` (replies are forwarded to an intake that has no handler), `crates/pipeline/src/remote.rs:867-869` (`REMOTE_UNVERIFIED` is set "until WK-13 re-verifies it")
- What: §19.4 accepts a worker's prepared record on the strength of 0.5 % re-verification by another worker, quarantine on a failed sample and purge-by-worker. None of the three exists at `main`: the protocol messages are defined, nothing sends `Reverify`, the worker drops one, and the standing check refuses nobody. The intake does the checks it can (fencing, seal bound to the session's worker id, counts and piece arithmetic against the decoded file tree, its own enrichment and classification, a second filter pass at commit), but the name and the file list are the worker's word because the info dict is not sent.
- Failure scenario: a compromised worker holding a lease on key K submits a record for K with an invented name and file list. It passes every intake check and is stored and searchable under K. The coordinator's name rules judge the invented name, so content the rules would hold is stored under an innocuous one, or junk is stored under real hashes. There is no later sample to catch it and no command to remove what that worker contributed.
- Suggested fix: land WK-12 and WK-13 before workers are offered in a release, and say in the user guide until then that a worker is trusted as much as the coordinator.
- Tracked?: slices WK-12 (store: `torrent_worker`, purge), WK-13 (re-verification, quarantine) and WK-G (the §19.8 gate), all in `runner/slices.yaml`.

### CLUSTER-05 Stopping a worker discards its fetched results and reports nothing
- Severity: important
- Kind: correctness / gap
- Confidence: confirmed
- Where: `crates/lodestar/src/worker/session.rs:390`, `crates/lodestar/src/cli/worker.rs:361-366`, `crates/lodestar/src/worker/runtime.rs:821-841` (`Drop`), `crates/lodestar/src/worker/runtime.rs:2070-2221` (`flight` lives in the `send_results` task), `crates/lodestar/src/worker/runtime.rs:2227-2262` (`spool_live` runs only from that task's loop, with no session)
- What: on SIGTERM `Worker::run` returns `Exit::Shutdown` immediately and the runtime is dropped, which aborts the stages, the reporter and the results task. Batches sent but not yet accepted, batches refused for now, sealed records still in the outbound queue, and queued loss outcomes are all in memory and are lost. The spool is written only while no session is up, so a worker stopped while connected spools nothing. No `Drained` and no outcomes are sent, so the leases stay with the coordinator (see CLUSTER-01). The comments at `runtime.rs:823-826` and `session.rs` ("the keys' leases go back to the coordinator") describe expiry after 10 minutes of absence, not a hand-back. The graceful path `Runtime::shutdown` exists but is called only by tests, and it does not flush to the spool either.
- Failure scenario: a routine container update stops a busy worker. Up to 8 MiB of sealed results waiting for `Accepted`, plus everything the prepare stage had finished, is thrown away; with CLUSTER-01 the keys are then stranded instead of re-granted. PR 797's compose file gives the container a 60 s grace period "to flush the spool", which the binary does not use.
- Suggested fix: on a local stop, run the drain the coordinator-initiated `Drain` already runs (stop the stages within the deadline, flush what is sealed, report losses, send `Drained`), and if no session is up or the flush times out, write what is in flight to the spool before exit.
- Tracked?: not found (WK-11's decisions cover the no-session case only).

### CLUSTER-06 Worker packaging: the Unraid template at `main` yields a permanently "unhealthy" container and a token field that cannot be cleared; its Intensity field does nothing
- Severity: important at `main` for the first two points (fixed by open PR 797); polish for the rest
- Kind: gap / docs
- Confidence: confirmed (template, Dockerfile and CLI read; PR 797's diff read)
- Where: `packaging/unraid/lodestar-worker.xml:16, 26, 31`, `packaging/docker/Dockerfile:141-142`, `crates/lodestar/src/main.rs:147-159, 384-416`, `crates/lodestar/src/worker/dht.rs:290-296`, `crates/lodestar/src/worker/budget.rs:23, 41-55`
- What:
  1. The image's `HEALTHCHECK` runs `lodestar healthcheck`, which requests `/healthz` on the HTTP bind. `lodestar worker` serves no HTTP, so a worker container fails every probe. The template at `main` passes only `--stop-timeout=60`. PR 797 adds `--no-healthcheck` (and `healthcheck: disable` in its compose file).
  2. The token field is `Required="true"`, while the worker tells the user to remove the variable once joined (`cli/worker.rs:78-79`). PR 797 makes it optional.
  3. Not addressed by PR 797: the "Intensity" field sets `LODESTAR_INTENSITY`, which a worker does not use. The worker's DHT intensity and socket count come from the coordinator's `Assign` (Settings → Workers), and a worker never opens more than 200 sockets (`MAX_SOCKETS`), so the description ("Balanced opens about 400 metadata connections at once. Aggressive opens about 1000") is wrong for a worker. PR 797's compose file repeats the variable as a comment.
  4. For PR 797's compose file (`restart: unless-stopped`): a drained worker exits with status 0 (`cli/worker.rs:441-444`) and Docker starts it again; the drain flag survives reconnects on the coordinator (`server.rs:1283, 1301-1303`), so it is told to drain again, exits again, and loops until someone resumes or removes it. The in-process back-off does not apply across process restarts. Not verified by running.
  5. The rest matches the CLI: `PostArgs worker`, `LODESTAR_JOIN`/`LODESTAR_TOKEN` for a first join only, `/data` as `LODESTAR_DATA_DIR`, PUID/PGID through the same privilege drop, a state path separate from the coordinator's (the CLI refuses a folder holding `lodestar.db`).
- Suggested fix: land PR 797; remove the Intensity field from the worker template and compose file (or say it is set in the main Lodestar); make a drained worker's container stay down (a non-zero "do not restart" convention does not exist in Docker, so document `restart: on-failure` for workers, or have a drained worker wait instead of exiting).
- Tracked?: points 1 and 2 by PR 797 (WK-18). Points 3 and 4 not found.

### CLUSTER-07 Full scans of the lease table under the shared lock on every heartbeat, tick and intake batch
- Severity: polish (important once CLUSTER-02 can inflate the table)
- Kind: resource
- Confidence: confirmed
- Where: `crates/cluster/src/leases.rs:329-331` (`held_by` iterates every lease), `crates/cluster/src/server.rs:1540-1541` (per heartbeat, under the fleet and authority locks), `crates/cluster/src/server.rs:835-861` (`snapshot`: one scan per worker), `crates/lodestar/src/cluster/serve.rs:1177` (a snapshot every 1 s tick), `crates/lodestar/src/cluster/intake.rs:27-38` (`ServerClocks::offset_ms` takes a whole snapshot to read one number, for every `Results` batch and, on the session task, every `Counters` batch), `crates/cluster/src/leases.rs:912-915` (per target on every re-grant)
- What: the authority's mutex is the one the built-in worker's claims, commits and losses take. Each of these calls walks the whole table (local leases included) while holding it; `snapshot` does so once per known worker.
- Failure scenario: 200,000 leases (the local pool's cap alone) and 8 workers: a tick's snapshot walks 1.6 million entries under the lock every second, and each results batch does it again. Throughput of the local pipeline drops as the fleet grows.
- Suggested fix: keep a per-worker lease count in `LeaseTable` (updated on insert, release, expire, evict), and give `Clocks` a direct per-worker offset lookup.
- Tracked?: not found (PR 779's decisions file notes "as each heartbeat's lease count already does").

### CLUSTER-08 The join retry the coordinator supports is unreachable from the worker: a lost reply spends the token and blocks the name
- Severity: polish
- Kind: gap
- Confidence: confirmed
- Where: `crates/cluster/src/coordinator.rs:640-671` (`redeem_again`: same token, same key, before expiry), `crates/cluster/src/client.rs:88-130` (one attempt; `Joining`, the only holder of the key pair, is consumed), `crates/lodestar/src/worker/mod.rs:297-311` (no retry; a failed save returns after the token is spent), `crates/cluster/src/coordinator.rs:858-866` (`name_in_use`)
- What: the coordinator keeps the spent token's grant so that "a worker whose grant got lost can ask again with the same key". The worker makes its key pair in memory, tries once and exits on a timeout, and a rerun makes a new key, which `redeem_again` refuses. The same happens when the grant arrives but `credentials` cannot be written.
- Failure scenario: the reply is lost on a slow link (30 s timeout), or the data folder is not writable. The user reruns the command: "refused". A new token for the same name fails with "a worker with this name is already joined" until the phantom worker is revoked in Settings → Workers or its certificate expires (72 h).
- Suggested fix: retry the exchange inside `join` with the same `Joining` until the timeout, and keep the key pair until the credentials are saved (or save the pending key first). Have the "couldn't save" message say that the entry must be removed in Settings → Workers.
- Tracked?: not found.

### CLUSTER-09 CA rotation and the coordinator's secrets-key reseal have no caller
- Severity: polish
- Kind: gap
- Confidence: confirmed (grep: `rotate_ca` is called only from `crates/cluster` tests)
- Where: `crates/cluster/src/coordinator.rs:757-774` (`rotate_ca`), `crates/lodestar/src/cluster/serve.rs:29-35, 1798-1810` (`ClusterHandle::reseal`, `allow(dead_code)` outside tests)
- What: the overlap logic, `renew_now`, and the trust rebuild for a rotation are built and tested, but no CLI command, API route or schedule starts one. The same holds for resealing the cluster keys after a coordinator secrets-key rotation ("`serve` has no rotation flow yet").
- Failure scenario: the cluster CA key is suspected leaked (a copied `lodestar.db` plus `secrets.key`). The only remedies are deleting the `cluster.` rows by hand or a new install; every worker must join again.
- Suggested fix: expose rotation (an admin action next to Revoke) or record that it is deferred, with the manual procedure.
- Tracked?: not found in `runner/slices.yaml` (grep for "rotat" in the WK entries).

### CLUSTER-10 Join attempts are limited by concurrency only; each one takes the PKI lock and reads the store
- Severity: polish
- Kind: security / resource
- Confidence: confirmed
- Where: `crates/cluster/src/server.rs:1076-1089` (`admit`: 256 pending, 8 per address), `crates/cluster/src/server.rs:1153-1191` (`answer_join`, `redeem`: `spawn_blocking`, the PKI mutex, `Coordinator::reload`, which reads and digests every `cluster.` row)
- What: any host that can reach the cluster port may open join connections without a certificate. A token with a valid checksum is easy to forge (the checksum is public), so each attempt reaches `redeem`. Attempts are not rate limited or counted per address; only eight may be in their handshake at once from one address.
- Failure scenario: one host loops join requests. Each costs a TLS 1.3 handshake, a blocking-thread hop and a store read under the lock the heartbeat tick's revocation refresh and certificate renewals also take. Guessing a token stays infeasible (256-bit secret); the cost is load and delayed ticks.
- Suggested fix: a small per-address token bucket for join attempts, and skip `reload` when the token id is unknown in memory and no reload is due.
- Tracked?: not found.

### CLUSTER-11 The workers' have-filter panics on a poisoned lock, and its insert hook sits on the commit path
- Severity: polish
- Kind: correctness
- Confidence: suspected (no panic under these locks was found; the hazard is what happens if one ever occurs)
- Where: `crates/lodestar/src/cluster/have.rs:259-265` (`.expect("worker filter lock")`), `crates/lodestar/src/cluster/have.rs:177-195` (`HaveObserver::inserted`, called for every stored hash once workers are allowed)
- What: the rest of the cluster code recovers poisoned mutexes with `PoisonError::into_inner`; these two accessors panic. A single panic while either lock is held (in `sync_with`, `pump`, `ack_with` or `rebuild`'s bookkeeping) would make every later `inserted` call panic in the writer.
- Suggested fix: recover the guard as elsewhere; every operation leaves the maps consistent.
- Tracked?: not found.

## Checked and found sound
- Join tokens (`crates/cluster/src/join.rs`, `coordinator.rs:536-671`): 256-bit CSPRNG secret, only a domain-separated SHA-256 kept, constant-time compare, 15-minute expiry on the coordinator's clock, single use enforced inside one `IMMEDIATE` SQLite transaction that re-reads the row, at most 64 open, every refusal answered alike. The token never prints (`Debug`), is zeroised in the buffers it passes through, and is never taken from argv.
- `LODESTAR_TOKEN` (`cli/worker.rs:96-249`): first join only, never printed, and the process re-executes without it once the folder has joined. Recorded as an owner decision (§2.47).
- TLS (`crates/cluster/src/tls.rs`): TLS 1.3 only on both sides, ring provider, no system roots, no session resumption or tickets (so revocation and expiry apply to every connection), exactly one of two ALPN values accepted. Verification is never disabled: the custom server verifier delegates to webpki against the pinned CA (by fingerprint while joining, by bundle afterwards) and checks the fixed coordinator name.
- Client certificates: must chain to a cluster CA and be on the coordinator's issued list by SHA-256 fingerprint and not revoked, at the handshake (`tls.rs:286-307`), again before the WebSocket upgrade (`identify`), and again every heartbeat tick (`still_valid`, `server.rs:1808-1841`). The worker id comes from the record, never from the certificate text or `Hello` (a mismatch stops the session).
- Role separation (`x509.rs:207-242`): worker leaves carry clientAuth only and a `*.worker.cluster.lodestar.invalid` name; the coordinator leaf carries serverAuth and its own name; the CA has pathlen 0. A worker certificate cannot impersonate the coordinator to another worker, and a join cannot be intercepted without the CA key.
- Revocation (`coordinator.rs:726-751`, `server.rs:687-733`): revokes every certificate of the worker in one write, evicts its leases, stops the session and forgets its floor reports. Renewal is refused for a revoked or expired certificate and rate limited to one an hour.
- Keys at rest: CA, coordinator and worker private keys are sealed with the install's secrets key with the certificate fingerprint as context (`coordinator.rs:1025-1060`, `worker.rs:208-248`); the worker's credentials file is created 0600, written to a fresh temp file and renamed, with the folder synced.
- Frame decoding (`frame.rs`, `link.rs:27-34`): WebSocket message and frame size capped at 1 MiB before the message is assembled; zstd limited to a 1 MiB window, a single frame and 1 MiB of output; postcard decoding is strict (no trailing bytes) and a huge stated length fails without allocating (tested); per-message limits are checked after decode on both transports. The join request is capped at 1 KiB before authentication and read under the handshake timeout.
- Pre-authentication exposure (`server.rs:1076-1150`): bounded pending connections in total and per address (IPv6 by /64), one timeout over TLS plus upgrade or join, the peer identified before any application byte is read.
- Fencing (`server.rs:1729-1762`, `leases.rs:748-863`, `pipeline/src/remote.rs:593-699`): outcomes and results count only when the sender holds the key under the same token; a replaced or revoked session's payloads are not taken; late results from an earlier epoch are committed only against the coordinator's own record and only once. A worker cannot report or commit for a lease it does not hold, and a replay after re-grant is stale.
- Filter bypass through the intake (`pipeline/src/remote.rs:786-902`, `pipeline/src/writer.rs:1869-1872`): the worker's verdict, enrichment, classification, source bits and private-source flag are discarded and rebuilt; every remote result is judged again at commit with the coordinator's current engine, so a worker running an older or weaker filter snapshot fails closed. Clear (unsealed) remote records are refused. Hash-list and held-key checks happen at claim time on the coordinator (`serve.rs:588-646`) and fail closed on a read error.
- Decompression at the intake: per-record and per-batch byte, component and path budgets, charged even for refused blobs (`remote.rs:797-852`).
- Floor reports (`serve.rs:296-521`): a worker's "blocked" report tombstones a key, but only for a lease it holds, within 100 an hour; the first report over budget pauses the worker and evicts its leases; the pending map is capped.
- Peer hints: relayed in memory only, filtered by the network policy on the coordinator before relay and again on the worker before dialling (`server.rs:578-585`, `runtime.rs:881-906`).
- Session queues: every coordinator-to-worker queue is capped by count and bytes, snapshots are paced by acknowledgements with a window of 4, a worker's resync requests are limited to one per 5 s, and acknowledgements are queued per worker with a cap (`serve.rs:1007-1025`).
- Worker spool format and recovery (`pipeline/src/spool.rs`): counts and lengths checked before allocation, per-frame keyed checksum, torn tail versus mid-file damage told apart, records sealed with the worker's key, byte and age caps enforced on append and read, a clock set back cannot keep a record past 24 h, segments of another worker id are dropped at open. Replayed results are idempotent through the lease token.
- Reconnect behaviour (`session.rs:164-202, 296-359`): capped exponential back-off with jitter, reset only after a session lasted a minute; a fresh session is asked for at most once per 10 minutes; a second copy of the same credentials stops instead of fighting.
- Loss reasons and source ids from workers are range-checked (`pipeline/src/coordinator.rs:44-57`, `server.rs:1399-1406`); discovery times are corrected by the measured offset and clamped to now ± the skew limit.

## Area-level concerns
- Lease lifetime is tied to "the worker is heartbeating", not to "the worker still knows the lease". CLUSTER-01, CLUSTER-03 and CLUSTER-05 are three faces of that. `Outcome` being unacknowledged and the worker's shutdown being abrupt both feed it. The chaos harness has no kill-then-restart-inside-the-TTL scenario and no silent-partition scenario where the worker's side of the socket stays open.
- A worker is, today, trusted as much as the coordinator for the content it reports (CLUSTER-04) and for the resources it can make the coordinator spend (CLUSTER-02). The only per-worker budget is the floor-report one. If workers ship before WK-13, the user guide should say so plainly.
- Liveness and lease expiry use the wall clock (`crates/cluster/src/clock.rs:26-30`). A forward step larger than the 15 s session timeout on the coordinator (a machine without a clock battery being corrected by NTP) ends every session, and one larger than the TTL expires every remote lease at once. It heals by itself; a monotonic clock for `last_seen` and lease renewal would avoid it.
- Protocol evolution: messages are postcard enums, identified by variant position. A new variant sent to a peer one minor version behind fails decoding, which the worker treats as a lost connection and retries forever with no "update" message (only a major mismatch says so). `PROTOCOL_MINOR` is exchanged but nothing gates on it at `main`; PR 779 introduces the first minor-gated message. The module docs say the wire may change until WK-G freezes it.
- `Counters` are documented as idempotent per batch id (`proto.rs:191-192`), but the coordinator keeps no batch memory for them and adds whatever arrives, bounded by `Admit`'s caps. Workers send none yet (recorded in the WK-11 decisions); when they do, a replayed counters batch will count twice unless that is added.
- A restarted worker whose certificate was revoked cannot tell: the TLS handshake fails, which it reports as "couldn't connect", and it retries every two minutes until its certificate expires (up to 72 h), then stops as expired. `Exit::Revoked` is reached only when the revocation arrives on a live session. This is a recorded trade-off (`docs/decisions/2026-11-06-WK-2.md:26`), listed here because the message gives the user no hint.
- `JoinRequest` derives `Debug` with the token as a plain `String` (`proto.rs:592-598`). Nothing prints it today; a hand-written `Debug`, as `ToWorker` has, would keep it that way.
- Every cluster private key (CA, coordinator, worker, spool) rests on `secrets.key` in the data folder. Its file permissions on Windows and its handling belong to `crates/integrations`, which this review did not cover.
- Very large modules: `crates/lodestar/src/worker/runtime.rs` (4,784 lines) and `crates/lodestar/src/cluster/serve.rs` (4,112 lines) each hold several state machines and their tests. The shared `Shared` struct in the runtime has more than thirty fields coordinated by watches and atomics; the stop paths (drain, drop, `shutdown`) have already diverged (CLUSTER-05).
