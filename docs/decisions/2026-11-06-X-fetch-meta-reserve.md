# X-fetch-meta-reserve decisions

Runner brief 2026-11-06T23:35Z. It follows `X-fetch-pool-bytes` (#554, `docs/decisions/2026-11-06-X-fetch-pool-bytes.md`, "For X-fetch-meta-reserve"). Architecture §18.4 (the fetch pool), §6 (hostile-peer limits), §3 (budgets). Owner §1.6.5: a worker uses at most 256 MB.

## Reserve before the pieces
- **Where it hooks in.** `fetch_on_stream` reads `metadata_size` from the peer's extended handshake and checks it against `max_metadata`, as before. Then it awaits `FetchOptions::reserve`, a new optional hook (`lodestar_peer_wire::Reserve`, a newtype like `Deny`). Only once the hook returns does it build the `Assembly` and send the first `metadata_request`. `None` keeps the old behaviour, so every other caller is unchanged.
- **What it reserves:** an upper bound on what the returned `Fetched` can hold on its heap:
  - the announced size;
  - the client name's length (known by then);
  - a full PEX list (`MAX_PEX` × `size_of::<SocketAddr>()`, 6,400 B).

  The pool adds `size_of::<FetchedKey>()`, so a reservation is in `pool::weigh`'s units.
- **The result never outgrows its reservation:**
  - **The dict:** `Assembly` already refused a piece that wasn't requested or had the wrong length, so a peer can't write past the announced size. Its buffer now also grows only up to that size: it doubles as `Vec` does, but is clamped with `reserve_exact`. Before, doubling could leave the capacity up to twice the size.
  - **The PEX list and client name** are returned at exact capacity (`into_boxed_slice`, `into_boxed_str`). `weigh(fetched)` is therefore at most the reservation, and finishing only ever gives bytes back.
- **A peer that announces more than it sends** holds its reservation until its fetch ends: at the deadline (8 s) at most, or when it hangs up. It allocates nothing more than it sends (architecture §6).
- **A peer that sends more than it announced** is refused (`Protocol`), and its buffer never passed the announced size.

## The shared budget (`pool::Budget`)
- **One budget for held and reserved bytes.** `max_held_bytes` (64 MiB by default) now limits the bytes reserved by fetches in flight plus the bytes of finished results the pool holds. Held bytes are counted the same way and over the same span as in X-fetch-pool-bytes: not yet joined, in the outbox, and in the channel not yet taken.
- **A claim per fetch.** The pool gives every fetch a `Reservation` (`Fetcher::fetch_reserving`), and `PeerWireFetcher` wires it into the hook. Clones share one claim, and the last clone to drop releases it. Each path releases exactly once:
  - **Success:** the fetch task settles the claim as the fetch finishes. The reservation becomes the result's weight in held bytes, and any surplus is released. There's no double count: the claim reads 0 afterwards, so its drop releases nothing.
  - **Failure, timeout, a peer that closes, the abort at shutdown, a panic:** the future that holds the claim is dropped, and the drop releases it.
  - **The pool lets go of a result** (the consumer takes it, a purge drops it, the shutdown drain sends or requeues it, a purged key's fetch ends): its weight leaves the held bytes. The old `finished` counter, `outbox_bytes` and `sent_bytes` are folded into the budget.
- **The trait change is one default method.** `fetch_reserving(peer, key, reservation)` calls `fetch` by default, and the pool always calls `fetch_reserving`. The test fetchers, the crawler simulator's and WK-7's stages compile unchanged. A fetcher that doesn't reserve has its result counted when it finishes, as before: its bytes still stop fetching once they're held, but a burst can pass the budget by what's in flight. Only `PeerWireFetcher` reserves. `FetchedKey` and the output channel are unchanged.

## A full budget: the fetch waits
- **The choice: wait (backpressure), not fail.** A fetch that can't reserve keeps its connection and waits for room; its 8 s deadline still runs.
- **Why not fail?**
  - **A new outcome:** failing needs a new `FailureKind` and funnel reason, in `peer-wire/src/lib.rs` and the funnel. Both are outside this slice and seen by Insights.
  - **Wasted work:** a failure throws away a handshake that succeeded, and the retry has to dial again.
  - **Most waits are short:** room usually frees within a dict's download time, since the consumer takes results continuously.
- **What waiting costs:** a socket and a peer connection, held while it waits.
- **Never over-allocates.** A reservation is granted only if held plus reserved bytes stay within the budget after it. There's one exception: if nothing else is reserved or held, it's always granted. That lets a dict larger than the whole budget through, alone, as X-fetch-pool-bytes allowed. Without it, such a dict would never arrive.
- **Oldest first.** Waiting reservations form a queue, and only the oldest may take room. Without the queue, small dicts that fit could keep a large one waiting until its deadline. With it, a large one at the head makes the small ones behind it wait. That's bounded: the pool starts nothing while a reservation waits (below), so what's ahead of it drains within one deadline, apart from held results that need the consumer.
- **Cancel-safe.** A wait that's dropped (the deadline, a shutdown) leaves the queue and has taken nothing. Whenever bytes are released, every waiter re-checks; only the head can take room.
- **The pool starts no fetch while the budget is full or a reservation waits.** This replaces X-fetch-pool-bytes' `in flight × estimate` guess: the moving-average estimate is gone. A fetch reserves only once its handshake is done, so dispatch can't foresee what it will need. Real reservations make the guess unnecessary, though: whatever starts, held plus reserved can't pass the budget. When a waiting reservation is granted or cancelled, the pool wakes to dispatch again.
- **A fetch that runs out its deadline while waiting isn't the peer's failure.** The fetch task notices that its claim was still waiting when the fetch ended. The pool then counts `fetch_reserve_timeouts_total`, not a failure. It marks the peer untried again and puts the key back in its queue. No lookup attempt is used.
  - **The known cost:** with the consumer stalled and the budget nearly full, keys can cycle through dial, wait and time out every deadline. That's at most `max_sockets` dials per 8 s, below the normal crawl rate, until the consumer moves. Nothing is lost, and memory stays within the budget.
- **`held_full` keeps its meaning.** It counts the times the budget (now held plus reserved) or a waiting reservation stopped dispatch while keys were ready. Outbox backpressure still stops dispatch, but isn't counted.

## The pool notes from #554's review
- **A deadline on the shutdown drain: `pool::SHUTDOWN_DRAIN`, 5 s.**
  - **The problem:** at shutdown, the pool sends the outbox to a consumer that runs until the pool ends. A consumer that's alive but stalled kept `out.send` waiting for ever.
  - **The fix:** the drain now reserves each slot with `timeout_at`, all against one deadline. After it, the rest go back into the saved queue as `QueuedKey`s, exactly as when the receiver is gone.
  - **Why 5 s:** half of Docker's default 10 s stop timeout, which leaves the rest of the pipeline's stop the other half. The runner decides the number (§2.26).
- **A purge drops a result's place in the recently-fetched order** too, not only its entry in the set. Before, the stale position remained: once the key was fetched again, ageing that position out removed the key from the set early, and a later sighting was fetched a second time. `a_purged_result_leaves_the_recent_order` fails without the fix.
- **The snapshot comment** said a key stored after the save is skipped on restore. Restore skips only keys already queued, so such a key is fetched again next run. The comment now says so.
- **`PoolIo::out` says the pool must be its only sender,** and what another sender would do (an overcount, which only slows fetching).

## Metrics (`docs/metrics.md`)
- **`fetch_reserved_bytes`** (gauge): bytes reserved by fetches in flight. Added to `fetch_held_bytes`, it's what the budget limits. Both are read under one lock.
- **`fetch_reserve_timeouts_total`** (counter): fetches whose deadline ran out while they waited. A steady rise means the budget is too small for the dicts being fetched.
- **`fetch_held_full_total`'s help text** now matches the doc. It counts stops caused by the budget or a waiting reservation; outbox backpressure doesn't count.

## The numbers (the runner decides the defaults, §2.26)
- **What the budget now bounds:** held plus reserved info-dict bytes, at most `max(max_held_bytes, max_metadata + 24 KiB)`. The second term is the one dict over the budget that passes alone. The 24 KiB is the most a client name and a PEX list add.
- **What it doesn't bound: each socket's working memory before or outside its reservation.** In the worst case, against a hostile peer, that's:

  | Item | Bytes |
  | --- | --- |
  | The extended message being parsed (`MAX_EXTENDED`) | 17,408 |
  | The client name (part of the reservation once it's made) | ≤ 17,407 |
  | The PEX list before its final shrink | ≤ 8,192 |
  | The `BufReader` | 8,192 |
  | The piece bitmap (1 B per 16 KiB piece) | ≤ 1,024 |
  | MSE and cipher state, the task and its futures | a few KiB |
  | **Total** | **under 64 KiB** |

  A normal peer uses a fraction of that. Kernel socket buffers aren't the process's RSS.
- **Single machine, today's defaults** (`max_held_bytes` 64 MiB, `max_metadata` 16 MiB, 400 sockets):
  - **Dicts:** at most 64 MiB, since 16 MiB is under the budget, so no dict passes alone over it.
  - **Working memory:** 400 × 64 KiB = 25 MiB.
  - **The peak is about 89 MiB, against 6,464 MiB (about 6.3 GiB) before this slice.**
  - **The aggressive preset's 1,000 sockets:** 64 MiB + 62.5 MiB, about 127 MiB, against about 15.7 GiB before.
  - **Against the single-machine 1 GB (§3):** both fit.
- **Throughput at the defaults is unchanged.** Ordinary dicts are 10–60 KB (§3), so a reservation is about 17–67 KB. 64 MiB holds about 1,000–3,900 of them, more than 400 or 1,000 sockets ever reach at once: most fetches fail before the handshake, and those hold no reservation. The budget binds only for runs of large dicts or a slow consumer, which is when it should.
- **A worker** (owner §1.6.5, 256 MB; WK-7's queues at 227 MiB). With the 4 MiB worker budget X-fetch-pool-bytes proposed, about 13.1 MiB of the 244.1 MiB is left (the brief's "about 14 MB"):
  - **Dicts:** at most `max(4 MiB, worker max_metadata + 24 KiB)`. With WK-7's proposed worker `max_metadata` of 4 MiB (`FETCHED_BYTES`), that's 4 MiB + 24 KiB. Those 24 KiB come out of the 13.1 MiB.
  - **Working memory:** 13.1 MiB ÷ 64 KiB is about **200 sockets** in the hostile worst case, against about 3 before this slice.
  - **Concurrent downloads:** at most about 4 MiB ÷ the average reservation, about 60–240 at ordinary sizes.
  - **The proposal:** a worker `max_sockets` of 200, `max_held_bytes` of 4 MiB and `max_metadata` of 4 MiB. The runner decides, with WK-10's measured RSS (`ext:owner-wkq3`).
- **Nothing here changes a default.** `max_held_bytes`, `max_metadata` and the presets are as before; only what the budget covers changed.

## For WK-10
- **Use `PeerWireFetcher`** (or any fetcher that implements `fetch_reserving` and wires it into `FetchOptions::reserve`) so in-flight bytes are inside the budget. A fetcher that only implements `fetch` is counted after the fact, as before this slice.
- **Size the worker from the table above:** `max_held_bytes` 4 MiB, `max_metadata` at most `FETCHED_BYTES`, and `max_sockets` from the headroom left ÷ 64 KiB.
- **Watch `fetch_reserved_bytes` + `fetch_held_bytes`,** which stay within the budget, and `fetch_reserve_timeouts_total`, which says the budget is too tight for the dicts being fetched.
- **The 5 s shutdown drain applies to a worker too:** at most 5 s of a stop goes to handing results to a stalled session task. The rest go back to the saved queue, or, on a worker, to the coordinator's re-grant once their leases expire.

## Files outside the slice's `owns`
- `crates/peer-wire/src/lib.rs`: `Reserve` added to the existing `pub use fetch::{…}` line, so `lodestar-sources` can name the hook. Approved by the runner (2026-11-06T23:36Z message to this session, which adds the file to the slice's owns). The same message accepted waiting rather than failing, and asked for three things to be shown; they are below.

## What the runner asked to be shown (2026-11-06T23:36Z)
- **No deadlock.** A waiting fetch holds a socket and a peer connection, but the bytes it waits for are released only by the consumer taking results, by joins and by fetches that end, never by another waiting fetch. A waiting fetch holds no bytes, so waiters can't wait on each other. `reservations_keep_held_and_reserved_bytes_within_the_budget` fills the budget with held results while every other socket waits on a reservation that has no deadline. Nothing moves for 120 s; then the consumer drains, and every key arrives once.
- **The wait ends at the fetch's own deadline, and is counted.** Peer-wire's deadline covers the wait. A fetch that runs it out is counted in `PoolStats::reserve_timeouts`, exported as `fetch_reserve_timeouts_total`; it isn't a failure or a loss, and the peer isn't charged. `a_reservation_that_waits_out_its_deadline_is_freed_and_retried` checks the count, that no failure was counted, and that the key is retried with the same peer.
- **The reservation is the heap bound, not the raw size:** `metadata_size + client name length + 200 × size_of::<SocketAddr>()` (6,400 B on 64-bit), plus `size_of::<FetchedKey>()` on the pool's side. `a_peer_announcing_max_metadata_reserves_exactly_its_bound` checks that a peer announcing exactly `max_metadata` reserves exactly that bound, and that one byte over the cap reserves nothing.

## Checks
- **`crates/peer-wire/tests/fetch.rs`**, against the fake peer and a raw one over TCP, with a semaphore standing in for the budget:
  - **`fetches_wait_for_room_and_stay_within_their_reservations`:** 8 peers with 200 KB dicts against a budget that fits two. The budget starts full, so every fetch waits at its hook and none downloads. Once the budget is freed, all complete; the most reserved at once stays within the budget, and each result's heap fits its reservation.
  - **`no_piece_is_requested_before_the_reservation`:** the raw peer sees its first metadata request only after the hook's grant.
  - **`a_peer_announcing_max_metadata_reserves_exactly_its_bound`** (above).
  - **`a_peer_that_announces_then_stalls_ends_at_the_deadline`**, **`a_peer_that_announces_then_closes_frees_its_reservation`**, **`a_peer_that_sends_more_than_it_announced_is_refused`:** each one reserved, ended as it should, and left nothing reserved once its holder dropped.
- **`crates/peer-wire/src/fetch.rs` unit tests:** `Assembly` never grows past the announced size, whatever order pieces arrive in, and the bound covers the name and a full PEX list.
- **`crates/sources/src/pool.rs` unit tests (`mod budget`):**
  - a claim is released once, when its last clone drops;
  - settling turns it into held bytes and releases the surplus, with nothing released twice;
  - a full budget makes reservations wait, oldest first, and stops new fetches;
  - a cancelled wait leaves the queue and takes nothing;
  - a claim over the budget passes alone.
- **`crates/sources/tests/pool.rs`**, on paused time with 60 s condition deadlines:
  - **`reservations_keep_held_and_reserved_bytes_within_the_budget`:** 48 keys of 1 MiB, 16 sockets, a 4 MiB budget and a stalled consumer. Held plus reserved stays within the budget at every check; X-fetch-pool-bytes' bound was budget + sockets × dict. Nothing more starts while reservations wait. Once the consumer moves, every key arrives once and nobody waited out a deadline.
  - **`a_reservation_that_waits_out_its_deadline_is_freed_and_retried`:** two fetches time out waiting. They count as reserve timeouts, not failures, and are retried with the same peer; all four keys arrive.
  - **`a_reservation_over_the_budget_waits_for_the_budget_to_empty`:** 2 MiB dicts against a 1 MiB budget: one held at a time, all delivered.
  - **`a_receiver_closing_while_the_pool_runs_stops_fetching`:** nothing more starts, and everything fetched is saved once at shutdown.
  - **`shutdown_with_a_stalled_consumer_saves_the_rest_after_the_deadline`:** the pool stops within `SHUTDOWN_DRAIN` plus 1 s and saves every result it couldn't hand over.
  - **`a_purged_result_leaves_the_recent_order`.**
- **The tests catch each mechanism's absence.** Each of these mutations makes at least one test fail:
  - no recent-order fix;
  - no budget check in `try_grow`;
  - no drain deadline;
  - waited-out fetches charged as failures;
  - dispatch ignoring waiting reservations.
