# X-worker-839-followups decisions

Follow-ups from #839 (X-lease-reconcile-every-session) and two nits from #853. Brief: runner, 2026-11-13. Affects §19.3 and §19.6; nothing on the wire changes.

## A local stop reads events while `Drained` waits
- **A full event queue no longer holds up the stop's confirmation.**
  - The session's loop stops reading the connection while its event queue is full, and it takes no commands in the meantime, `Drained` included.
  - The local stop now sends `Drained` through the session handle, and keeps reading events until the coordinator's `Stop(Drained)` arrives.
  - A `Stop(Drained)` read in the same wake as `drained`'s reply still counts: the coordinator says it only once it has evicted the leases.
- **Shared edit (runner, 2026-11-13):** `SessionHandle::drained()` in `crates/cluster/src/client.rs`, as `WorkerSession::drained`. It only adds the method; nothing else changes.
- **Test:** `a_full_event_queue_doesnt_hold_up_a_local_stop`. A one-slot event queue is flooded with large `Assign`s, and the drain returns only once several are on the wire, so the session's loop is already blocked when `Drained` reaches it. The stop still ends on the coordinator's confirmation, every lease back. Against the old stop it fails: it waits out its deadline and keeps the work for the next start (self-review).

## A flush on a session change
- **The flush now sends on a new session before it answers, instead of correcting the comment** (runtime.rs). Tested by `a_flush_that_sees_a_new_session_sends_there_before_answering`.
  - The test makes the flush's own loop the only path that can send the batch. The batch is refused for now (the loop top never sends a refused batch), the old link is full, and the session changes, without a new generation, once the flush has read the old one. Two test-only hooks mark those points (`TestHooks`, never in the binary).
  - The batch is on the new link when the drain returns. With the loop removed, the test fails (runner's review).
- **The session task's retry interval starts one period in, not at once.** At the start nothing has been sent, so the immediate first tick could only resend a batch refused before it was first polled. The test found it doing exactly that.
  - When the session changes while a flush sends, a new session gets the batches before the flush answers, and no session sends them to the spool.
  - Each extra round needs a real session change, and the caller's drain deadline bounds the wait for the answer.
  - Why: a drain that answered "flushed" with nothing sent on a healthy session would hand back leases whose results never left.
  - **No round limit of its own:** each round needs a real session change (a connect or a disconnect), and the caller waits for the answer only as long as its drain deadline, so a flapping session can't hold a stop past it.

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
  - the room a worker reports is stale by the time it's used, so the coordinator subtracts what it has granted that worker since: room − (held now − held at the report). Otherwise the over-grant comes back within each heartbeat window;
  - the worker reports its pool room in `Stats` with each heartbeat;
  - `regrant_pending` takes `min(lease-cap room, reported pool room, per_target)` per target;
  - a worker that hasn't reported yet, or an older worker, keeps today's bound.
- **It isn't small or local:** it changes `Stats` on the wire (a new minor), the heartbeat and the server's per-worker state.

## #853 nits
- **The held-port test asserts exactly `AddrInUse`.** An excluded-range `PermissionDenied` can't arise for a port the system just handed out.
- **The X-flake-companions-loopback decisions no longer say "(or 127.0.0.1)":** only the `::1` bind retries.
- **The `CI` check's comment says any value counts.**

## Not in this PR
- **A drain the coordinator asks for (`WorkerEvent::Drain`) still reads no events while it hands back** (`work.drain()`, `session.drained()`), so a full event queue can stall it the same way. It predates this PR; a follow-up for the runner.
- **The `cluster.claims_capped` log** (above).

## Other
- **`reconcile` keeps its holder check** although the index already gives only that worker's keys: a cheap guard should the two ever disagree.
- **The `leases.rs` test panic now says what was expected** ("a fresh key wasn't granted").
