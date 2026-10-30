# DHT queued drops are counted (#232)

A follow-up from CR-SIM, which found the DHT actor's owned-request counters didn't balance: admitted − completed − cancelled − active came out 13 in the `admission-pressure` scenario (runner, 2026-10-30).

- **New series `dht_peer_queued_dropped_total{reason}`,** with `reason` = `caller_closed` or `paused`. It counts admitted requests that leave the actor's queue without starting, either because the caller dropped its receiver or because a pause dropped them.
  - Before this, those drops were counted only through `dht_peer_permit_returns_total`, and only when a permit existed. With `coordinated_admission` off, the default, they weren't counted at all.
  - `dht_peer_cancelled_total` keeps its meaning: active requests only (CR-0f).
- **The contract,** in `docs/metrics.md`: every admitted request ends in exactly one of completed, cancelled or queued-dropped, or is still active or queued. Requests held at shutdown are the exception.
- **CR-SIM:** its conservation check now counts the queued drops. The `admission-pressure` golden gains the two new lines (`caller_closed=11`). Nothing else in any golden changes: the crawl's behaviour is unchanged.
