# X-fetch-pool-bytes decisions

Runner brief 2026-11-06T21:50Z. It comes from WK-7's review (`docs/decisions/2026-11-06-WK-7.md`, "Not bounded by WK-7"). Architecture §18.4 (the fetch pool), §6 (hostile-peer limits), §3 (budgets). Owner §1.6.5: a worker uses at most 256 MB.

## The finding
- **Finished fetches piled up while the pool waited on its output.**
  - **Before:** `on_result` awaited `out.send` inside the pool's loop. While a slow consumer held that send, every fetch still running finished into the `JoinSet` and kept its info dict. After each send, `dispatch` refilled the free socket, so the set stayed full of finished results.
  - **The cost:** up to `max_sockets` × `max_metadata` (400 × 16 MiB ≈ 6.3 GiB), plus the 64-slot channel (up to 1 GiB).
  - **A side effect:** the pool answered no command and no shutdown while a send waited.

## The budget
- **`PoolConfig::max_held_bytes`, 64 MiB by default (`MAX_HELD_BYTES`).** The pool counts what it holds in finished fetches. Each result is weighed by `pool::weigh`: an upper estimate, the `FetchedKey` plus the capacity of its dict, PEX list and client name. It counts from the moment the fetch finishes until the consumer takes the result from the output channel:
  - **finished, not yet joined:** the fetch task adds its weight to a shared counter as it finishes, and joining takes it off;
  - **joined, waiting for a channel slot:** a new outbox, oldest first;
  - **in the channel, not yet taken:** the pool is the channel's only sender (true of `Pipeline::start_with` and WK-7's `WorkerStages`). So the channel's length (`max_capacity − capacity − slots the pool has reserved`) says how many of the last results sent are still there. A queue of their weights is trimmed to that length. If the pool ever stopped being the only sender, it would overcount, never undercount.
- **When the budget is full, the pool starts no fetch.** Nothing is dropped: every key is still delivered or reported as a `Loss`, exactly once.
  - **Waking up:** the pool waits to reserve an output slot. The wait ends when the consumer takes an item, and the pool keeps the slot for its next result.
  - **Commands and shutdown:** the pool no longer blocks in a send, so it answers both while it waits.
- **Count backpressure is unchanged.** While any result waits for a channel slot, the pool starts nothing, as the blocked send did before. WK-7's `a_stalled_link_pauses_fetching` bound (one result per socket, plus the channel and the forwarder) still holds. The budget only adds a byte limit on top.
- **An estimate limits fetches in flight.** A fetch adds no bytes until it finishes, so the budget alone would let every free socket start the moment room appears.
  - **The estimate:** a moving average (1/16) of the weight of each fetch that ends. A failure counts as 0, so it tracks the bytes an average fetch in flight brings. It starts at 64 KiB, the high end of real dicts (architecture §3: 10–60 KB).
  - **The rule:** a fetch starts only while `held + (in flight + 1) × estimate ≤ budget`.
  - **Normal traffic:** most fetches fail (timeouts dominate in soaks) and dicts are small, so the estimate stays far below `budget / max_sockets` and the cap on sockets decides.
  - **A run of large dicts:** the estimate rises, and fewer fetches run at once.
- **A dict over the remaining budget still finishes and is delivered.**
  - **When nothing is in flight, one fetch always starts,** so even a budget smaller than one dict makes progress.
  - **A dict larger than the whole budget passes alone:** once it's held, nothing else starts until the consumer has taken it (`a_dict_over_the_budget_passes_alone`).
- **The bound, and why it's not hard.** The pool can weigh a dict only once its fetch has finished: the bytes of a running fetch live in peer-wire's buffer, which grows as pieces arrive (architecture §6). So held bytes reach at most `max_held_bytes + (fetches in flight when it filled) × max_metadata`.
  - **When does the overshoot happen?** The estimate keeps the in-flight count small once large dicts are flowing. The worst overshoot is the first wave, when every socket starts on small-dict history and every fetch returns a maximal dict. That takes a hostile peer actually sending `max_metadata` on every socket at once.
  - **This is the same exposure as before:** peer-wire's in-flight bytes are bounded only by `max_sockets × max_metadata`. The budget doesn't make it worse.
  - **What a hard cap needs:** the announced `metadata_size`, which peer-wire learns from the extended handshake before it requests any piece. Peer-wire would report that size to the pool, and the pool would reserve it before the pieces come. That changes peer-wire and the `Fetcher` trait, so it's proposed to the runner as a follow-up slice, not done here.
