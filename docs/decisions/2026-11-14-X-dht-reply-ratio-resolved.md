# X-dht-reply-ratio-resolved decisions

## Calls
- **The DHT reply ratio counts resolved queries only** (runner, a UX call under `docs/owner-decisions.md` §2.19). It is answers (responses and errors) over answers plus timeouts. Queries still awaiting their reply have no outcome yet and are left out.
  - **Why:** before, the ratio was answers over all queries sent, so it dipped whenever lookups were under way, at start-up most of all. The number users see shouldn't fall just because queries are in flight. It still falls when replies are lost or late, which is the NAT-saturation signal it exists for (§5).
- **Unknown until a query resolves, not 0%.** `StatsSnapshot::reply_ratio()` returns `None` until then:
  - insights shows the KPI as unknown (—), as for any null;
  - the status JSON gives `null`;
  - `/metrics` leaves `dht_reply_ratio` out until a query resolves.
- **`dht_reply_ratio` keeps its name;** its help text gives the new meaning. Two changes are visible to dashboards:
  1. it reads higher while lookups run, because queries in flight no longer count against it;
  2. it has no series until the first resolved query, where before it showed 0.
- **A help line under the insights KPI** (runner, UX): "Share of DHT queries answered, among those that got an answer or timed out. Shows — until the first one resolves.", in en and zh, so a bare — is explained. Stage KPIs gain an optional `help` line for it. `docs/metrics.md` gives the definition.

## Tests
- `crates/dht/src/node/mod.rs` (`reply_ratio_tests`):
  - unknown until a query resolves, however many are out;
  - the same answers and timeouts give the same ratio whatever is in flight;
  - errors count as answers, and timeouts lower it.
- `web/src/lib/features/insights/stages/model.test.ts`: the reply ratio KPI carries its help line.
