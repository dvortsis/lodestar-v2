# X-wk-fetched-lease decisions: the holder's own claim on a fetched lease

Closes the window that X-wk-out-set-polish (#737) named: a coordinator writer lagging past the worker's 180 s grace could still end the holder's own lease as lost. Brief: runner, 2026-11-12, option a′ from rv-737.

## The problem
- **`Accepted` doesn't end a lease.** It means the batch is in the coordinator's intake queue. The lease ends when the writer commits, behind up to 16 batches in intake and 64 in the writer's queue.
- **Heartbeats renew all of a worker's leases** (`LeaseTable::heartbeat`), so until then the lease lives, and the coordinator answers the holder's own claim for the key with it.
- **The worker can't tell** when the writer commits. Its out set remembers a committed entry for 180 s only (`FINISHED_GRACE`).
- **Past that:**
  1. the holder's own lease comes back and looks new;
  2. the pool turns it away as a duplicate (`WorkerFunnel::duplicate`);
  3. the lease ends as lost while its result still waits in the writer's queue.

## What changes
- **The worker reports `Fetched` on `Accepted`** (`crates/lodestar/src/worker/runtime.rs`).
  - For every lease in the accepted batch, it reports `Outcome::Fetched` before telling its session the leases are committed. One task does both, so they're in that order.
  - The protocol already had `Fetched`, but the runtime never sent it.
  - Reports are split at `MAX_OUTCOME_BATCH`. A results batch holds at most 1,024 records, the same number, so today one report per batch is enough.
- **The coordinator marks the lease** (`LeaseAuthority::fetched`, `LeaseTable::mark_fetched`).
  - Only the holder, under the lease's token, can mark it; anything else is refused and changes nothing.
  - The marks are a map from token to the key and when it was marked. Tokens are never reissued, so a token names one lease.
  - The first mark counts; a repeat keeps it.
  - The mark goes with the lease, however it ends: the commit, a loss (`release`), an eviction or an expiry. So the map holds only live, fetched leases.
- **The holder's own claim on a fetched lease is answered as committed** (`claim_batch` returns `BatchAnswer::Committed`, which the server sends as `Denied{Stored}`).
  - The worker drops a `Denied` key (`retries.done`). It never reaches the pool, so nothing ends the lease, however long the writer takes.
  - The claim's source still joins the lease, as any claim's does.
- **Any other worker's claim is answered exactly as before:** `Held`, with its hints relayed to the holder.
- **A fetched lease expires a TTL after the mark** (600 s at defaults), even while its holder heartbeats (`LeaseTable::expire`).
  - **Why:** a batch the coordinator accepted but never committed would otherwise keep its lease for as long as the holder lives, and the holder's claims would be denied all that time.
  - **What happens to the key then:** it expires like a silent worker's lease. It waits for a re-grant with its cohort and sources, and the next claimer gets it under a new token. That may be the same worker, which then fetches it again.
  - **A commit that comes later** under the old token is stale, and the writer handles it as a late result (plan Q7).
- **A `Fetched` for a key with no lease isn't counted as a stale report.**
  - The worker sends `Fetched` on `Accepted`, so the writer's commit often ends the lease before the report arrives. That's expected.
  - A `Fetched` under another token, for a key that still has a lease, still counts in `cluster_stale_reports_total`, as before (`fetched_keeps_the_lease_and_results_need_a_sink`).

## Compatibility
- **An older worker never sends `Fetched`.** Its leases are never marked, and its own claims get its lease back as today (`a_holder_that_never_reports_fetched_gets_its_lease_back`).
- **A newer worker with an older coordinator:** the older coordinator checks a `Fetched` and keeps the lease, as it always did, and still answers the holder's claims with the lease. The worker's 180 s grace still covers it there.
- **No wire change:** `Outcome::Fetched` and `DenyReason::Stored` already exist.

## The worker's grace stays at 180 s
- **The window it still covers:** a claim the worker sent before its `Fetched` report reached the coordinator is answered with the lease. That's milliseconds to seconds.
- **Older coordinators:** the grace is the only cover against a coordinator that doesn't mark.
- **Shrinking it** is for once every coordinator in the field marks leases. Not now.

## The built-in worker
- **It never sends `Fetched`:** its results go straight to the writer, not through `Accepted`. So its leases are never marked.
- **No extra check for it:** the claim check doesn't special-case `WorkerId::LOCAL`. The server test's session connects under that id, which lets the test exercise the real path.

## Tests
- `a_fetched_lease_answers_its_holders_claims_as_committed` (`crates/cluster/src/leases.rs`), on the authority's injected time:
  - the writer is stalled 500 s, past the 180 s grace;
  - the holder's claim is `Committed`;
  - the lease is still current, with the claim's source added;
  - nothing is lost;
  - another worker's claim is `Held` as before;
  - the commit ends the lease and its mark.
- `a_holder_that_never_reports_fetched_gets_its_lease_back`: an older worker.
- `a_fetched_lease_with_no_commit_expires_a_ttl_after_the_mark`:
  - with the holder still heartbeating, there's no expiry at the TTL and one just past it;
  - the key waits for a re-grant;
  - the old token is stale;
  - a loss or an eviction takes the mark too.
- `an_accepted_batch_reports_its_leases_fetched` (the runtime): `Accepted` sends `Fetched` for the batch's lease.
- `fetched_keeps_the_lease_and_results_need_a_sink` (`crates/cluster/src/tests.rs`, a shared edit): over a real session, the holder's claim after `Fetched` is `Denied{Stored}`, and the lease stays for the commit.
