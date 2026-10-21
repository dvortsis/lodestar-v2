# Operations: the 24-hour M1 run

The M1 exit criteria that need the real network can't be checked in the development sandbox: steady ingest at "Balanced" without overloading the router, flat memory over 24 hours, and real success rates. This is how to run and read that test.

## Start

```sh
docker compose up -d --build
```

- Web status page, `/metrics` and `/healthz`: `http://<host>:8090`
- DHT and peers: `6881` UDP and TCP. Forward both on the router if you're behind NAT.
- Data lives in the `/data` volume (`lodestar.db`, `dht-state.bin`). The container runs as the distroless `nonroot` user (uid 65532), so a bind-mounted host directory must be writable by that uid. On Unraid, use a path under `/mnt/cache`, not `/mnt/user` (FUSE and SQLite WAL don't mix well).
- Bridge networking with a UDP mapping works. `network_mode: host` gives better DHT reachability (see the comment in `docker-compose.yml`).
- Intensity is set with `LODESTAR_INTENSITY` (`low`, `balanced`, `aggressive`). The run should use `balanced`.

## What to record

Scrape `/metrics` at the start and then every hour (`curl -s http://<host>:8090/metrics > m-$(date +%H).txt`). Counters only grow, so rates are differences between two scrapes.

| What | Metric | Healthy |
|---|---|---|
| DHT reply ratio | `lodestar_dht_reply_ratio` | Stays steady. A collapse means the router's NAT table is saturated: drop to `low` and note it. |
| DHT timeouts | `rate(lodestar_dht_timeouts_total)` against `lodestar_dht_queries_out_total` | No upward drift |
| Routing table | `lodestar_dht_routing_nodes` | Grows, then plateaus |
| Keys harvested | `lodestar_intake_received_total` | Hundreds of thousands per day or more |
| Keys that were new | `lodestar_intake_new_total` | Falls as a share of received as the index fills |
| Fetch successes | `lodestar_fetch_success_total` | The numerator of the success rate |
| Fetch failures by kind | `lodestar_fetch_failures_total{kind=...}` | Many dead swarms are expected. Note the split. |
| Fetch losses by reason | `lodestar_fetch_lost_total{reason=...}` | Not dominated by one unexpected reason |
| Pool pressure | `lodestar_fetch_pending`, `lodestar_fetch_lookup_backlog` | Bounded, not climbing for hours |
| Dropped keys | `lodestar_source_dropped_total`, `lodestar_intake_dropped_total` | Near zero |
| Stored per hour | `lodestar_writer_stored_total` | Steady. Also `lodestar_torrents`. |
| Writer health | `lodestar_writer_errors_total`, `lodestar_writer_invalid_total` | Errors stay 0 |
| Have-filter | `lodestar_have_entries`, `lodestar_have_bytes` | Grows with the index. Compare `lodestar_writer_filter_false_positives_total`. |
| Source liveness | `lodestar_source_up` | 1 throughout |
| Yield by strategy | `lodestar_fetch_strategy_total{strategy,stage}` | Compare `fetched` ÷ `harvested` and `fetched` ÷ `lookup` across `sample`, `get_peers` and `announce` |
| Yield by evidence | `lodestar_fetch_by_rank_total{rank,outcome}` | The fetched share rises with rank. If it doesn't, the evidence levels need revisiting. |
| Lookup budget | `lodestar_fetch_lookups` against `lodestar_dht_throttled_total` | Lookups near their cap (64 at Balanced) and throttling still rare |
| Memory | `lodestar_resident_memory_bytes` | See below |

Derived figures to report:

- **Fetch success rate** = `fetch_success_total` ÷ (`fetch_success_total` + `fetch_lost_total` summed over reasons), over the whole run.
- **Metadata yield** = `writer_stored_total` ÷ `intake_received_total`.
- **Stored per hour**, from hourly `writer_stored_total` differences.

## Memory

`lodestar_resident_memory_bytes` (Linux, so also in the Docker image) is in every hourly scrape. As a cross-check, sample the container too:

```sh
docker stats --no-stream lodestar          # once an hour, record MemUsage
```

Memory should climb while the have-filter and caches fill, then go flat. `lodestar_have_bytes` accounts for part of it. A line that keeps rising after hour 6 or so is a leak to report.

## Router load

Watch the router's connection/NAT table count and CPU during the first hours. Signs of trouble are `dht_reply_ratio` dropping, home devices losing connections, or the router's table filling. If any of these happen, stop, switch to `low`, and report it.

## Send back

Send the first and last `/metrics` scrapes plus the hourly series (or the hourly `docker stats` and `writer_stored_total` values), the router observations and any warnings in `docker logs lodestar`. The measured rates go into architecture §3 and the M1 decision log is closed out.

## Stop

```sh
docker compose down        # SIGTERM: HTTP drains, then the queue and DHT state are saved
```
