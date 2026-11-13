# X-lease-reconcile-every-session decisions

Track Q (the 2026-11-12 code review): CLUSTER-01, CLUSTER-02 and CLUSTER-05. Brief: runner, 2026-11-13. Builds on `docs/decisions/2026-11-12-WK-11b.md`. Affects §19.3, §19.5, §19.6 and §19.8.

## CLUSTER-01: a lease list on every new session
- **Each new session sends the worker's lease list once, whatever the counts say.** The coordinator then gives back the leases on record for it that came from earlier sessions and that the list leaves out, exactly as WK-11b's reconcile does. That covers a worker restarted inside the lease TTL with nothing spooled, and a connection dropped with a `Grants` message in it.
  - **WK-11's trigger is kept as a special case of the same path** (`Worker::reconcile` in `worker/session.rs`). After a spool replay drains, the counts are still compared (the drift counter), and a difference sends the list again on the same session.
  - **Why no count gate on a new session:** equal counts don't mean equal sets. A worker can hold leases the coordinator dropped while missing others it was granted. One list per session is cheap: sessions are rare.
- **The list waits while the spool has results to replay** (`Work::holding` returns `None`, `WorkerSpool::replay_pending`). Until their commit, those results' leases aren't in `WorkerLeases`, so a list sent earlier would give them back and turn each replayed result into a late one. The session asks again at each upkeep until it gets a list. The replay draining sets WK-11's flag, and the list goes out then. "Waiting" also covers a spool that isn't open yet. A spool that couldn't be opened has nothing to replay.
- **What counts as sent:** an answer (`Done` or `Refused`), an older coordinator that takes no list, or a list over `MAX_HOLDING` (WK-11b: such a worker sends none, so it isn't rebuilt at every upkeep). A timeout or a closed session is retried at the next upkeep while the session lasts.
- **The coordinator always carries out a session's first round** (`server.rs`: `last_round_ms` is cleared when a session starts). Without that, a worker back within one heartbeat interval of its last list would be refused as too soon and would keep the leases it lost. The scan this allows costs at most one per handshake (mTLS, PKI check). The one-round-per-interval limit still holds within a session.
- **Chaos scenario:** `a_worker_restarted_inside_the_ttl_gets_its_lost_leases_regranted`. A worker is killed while holding three leases and restarted with no clock movement and no spool. The leases are given back on its new session (`reconcile_returned` +3), none stays on record under its old token, each key is stored once, the funnel reconciles, and there are no fetch violations.

## CLUSTER-02: a per-worker lease cap
- **The cap is 2 × the pool cap of the last assignment sent to the worker.** That's 100,000 leases at the default pool (`DEFAULT_POOL_CAP`, 50,000), and 2,000,000 at the largest an assignment allows (`MAX_POOL_CAP`).
  - **Why 2×:** the worker's pool keeps the keys it holds within its pool cap (`pool_room`). The other half covers results not yet committed and re-grants, the same reasoning as WK-11b's `MAX_HOLDING`.
  - **Why it follows the assignment:** the pool cap is the admin's memory budget for that worker (plan Q5: about 690 B per key on the worker). One lease on the coordinator is a fixed-size entry (key, `Lease`, its delivery mark and the count), so the count is also a bound on cost.
  - **It stays within `MAX_HOLDING` (2,097,152),** so a worker under its cap stays well within what a reconcile list can carry (tested). The worker's list also carries its finished out entries for their 180 s grace, which the coordinator no longer counts, so the list can be a little longer than the coordinator's count; the 97,152 of headroom at the largest pool covers that.
  - **Before any assignment,** a worker's cap is the default pool's. The built-in worker is never capped.
- **Enforced in the lease authority, under its lock** (`LeaseAuthority::claim_batch`, `regrant_pending`).
  - A claim for a new key past the cap is `BatchAnswer::Capped`: never leased, never harvested, and denied to the worker as `Busy`, "claim again later". It's counted apart, in `FleetSnapshot::busy_capped`.
  - A claim for a key the worker already holds still gets its lease.
  - A key waiting for a re-grant counts as new.
  - The admission hook is asked only about keys within the cap.
  - Re-grants pass a target at its cap by and never take one past it. What's left keeps waiting.
  - A lowered cap takes no lease back.
- **The cap is set as the `Assign` is queued** (`ClusterServer::send`), under the fleet lock. A revoked worker's cap is forgotten with it.
- **Per-worker counts are kept with the table** (`LeaseTable::held`), so `held_by` is one lookup instead of a scan. That also makes the heartbeat's lease count and each re-grant cheaper. A property test checks the counts against a scan after any sequence of claims, releases, expiries, evictions, re-grants and reconciles.
- **Not on `/metrics` yet.** `busy_capped` is in the snapshot only. Exporting it needs a `serve.rs` line and a `docs/metrics.md` row, which this slice doesn't own. That's a follow-up for the runner to place.

## CLUSTER-05: a local stop drains
- **A local stop (Ctrl-C, SIGTERM, the service stopping) with a session runs the same hand-back the coordinator can ask for.** The stages stop. What's fetched and sealed goes out. Outcomes not yet reported are reported. `Drained` hands back every lease left, for re-grant now instead of after the TTL.
  - **The session's events are read meanwhile and dropped.** A local stop isn't marked draining on the coordinator, which keeps queueing grants, hints and filter updates. Unread, they'd fill the session's queue and stall the replies the hand-back waits on. `Drained` gives the dropped grants' leases back too.
  - **The coordinator's `Stop(Drained)` ends the hand-back.** `Drained` returns once it's written, so the stop then reads the session until the coordinator's stop arrives, sent once it has evicted the leases. Closing the connection earlier could reset it with the trailing results and `Drained` still unread.
  - **If the session fails partway, or the hand-back runs out of time,** the work is disconnected and the offline path below runs, within a 5 s reserve (`SPOOL_RESERVE`). A batch that was sent but not accepted is spooled, then, and is replayed at the next start. Replays are idempotent (§19.8).
  - **A send stalled on a coordinator that stopped reading doesn't hold the fallback up.** The runtime's session task gives up any send as soon as the session changes, and the disconnect is a change. Without that, a send could wait out the 15 s reply timeout with the spool flush queued behind it, past the reserve. Tested against a link that's never read (`a_stop_against_a_stalled_coordinator_still_spools`).
  - **A stop during an upkeep isn't held up by it.** A list, heartbeat or renewal waiting on the coordinator is dropped, and the stop's deadline runs from the signal.
- **With no session,** the stages stop and what's fetched goes to the spool, durably, before the stop returns (`Work::stop`; the runtime's flush now spools when there's no session). The next start sends it. Unreported outcomes can't be sent offline. Their leases come back at the next start's list (CLUSTER-01), or after the TTL.
- **Bounded by `Settings::stop_deadline`, 30 s** (`STOP_DEADLINE`), from the signal: 25 s for the hand-back, then the 5 s spool reserve. That's within the 60 s the Docker and Unraid templates give a stop (`stop_grace_period`, `--stop-timeout=60`), with room for the DHT to save its state. Past it the worker stops anyway, and the coordinator takes the rest back by the TTL, or at the next start's list.
- **A worker stopping on its own leaves the re-grant targets before its leases go back** (`server.rs`, `Drained`). It isn't marked draining, so the re-grant that follows would otherwise hand its keys straight back to it.
- **Chaos scenario:** `a_stopped_workers_leases_go_back_at_once_within_the_takers_cap`.
  - A worker stopped locally while holding three leases hands them back as it stops, and they're granted again with no clock movement.
  - The worker taking them has a pool cap of 1, set through the real placement, so its lease cap is 2. It's granted two, and the third waits until the cap is lifted.
  - Each key is stored once, the funnel reconciles, and there are no fetch violations.
- **Tests:** a stop with a session drains and hands back every lease (`a_local_stop_drains_and_hands_the_leases_back`). A hand-back that never finishes still ends within the deadline, disconnects the work once and spools (`a_stop_not_taken_back_in_time_keeps_the_work`). A stop with no session keeps what's fetched (`a_local_stop_without_a_session_keeps_what_is_fetched`). A stop in the middle of a batch (sent, not accepted, session lost) leaves it on disk when the stop returns, and the next start replays it and lists after the replay (`a_stop_mid_batch_spools_it_and_the_list_waits_for_the_replay`).

## Wire compatibility
- **Nothing on the wire changes.** No message or field is added, and `PROTOCOL_MINOR` stays 1.
- **A newer worker and an older coordinator:**
  - At minor 0 the worker sends no list, as before (`holding` returns `Ok(None)`).
  - At minor 1 (WK-11b) the list on a new session is the message that coordinator already takes. That coordinator's one-round-per-interval limit may refuse a list sent within one heartbeat interval of the last, and the refusal counts as sent. The lost leases then wait for the TTL, as before this slice.
  - The local-stop `Drained` is the message a drain already sends. An older coordinator evicts and re-grants as it does for a drain, possibly back to the stopping worker before its session closes, which then lapse with it.
- **An older worker and a newer coordinator:** the worker lists only as before, and stops without a drain, as before. The coordinator's lease cap applies to it too. Its pool already keeps its own claims within its pool cap, so the cap only bounds re-grants.

## Refinements flagged (§2.19)
- **The new numbers:** the lease cap, 2 × the assigned pool cap (100,000 at defaults), and the stop deadline, 30 s, with a 5 s spool reserve. All are derived as above.
- **A session's first reconcile round is never refused as too soon.** That loosens WK-11b's per-interval limit across sessions.
- **A coordinator-side cap answers `Busy`,** the reason a worker already retries on, rather than a new deny reason, so no wire change is needed.

## Shared edits (runner, 2026-11-13)
- **`crates/lodestar/src/cluster/testing.rs`: `Harness::stop(i)`, a graceful local stop of one worker (runner, 2026-11-13, §2.33), for the scenario below.**
- **Same file: `Probe` now forwards `reconcile`, `reconciled`, `holding`, `listed` and `stop` to the runtime.** It didn't forward WK-11's or WK-11b's hooks, and their default bodies kept that silent, so **no chaos scenario ran a reconcile before this PR.** A comment on `Probe` now says every `Work` method must be forwarded. All existing scenarios still pass unchanged with reconciles and the stop drain active.

## Review findings
- **The runner's review (of 3182efc8) raised 2 important findings, both fixed here:** the stalled send above, and waiting for `Stop(Drained)`.
- **The fresh self-review's findings are all fixed in this PR:** 1 blocking and 4 important. The last important one, a chaos scenario for the cap and the local stop, was first left open because it needed a further harness edit, which the runner then approved.
- **Left as follow-ups (polish):**
  - per-worker caps are forgotten only on revoke (a small map);
  - an index would make the reconcile scan O(own leases);
  - the list could carry the spooled results' leases instead of waiting for the replay.

## Observed, not changed
- **Re-grants don't check the worker's pool room, and never did.** A key re-granted to a worker whose pool is full is evicted there and reported lost, then harvested again when it's sighted. The lease cap now bounds that at 2 × the pool cap, where before it was unbounded. Sizing re-grants to the pool's room is a follow-up for the runner to place.
