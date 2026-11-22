# X-pool-past-cap-room decisions

Follows X-pool-first-request-docs (#1061). That slice found seven first reservations waiting past the budget model's 30 s `MAX_WAIT`, at the test-sized budget (`LIMIT`, 1,000,000 B) with 640 seeds. It asked whether a growth past the grown claims' cap should leave room for two first requests.

**The runner chose option (d) under `docs/owner-decisions.md` §2.26 and flagged it to the owner under §2.19:** a claim held back by its network's share keeps its place in the queue.
- No bound moves.
- `MAX_HELD_BYTES`, `HELD_BYTES`, `PARTIAL_NET_SHARE`, `FLOOR_SHARE`, `SMALL_CLAIM` and `largest_dict` are unchanged.
- The past-cap rule is unchanged.

## Root cause: a held-back claim lost its place
- **The trace:** seed 109, the 43.6 s wait. It was claim 178, from network 5: a five-piece dict, with a 72,064 B first request including the key.
  - It reached position 1 in `partials` eight times.
  - Each time, a younger claim from network 5 was let in first. That put the network over its share: 93,750 B at `LIMIT`, about one first request.
  - Claim 178 was then taken out of the queue to "wait apart".
  - When the share freed, it rejoined at the back, behind 5–17 other claims. Nothing ordered a network's own waiting claims, so a younger one could win each time.
- **The past-cap growth only stretched each cycle.**
  - At `LIMIT`, a growth past the cap, such as 672,089 B, lets no first request in while it waits at the front. Its own 88,448 B plus one other, plus its 583,641 B extra, is over the 750,000 B beside the floor.
  - It also lets none in while it's grown: 672,089 + 88,448 > 750,000.
  - That makes stalls of about 3–10 s, which are bounded and drain. They turn into a 30 s+ wait only when a claim keeps losing its place across several of them.
- **Why only at `LIMIT`:** at `WORKER` (4 MiB) a network's share is about 4 first requests (393,216 B), so a network is rarely past it.

## The rule
- **Partial claims:** they queue in `partials` as (place, id), oldest first. Only the front may take room, as before.
- **A claim held back by its network's share** moves to its network's `apart` queue, keeping its place. While it waits there it holds up no other network's claim.
- **When the share has room, the network's oldest held-back claim takes its place back.** It is inserted by place, so it goes ahead of younger claims of any network. The younger claims of its network stay in `apart` behind it.
- **A new claim waits behind its network's held-back claims,** even one small enough for what's left of the share. Otherwise a stream of small claims from its own network could keep a larger one out, which is the same starvation.
- **The head is found in O(1),** since nothing in `partials` is held back.
  - `partials` is bounded by the fetches in flight, one claim each: `max_sockets`, which is 400 by default, at most 4,096 in settings, and 200 in a worker.
  - The rejoin is one binary search and one insert per transition. It allocates nothing beyond the queues' own growth.
  - Membership in `apart` is a flag on the claim, so a woken claim never scans its network's queue.
- **`starved` stays on the head:**
  - A claim that takes its place back at the head clears another claim's `starved`, and wakes whole claims and dispatch.
  - Before, the head could change only by leaving.
  - The model now checks on every tick that `starved` is only ever the head.

## A lost wake-up fixed on the way
- **Before:** a partial claim that settled without growing woke nobody. `settle` woke waiters only for a grown or small claim, or one whose weight fell. So a claim its network's share held back stayed asleep until something else freed bytes.
- **The fix:** `settle` now also wakes waiters for a partial claim.
- **The proof:** `a_held_back_claim_is_woken_however_its_network_frees_its_share` failed on its "settles" row without the fix.

## What a slow or malicious peer can hold
- **Unchanged.**
  - A network holds at most max(share, one first request) in partial claims, until `FIRST_WINDOW_DEADLINE`.
  - A proven peer holds its growth until its deadline or the pace rule.
  - The floor is untouched.
- **A flooding network** only queues behind its own older claims. `a_flooding_network_holds_up_no_other` shows another network's claim let in at once beside 200 held-back ones.

## Options considered
All numbers are from the budget model: waits past 30 s, the worst wait, waits over 1 s, and the most grown and held bytes at any check.

| Option | `LIMIT`, 640 seeds | Cost |
|---|---|---|
| (c) No change | 7 past 30 s; worst 43.6 s; 200,323 over 1 s; grown 700,106; held 999,786 | none; the model would have to drop its 30 s claim at `LIMIT` |
| (a) `largest_dict` leaves room for two first requests (margin 176,896 B, not 32 KiB) | 0; worst 18.4 s; 196,538; grown 566,832 | the worker's `MAX_METADATA` falls from 3,112,960 B to 2,968,832 B, a coverage cut |
| (a′) a lone past-cap growth borrows one first request of the floor | not measured | loosens the floor against slow peers: the owner's (owner_queue n 34) |
| (b) the margin scales with the budget (room / 4) | 0; worst 18.4 s; 196,486; grown 556,228 | the main node's `MAX_METADATA_MIB` falls from 47 to 36, and the worker's largest dict to 2.25 MiB |
| **(d) held-back claims keep their place (chosen)** | **0; worst 18.9 s; 200,575; grown 700,106; held 999,863** | none; no bound moves |

**The final head:**

| Budget | Seeds | Past bound | Worst wait | Over 1 s | Most grown | Most held |
|---|---|---|---|---|---|---|
| `LIMIT` | 64 | 0 (c: 0) | 15.3 s (c: 15.3 s) | 19,443 (c: 19,440) | 699,903 | 998,908 |
| `LIMIT` | 640 | 0 (c: 7) | 18.9 s (c: 43.6 s) | 200,575 (c: 200,323) | 700,106 | 999,863 |
| `WORKER` | 64 | 0 (c: 0) | 3.6 s (c: 3.6 s) | 279 (c: 279) | 2,800,028 | 3,835,861 |
| `WORKER` | 640 | 0 (c: 0) | 6.7 s (c: 6.7 s) | 3,659 (c: 3,659) | 2,800,028 | 4,173,612 |

**Why the past-cap rule is unchanged:**
- Letting such a growth leave room for two first requests means either a smaller largest dict, as in (a) and (b), which is a coverage cut, or letting it into the floor, as in (a′), which loosens a slow-peer bound.
- Once a claim keeps its place, the past-cap stalls stay well inside `MAX_WAIT`.

## The model
- **The CI run:** the default 64-seed run is still the CI run, and now asserts that no wait passed its bound.
- **New counts:** `Counts` reports waits past their bound, the worst wait and its seed, and the most grown and held bytes.
- **Measuring:** `POOL_MODEL_MEASURE=1` counts a wait past its bound instead of failing the seed, up to ten times the bound, to compare rules.
- **The 640-seed caveat is gone** from the module doc. `POOL_MODEL_SEEDS=640` passes at both budgets on this head.

## Tests
- **The changed test:** `a_claim_held_by_its_networks_share_keeps_its_turn` now asserts the opposite order.
  - B, held back by its network's share, is older than C, another network's claim that heads the queue meanwhile. When the share frees, B goes first, while C still waits on its own rule.
  - The old test had B queue behind C. That order was the bug: a claim's wait had no bound, since each time its network's share freed it went behind every claim that had come since.
- **`one_networks_partial_claims_keep_to_their_share`:** it now checks that the held-back claim is in its network's `apart` queue.
- **New tests,** all deterministic in paused time, with waits on conditions with 10 s deadlines:
  - `a_held_back_claim_is_woken_however_its_network_frees_its_share`: one table row each for the claim holding the share growing, settling, being dropped, being cancelled while it waits to grow, and running out a deadline waiting.
  - `a_dropped_held_back_claim_leaves_no_dead_head`.
  - `a_networks_oldest_held_back_claim_goes_first`.
  - `a_younger_claim_waits_behind_its_networks_held_back_one`.
  - `a_claim_back_at_the_head_ends_anothers_precedence`.
  - `a_flooding_network_holds_up_no_other`.
- **Mutations,** each in a scratch copy, then restored. Each failed a test:
  - no wake-up on settle;
  - `Queued`'s drop leaving `apart` stale;
  - rejoining at the back;
  - ignoring older held-back claims;
  - letting any held-back claim rejoin;
  - keeping `starved` when a claim takes the head back.

## Nits from #1061's review
- `pool.rs`'s budget doc says "first request" where a partial claim's first reservation is meant.
- The stats test's lower bound is `>= 80 << 10`, the 80 KiB first request.
- The model's `Counts` doc and `utp.rs`'s measurement doc are re-wrapped.
