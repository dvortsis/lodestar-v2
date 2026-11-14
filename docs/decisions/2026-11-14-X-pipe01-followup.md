# X-pipe01-followup decisions

A follow-up to X-pipe01-fix (#877, `2026-11-14-X-pipe01-fix.md`), which bounded the keys in the fetch pool with a peer at one address or in one network. It refines two things in that file: what holds a place, and what happens to a peer past an address's limit. The limits themselves, and their numbers, don't change. The case it closes is forged peer claims against one address; its detail is held under `docs/security/v1/README.md` §5. This file says only what the code now does.

## Calls
- **A place is held only by a peer in use** (architecture §6, the fetch pool; refines #877's "every address a key's peers are at counts"). A peer is in use while it is still to be tried, or while a fetch from it runs. Once it has been tried it holds nothing, at its address or in its network. Reasons:
  - The limits are there to bound the keys one address can keep ready or fetching. A key with no peer left to try is looking for peers again, and gives way to newer keys like any other.
  - A key whose fetch failed no longer keeps its place through its back-off, so a busy address's places turn over as fast as its fetches end.
- **A peer past its address's limit waits for a place** instead of being dropped. The key is still taken, with its other peers or with none, and looked up as before. The peer it was found with is kept in a wait list for that address.
- **First come, first served.** When a place frees at an address, the peer that has waited longest there is added to its key, and the key is ready to fetch from it. A place freed in a step goes to a waiter before any key that arrives in the same step.
- **A key a user asked for waits ahead of the others,** whether it was asked for before it waited or while it did. Asked keys are served in the order they came.
- **A waiter that also needs a place in a full network is passed over** and keeps its turn. It is tried again whenever a place frees at its address or in its network.
- **The wait lists are bounded** (`wait_caps`, `MAX_WAITS_PER_KEY`):
  - per address: 8 times the address's limit, so at most 2,048;
  - in all: a quarter of the queue;
  - per key: 4 addresses.

  | Queue (`max_pending`) | Per address | Waiting per address | Waiting in all |
  |---|---|---|---|
  | 200,000 (the default) | 256 | 2,048 | 50,000 |
  | 50,000 | 97 | 776 | 12,500 |
  | 1,000 | 1 | 8 | 250 |
- **Why these bounds** (the runner's call, §2.26; not owner-set numbers):
  - Eight rounds of an address's places is long enough to take a burst from a large seeder, and short enough that the last in line is served within eight turnovers of the address.
  - A quarter of the queue keeps the lists' memory small beside the queue's own (below).
  - Four addresses a key: a key rarely has peers at more full addresses than that, and one key can't take a turn in many lists.
- **A full list turns the newest arrival away.** That key goes without the peer, as every key past the limit did before this change, and may wait at its next sighting or lookup if there is room then. The keys already waiting keep their turns. Reason: a waiter's turn then comes within the number of turnovers ahead of it when it joined, whatever arrives later. Pushing out the oldest would let later arrivals take turns away from keys that had waited longest.
- **A key a user asked for is not turned away for a key nobody asked for:** at a full list it takes the place of the newest waiter that wasn't asked for. If every waiter there was asked for, it is turned away too.
- **A waiting key that has used its lookups is not given up while its address keeps giving places.** Before, a key out of attempts with no peer ended as `no_peers`. Now, if it waits in a list, it rests for its usual back-off and is asked again: it is kept if any list it waits in has given a place since the last time, and given up if none has. It makes no more lookups meanwhile.
- **A waiting key holds no place.** With no other peer it is a key without one: it gives way to newer keys when the queue is full, as before.
- **A key that leaves the pool stops waiting.** A key that turns private (its public source was dropped) gets the peers it waited with at once: a private key's peers are never limited.
- **Only public keys in a pool with a peer finder wait,** as only they are limited (#877).
- **The wait lists aren't saved with the queue.** After a restart the saved keys are looked up again, and wait again if their peers' addresses are still full.
- **No new counters or metrics.** `docs/metrics.md` is unchanged.

## What it costs
- **Memory:** a few hundred bytes at most for each waiting peer, so at the very most about 25 MB if the default queue's whole bound of 50,000 were waiting, and nothing where no address is full. A key that leaves frees its waits at once; the turn it leaves behind in a line is skipped, and cleared out before such turns outnumber the waiters. No field was added to an entry, so `diagnostic_entry_storage_is_bounded` holds as before.
- **Time:** giving a freed place away looks at the waiters ahead of the one that takes it, which are only those passed over for their network.
- **An address with waiters is fetched from continuously,** within its per-address connection cap, which is unchanged. Before, keys past the limit never dialled it.
- **A large honest seeder** that is the only peer of more keys than its limit now has the rest wait their turn (up to the bound) instead of ending with no peers.

## Tests
- `crates/sources/tests/pool.rs`:
  - `a_key_waiting_for_a_full_address_is_fetched_in_its_turn`: an address's places are taken by keys whose fetches fail; ten keys whose only peer is there, and twenty more failing keys after them. The ten are fetched in the order they came, with only the keys that were ahead of them tried first, and none is given up, though each used its one lookup while it waited.
  - `a_peer_that_was_tried_holds_no_place`.
  - `a_full_wait_list_turns_the_newest_away`: 42 keys for a list of 32; the first 32 are fetched in order, and the last ten end with no peers.
  - `the_wait_lists_are_bounded_in_all`.
  - `a_key_a_user_asked_for_waits_at_the_front`.
  - `a_waiting_key_is_given_up_once_its_address_stops_giving_places`.
  - `a_waiting_key_that_turns_private_gets_its_peer`.
  - `a_waiter_its_network_has_no_room_for_is_passed_over`.
  - `a_place_freed_as_fetches_start_goes_to_a_waiter_at_once`.
- `crates/sources/src/pool.rs` (`vouch`): `the_wait_bounds_follow_the_queue`, `a_line_is_first_come_first_served`, `the_wait_lists_are_bounded`, `a_key_that_leaves_ends_its_waits`, `a_line_moves_while_it_gives_places`, `a_freed_place_is_noted_for_its_waiters`; and `a_peer_takes_both_places_or_neither` now also checks which limit turned a peer away.
- **The check in debug builds** (#877) now also compares the wait lists' counts with their contents and bounds, and checks that only public keys in the pool wait, at every snapshot of the queue.
- **Each new test fails with its part of the change taken out,** except one line: giving freed places to waiters before a newcomer's peers are added. The orders that reach it need a fetch that ran out of time waiting for a reservation, and no test sets that up.
- The tests run on tokio's paused clock, as the pool's other tests do: their waits are virtual time, not sleeps.

## Also in this PR (from the #877 review)
- `2026-11-14-X-pipe01-fix.md`: the reason given for the `stages.rs` helper was wrong (that pool has no peer finder, so the limits don't apply to it) and is corrected there and in the helper's comment; the NAT64 bullet's bold is closed; the table has a row for a queue of 1,000; one sentence under "What it costs" is smoothed. Its line about a follow-up now names this file.
- `Places::take` reads a key's peers once, not twice.

## Follow-ups
- **The exact-input regression test** lands after the release with the fix (`docs/security/v1/README.md` §5).
- **A metric for waiting peers and for peers turned away,** if the next soak shows any.
