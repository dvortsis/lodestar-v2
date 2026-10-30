# crawler-sim (CR-SIM)

A deterministic replay harness for crawler candidates. It runs the **real**
DHT node actor (`lodestar_dht::node`) and the **real** metadata-fetch pool
(`lodestar_sources::pool`). They are wired the way `crates/pipeline/src/dht.rs`
wires them, in tokio's paused clock, against:

- **a scripted DHT network** (`net.rs`): N nodes with BEP 51 `sample_infohashes`
  behaviour (interval jittered ±25% per node, sample size, overlap; a node
  asked again before its interval returns the same window and counts
  `sample_early`), `get_peers` and `find_node`
  answers (peers, closer nodes), reply latency, dead addresses, packet loss,
  KRPC errors and per-node rate limits;
- **scripted metadata peers** (`fetch.rs`) at the level the pool sees them:
  success, slow success, refusal, timeout or unreachable.

Ten simulated minutes take one to five seconds of wall time.

## Run

```sh
cargo test -p lodestar-crawler-sim --test sim                 # all baselines vs goldens
CRAWLER_SIM_BLESS=1 cargo test -p lodestar-crawler-sim --test sim matches_golden   # regenerate goldens
CRAWLER_SIM_LOG=/tmp/sim cargo test -p lodestar-crawler-sim --test sim healthy     # also write the full outcome log
cargo test -p lodestar-crawler-sim --test sim -- --ignored --nocapture   # the 60-minute stress run and the noise floor
```

A golden mismatch prints the first differing line and the whole new report.
Regenerate only when the change is intended, and say why in the PR.

## The baselines (`scenario.rs`, goldens in `golden/`)

| Scenario | What it stresses |
|---|---|
| `healthy` | 2,000 nodes, 15% dead, 2% loss, 40–200 ms replies; the reference shape |
| `high-latency` | H1: 35% dead, 6% loss, 0.35–1.75 s replies and 20% at 3.6 s (near the 4 s query timeout), so each owned lookup holds a slot longer |
| `admission-pressure` | H2: 90% BEP 51 nodes on 60 s intervals over a 200,000-hash catalogue, 15% live, so keys far outrun the 32-request cap and churn through retries and eviction |

`admission-pressure` also departs from the production pool defaults:
`max_pending` 20,000 (not 200,000) and `retry_backoff` 30 s (not 120 s).
That compresses hours of queue growth and retry rounds into ten
minutes, so its evictions come mostly from that cap. A candidate that
screens in on it must be re-checked at the production defaults, with the
60-minute `#[ignore]` run or its own scenario.

The first minutes are warmup: the first BEP 51 sweep and the lock-step
retries of its rejected keys. The report's `steady` line averages the
minutes after the third; compare candidates on it, not on the totals.

## The report

The DHT, pool and funnel numbers are the counters production exports
(`node::StatsSnapshot`, `PoolStats`, `FunnelCounts`), read, not
recomputed. Fetches started and metadata stored are counted by the
harness's fetcher and writer, and the network's tallies by the network.
It has four parts:

- **Header:** the scenario's parameters.
- **One row per simulated minute,** then a `steady` line of per-minute means after warmup:
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
- **Seeded, order-free outcomes:** all randomness comes from the scenario seed. Each packet's loss, error and latency are a hash of (seed, node, that node's query index), and each fetch's outcome a hash of (seed, peer, hash). So nothing depends on the order the real code asks, and a switch that changes one send doesn't reshuffle the luck of every other node.
- **The `testing` cargo feature** on `lodestar-dht` and `lodestar-sources` (enabled only by this crate, `lodestar-crawler-sim`, so neither those crates' own tests nor any production build has it) supplies these:
  - an in-memory socket for the actor;
  - a seeded actor RNG;
  - B-tree maps and sets in place of the actor's hash tables and the pool's request map, whose iteration order reaches scheduling;
  - fixed keys for the DHT seen filter, so its rare false positives (dropped harvests) land on the same hashes every run.

  Production keeps the randomly keyed std hash tables and seen-filter keys.
- **Platforms and toolchains:** a fixed hasher would not be enough. A hash table's layout depends on its SIMD group width (16 on x86_64, 8 on arm64) and on the std release, and changing only the hash keys moves every golden. B-tree order depends on `Ord` alone, so the goldens hold on every CI runner and across toolchain updates.
- **Dependencies:** the actor's `StdRng` comes from `rand`; a `rand` upgrade that changes it moves the goldens. Regenerate them in that PR.
- **Golden fields are listed explicitly**, not printed with `Debug`. A report line `name=0` that the golden doesn't have is ignored, so a candidate's new counter, which reads 0 with its switch off, leaves the golden matching. This only works for counters printed one per line: a new field added to a multi-field line (the pool, failure, loss and network lines) changes that line and moves the golden.
- **Every run checks conservation:**
  - fetched equals stored once the run has shut down;
  - funnel `Fetched` equals `PoolStats::fetched`;
  - admitted ≥ completed + cancelled, and the same plus the active and queued gauges, less the 32-request cap the 1 s gauge refresh can lag by.

  `≥`, not `=`: the actor drops closed-caller requests from its queue without counting them, a counter contract that predates CR-SIM.

## A/B a candidate

Each candidate (CR-ADM, CR-1a-H, CR-1a-S, CR-1c, CR-1b, CR-4) ships its runtime switch, default off, in the real `Config` or `PoolConfig`. To screen it here:

1. Add a field to `Switches` in `scenario.rs` (for example `bounded_admission: bool`), and map it onto the real config in `Switches::apply`. List it in `Switches::describe` so the report header names it.
2. Screen it with `screen(off, on)`, the only comparison candidates use. It runs both arms on each of the fixed `SCREEN_SEEDS` (1–8); the arms of one seed share their network luck. It then applies a sign test to the post-warmup stored rate (`steady_stored`, metadata stored per minute after the 3-minute warmup):
   - `Better` means "on" stored more on every seed;
   - `Worse` means it stored less on every seed;
   - `Inconclusive` means mixed signs, or a tie on any seed.

   One seed, or the totals, which include warmup, would be inside the noise: across `SCREEN_SEEDS` the steady rate spreads 11–20% of its mean (run the ignored `noise_floor` test to re-measure).
   ```rust
   let off = scenario::admission_pressure();
   let on = Scenario { switches: Switches { bounded_admission: true }, ..off.clone() };
   let v = screen(&off, &on);
   assert_eq!(v.outcome, Outcome::Better, "{:?}", v.diffs);
   ```
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
- **Node behaviour over time:** every node's latency and loss come from the same per-scenario distribution. There are no persistent per-node classes (always-slow or lossy nodes) and no churn: nodes never join, leave or change address during a run.
- **Fetch timing:** an unreachable peer fails at the 4 s connect timeout, and anything slower than the 8 s fetch deadline times out there, as in peer-wire. There's no per-phase handshake model.
- **Routing:** an approximation on numerically sorted ids. Lookups converge in about log₈(N) hops, and the 8 XOR-closest ids in a ±16-rank window hold a swarm.

Add any of these as a scenario knob when a candidate needs it.
