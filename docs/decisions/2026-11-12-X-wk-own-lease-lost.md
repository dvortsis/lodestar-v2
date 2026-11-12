# X-wk-own-lease-lost decisions: a worker that re-sights a key it holds never ends its own lease as lost

Found by WK-20's harness (#712, `docs/decisions/2026-11-11-WK-20.md`, finding (a)). Brief: runner, 2026-11-12.

## The bug
A worker claims every key its DHT sees, including keys it already holds. The coordinator answers a claim from the key's holder with the holder's own lease, under the same token (`crates/cluster/src/server.rs`, `Claimed::Held` by the holder).
- The runtime passed that grant to its pool as a new sighting. By then the pool had usually fetched the key: `PrepareStage` had moved the token out of `WorkerLeases` into the `Results` record, which carries it to the commit.
- So the pool turned the sighting away as recently fetched. The worker's `Funnel::duplicate` override then took the token just inserted again and reported the lease lost.
- The coordinator ended the lease, the result arrived stale, and another worker harvested and fetched the key again. With three workers that showed as harvested 6, stored 3, lost 3, with two workers fetching each key.

## The fix: the worker knows its own lease wherever it is (option A, the runner's call)
- **`WorkerLeases` tracks both places a lease can be,** under one mutex (`crates/pipeline/src/worker.rs`, a shared edit the runner approved):
  - *held*: granted and not yet through the prepare stage;
  - *out*: its result is on its way out under that token.
- **`PrepareStage` moves a token from held to out in one step** (`send_out`), where it used to `take` it. A loss in the prepare stage still `take`s it.
- **`insert` gives a three-way answer under the same lock:** `New` (held now), `Held` (already held under this token) or `Out` (its result is out under this token). On `Out` the key isn't held again.
- **The runtime** (`worker/runtime.rs`, `granted`) treats `Held` and `Out` as the holder's own lease coming back:
  - it isn't counted as a grant (`Counts::held_again`) and never goes into the pool as a new sighting;
  - its peers, if any, go in as a reply (`reply: true`), as hints do. A key still waiting for peers uses them, and a key the pool is done with ignores them. A reply never calls `Funnel::duplicate`, so it never ends a lease.
  - **Why not a plain no-op:** the peers are worth keeping. A re-granted key arrives with none, and this is one way it gets some.
- **A grant under a new token** (another lease for the key) behaves as before: held, and into the pool as a sighting. So the pool still ends a new lease for a key it fetched lately but didn't store (R-mem-triage).
- **Rejected, and why:**
  - **The runtime alone** (record the token when the result reaches the runtime's `Sealing` outbox): it leaves a gap after `PrepareStage`'s `take`, with no await but other threads running, where the bug still happens.
  - **The session client's held set** (`crates/cluster`): it's per session, rebuilt on every reconnect, while a spooled result can come back under a later session. It's also next to WK-11b's work.

## Keeping `out` bounded
- **An out entry is cleared** (`done(key, token)`, only for its own token) when:
  - its batch is committed: each of an accepted batch's `(key, token)` pairs;
  - its result is released (`release`: it couldn't be sealed in time, or similar). Every runtime path that gives up on a result goes through `release`.
- **Entries older than a lease TTL (600 s) are pruned on each `insert`,** from a queue in the order they went out, so a missed path can't grow the map beyond one TTL of results.
  - A spooled result replayed more than a TTL later has been pruned. Its own lease coming back then counts as new, which is the old behaviour, not worse. By then the coordinator has expired the lease anyway, unless the worker kept heartbeating.
- **The funnel's identity holds:** a held-again grant changes no lease on either side, so harvested = Σ terminal − late + in flight, as the chaos scenario asserts.

## Tests
- **`WorkerLeases` unit tests:**
  - the three answers;
  - each clearing path (`done` for its own token only, `take` on a loss);
  - the prune, at the TTL and when an entry was cleared and sent out again;
  - a two-thread race between `send_out` and the same lease's `insert`, 2,000 rounds: the token ends up held or out, never both and never neither.
- **A runtime unit test** (`the_holders_own_lease_coming_back_never_ends_it`), through the real claim path on the claim test rig: claim, fetch, results out, then two more sightings answered with the same lease. Nothing is lost, there's one grant, and the lease stays with the result.
- **A chaos scenario** (`a_holder_sighting_its_own_keys_keeps_their_leases`), made deterministic with WK-20's relay:
  1. The holder's fetch is held, its link is cut coordinator → worker, and it sights its own keys. Its heartbeats show the claims arrived.
  2. The fetch is released and the results are committed.
  3. The link heals, so the answers land after the fetch and before the commit reaches the worker.

  It asserts no loss on the worker or in the funnel, one fetcher, no `fetch_violations`, and `out` back to 0. It fails 5 out of 5 runs on main.
- **With the old behaviour restored** (prepare `take`s, every grant counts as new), both the runtime test and the scenario fail.
- **rv-712 nit 2:** a hand-back recorded after both fetches excuses neither of them (`a_recorded_hand_back_excuses_only_later_fetches`).
