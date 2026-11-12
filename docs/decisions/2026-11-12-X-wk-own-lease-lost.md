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

## Only a remote worker tracks `out`
- **The coordinator's built-in pipeline** also uses `WorkerLeases` and `PrepareStage`, but its intake never asks `insert` about its own leases: a held key's sighting is answered as a reply there.
- So **tracking is opt-in.** The remote worker's runtime builds its leases with `WorkerLeases::tracking_out(horizon)`, and `Default` tracks nothing. With tracking off, `send_out` is the old `take` and `insert` answers as before.
- The coordinator's memory and behaviour don't change (#712-style self-review, I2).

## Keeping `out` bounded, and its order race
- **An out entry is finished, not removed** (`done(key, token)`, for its own token only). That happens when:
  - its batch is committed: each of an accepted batch's `(key, token)` pairs;
  - its result is released (`release`);
  - its result is lost in the judge (`Judged::Dropped`, `Judged::Invalid`);
  - its result is refused while batching (`batch_up`);
  - the prepare stage's send fails.
- **A finished entry still answers `Out`.** The commit (`Accepted`, through the session's events to the results task) and the answer to the holder's claim (a oneshot to the claim task) take different paths, so the commit can be seen first. Tokens are never reissued, so answering `Out` for a finished entry is always right. The race is closed instead of narrowed (self-review B1).
- **`pending_len`** counts out entries not finished yet. The chaos scenario and the runtime test assert it returns to 0.
- **Every entry is pruned on `insert` once it's older than the horizon**, a lease TTL (600 s) plus the longest a result may wait to be sealed (`seal_wait`, 600 s). That bounds the map to the results of one horizon.
  - **The limit:** a result still out past the horizon is forgotten. That covers a batch the coordinator keeps refusing for longer, or a spool replayed later, with the session up or not. Its own lease coming back then counts as new, which is the old behaviour, not worse, and the coordinator fences any stale result as before. WK-G's gates should know a holder's sightings stay safe only within the horizon.
- **The funnel's identity holds:** a held-again grant changes no lease on either side, so harvested = Σ terminal − late + in flight, as the chaos scenario asserts.

## Tests
- **`WorkerLeases` unit tests:**
  - the three answers;
  - untracked leases behaving as before;
  - `done` finishing only its own token and staying known;
  - a loss before the result goes out;
  - the prune, at the horizon and for an entry finished and sent out again;
  - two-thread races, 1,000 rounds each, between the same lease's `insert` and `send_out`, and between that `insert` and `send_out` then `done`: the key is never held again once its result went out.
- **A runtime unit test** (`the_holders_own_lease_coming_back_never_ends_it`), through the real claim path on the claim test rig: claim, fetch, results out, then a sighting answered with the same lease.
  - It waits until the pool has taken the reply (its `duplicates` count). Then it asserts nothing is lost, one grant, and the lease out with the result.
  - Then the batch is committed (`pending_len` back to 0), and another sighting answered with the same lease is still not new.
- **A chaos scenario** (`a_holder_sighting_its_own_keys_keeps_their_leases`), made deterministic with WK-20's relay:
  1. The holder's fetch is held, its link is cut coordinator → worker, and it sights its own keys. Its heartbeats show the claims arrived.
  2. The fetch is released and the results are committed.
  3. The link heals, so the answers land after the fetch and before the commit reaches the worker.

  It asserts no loss on the worker or in the funnel, one fetcher, no `fetch_violations`, and `out` back to 0. It fails 5 out of 5 runs on main.
- **With the old behaviour restored** (prepare `take`s, every grant counts as new), both the runtime test and the scenario fail.
- **rv-712 nit 2:** a hand-back recorded after both fetches excuses neither of them (`a_recorded_hand_back_excuses_only_later_fetches`).
