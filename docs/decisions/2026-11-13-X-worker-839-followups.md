# X-worker-839-followups decisions

Follow-ups from #839 (X-lease-reconcile-every-session) and two nits from #853. Brief: runner, 2026-11-13. Affects §19.3 and §19.6; nothing on the wire changes.

## A local stop reads events while `Drained` waits
- **A full event queue no longer holds up the stop's confirmation.**
  - The session's loop stops reading the connection while its event queue is full, and it takes no commands in the meantime, `Drained` included.
  - The local stop now sends `Drained` through the session handle, and keeps reading events until the coordinator's `Stop(Drained)` arrives.
  - A `Stop(Drained)` read in the same wake as `drained`'s reply still counts: the coordinator says it only once it has evicted the leases.
- **Shared edit (runner, 2026-11-13):** `SessionHandle::drained()` in `crates/cluster/src/client.rs`, as `WorkerSession::drained`. It only adds the method; nothing else changes.
- **Test:** `a_full_event_queue_doesnt_hold_up_a_local_stop`. A one-slot event queue is flooded with large `Assign`s as the drain ends, and the stop still ends on the coordinator's confirmation, every lease back.
  - The old code's window (events arriving after the hand-back ends, before `Drained` is written) is too narrow to hit on demand, so the test pins the behaviour rather than reproducing the old stall.

## A flush on a session change
- **The flush now sends on a new session before it answers, instead of correcting the comment** (runtime.rs).
  - When the session changes while a flush sends, a new session gets the batches before the flush answers, and no session sends them to the spool.
  - Each extra round needs a real session change, and the caller's drain deadline bounds the wait for the answer.
  - Why: a drain that answered "flushed" with nothing sent on a healthy session would hand back leases whose results never left.

## `capped` on /metrics
- **A fourth cause, `capped`, on the existing `cluster_claims_busy_total{cause}`.** No new family; the cause set stays closed. It reads `FleetSnapshot::busy_capped`, summed over the port's openings like the other causes.
- **Shared edit (runner, 2026-11-13):** `crates/lodestar/src/cluster/serve.rs`, for the cause, and its `docs/metrics.md` row.
- **Follow-up, not in this PR (runner, 2026-11-13):** a `cluster.claims_capped` Warn event, said once each time a worker starts being capped.
  - The cluster crate has no logging, so the event would live in `serve.rs`'s tick.
  - It needs the event catalogue, its golden file and the web event names and messages. `cause="capped"` covers it meanwhile.

## A per-worker lease index
- **`LeaseTable` keeps each worker's keys** (`by_worker`), replacing #839's counts. It's kept in step on every insert and removal, the only two places `leases` changes.
- **What gets cheaper:**
  - an eviction, an expiry and a reconcile visit only that worker's keys, where each used to scan the whole table under both locks;
  - `held_by` is still one lookup.
- **Lock order is unchanged.**
- **Test:** the property test now checks the index against a full scan after any mix of grants, renewals, releases, expiries, evictions, re-grants, reconciles and fetched marks.

## Re-grants and the pool's room (investigated, not changed)
- **Re-grants don't see a worker pool's room.** A key re-granted to a full pool is evicted there and reported lost, then harvested again when it's sighted. #839's lease cap (2 × the pool cap) bounds it.
- **Why not here:** the coordinator doesn't know a pool's fill. Only the worker does (`pool_room`: pool cap less leases held).
- **Proposal for a follow-up:**
  - the worker reports its pool room in `Stats` with each heartbeat;
  - `regrant_pending` takes `min(lease-cap room, reported pool room, per_target)` per target;
  - a worker that hasn't reported yet, or an older worker, keeps today's bound.
- **It isn't small or local:** it changes `Stats` on the wire (a new minor), the heartbeat and the server's per-worker state.

## #853 nits
- **The held-port test asserts exactly `AddrInUse`.** An excluded-range `PermissionDenied` can't arise for a port the system just handed out.
- **The X-flake-companions-loopback decisions no longer say "(or 127.0.0.1)":** only the `::1` bind retries.
- **The `CI` check's comment says any value counts.**

## Other
- **The `leases.rs` test panic now says what was expected** ("a fresh key wasn't granted").
