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

Ten simulated minutes take one to five seconds of wall time. The restart
scenario (6 h 20 min of leg 1, mostly idle, a 2 min gap and an hour of
leg 2) takes about ten.

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
| `restart-honoured` | CR-0d's restored restrictions end to end: `healthy` on 15 min ± 25% intervals, restarted cleanly with `dht.safe_restart` on (below) |
| `live-lookup` | The live lookup as CR-0 measured it: slots full, lookups held to the 25 s deadline by stale contacts in the closest set. The calibrated scenario for lookup-cost changes (below) |

`admission-pressure` also departs from the production pool defaults:
`max_pending` 20,000 (not 200,000) and `retry_backoff` 30 s (not 120 s).
That compresses hours of queue growth and retry rounds into ten
minutes, so its evictions come mostly from that cap. A candidate that
screens in on it must be re-checked at the production defaults, with the
60-minute `#[ignore]` run or its own scenario.

### The restart scenario

A scenario with `restart: Some(Restart { .. })` runs two legs in one
runtime, against one network that keeps its state (sample windows,
intervals) and its record of every `sample_infohashes` (`Net::asks`: the
node, when it was sent, and when its reply reached the node with which
`interval`, if one did):

1. **Leg 1** starts fresh with the restart proof on (`Config.restart`,
   `state_path` and the witness dir in a temp dir, `inline_io`, and a
   simulated wall clock: a fixed base plus elapsed virtual time). A first
   start suppresses for 6 h plus the query timeout; leg 1 then samples for
   `leg1_minutes` and shuts down cleanly, which commits the proof. Replies
   still on their way are lost with the process.
2. **Leg 2** starts `gap_s` later from the same dirs, with the pool's queue
   restored, and runs the scenario's `minutes`. The per-minute rows and
   totals are leg 2's.

The run asserts, and the report's last sections show:

- **No node is sampled before its restored obligation expires.** Each
  node's earliest re-sample is computed from its last leg-1 sample with
  the sampler's rules: the reply's arrival plus its interval, clamped to
  the 60 s floor and the 6 h ceiling; the ceiling for an error reply; and
  the send plus the ceiling when no reply arrived before the stop (a
  timeout cools from later than that, a sealed sample from the stop).
  That's a lower bound on the real deadline, so any earlier leg-2 sample
  is a violation.
  - With `dht.sample_backoff_escalate` on (CR-SAMPLE-BACKOFF-ESCALATE), a
    failure (no reply, or an error other than a KRPC 204) cools for the
    first failure's backoff (300 s) unless the node's previous leg-1
    sample failed too and was sent within 6 h of it; then it cools for
    6 h. A 204 still cools for 6 h. An unanswered sample sent less than a
    query timeout before the stop can't have timed out, so it was sealed:
    the stop plus 6 h. That's still a lower bound: the sampler can only
    have escalated where the model says it didn't (a late answer the model
    reads as answered). `restart_with_escalation_honours_the_bound` runs
    the restart with the switch on, and it has no golden. With the switch
    off, the model is unchanged.
- **Sampling resumes at once** for everything else: the first leg-2 sample
  comes within a minute, every node already free at the restart is
  sampled again, and so is every node whose obligation expires during
  leg 2. The report gives how soon after expiry (median and max).
- **The counters add up:** `restart_obligations_restored` = `pending` +
  `honoured`; `restored` ≥ the restrictions the network's record says were
  still live at the restart (equal in the golden); and each leg's
  `samples_sent` equals the samples in the network's record (`Net::asks`),
  and `sample_failures` equals its errors and unanswered samples less those
  still in flight at the stop (sealed, not failed; printed as `sealed=`).
  `restart_obligations_refused` counts sampling candidates a restored
  restriction turned away, not obligations, so it isn't in the sum and
  can exceed `restored`. The proof's `sample_suppression_reason` is
  `resumed`.
- **The restart cost:** metadata stored in the first hour after the safe
  restart, against leg 1's first hour of a fresh 6 h suppression (zero:
  the simulated network sends no queries, so there are no passive
  harvests).

The negative control (`restart_without_safe_restart_suppresses_in_full`)
turns `safe_restart` off for leg 2: the reason is `off`, nothing is
restored, the suppression is the full 6 h window, and no node is sampled
in it.

The report's `# scripted network` tallies cover both legs; the per-minute
rows and the totals are leg 2's.

The first minutes are warmup: the first BEP 51 sweep and the lock-step
retries of its rejected keys. The report's `steady` line averages the
minutes after the third; compare candidates on it, not on the totals.

## The calibrated scenario (`live-lookup`)

The baselines end lookups in 5.5–16 s, and CR-0's live lookups held a
slot about 24 s, so a change to lookup length couldn't be judged on them
(`docs/reviews/2026-11-20-path-to-100-review.md` §3.6). `live-lookup` is
calibrated to CR-0's 24 h measurement window (86,440 s,
`docs/soak/evidence/2026-10-30-cr0-final/`). Every lookup-cost change
(CR-LOOKUP-END, CR-HINT-FIRST, CR-1c, X-sampler-cooldown-split) screens
on it, off against on, before it takes host time (build-plan §0.5).

