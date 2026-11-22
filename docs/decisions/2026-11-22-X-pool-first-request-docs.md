# X-pool-first-request-docs decisions

Since CR-UTP-FIRST-WINDOW (#1054), peer-wire's first request is:
- 5 pieces for a dict over 5 pieces, 80 KiB of dict plus the bound;
- the 4-piece window for a dict of exactly 5 pieces, then growth;
- the whole dict for one of 4 pieces or fewer.

This slice makes the pool's docs, fixtures and budget model say the same.

**No pool rule, bound or budget changes.** The changes are docs, test fixtures and the model.

## Pinned constants, not a re-export (runner, step 0)
- **Why not a re-export:** `first_request` is private in `crates/peer-wire/src/fetch.rs`, and `FIRST_WINDOW` and `result_bound` are `pub(crate)`. A re-export would need visibility changes in a file that's docs-only for this slice (E0364), so `crates/peer-wire/src/lib.rs` is untouched.
- **The local copies:**
  - `pool_budget_model.rs` has `FIRST_REQUEST`, `FIVE_PIECE_FIRST` and `first_of(total)`.
  - `tests/pool.rs` has `first_reservation(size)`.
  - Both use the public `message::METADATA_PIECE` and a PEX bound of 200 × `size_of::<SocketAddr>()`, as `result_bound` has it with no client name.
- **The pins:** two tests, both called `peer_wire_reserves_these_first`, run the real `fetch_on_stream` over a tokio duplex against a scripted peer. The peer announces a size and the client name "Lodestar". The tests record the fetch's first `(now, total)`.
  - The model's test covers dicts of 1 KiB, 4 pieces, 4 pieces + 1, 5 pieces, 5 pieces + 1, 6 pieces and 8 pieces.
  - The fixture's test covers 1 KiB, 4, 5, 6 and 64 pieces.
  - The two scripted peers are duplicates, because an integration test can't see a unit module. Each names the other, so they change together.

## Fixtures
- **The silent fake's reservation:** silent peers reserve `first_reservation(size)` instead of `(size.min(64 KiB), size)`.
- **`silent_claimers_pin_one_small_window_and_small_dicts_still_flow` asserts exact sums:** 20 × (one first request for 1 MiB + one for 64 KiB), each with the key, instead of `64 KiB + 8 KiB` of slack each.
  - The 1 MiB announcers pin 80 KiB + the bound, about 87 KiB with the key.
  - The 64 KiB announcers pin 64 KiB + the bound.
- **What it proves is unchanged:** 64 small dicts still flow within the silent peers' deadline, under a 4 MiB budget, with no reserve timeouts.

## The per-network invariant: `held <= max(share, one first request)`
- **What `net_partials_full` does:** it admits a network's first partial claim whatever its size, and a later one only while `held + want <= share`. So a network holds its share, or one first request alone.
- **One first request** is `size_of::<FetchedKey>() + FIRST_REQUEST`, 88,448 bytes.
- **Why not the brief's literal `share + one first request`** (runner's call at step 0):
  - The literal form would loosen the model's check by about 6.5 KB against the old `share + SMALL_CLAIM`.
  - It would miss a mutated first reservation unless that reservation passed about 182 KB.
  - The max form is tighter than both at both budgets: share is 93,750 at LIMIT and 393,216 at WORKER.

## The model reserves peer-wire's sizes
- Every hand-written `64 << 10` is now `first_of(total)`.
- **A five-piece dict needs its own size.** `growth_pressure` draws totals down to 80,000. A flat 88 KB first request against an 80 KB total would make a whole claim over `SMALL_CLAIM`, which the pool counts as large, outside the grown cap. Peer-wire never makes such a claim; that's #1054's review fix. The first run with one constant failed the grown cap at seed 1 that way.
- `POOL_MODEL_SEEDS=n` runs seeds 1 to n. The default stays 64.
- With `--nocapture`, each test prints its counts: first reservations, growths, waits for room over 1 s, and the most any network held in partial claims.

**Counts, 64 seeds (the default), before → after**

| Budget | Claims | Growths | Waits over 1 s | Most one network held |
|---|---|---|---|---|
| LIMIT (1,000,000) | 38,597 → 38,597 | 5,188 → 5,188 | 19,140 → 19,440 | 65,664 → 88,448 |
| WORKER (4 MiB) | 38,597 → 38,597 | 5,188 → 5,188 | 277 → 279 | 196,992 → 265,344 |

**Counts, 640 seeds (10×), before → after**

| Budget | Claims | Growths | Waits over 1 s | Most one network held | Waits past `MAX_WAIT` |
|---|---|---|---|---|---|
| LIMIT | 391,675 → 391,675 | 52,405 → 52,405 | 198,256 → 200,323 | 65,664 → 88,448 | 0 → 7 |
| WORKER | 391,675 → 391,675 | 52,405 → 52,405 | 3,636 → 3,659 | 196,992 → 265,344 | 0 → 0 |

## Seven waits past 30 s at the test-sized budget (runner's call under §2.26, flagged under §2.19)
- **At LIMIT with 640 seeds,** seven first reservations, in five seeds, waited past `MAX_WAIT` (30 s): seed 109 43.6 s; 495 33.6 s; 131 31.3, 31.9 and 32.4 s; 111 31.6 s; 623 30.3 s. Every one was granted in the end, so nothing deadlocked.
- **Why, at seed 109:** a growth past the grown cap waits at the front: 672,089 in all, its own first request 88,448. It grows only within the 750,000 outside the floor.
  - With a 64 KiB first request, it fit beside one other partial claim: 65,664 + 65,664 + 606,425 = 737,753 ≤ 750,000.
  - At the real size it doesn't: 88,448 + 88,448 + 583,641 = 760,537 > 750,000.
  - So it waits until every other first request is gone, and first requests behind it wait longer.
- **Not a production regression:** no production pool runs at LIMIT. The pool's default is `MAX_HELD_BYTES` (64 MiB), a worker's is `HELD_BYTES` (4 MiB), and neither is configurable. WORKER passes all 640 seeds.
- **The default run is unchanged:** no change to `MAX_WAIT`, the traffic or the seed count, and no test exception or skip. The model's module doc says where `MAX_WAIT` is known to hold.
- **Follow-up for the runner to place:** should a past-cap growth at a small budget leave room for two first requests? It's a pool-rule question, for a core slice.

## Docs
- `pool.rs` covers the budget's two steps, `SMALL_CLAIM` and the stats test's comment.
- `fetch.rs` covers `FIRST_PIECES`: the first window is what must arrive, and a silent peer pins `first_request`'s 80 KiB. It also covers `Assembly.limit`.
- `utp.rs` has three comments: the product asks for 5 pieces until the window is in.
- `docs/metrics.md`'s First windows paragraph no longer says the first window is asked for all at once.

## Mutations (each in a scratch copy, restored)
- **The model's first reservation doubled** (`2 × FIRST_REQUEST`): the invariant fails ("seed 1: network … holds 176768 in partial claims"), and so does the pin.
- **`FIRST_REQUEST` raised by one piece:** the pin fails ("a dict of 81921: first", 88,328 against 104,712).
