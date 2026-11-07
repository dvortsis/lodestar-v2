# X-fetch-meta-reserve decisions

Runner brief 2026-11-06T23:35Z. It follows `X-fetch-pool-bytes` (#554, `docs/decisions/2026-11-06-X-fetch-pool-bytes.md`, "For X-fetch-meta-reserve"). Architecture §18.4 (the fetch pool), §6 (hostile-peer limits), §3 (budgets). Owner §1.6.5: a worker uses at most 256 MB.

## Reserve before the pieces
- **Where it hooks in.** `fetch_on_stream` reads `metadata_size` from the peer's extended handshake and checks it against `max_metadata`, as before. Then it awaits `FetchOptions::reserve`, a new optional hook (`lodestar_peer_wire::Reserve`, a newtype like `Deny`, called with `(now, total)`). It sends no request the reservation doesn't cover until the hook returns. `None` keeps the old behaviour, so every other caller is unchanged.
- **Two steps, so an unproven claim is cheap** (from the self-review, which found a one-step design let 4 lying peers stall ingest; the first window cut to 64 KiB by the runner, 2026-11-07, option (a)):
  - **At the handshake:** the first request window, the dict's first 4 pieces (64 KiB), or all of a smaller dict. The 10–60 KB dicts most torrents have (§3) are whole at this step.
  - **Once that whole window has arrived:** the full bound, before any request past the window. The peer has then sent 64 KiB of valid-length bytes (the hash is checked only at the end).
  - **What a silent peer costs now:** one that announces any size and sends nothing holds its first window's bound until its deadline: 64 KiB + its client name + 6,400 B of PEX room + the struct, about 71 KiB, against nothing on `main` and 1 MiB with the earlier 1 MiB window (the runner's review: 3 silent peers announcing 1 MiB pinned about 3 MiB of a 4 MiB worker budget, and a fourth blocked every small claim).
    - **Single machine (64 MiB, 400 sockets):** filling the budget takes about 920 silent handshakes, more than the 400 sockets, so silent peers alone can't fill it. They can hold at most 400 × 71 KiB, about 28 MiB.
    - **A 4 MiB worker:** filling it takes about 57 silent handshakes, from 29 addresses at `per_ip` 2, all at once, every 8 s. 20 announcing 1 MiB and 20 announcing 64 KiB leave small dicts flowing (`silent_claimers_pin_one_small_window_and_small_dicts_still_flow`).
    - **But see the partial share and growth below**, which a silent peer can use more cheaply on a worker.
- **What it reserves in total:** an upper bound on what the returned `Fetched` can hold on its heap:
  - the announced size;
  - the client name's length (known by then);
  - a full PEX list (`MAX_PEX` × `size_of::<SocketAddr>()`, 6,400 B).

  The pool adds `size_of::<FetchedKey>()`, so a reservation is in `pool::weigh`'s units.
- **The result never outgrows its reservation:**
  - **The dict:** `Assembly` already refused a piece that wasn't requested or had the wrong length, so a peer can't write past the announced size. Its buffer now also grows only up to the pieces requested so far, which is what's reserved: the first window until growth, then the whole size. It doubles as `Vec` does, but clamped with `reserve_exact`. Before, out-of-order pieces could double the capacity past the window or the size (second review; `assembly_never_grows_past_what_was_requested` replays its case).
  - **The PEX list and client name** are returned at exact capacity (`into_boxed_slice`, `into_boxed_str`). `weigh(fetched)` is therefore at most the reservation, and finishing only ever gives bytes back.