### What makes it live-like

- **Stale contacts** (`NetConfig::stale_permille`, 1,750‰): about 1.75
  addresses beside every node in id space that never answer. Replies
  from inside a target's neighbourhood list them next to the nodes they
  list. Each one in the closest set must time out (4 s, at most 3 in
  flight) before the lookup can end, and each closer responder lists
  more, so most lookups run to the 25 s deadline, as live.
  - Far hops list none, which keeps most of them out of the sampler's frontier.
  - Some still reach it: in the golden, about 6,000 of 24,572 `sample_infohashes` go to stale contacts.
- **Keys far outrun the slots:** 70% BEP 51 nodes on 120 s intervals over
  a 200,000-hash catalogue, with the production pool defaults.
- **Fetch outcomes per attempt as CR-0's:** 4.4% stored, 10.7% refused,
  14.1% unreachable, the rest timed out. 70% of hashes have a swarm of
  1–16 peers.
- 5,000 nodes, 3% dead, 0.5% loss each way. Replies take 60–460 ms
  (uniform), and 15% take 3 s: a mean of about 0.67 s.

### The ledger

With `Scenario::ledger` on, the report adds the capacity ledger over the
minutes after warmup (`run::Ledger`). Each figure is named after the
CR-LEDGER metric it stands for, with what the sim reads it from: the
node's own CR-LEDGER counters (#975) where they exist, deltas of
`StatsSnapshot::ledger` after warmup. The report ends with the share of
lookups by `lookup_ended_total{reason}`, so a lookup-cost change shows
lookups ending on values instead of at the deadline.

| Report name | Field | What the sim reads |
|---|---|---|
| `lookup_slots_occupied` | `slots` (`L`) | `peer_active`, sampled each second (of `lookup_slots_capacity`, 32) |
| `peer_admitted_per_s` | `admitted_per_s` (`λ`) | `peer_admitted` deltas |
| `W_little` | `w` | `L / λ`: how CR-0's 23.6 s was derived, so it's the calibrated `W` |
| `lookup_residence_seconds_total/lookup_ended_total` | `w_residence` | CR-LEDGER's `W`: residence over ends, every reason. `W_little` stays the calibrated `W` |
| `queries_sent_total{purpose=peer_lookup\|hint}/lookup_ended_total` | `q` | CR-LEDGER's queries by purpose, per ended lookup. Nothing here sends harvest-only requests, so these are the owned requests' own queries |
| `queries_out_total/admitted` | `q_all` | Every query: CR-0's upper bound on `q` |
| `stored/admitted` | `y` | What reached the writer |
| `fetches/admitted` | `fetches` | Peers tried |
| `peers_found/admitted` | `found` | Keys whose peers were found (funnel `PeerFound`): about the share of lookups that return peers |
| `outstanding` | `outstanding` | Mean of the `outstanding` gauge. Over `queries_out_total/s` it's the mean time a query is held |
| `timeouts/queries_out` | `unanswered` | |
| `lookup_deadlines/completed` | `deadlines_per_completed` | Every lookup leg that hit a deadline: the node's own `find_node` lookups and non-owned `get_peers` lookups too |
| `lookup_ended_total{reason=…}` | `ended_share` | CR-LEDGER's ends by reason, each over their sum |

CR-LEDGER's time to first values, values per request and hint outcomes
aren't in the report; `StatsSnapshot::ledger` has them for a test that
needs them.

### Calibration targets and results

`live_lookup_is_calibrated` asserts each target on every one of the 8
`SCREEN_SEEDS` and on their mean. It takes about 12 s of wall time (8
threads; about 40 s of CPU in a debug build), and the whole `sim` test
about 26 s.

| Target (CR-0, 24 h) | Live | Tolerance | 8-seed mean | Seed range |
|---|---|---|---|---|
| Queries unanswered | 0.382 | ±0.03 | 0.385 | 0.378–0.391 |
| `L`, slots occupied | 30.68 | ±1.0 | 30.82 | 30.80–30.85 |
| `W`, slot residence | 23.86 s | ±1.5 s | 23.11 s | 22.53–23.51 s |
| Admitted lookups/s | 1.286 | ±0.10 | 1.334 | 1.311–1.369 |
| `y`, stored per admitted | 0.160 | ±0.04 | 0.164 | 0.149–0.181 |
| Fetch attempts per admitted | 3.66 | ±0.6 | 3.84 | 3.65–4.18 |
| Stored per minute | 12.34 | ±3.1 | 13.12 | 11.78–14.56 |
| DHT queries/s | 76.1 | ±7.6 | 77.4 | 76.5–78.5 |
| `q_all`, queries per admitted | 59.2 | ±6.0 | 58.0 | 57.1–58.6 |
| Mean queries outstanding | 160 | ±24 | 151.9 | 150.9–153.1 |