- **Shutdown drains or reports every key.**
  - **Fetches:** the pool aborts its fetches and joins them all. One that had already finished is delivered; one the abort cancelled keeps its entry and is saved with the queue.
  - **The outbox:** it's sent to the consumer, which keeps running until the pool ends (`Pipeline::stop` stops the pool before prepare).
  - **No consumer left:** if the receiver is gone, a waiting result goes back into the saved queue as a `QueuedKey` (priority 0, no attempts used), to be fetched again next run. Before, a failed send dropped it silently.
  - **The cost:** the funnel counts such a key's `Fetched` stage twice across the two runs. That's accepted for a path only a broken pipeline takes.
  - **What's still dropped:** results already in the channel go with the receiver, as before.
- **A purge reaches the outbox.** `drop_source` (SRC-14b) applies its rule to results waiting for a slot, as it does to the fetch they came from. A result only the purged source found ends as evicted, and a shared one passes to the lowest remaining finder and becomes private. Before, a finished fetch waited unjoined and was dropped when the pool joined it, so the outbox mustn't let a purged key through (`drop_source_drops_its_results_waiting_for_the_consumer`). Results already in the channel are the consumer's, as before.
- **With the receiver gone, the pool stops fetching.** Its results wait in the outbox, and none can be delivered. Before, it fetched on and dropped every result.
- **Periodic snapshots are unchanged.** `PoolHandle::snapshot` still lists only keys the pool hasn't fetched. Results in the outbox count as handed over, like those in the channel.

## What's still open (runner, 2026-11-06T21:52Z)
- **The peak with this PR at today's defaults** is `max_held_bytes + max_sockets × max_metadata`: 64 MiB + 400 × 16 MiB = 6,464 MiB, about 6.3 GiB. At the aggressive preset's 1,000 sockets it's 64 MiB + 16,000 MiB, about 15.7 GiB.
  - **That's a transient, not a steady state.** The estimate shrinks the in-flight count once large dicts arrive, and the budget then holds what's finished at about 64 MiB.
  - **Before this PR, the same bytes were the steady state** under a slow consumer: 400 × 16 MiB in finished tasks plus 64 × 16 MiB in the channel, about 7.3 GiB.
- **Reaching the peak needs peers that actually send that many bytes:** every socket at once on a hostile peer, each delivering a full 16 MiB verified dict. Peer-wire never allocates before the bytes arrive (architecture §6), so a peer that only *announces* a large size costs nothing.
- **The hard cap is follow-up slice `X-fetch-meta-reserve`** (runner): peer-wire plus the `Fetcher` trait. See its note below.

## Numbers (the runner decides them, §2.26)
- **`max_held_bytes` = 64 MiB on a single machine.**
  - **Normal traffic never reaches it:** about 1,000–6,000 ordinary dicts, against at most 400–1,000 sockets plus 64 channel slots of 10–60 KB each, about 25–65 MB.
  - **The steady worst case:** a slow writer's worst steady state falls from about 7.3 GiB to 64 MiB, plus the in-flight overshoot above.
  - **The RSS budget:** 64 MiB sits comfortably inside the single-machine 1 GB (§3).
  - **Against WK-7's worker estimate:** a worker's whole queue chain (Q5 plus the fetched queue) is about 227 MiB against its 256 MB. That's why a worker needs its own, much smaller budget (below), while a single machine's 64 MiB is about 6% of its 1 GB.
- **The per-fetch estimate starts at 64 KiB** and reacts to 1/16 of each result.

## `max_metadata` (16 MiB): keep it for single machines, 4 MiB proposed for workers
- **The evidence:**
  - **Real dicts are mostly small:** architecture §3 puts them at 10–60 KB, dominated by `pieces`.
  - **Legitimate large dicts exist, though:** 20 B a piece means a 1 TiB torrent with 256 KiB pieces carries about 80 MiB of `pieces`. A 100k-file pack with long paths reaches several MiB.
  - **Soaks:** the CR-0 runs saw 6–7 oversize losses at the 16 MiB cap (`docs/soak/2026-10-29-native-cr0-checkpoint.md`, `docs/soak/evidence/2026-10-30-cr0-final/`). Lowering the cap would turn more real torrents into losses.
- **The verdict:** no clear evidence to lower it for single-machine installs, so it stays at 16 MiB (architecture §3, §6). The byte budget, not the cap, now bounds what a slow consumer costs.
- **What a lower cap would buy:** it shrinks the in-flight worst case directly. At 400 sockets:

  | `max_metadata` | In-flight worst case | Peak with a 64 MiB budget |
  | --- | --- | --- |
  | 16 MiB (today) | 6,400 MiB | about 6.3 GiB |
  | 8 MiB | 3,200 MiB | about 3.2 GiB |
  | 4 MiB | 1,600 MiB | about 1.6 GiB |

  **None of these makes the single-machine worst case fit its 1 GB RSS budget.** Only the reservation does (`X-fetch-meta-reserve`).
