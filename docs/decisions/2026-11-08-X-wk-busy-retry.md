# X-wk-busy-retry decisions

The worker claims a `Busy` key again instead of dropping it (#628's review; runner brief and calls, 2026-11-08T19:55Z, §2.26).

## What is retried
- **`DenyReason::Busy` only:** "claim again later". The coordinator sends it when:
  - the writer's queue is full (§19.5);
  - its store check died or came back short for the batch (#607);
  - one key's read failed (#628).
- Since #628, a key whose check came back `Unknown` reaches the worker as `Busy`, so it's retried too. That was #628's point.
- **Every other reason drops the key at once,** as before: `Stored`, `HeldElsewhere` and `Draining`. The DHT finds it again.

## The numbers (runner-approved)
- **Back-off:** 2 s doubling to a 60 s cap, with equal jitter. It's the session's own `Backoff`: half of each step fixed, half random.
- **At most 5 attempts** after the first claim, about a minute in all.
- **At most 1,024 keys waiting,** the claims channel's own size. A `Busy` key past that is dropped and counted (`busy_dropped`).
- **Why:** `Busy` is backpressure that clears in seconds (a writer's queue, a store read). Past a minute, the DHT finds the key again anyway. So a coordinator stuck on `Busy` costs the worker at most 1,024 small entries and five claims for each.
- All four numbers are in `Config::busy_retry` (`BusyRetry`). The defaults are these; tests use milliseconds.

## How
- `BusyRetries` holds the keys in a retry: the claims so far and the back-off for each, and when each is due along with what to claim it with.
- A small task feeds each key back into the claims channel when it comes due. So retried keys go through the same batching, room check and sealing check as new ones, and the claim loop, whose `next_claims` isn't cancel-safe, never waits on a timer.
- A key's retry ends when it's granted, denied for another reason, or not claimed after all (draining, sealing down, no room in the pool, a failed claim). A key whose retry ended meanwhile isn't fed back.
- A key keeps its original discovery time, so it stays in its own cohort.

## Logged once per streak
- When keys use up their attempts, the worker logs `worker.claims_gave_up` (Warn, the number of keys), once per streak. The streak ends with a batch answered with no `Busy` at all. So a stuck coordinator can't flood the log.
- The counters `busy_retried`, `busy_gave_up` and `busy_dropped` count every case.

## The event-names budget (runner-approved, §2.26, 2026-11-08T20:29Z)
- The new event's labels put the "Settings logging event names (on demand)" chunk over its 14.9 KB budget once `main`'s newer events were in: it measured 15,056 B.
- By S-logs-3's rule (measured + 10%), the budget is now **16.6 KB**, up from 14.9 KB. It leaves room for CMP-6's one event. The chunk grows with every wave of events, and splitting it by component is the runner's follow-up. The runner flags the change to the owner (§2.19).

## For the runner
- `crates/cluster/src/server.rs`'s `KeyCheck` doc still says "(Today's worker drops any denied key; a busy retry is a follow-up.)". That file is outside this PR, so the runner updates the line or folds it into the next cluster PR.

## Tests
- `busy_retries_are_bounded`: capacity, the attempt limit, ended retries not fed back, and the first wait.
- `a_busy_key_is_claimed_again_and_granted`: over a real local session.
- `a_key_busy_past_its_limit_is_dropped`: claimed 1 + `attempts` times, then dropped and counted.
- `a_key_denied_for_another_reason_is_dropped_at_once`: `Stored`, `HeldElsewhere` and `Draining`.
- Mutation-checked: the retry arm and the attempt limit.
