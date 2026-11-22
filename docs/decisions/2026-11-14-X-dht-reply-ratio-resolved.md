# X-dht-reply-ratio-resolved decisions

## Calls

- **The DHT reply ratio counts resolved queries only** (runner, a UX call under `docs/owner-decisions.md` §2.19). It is queries answered over those plus timeouts. Queries still awaiting their reply have no outcome yet and are left out.
  - **Why:** before, the ratio was answers over all queries sent, so it dipped whenever lookups were under way, at start-up most of all. The number users see shouldn't fall just because queries are in flight. It still falls when queries time out, which is the NAT-saturation signal it exists for (§5).
- **"Answered" counts queries, not reply packets** (runner, from the review). A new counter, `answered`, counts a response or error only when it matches a query still awaiting it, from the address asked, so each query resolves exactly once: answered, or timed out by `maintain`. Reply packets (`responses_in`, `errors_in`) also count a reply that arrives after its query timed out, a duplicate, one with an unknown transaction id and a spoofed one. Counting packets would put a late reply on both sides and let anyone push the ratio toward 1 by sending responses. Queries a finished lookup drops resolve neither way and are left out.
- **Unknown until a query resolves, not 0%.** `StatsSnapshot::reply_ratio()` returns `None` until then:
  - insights shows the KPI as unknown (—), as for any null;
  - the status JSON gives `null`, and the `/status` page shows — (it showed 0% before, from `null * 100`);
  - the OpenAPI text for `reply_ratio` gives the new meaning;
  - `/metrics` leaves `dht_reply_ratio` out until a query resolves.
- **`dht_reply_ratio` keeps its name;** its help text gives the new meaning. Two changes are visible to dashboards:
  1. it reads higher while lookups run, because queries in flight no longer count against it;
  2. it has no series until the first resolved query, where before it showed 0.
- **A help line under the insights KPI** (runner, UX): "Share of DHT queries answered, among those that got an answer or timed out. Shows — until the first one resolves.", in en and zh, so a bare — is explained. Stage KPIs gain an optional `help` line for it. `docs/metrics.md` gives the definition.

## Tests

- `crates/dht/src/node/mod.rs` (`reply_ratio_tests`):
  - unknown until a query resolves, however many are out;
  - the same answers and timeouts give the same ratio whatever is in flight;
  - timeouts lower it;
  - reply packets that answered no query don't raise it.
- `crates/dht/src/node/actor.rs` (`only_matched_replies_count_as_answered`): a matched reply counts; a duplicate, an unknown transaction id, a spoofed sender and a reply after its query timed out don't.
- `crates/lodestar/src/metrics.rs`: `/metrics` has no `dht_reply_ratio` line until a query resolves, and the answered share after.
- `crates/lodestar/src/http.rs`: the `/status` page shows — for a null ratio.
- `web/src/lib/features/insights/stages/model.test.ts`: the reply ratio KPI carries its help line.
- `web/e2e/insights-stages.routes.test.ts`: a null ratio reads —, with "Not available yet" and the help line.