- **What it would cost:** a 4 MiB cap refuses any dict with more than about 200,000 pieces, for example 200 GiB at 1 MiB pieces. Such torrents exist (full-season packs, datasets). The soaks didn't record the size distribution of fetched dicts, so the number of extra oversize losses can't be estimated from the docs.
- **What would settle it:** a soak database's `torrents.info_size` can give the p99.9, p99.99 and maximum of what was fetched. If fewer than about 1 in 100,000 fetched dicts are over 4 MiB, the runner can lower the single-machine default with that measured loss.
- **For a worker it's different (below):** its whole budget is 256 MB, so 4 MiB (WK-7's `FETCHED_BYTES`) is proposed.

## For X-fetch-meta-reserve
- **Where the size is known:** `fetch.rs`'s extended-handshake branch reads `metadata_size`, checks it against `max_metadata` and only then builds the `Assembly` and requests pieces. That's the point to reserve, before the first `metadata_request`.
- **What to reserve against:** the pool's `finished` counter is the natural place, or a reservation the pool hands to the fetch. A reservation taken at the handshake would replace the post-finish `fetch_add` in `dispatch`'s task wrapper. The join and send paths already release by `weigh`, so the reservation should be in the same units: the announced size plus `size_of::<FetchedKey>()`, adjusted to `weigh` on success.
- **Waiting or failing:** a fetch that can't reserve should wait, holding its socket, or fail as a new `FailureKind` that retries the key later. Waiting is simpler, but it pins a socket and a peer connection, and the fetch's 8 s deadline still runs. Failing keeps sockets free, but it needs a funnel reason. Either way, the first reservation must always succeed when nothing is held, as `has_room` does, or a dict over the budget never arrives.
- **The `Fetcher` trait:** every test fetcher (about 10 implementations across the workspace) returns a whole `Fetched`. A default method that reserves after the fact keeps them compiling.
- **The estimate becomes unnecessary once sizes are reserved,** and can go. `held_full` keeps its meaning.

## For WK-10
- **Set a worker's `max_held_bytes` tight,** for example 4 MiB, the same as WK-7's `FETCHED_BYTES`. The pool's channel holds one item on a worker (`Queues::worker()`), so the held bytes are mostly finished results waiting for that one slot.
- **Cap the in-flight overshoot.** Until peer-wire reports announced sizes, a worker's worst case is `max_held_bytes + max_sockets × max_metadata`. Either:
  - set a worker `max_metadata` of at most `FETCHED_BYTES`, which WK-7 already asks for, with `max_sockets` sized so that `max_sockets × max_metadata` fits the 256 MB; or
  - build on the follow-up (peer-wire reserves the announced size) for a hard cap.
- **Watch the held bytes:** `PoolStats::held_bytes` is a gauge of the held bytes. `PoolStats::held_full` counts the times the budget or the estimate stopped new fetches.
- **Nothing to change in WK-7's or WK-10's code:** `PoolConfig` gains one field, and every construction uses `..PoolConfig::default()`. `FetchedKey` and the output channel type are unchanged.

## Files outside the slice's `owns`
Approved by the runner (2026-11-06T21:52Z message to this session, which adds both to the slice's owns):
- `crates/lodestar/src/metrics.rs`: `fetch_held_bytes` and `fetch_held_full_total`, with a test that both are documented;
- `docs/metrics.md`: the "Fetch pool held bytes" section.

## Checks
- `crates/sources/tests/pool.rs`, all on paused tokio time, with condition waits under 60 s deadlines and no sleep that assumes work has finished:
  - **`a_stalled_consumer_pauses_fetching_within_the_budget`:** 64 keys of 1 MiB, an 8-socket pool, a 4 MiB budget and a 16-slot channel, with the consumer stalled. The budget stops fetching, and nothing more starts over 120 s. The held bytes stay within `budget + sockets × weight`. Once the consumer moves, every key arrives once, within the same bound.
  - **`a_dict_over_the_budget_passes_alone`:** 4 MiB dicts against a 1 MiB budget are delivered, and only one is held at a time.
  - **`shutdown_with_the_budget_full_drains_or_saves_every_key`:** every key is delivered, saved or lost, exactly once.
  - **`shutdown_without_a_consumer_saves_what_it_fetched`:** with the receiver gone, the outbox goes back into the saved queue.
- **The tests catch a missing budget:** with the budget check disabled, the first three fail.