The live figures:
- **From CR-0's counters over the window:** unanswered is `dht_timeouts_total / dht_queries_out_total`; admitted, `y`, fetch attempts and stored per minute come from the same counters.
- **From the 73 baseline samples in `samples-hourly.jsonl`** (about 20 minutes apart): `L` and outstanding are means of the `dht_peer_active` and `dht_outstanding` gauges. The review's 30.3 and 23.6 s also averaged 3 warmup samples.
- **Derived:** `W = L / λ`.

The y and stored tolerances are wider: the 9 measured minutes hold about
110–130 stores per seed, about 9% Poisson noise.

Not calibrated, and why:
- **`q`, lookups' own queries:** 24.4 per admitted lookup (seeds 24.0–24.8). CR-0 can't separate lookup queries from sampling and maintenance. The review's 40–55 was an estimate, and the sim puts the lookup share lower with sampling taking the rest. CR-LEDGER's queries by purpose will settle it. If live `q` comes out far from 24, recalibrate: fewer stale contacts, and slower sampling to keep the total.
- **Reply latency:** CR-0 kept no latency histogram. The scripted distribution is constrained only through the mean hold time (outstanding over queries/s: 2.1 s live, 1.96 s here) and `W`.
- **Lookups that return peers, and how many of the K closest answer:** CR-0 has no live figure for either. The sim reports `peers_found/admitted`, 0.57 on seed 1, so CR-LEDGER's `lookup_without_values_total` can be compared with it later. The share of hashes with a swarm (70%) is set against fetch attempts per admitted lookup, not against a live share.
- **Fetch duration:** CR-0's attempts took about 3.8 s on average (review §3.5). Here, 70% time out at 8 s and unreachable peers fail at 4 s, about 6.3 s. That overstates fetch-socket pressure, so a CR-1c screen that leans on socket use should lower `timeout_ms` first.
- **Deadlines per completed:** 1.24 here, against CR-0's 1.05. Both include non-owned lookups, and CR-0's also include IPv6 lookups, so it's reported, not matched.

### Screening a lookup-cost change on it

As in "A/B a candidate", but read the ledger beside the sign test. The
review's rule (§5) keeps a change only if both `y·L/W` and `y·Q/q` rise.
`screen` returns each seed's `[off, on]` ledgers, and runs each arm's 8
seeds on 8 threads. CR-LOOKUP-END's `lookup_end_screen` (in `main.rs`) is
the worked example:

```rust
let off = scenario::live_lookup();
let on = Scenario { switches: Switches { lookup_end: true, ..Default::default() }, ..off.clone() };
let v = screen(&off, &on);
for (seed, [a, b]) in SCREEN_SEEDS.zip(&v.ledgers) {
    println!("{seed}: W {:.1}→{:.1} q {:.1}→{:.1} y {:.3}→{:.3}", a.w_residence, b.w_residence, a.q, b.q, a.y, b.y);
}
assert_eq!(v.outcome, Outcome::Better, "{:?}", v.diffs);
```

Off must leave `golden/live-lookup.txt` unchanged, like every golden.
Slot-count changes (CR-1c) should also show `queries/s` against the
Balanced 200: at 76/s the budget isn't what binds today.

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
  - fixed keys for the DHT seen filter, so its rare false positives (dropped harvests) land on the same hashes every run;
  - `testing::stats`, which reads a node's counters through a handle after its shutdown, so a leg's totals include what it sent while stopping.

  Production keeps the randomly keyed std hash tables and seen-filter keys.
- **Platforms and toolchains:** a fixed hasher would not be enough. A hash table's layout depends on its SIMD group width (16 on x86_64, 8 on arm64) and on the std release, and changing only the hash keys moves every golden. B-tree order depends on `Ord` alone, so the goldens hold on every CI runner and across toolchain updates.
- **Dependencies:** the actor's `StdRng` comes from `rand`; a `rand` upgrade that changes it moves the goldens. Regenerate them in that PR.
- **Golden fields are listed explicitly**, not printed with `Debug`. A report line `name=0` that the golden doesn't have is ignored, so a candidate's new counter, which reads 0 with its switch off, leaves the golden matching. This only works for counters printed one per line: a new field added to a multi-field line (the pool, failure, loss and network lines) changes that line and moves the golden.
- **Every run checks conservation:**
  - fetched equals stored once the run has shut down;
  - funnel `Fetched` equals `PoolStats::fetched`;
  - admitted ≥ completed + cancelled + dropped while queued (`peer_queued_dropped`, by reason), and admitted + 32 ≥ the same plus the active and queued gauges.

  The second check is a bound, not an equality, only because the active and queued gauges refresh on the 1 s tick: at the boundary they may still count up to the 32-request cap that has since finished.

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
