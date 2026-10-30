# crawler-sim (CR-SIM)

A deterministic replay harness for crawler candidates. It runs the **real**
DHT node actor (`lodestar_dht::node`) and the **real** metadata-fetch pool
(`lodestar_sources::pool`). They are wired the way `crates/pipeline/src/dht.rs`
wires them, in tokio's paused clock, against:

- **a scripted DHT network** (`net.rs`): N nodes with BEP 51 `sample_infohashes`
  behaviour (interval, sample size, overlap), `get_peers` and `find_node`
  answers (peers, closer nodes), reply latency, dead addresses, packet loss,
  KRPC errors and per-node rate limits;
- **scripted metadata peers** (`fetch.rs`) at the level the pool sees them:
  success, slow success, refusal, timeout or unreachable.

Ten simulated minutes take one to three seconds of wall time.

## Run

```sh
cargo test -p lodestar-sources --test sim                 # all baselines vs goldens
CRAWLER_SIM_BLESS=1 cargo test -p lodestar-sources --test sim matches_golden   # regenerate goldens
CRAWLER_SIM_LOG=/tmp/sim cargo test -p lodestar-sources --test sim healthy     # also write the full outcome log
cargo test -p lodestar-sources --test sim -- --ignored --nocapture   # the 60-minute stress run and the noise floor
```

A golden mismatch prints the first differing line and the whole new report.
Regenerate only when the change is intended, and say why in the PR.

## The baselines (`scenario.rs`, goldens in `golden/`)

| Scenario | What it stresses |
|---|---|
| `healthy` | 2,000 nodes, 15% dead, 2% loss, 40–200 ms replies; the reference shape |
| `high-latency` | H1: 35% dead, 6% loss, 0.35–1.75 s replies and 20% at 3.6 s (near the 4 s query timeout), so each owned lookup holds a slot longer |
| `admission-pressure` | H2: 90% BEP 51 nodes on 60 s intervals over a 200,000-hash catalogue, 15% live, so keys far outrun the 32-request cap and churn through retries and eviction |

## The report

Every number comes from the counters production exports: `node::StatsSnapshot`,
`PoolStats` and the funnel. Nothing is recomputed. It has four parts:

- **Header:** the scenario's parameters.
- **One row per simulated minute:**
  - lookups admitted, refused and completed (`peer_admitted`, `peer_rejected`, `lookups_completed`);
  - keys whose peers were found (funnel `PeerFound`);
  - fetches started (the scripted fetcher's calls) and succeeded (`PoolStats::fetched`);
  - metadata stored (what reached the writer);
  - DHT harvests;
  - slot occupancy, sampled every virtual second as mean/max: `peer_active`, `peer_queued`, the pool's `lookups` and `active`.
- **End-of-run totals:**
  - every DHT counter, including L5's rejection reasons (`peer_rejected_capacity`/`paused`/`caller_closed`, `peer_ack_failed`, `peer_command_full`/`closed`);
  - the pool's `finder_*`, `lookup_unaccepted`, `retry_unaccepted`, `evicted_service`, failures and losses;
  - the funnel and the scripted network's own tallies.
- **The outcome log:** every harvest, fetch start (with its scripted result) and store, stamped with virtual milliseconds. The golden keeps its length and FNV-64; `CRAWLER_SIM_LOG` writes it in full, so two runs can be diffed line by line.

## Why it repeats

- **One scheduler:** a current-thread runtime with the clock paused, and every `select!` in the harness `biased`.
- **Seeded, order-free outcomes:** all randomness comes from the scenario seed. Network decisions use one SplitMix64 stream, consumed in the (deterministic) order packets arrive. Fetch outcomes are a hash of (seed, peer, hash), so they don't depend on the order the pool asks.
- **The `testing` cargo feature** on `lodestar-dht` and `lodestar-sources` (dev-only, never enabled by a production build) supplies three things:
  - an in-memory socket for the actor;
  - a seeded actor RNG;
  - B-tree maps and sets in place of the actor's hash tables and the pool's request map, whose iteration order reaches scheduling. Production keeps the randomly keyed std hash tables.
- **Platforms and toolchains:** a fixed hasher would not be enough. A hash table's layout depends on its SIMD group width (16 on x86_64, 8 on arm64) and on the std release, and changing only the hash keys moves every golden. B-tree order depends on `Ord` alone, so the goldens hold on every CI runner and across toolchain updates.
- **Golden fields are listed explicitly**, not printed with `Debug`. A report line `name=0` that the golden doesn't have is ignored, so a candidate's new counter, which reads 0 with its switch off, leaves the golden matching.

## A/B a candidate

Each candidate (CR-ADM, CR-1a-H, CR-1a-S, CR-1c, CR-1b, CR-4) ships its runtime switch, default off, in the real `Config` or `PoolConfig`. To screen it here:

1. Add a field to `Switches` in `scenario.rs` (for example `bounded_admission: bool`), and map it onto the real config in `Switches::apply`. List it in `Switches::describe` so the report header names it.
2. Compare the arms over several seeds, never one. Across seeds 1–8, stored metadata spreads 20–28% of its mean (min to max; run the ignored `noise_floor` test to re-measure). A single-seed difference is inside that noise. Use `stored_over_seeds` and require the "on" arm's worst seed to beat the "off" arm's best, or report both spreads:
   ```rust
   let off = scenario::admission_pressure();
   let on = Scenario { switches: Switches { bounded_admission: true }, ..off.clone() };
   let (off_mean, off_min, off_max) = stored_over_seeds(&off, 1..9);
   let (on_mean, on_min, on_max) = stored_over_seeds(&on, 1..9);
   println!("off {off_mean:.1} [{off_min}..{off_max}]  on {on_mean:.1} [{on_min}..{on_max}]");
   ```
   Optionally, check in `golden/<scenario>+<switch>.txt` for the "on" arm.
3. Off must leave every existing golden unchanged. That is the proof the switch is inert by default.

Screening here is a filter, not the acceptance test. The live A/B runs in the master plan ("Crawler program") still decide.

## What it doesn't model

- **The pipeline around them:**
  - no intake have-filter or store: discoveries go straight to the pool, and "stored" means "reached the writer";
  - no BEP 33 scrapes or swarm health.
- **The network:**
  - IPv4 only;
  - no inbound queries from other nodes, so passive `get_peers`/`announce_peer` harvests are zero;
  - no uTP;
  - no NAT or kernel buffer limits: the in-memory socket never returns `WouldBlock`.
- **Fetch timing:** an unreachable peer fails at the 4 s connect timeout, and anything slower than the 8 s fetch deadline times out there, as in peer-wire. There's no per-phase handshake model.
- **Routing:** an approximation on numerically sorted ids. Lookups converge in about log₈(N) hops, and the 8 XOR-closest ids in a ±16-rank window hold a swarm.

Add any of these as a scenario knob when a candidate needs it.