- **A peer that announces more than it sends** holds at most its first window until its fetch ends: at the deadline (8 s) at most, or when it hangs up. It allocates nothing more than it sends (architecture §6).
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
- **Partial claims and growth.** A claim for a dict over the first window is *partial* until it grows. Two rules keep the two steps from deadlocking, since a partial claim waits for its growth while holding its window:
  - **Growth goes first.** While a growth waits, no first reservation is granted, and the pool starts no fetch. The bytes it needs can only come free: claims that hold their whole bound never wait, so they finish, and the consumer takes held results.
  - **Partial claims leave room for any one of them to grow.** A partial claim is admitted only if every partial claim's bytes, plus the largest growth any of them still needs, fit the budget. Once the rest has drained, the oldest growth therefore always fits beside the other partial claims, which wait behind it holding only their windows. That invariant only tightens as claims grow or end.
  - **Partial claims wait in their own queue.** A partial claim held back by its own rule (perhaps a liar's) never blocks a whole claim that fits, and the pool keeps starting fetches while one waits (second review).
  - **A partial claim that only lacks room goes first** (runner, 2026-11-07). When the oldest partial claim passes its own rule but not `used + extra ≤ budget`, it stops dispatch and goes before new whole first claims, as the head of the whole queue does. Otherwise steady small-dict traffic could keep a large honest dict out for ever (`a_room_starved_partial_claim_goes_before_whole_ones`). It's deadlock-free for the same reason as growth: what it waits for comes from whole claims that finish and held results the consumer takes, never from another waiter.
  - **What it means for liars: the partial share.** Silent liars announcing large dicts can fill the share the rule leaves for partial claims, which holds up large dicts (not small ones) until the liars' deadlines. Whole small dicts still reserve beside them, and dispatch goes on, as long as held plus reserved bytes fit (`partial_claims_that_never_grow_leave_room_for_small_dicts`, with the blocked claim still waiting; `growths_go_first_and_always_complete`).
    - **Single machine:** the share is the budget less the largest growth, about 48 MiB, room for about 690 partial claims, more than the 400 sockets. Liars can't fill it.
    - **A 4 MiB worker with `max_metadata` 4 MiB − 32 KiB:** the share is about 96 KiB, one partial claim at a time. **One silent peer announcing a large dict holds up every large dict on that worker for its deadline (8 s), at the cost of a handshake.** Small dicts are unaffected. That's the residual risk on a tight worker budget; a budget well above `max_metadata` widens the share (16 MiB with 4 MiB dicts leaves room for about 170). WK-10 sizes it (below).
  - **A dict whose bound is over the whole budget** is admitted only when nothing else is reserved or held. Its own rule holds it back whenever anything else is reserved or held, and a claim held back by its rule doesn't stop dispatch, so under steady traffic that may not happen within its deadline: the key waits out, and from the 3rd time it's charged (below), so it can end lost. That doesn't arise at the single-machine defaults (16 MiB is under 64 MiB). **A worker must keep `max_held_bytes` above `max_metadata` + 24 KiB + 1 KiB** (for example a 4 MiB budget with a `max_metadata` of 4 MiB − 32 KiB, or a 4.1 MiB budget), or its largest dicts won't arrive while it's busy.
- **Oldest first.** Waiting first reservations of whole results form a queue, and only the oldest may take room (partial claims have their own queue, and growths theirs, ahead of both). Without the queue, small dicts that fit could keep a large one waiting until its deadline. With it, a large one at the head makes the small ones behind it wait. That's bounded: the pool starts nothing while a whole reservation waits (below), so what's ahead of it drains within one deadline, apart from held results that need the consumer.
- **Cancel-safe.** A wait that's dropped (the deadline, a shutdown) leaves the queue and has taken nothing. Whenever bytes are released, every waiter re-checks; only the head can take room.
- **The pool starts no fetch while the budget is full, or a whole reservation, a growth or a room-starved partial claim waits.** This replaces X-fetch-pool-bytes' `in flight × estimate` guess: the moving-average estimate is gone. A fetch reserves only once its handshake is done, so dispatch can't foresee what it will need. Real reservations make the guess unnecessary, though: whatever starts, held plus reserved can't pass the budget. When a waiting reservation is granted or cancelled, the pool wakes to dispatch again.
- **A fetch that runs out its deadline while waiting isn't the peer's failure, up to 3 times per key.** The fetch task notices that its claim was still waiting when the fetch ended. The pool then counts `fetch_reserve_timeouts_total`, not a failure. It marks the peer untried again, after the key's other untried peers, and puts the key back in its queue if one is left. If the peer was its last, the key waits `retry_backoff` (120 s by default) and then tries it again, with no lookup, so one peer isn't redialled every deadline.
  - **The 3rd is charged** (`RESERVE_WAITS_FREE`, runner, 2026-11-07): it counts as the failed fetch it is (`TimedOut`), and the key goes on as after any timeout. So a key can't cycle for ever on waits alone (`the_third_reserve_timeout_is_charged`). The residual risk: a large dict that keeps losing its wait (behind liars holding the partial share on a tight worker, above) uses up its peers and attempts and ends lost, as a dict nobody serves would.
  - **With streaming peers on** (CR-1a-S), a key whose lookup is still streaming waits for the stream, as after a failure: its next batch or its end brings the key back, its peer still untried. Before, the key went into back-off while the stream held its lookup slot, and raising interest started a second lookup, so the lookup count leaked (runner's review). Raising interest on a key backing off with a peer still to try now makes it ready, with no lookup (`a_reserve_timeout_under_a_live_stream_starts_no_second_lookup`).
  - A peer that sends a 64 KiB window of valid-length garbage and then waits out its growth isn't charged either, before the 3rd time; it has paid 64 KiB of bandwidth.
  - **When the stream ends** (its terminal, or its deadline), a key still waiting on it is made ready if it has a peer left to try, and backs off only if it hasn't. Before, it always backed off, so a waited-out key whose stream ended on its last attempt was lost with its peer still untried (runner's delta review, 2026-11-07; `a_reserve_timeout_under_a_stream_on_the_last_attempt_keeps_the_key`).
- **This also fixes a CR-1a-S bug already on `main`: `lookups` could underflow.** When a stream ended while its key was in `Lookup` on its last attempt, the stream's end gave the lookup slot back, then `back_off` lost the key, and `lose()` gave it back a second time because the key still read `Lookup`. A debug build panicked; a release build wrapped `lookups` to `usize::MAX` and the pool never started another lookup. `lose()` now gives a slot back only if the key still holds its request: every lookup counted holds one, so that's the count's own record. `a_stream_that_ends_empty_on_the_last_attempt_counts_its_lookup_once` reproduces it. Streaming peers (`dht.streaming_peers`) are off by default, so released builds were exposed only with the switch on.
  - **Peer-wire still logs it as a timeout.** Peer-wire can't tell this timeout from a peer's, short of a new `Error` variant in `lib.rs`: `fetch.failed` logs and `fetch.failed_summary` count it as `timeout`. The pool's stats, `/metrics` and the funnel don't. While a stalled consumer cycles keys through waits, the log's timeout count is inflated by `fetch_reserve_timeouts_total`. A follow-up can add the variant if the logs matter.
  - **The known cost:** with the consumer stalled and the budget nearly full, keys can cycle through dial, wait and time out every deadline. That's at most `max_sockets` dials per 8 s, below the normal crawl rate, until the consumer moves. Nothing is lost, and memory stays within the budget.
- **`held_full` keeps its meaning.** It counts the times the budget (now held plus reserved) or a waiting reservation stopped dispatch while keys were ready. Outbox backpressure still stops dispatch, but isn't counted.

## Growth stops dispatch: what it costs (runner's review, 2026-11-07)
- **The mechanism:** a growth waits only when the budget lacks room for it, and while it waits the pool starts nothing. Once granted, the fetch holds its whole bound until it ends: at most the 8 s deadline.
- **What a peer can do with it:** send a valid-length first window (64 KiB; the hash is checked only at the end), take its growth, then send nothing. It holds up to `max_metadata` of the budget for its deadline. Peers doing this in turn can keep the budget full, and once it's full whole claims wait and dispatch stops.
  - **Single machine (64 MiB budget, 16 MiB `max_metadata`):** 4 grown liars hold the whole budget: 4 × 64 KiB every 8 s, about 32 KiB/s from 2 addresses, stalls ingest for as long as it's kept up.
  - **A 4 MiB worker with 4 MiB dicts:** one grown liar holds it: 64 KiB every 8 s, about 8 KiB/s per worker.
  - **On `main`,** the same peer costs 64 KiB of memory and nothing else; the cost here is availability, not memory, which stays within the budget.
- **Not fixed in this slice; the runner decides a follow-up.** Options: grow in steps tied to the bytes received (reserve at most twice what's arrived), so holding `max_metadata` costs about half of it in bandwidth; a lower `max_metadata`; or a budget many times `max_metadata`, so a few grown liars hold only a fraction.

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
- **What the budget now bounds:** held plus reserved info-dict bytes, at most `max(max_held_bytes, max_metadata + 24 KiB)`. Each fetch's buffer never passes what's reserved for it, at either step. The second term is the one dict over the budget that passes alone. The 24 KiB is the most a client name and a PEX list add.
- **What it doesn't bound: each socket's working memory before or outside its reservation.** In the worst case, against a hostile peer, that's:

  | Item | Bytes |
  | --- | --- |
  | The extended message being parsed (`MAX_EXTENDED`) | 17,408 |
  | The client name (part of the reservation once it's made) | ≤ 17,407 |
  | The PEX list before its final shrink | ≤ 8,192 |
  | The `BufReader` | 8,192 |
  | The piece bitmap (1 B per 16 KiB piece) | ≤ 1,024 |
  | MSE and cipher state, the task and its futures | a few KiB |
  | **Total over TCP** | **under 64 KiB** |
  | uTP only: the receive window, out-of-order data and the send buffer (`lodestar-utp` defaults) | up to about 192 KiB more |

  A normal peer uses a fraction of that. Kernel socket buffers aren't the process's RSS. While the dict's buffer grows, the old and new allocations live together for a moment, up to 1.5× the reserved bytes for that fetch; that's transient and not in the budget.
- **Single machine, today's defaults** (`max_held_bytes` 64 MiB, `max_metadata` 16 MiB, 400 sockets):
  - **Dicts:** at most 64 MiB, since 16 MiB is under the budget, so no dict passes alone over it.
  - **Working memory over TCP (the default transport):** 400 × 64 KiB = 25 MiB. With uTP on, up to 400 × 256 KiB = 100 MiB.
  - **The peak is about 89 MiB over TCP (about 164 MiB with uTP), against 6,464 MiB (about 6.3 GiB) before this slice.**
  - **The aggressive preset's 1,000 sockets:** 64 MiB + 62.5 MiB, about 127 MiB over TCP (about 314 MiB with uTP), against about 15.7 GiB before.
  - **Against the single-machine 1 GB (§3):** both fit.
- **Throughput at the defaults is unchanged for honest traffic.** Ordinary dicts are 10–60 KB (§3), so a reservation is about 17–67 KB, whole at the first step. 64 MiB holds about 1,000–3,900 of them, more than 400 or 1,000 sockets ever reach at once: most fetches fail before the handshake, and those hold no reservation. The budget binds only for runs of large dicts or a slow consumer, which is when it should.
- **A worker** (owner §1.6.5, 256 MB; WK-7's queues at 227 MiB). With the 4 MiB worker budget X-fetch-pool-bytes proposed, about 13.1 MiB of the 244.1 MiB is left (the brief's "about 14 MB"):
  - **Dicts:** at most `max(4 MiB, worker max_metadata + 24 KiB)`. With WK-7's proposed worker `max_metadata` of 4 MiB (`FETCHED_BYTES`), that's 4 MiB + 24 KiB. Those 24 KiB come out of the 13.1 MiB.
  - **Working memory:** 13.1 MiB ÷ 64 KiB is about **200 sockets** over TCP in the hostile worst case (about 50 with uTP), against about 3 before this slice.
  - **Concurrent downloads:** at most about 4 MiB ÷ the average reservation, about 60–240 at ordinary sizes.
  - **The proposal:** a worker `max_sockets` of 200 over TCP (50 with uTP), `max_held_bytes` of 4 MiB and `max_metadata` of 4 MiB − 32 KiB, so the largest dict's bound fits inside the budget (above). The runner decides, with WK-10's measured RSS (`ext:owner-wkq3`).
- **Nothing here changes a default.** `max_held_bytes`, `max_metadata` and the presets are as before; only what the budget covers changed.

## For WK-10
- **Use `PeerWireFetcher`** (or any fetcher that implements `fetch_reserving` and wires it into `FetchOptions::reserve`) so in-flight bytes are inside the budget. A fetcher that only implements `fetch` is counted after the fact, as before this slice.
- **Size the worker from the table above:** `max_metadata` below the budget by at least 25 KiB, and `max_sockets` from the headroom left ÷ 64 KiB (TCP) or ÷ 256 KiB (uTP).
- **Size the worker budget well above `max_metadata`, not at it.** At 4 MiB with 4 MiB dicts, one silent peer holds up every large dict for 8 s (the partial share), and one grown liar holds the whole budget (growth). Several times `max_metadata` (for example 16 MiB with 4 MiB dicts) keeps both to a fraction, at the cost of RSS. The runner decides the worker sizing in WK-10 (runner, 2026-11-07), with measured RSS.
- **Watch `fetch_reserved_bytes` + `fetch_held_bytes`,** which stay within the budget, and `fetch_reserve_timeouts_total`, which says the budget is too tight for the dicts being fetched.
- **The 5 s shutdown drain applies to a worker too:** at most 5 s of a stop goes to handing results to a stalled session task. The rest go back to the saved queue, or, on a worker, to the coordinator's re-grant once their leases expire.

## Files outside the slice's `owns`
- `crates/peer-wire/src/lib.rs`: `Reserve` added to the existing `pub use fetch::{…}` line, so `lodestar-sources` can name the hook. Approved by the runner (2026-11-06T23:36Z message to this session, which adds the file to the slice's owns). The same message accepted waiting rather than failing, and asked for three things to be shown; they are below.

## What the runner asked to be shown (2026-11-06T23:36Z)
- **No deadlock.** A waiting fetch holds a socket and a peer connection, but the bytes it waits for are released only by the consumer taking results, by joins and by fetches that end, never by another waiting fetch. A fetch waiting for its first step holds no bytes. One waiting to grow holds only its window, and the partial-claim rule (above) makes sure the oldest growth fits beside every other window once the rest has drained, so waiters can't wait on each other. `growths_go_first_and_always_complete` shows it for growth. `reservations_keep_held_and_reserved_bytes_within_the_budget` fills the budget with held results while every other socket waits on a reservation that has no deadline. Nothing moves for 120 s; then the consumer drains, and every key arrives once.
- **The wait ends at the fetch's own deadline, and is counted.** Peer-wire's deadline covers the wait. A fetch that runs it out is counted in `PoolStats::reserve_timeouts`, exported as `fetch_reserve_timeouts_total`; it isn't a failure or a loss, and the peer isn't charged. `a_reservation_that_waits_out_its_deadline_is_freed_and_retried` checks the count, that no failure was counted, and that the key is retried with the same peer.
- **The reservation is the heap bound, not the raw size:** `metadata_size + client name length + 200 × size_of::<SocketAddr>()` (6,400 B on 64-bit), plus `size_of::<FetchedKey>()` on the pool's side. `a_peer_announcing_max_metadata_reserves_exactly_its_bound` checks that a peer announcing exactly `max_metadata` reserves exactly that bound, and that one byte over the cap reserves nothing.

## Checks
- **`crates/peer-wire/tests/fetch.rs`**, against the fake peer and a raw one over TCP, with a semaphore standing in for the budget:
  - **`fetches_wait_for_room_and_stay_within_their_reservations`:** 8 peers with 50 KB dicts (whole at the first step) against a budget that fits two. The budget starts full, so every fetch waits at its hook and none downloads. Once the budget is freed, all complete; the most reserved at once stays within the budget, and each result's heap fits its reservation.
  - **`no_piece_is_requested_before_the_reservation`:** over an in-memory stream, with no timing: while the hook holds its grant, nothing is buffered for the peer (a read that doesn't wait finds nothing); after the grant, the first metadata request arrives. A request sent before the hook makes it fail.
  - **`a_peer_announcing_max_metadata_reserves_exactly_its_bound`** (above).
  - **`a_large_claim_reserves_its_first_window_until_the_window_arrives`:** a peer claiming 8 MiB whose pieces don't match its claim reserves only the first window; an honest 3 MiB dict reserves the window, then the whole bound.
  - **`growth_waits_for_the_whole_first_window`:** a raw peer announcing 4 MiB that sends 1 or 3 valid pieces and then goes silent never gets more than the window reserved; one that sends all 4 does.
  - **`a_peer_that_announces_then_stalls_ends_at_the_deadline`**, **`a_peer_that_announces_then_closes_frees_its_reservation`**, **`a_peer_that_sends_more_than_it_announced_is_refused`:** each one reserved, ended as it should, and left nothing reserved once its holder dropped.
- **`crates/peer-wire/src/fetch.rs` unit tests:** `Assembly` never grows past the pieces requested (the 4-piece window, a 64-piece request window, or the announced size), whatever order pieces arrive in, and the bound covers the name and a full PEX list. Its piece bitmap (≤ 1 KiB) sits outside the reservation, in the socket's working memory.
- **`crates/sources/src/pool.rs` unit tests (`mod budget`)**, the partial-claim tests above (`a_room_starved_partial_claim_goes_before_whole_ones` among them), `the_peer_wire_fetcher_reserves_from_the_pool_budget` (the real fetcher through the pool, over TCP to a raw peer: a stalled peer's claim shows in `fetch_reserved_bytes` and is released at its deadline, and a fetched dict is held until the consumer takes it), and:
  - a claim is released once, when its last clone drops;
  - settling turns it into held bytes and releases the surplus, with nothing released twice;
  - a full budget makes reservations wait, oldest first, and stops new fetches;
  - a cancelled wait leaves the queue and takes nothing;
  - a claim over the budget passes alone.
- **`crates/sources/tests/pool.rs`**, on paused time with 60 s condition deadlines:
  - **`reservations_keep_held_and_reserved_bytes_within_the_budget`:** 48 keys of 1 MiB, 16 sockets, a 4 MiB budget and a stalled consumer. Held plus reserved stays within the budget at every check; X-fetch-pool-bytes' bound was budget + sockets × dict. Nothing more starts while reservations wait. Once the consumer moves, every key arrives once and nobody waited out a deadline.
  - **`a_reservation_that_waits_out_its_deadline_is_freed_and_retried`:** two fetches time out waiting. They count as reserve timeouts, not failures, and are retried with the same peer; all four keys arrive.
  - **`a_reservation_over_the_budget_waits_for_the_budget_to_empty`:** 2 MiB dicts against a 1 MiB budget: one held at a time, all delivered.
  - **`a_receiver_closing_while_the_pool_runs_stops_fetching`:** nothing more starts, and everything fetched is saved once at shutdown. The one result in the channel goes with the receiver, uncounted, as before this slice (X-fetch-pool-bytes: "results already in the channel go with the receiver"); the test pins that.
  - **`shutdown_with_a_stalled_consumer_saves_the_rest_after_the_deadline`:** the pool stops within `SHUTDOWN_DRAIN` plus 1 s and saves every result it couldn't hand over.
  - **`a_purged_result_leaves_the_recent_order`.**
  - **`silent_claimers_pin_one_small_window_and_small_dicts_still_flow`**, **`the_third_reserve_timeout_is_charged`**, **`a_reserve_timeout_under_a_live_stream_starts_no_second_lookup`**, **`a_reserve_timeout_under_a_stream_on_the_last_attempt_keeps_the_key`** and **`a_stream_that_ends_empty_on_the_last_attempt_counts_its_lookup_once`** (above).
- **The tests catch each mechanism's absence.** Each of these mutations makes at least one test fail:
  - no recent-order fix;
  - no budget check in `try_grow`;
  - no drain deadline;
  - waited-out fetches charged as failures;
  - dispatch ignoring waiting reservations;
  - growth after the first piece instead of the whole window;
  - no partial-claim admission rule;
  - first reservations not waiting behind a growth;
  - partial claims sharing the whole claims' queue;
  - no priority for a room-starved partial claim;
  - no charge on the 3rd reserve timeout;
  - back-off instead of waiting for a live stream, and a second lookup on interest;
  - a piece requested before the hook;
  - `lose()` giving a lookup back by state instead of by request;
  - a stream's end backing off a key that still has a peer to try.
