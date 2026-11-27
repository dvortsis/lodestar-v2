# Revised path to 100/min: all-on first, on the box (2026-11-27, revision 4)

**Status:** a proposal for independent review. It is not decided. Nothing here changes an owner decision until the owner says so (`docs/owner-decisions.md`). Nothing changes CR-SPEC until a decisions file is merged before the run it governs (`docs/decisions/2026-11-02-CR-SPEC.md`, header).

**Amends:** `docs/plan/2026-11-26-fastest-1.0-proposal.md` §4 (branch `claude/amazing-darwin-vzxlgw`, commit 6c8b7cf27; not on `main`). The owner accepted that proposal on 2026-11-26 ("yes to all recommendations"; D13 "Let's go with B"). Its scope freeze (§3), 1.0 gates, D13 B scorecard and §5 politeness plan stand. This document replaces its crawler sequence (§4.1–4.3) only.

**Revisions:**
- **Revision 2** answered a first review that covered revision 1 up to §2.3.
- **Revision 3** answered the full review of revision 1.
- **Revision 4** answers the review of revision 3 (§12): per-arm VPN isolation, a replication-proof gate, the shipping rule after a memory-only miss, a defaults-only transfer, and deterministic retry rules.

**Why now:** the bitmagnet comparator cr-ab-comp-2 runs on the box from 2026-11-27T17:00Z to about 2026-11-28T18:00Z. The next box slot opens 12 h after its window ends (CR-SPEC rule 2.2). The current sequence spends that slot on CR-AB-ADM, the first of about six serial live stages. This proposal spends it on a run that can pass the gate. First it fixes the harness so it judges that run correctly (§5).

**Labels:**
- **Estimate:** arithmetic, not a measurement.
- **Reported:** from a session's own report, not re-derived from raw data.

---

## 1. What to review

1. **The harness changes (§5).** Without them the collector, analyzer, Compose file and leak test can't run or judge this design.
2. **Governance and eligibility (§3).** What this supersedes, who decides, Aggressive's eligibility, and the replacement for Checkpoint 1.
3. **The candidates, outcomes and pass rule (§6.1, §6.2, §6.4).** Balanced first, Aggressive only as a fallback authorized before launch, and crawler acceptance kept separate from shipping eligibility.
4. **The concurrent design (§6.3).** Rotation, rehearsal, event classification and contention.
5. **The transfer to the shipping image (§6.5).**
6. **The break-even arithmetic (§4).** It replaces any forecast.
7. **The CR-SPEC changes (§8),** the diagnostics (§6.6) and the round-2 triggers (§7).

---

## 2. Evidence

### 2.1 CR-0: the accepted baseline (of record)
Source: `docs/reviews/2026-11-20-path-to-100-review.md`, from the CR-0 24 h run.

| Quantity | Value |
|---|---|
| Stored torrents | 12.3/min. This counts stored torrents, not the CR-SPEC numerator, which is `searchable_new_total{origin="network"}` net of the carry-in bound (D2, rule 3) |
| Busy lookup slots | 30.3 of 32, never queued |
| Slot residence per admitted lookup | about 24 s |
| Wire queries per admitted lookup | ≤ 59, probably 40–55 |
| Stored per admitted lookup | 0.16 |
| DHT queries | 76/s of Balanced's 200/s |
| Metadata fetch | 17.9 of 400 sockets busy; about 3.8 s per attempt |
| Pool | `fetch_pending` pinned near 200,000; 98% of harvest events evicted before service |

The review derived both residence time and yield from the same admissions. So `60 × L × y / W` matching the measured rate is arithmetic, not an independent check.

### 2.2 Cloud screens, 2026-11-23 to 2026-11-24: grounds for testing, not evidence of a gain
The five lane reports are archived in `docs/plan/evidence/2026-11-24-cloud-screens/`. Its README lists each lane's commit, switches, measured interval and restart boundary.

**Setup and how it differs from CR-SPEC:**
- **Setup:** AWS c6a.xlarge, us-east-1, Balanced, inside gluetun WireGuard with the leak test passing.
- **Scraper:** each lane's own 1-minute scraper.
- **Hours:** counted from suppression end, with no 60 min warmup.
- **Counter:** "stored" is the hour's increase of the lane's stored counter. The reports don't name the series. There's no carry-in bound and no network-searchable cohort, and stored-to-searchable conversion isn't reported.
- **Store and design:** a fresh store, about 6 h per window, on-only, no control.

| Lane | Switches on | Stored/min | Source |
|---|---|---|---|
| `combo`, first hour after suppression | ADM, split, lookup end, preset slots | **66.2**, no stall, 200 q/s | **of record:** r18 (`docs/decisions/2026-11-27-runner-records-r18.md`, "X-test-vm: the combo screen's first hour") |
| `combo`, hours 2–6 | same | 63.4, 63.6, 64.4, 65.2, 61.7 | reported |
| `allon`, hours 0–5 (hour 6 is 9 min) | all 12 (below) | 100.5, 91.3, 91.1, 105.0, 97.5, 98.7 | reported |
| `admhint` | ADM, hint-first | 23.2 unstalled, 20.8 stalled | reported |
| `tadmsoft` | ADM, soft timeout | 21.4 | reported |
| `tbase` | none | 11.5; 19.5 unstalled, 10.4 stalled | reported |

**The 12 switches** (`[dht]` keys): `coordinated_admission`, `responder_hints`, `streaming_peers`, `target_prefixes`, `utp`, `route_backoff`, `lookup_end`, `preset_slots`, `hint_first`, `sampler_cooldown_split`, `sample_backoff_escalate`, `soft_timeout`. `safe_restart` was on too.

**What the screens support:** spending host time on an all-on test.

**What they don't support:**
- a 24 h net searchable mean, a worst hour or a latency;
- a gain from any one switch;
- any claim that a particular switch caused `combo`'s or `allon`'s yield or residence time. The bundles can't isolate a switch.

**Other reported readings:**
- **`combo` per hour:** residence 19.4–20.5 s, 29.0–31.0 queries per lookup, yield 0.262–0.274, 80 of 80 slots in every scrape, 199.9–200.0 q/s.
- **`allon`:**
  - 80 of 80 permits in use;
  - `fetch_admission_waiters` pinned at about 199,500;
  - 45–59k queries throttled per hour;
  - outstanding peak 521 of 1,024;
  - no per-hour residence, queries per lookup or yield.
- **Memory:**
  - `allon`: anonymous memory peak 477 MiB, 4 h slope +7.4 MiB/h;
  - `combo`: anonymous memory 389 to 519 MiB, +8.6 MiB/h;
  - both rose about 32 MiB when the spill filter was first allocated (§7).
- **Post-window restart drills:** sampling was suppressed again (`allon` 15,907 s, reason `locked`; `combo` 18,697 s, `resumed`). Both builds predate X-split-resume-gate (#1118) and CR-0w (#1134).

### 2.3 Records gap
- **Recorded:** r17 records the screens' launch, and r18 records `combo`'s first hour.
- **Missing:** the rest never reached the runner. The lanes' messages were refused (`elevated_session_untrusted_device`), and the results files were terminated with the instances.
- **Recovered:** the archive copies the reports from the lane sessions' own text.
- **For future lanes:** write the report to runner-state and verify it reads back before teardown (§11).

---

## 3. Governance and eligibility

### 3.1 The conflict
- **The plan the owner accepted:** the owner accepted the 2026-11-26 proposal, whose §4 is a top-down test.
- **The runner's reading:** on 2026-11-27 the runner read "prioritize the new plan" as the r5/r5.1 sequence (§2.84): comparator → 12 h → CR-AB-ADM and CR-AB-4 → Checkpoint 1 → CR-AB-LC.
- **Only the owner can settle which governs** (O1).

### 3.2 What this supersedes, if the owner agrees

| Item | Owner-set? | Under this amendment |
|---|---|---|
| CR-AB-ADM, CR-AB-4 as live stages before the gate | sequence: §2.80, §2.84 | Not run before the gate; kept for 1.1 attribution |
| Checkpoint 1 (CR-CP1) | yes: the owner added it on 2026-11-02 | Replaced by the miss report (§3.4) |
| CR-AB-LC, CR-AB-1, CR-5 before CR-COMB | sequence: §2.80 | Not run before the gate |
| CR-COMB's dependencies (CR-AB-1, CR-5, `ext:owner-checkpoint-1`) | runner's plan | Round 1 (§6) |
| CR-SPEC rule 5.5: every Lodestar segment on Balanced | owner-confirmed spec (`ext:owner-cr-spec`) | Amended (§8 item 1) |
| CR-SPEC rules 6.1 and 6.8 (one treatment; one CR-COMB window) | same | Amended (§8 items 2–3) |
| CR-1c's failed CR-SIM screen ("isn't a CR-AB-LC candidate unless the owner says so", `docs/decisions/2026-11-21-CR-1c.md`) | owner question still open | `preset_slots` is in both candidates, so O3 must decide it |

**Unchanged:**
- G-v06-crawler's three tests;
- rule 2.2's comparator spacing;
- the carry-in rules;
- one crawler per public IP;
- §2.27: the runner never loosens the gate.

### 3.3 Aggressive eligibility, fixed before either day is observed
- **If the owner authorizes A as the fallback before launch** (O2), A can ship when B doesn't and A is shipping-eligible on two valid days (§6.2).
- **If the owner decides eligibility after seeing the rates,** A's round-1 result only selects. The shipping default then needs a fresh two-day run.
- **A middle preset** (between Balanced's 200 q/s and Aggressive's 1,000) changes owner-set numbers and always needs its own run.

### 3.4 If round 1 misses: the miss report (replaces Checkpoint 1)
Within 24 h of the last window, the runner reports to the owner what CR-SPEC rule 6.9 asks of Checkpoint 1:
- the measured rates with per-hour and per-day spread;
- the binding limiter from the ledger (§4, §7);
- the comparator's rate on the box;
- the round-2 fix and a projection with stated uncertainty.

If that projection is short of 100/min, the owner re-decides before round 2 runs.

**Who decides:** the owner decides O1–O3. The runner records the answers in `docs/owner-decisions.md`, the CR-SPEC decisions file, `runner/slices.yaml` and the dashboard, all before launch.

---

## 4. Break-even arithmetic (no forecast)

**The bound:** lookup completions per second, `C`, are limited by the smaller of two terms, both measured over one window and one lookup cohort:

```
C ≤ min( S / W_slot ,  (R − Q_other) / q_lookup )        stored/min ≤ 60 · C · y
```

where:
- `S` is the slot count (80 at Balanced and 256 at Aggressive with `preset_slots`);
- `R` is the preset's query rate (200 or 1,000 per second);
- `Q_other` is sampling and maintenance queries per second;
- `q_lookup` is lookup queries per completion;
- `W_slot` is slot residence;
- `y` is stored per completion.

The two terms aren't independent: a lookup waiting for a packet token holds its slot, so full slots and an engaged rate limiter don't show two separate bottlenecks.

**Break-even for 100 stored/min, at `combo`'s illustrative reported values** (`y` = 0.27, `q_lookup` = 31):
- **Completions needed:** `100 / 60 / 0.27` = **6.17 completed lookups per second**.
- **Their cost:** `6.17 × 31` ≈ **191 lookup queries per second**, before sampling and maintenance.
- **At Balanced** (`R` = 200), with `combo`'s roughly 70/s of sampling, that's about 261/s, over the cap. At `combo`'s `y` and `q`, Balanced can't reach 100 stored/min.
  - B passes only if `y × (200 − Q_other) / q_lookup ≥ 1.67` stored/s.
  - That means a higher yield, fewer queries per lookup, or less non-lookup traffic than `combo` had.
  - `allon` reported 91–105/min at Balanced, but its `y`, `q` and `W` weren't recorded, so whether it met this condition is unknown.
- **At Aggressive** (`R` = 1,000, `S` = 256), the same 191/s is within budget. Whether `y`, `q` and `W` hold at 5× the query rate is unmeasured.
- **Stored isn't the gate's numerator.** The gate counts network-searchable keys net of carry-in, so round 1 also reports the measured conversion from stored to net searchable.
- **Fetch sockets:** "about 180 busy at 13 lookups/s" assumes CR-0's 3.7 attempts per lookup and 3.8 s per attempt survive the change in load. Round 1 measures them.

**Measured per hour, per arm, from the CR-SPEC window and CR-LEDGER:** admissions, slot residence by end reason, queries by purpose, completions, `y`, fetch attempts, successes and durations, and stored-to-searchable lag.

---

## 5. Harness changes before booking (on the critical path)

The current tools can't judge round 1 correctly (code on `main` at ebd148d4b):

| Tool | What it does today | Effect on round 1 |
|---|---|---|
| `collect.py` `config_identity` | records every unset switch as `false`, source `"default"` | a build whose switches default on is recorded as all off |
| `analyze_cr.py` `lodestar_checks` | `balanced_preset` and `preset_stayed_balanced` | Aggressive is void at the start and at every boundary |
| `analyze_cr.py` `gate` | judges one 24 h `comb` report; reports comparator `spacing_problems` but can still return `pass` | the same report supplied twice would satisfy a "two-day" check; spacing doesn't block |
| `collect.py` `open_eligible` | won't take `T_open` beside reason `off` until 6 h have passed | on a CR-0w build, an arm with `safe_restart` off loses 6 h |
| `collect.py` `PublicIP` | compares each collector's IP with the address it first saw (`unchanged` / `changed`) | two arms on the same exit both report `unchanged` |
| `infra/fleet/steps/vm/compose.yaml` | hard-codes `gluetun` and `lodestar` container names, ports and state | only one arm can run |
| `infra/fleet/steps/vm/leak-test.sh` | targets `container:gluetun`, uses fixed probe names, and stops and recreates the tunnel | can't target one arm, and disrupts the tunnel it tests |
| `analyze_cr0.py` memory fit | clocks hours from the first baseline sample | the bar is defined on measured-window hours 12–24 |
| `analyze_cr.py` `effect` | expects ABBA/BAAB treatment blocks | doesn't apply to an absolute two-day claim |
| the running binary | exposes no runtime value for the 12 switches (API, settings and metrics checked) | an environment dump can't show an image's compiled defaults |

### 5.1 X-dht-switch-identity (Opus; Rust, small)
- **A read-only gauge** reports each of the 12 crawler switches' effective values, with `safe_restart`, at start. Its catalogue entry goes in `docs/metrics.md`.
- **It's the runtime source** the collector reads (§5.2 item 1), so compiled defaults, the environment and a config file all show up as one effective value.
- **Tests:** the gauge matches the effective config with the switch set by environment, by file, and by default.

### 5.2 X-cr-round1-harness (Opus; the evidence path)
1. **Effective settings.** The collector records the preset from the settings API and each switch from §5.1's gauge, with the environment and its source beside them.
2. **Declared settings.** The analyzer checks each arm against its **declaration** in the CR-SPEC decisions file (round, day, arm, role, preset, the 12 switch values, image digest), at start and at every boundary. Any mismatch, or a mid-run preset change, voids the window.
3. **One VPN per arm.**
   - **Compose:** parameterized per arm, with distinct container names, ports, data and state directories, and witness directories.
   - **Leak test:** takes the arm's identity. It targets that arm's gluetun namespace with probe names unique to the arm.
   - **Fixture:** testing one tunnel leaves the other arms' containers running.
4. **Egress identity.**
   - **Each check:** at every hourly boundary, the collector reads the public IP from inside its own crawler's network namespace. It records a salted hash of the address, never the address itself.
   - **The round's salt:** kept outside the archive.
   - **`gate-round`:** compares the hashes across arms and days. It requires the declared exit for each arm and day, three distinct exits on each day, and the declared rotation.
5. **`gate-round` (a new command; `gate` stays as it is for historical runs).** It judges one candidate's two daily `comb` reports and passes only if all of these hold:
   - each report is bound to its declared round, day, candidate, exit, host and spec commit;
   - the two have distinct run IDs;
   - each shows fresh-store evidence;
   - the two 24 h windows don't overlap;
   - the exits match the declared rotation;
   - each report's contention verdict is `ok` (§6.3);
   - each passes the three tests (§6.4);
   - the booked comparator's report and timeline are supplied, and rule 2.2 spacing has no problems.
   - **A missing comparator or any spacing problem** gives `not_judged`.
6. **Memory fit on the measured window.** The `anon` slope and its 95% bound are fitted over `ws + 12 h` to `ws + 24 h` of each day's window, not from the first baseline sample.
7. **Fixtures, each with an expected verdict:**

| Fixture | Expected |
|---|---|
| all-on by compiled default | recorded on, valid |
| explicit all-off control | recorded off, valid |
| preset changed mid-run | void |
| a failed second day | `fail` |
| the same day's report supplied twice | `not_judged` (duplicate run) |
| a repeated exit (two arms, or two days of one candidate, on one exit) | `not_judged` |
| missing comparator | `not_judged` |
| comparator spacing violation | `not_judged` |
| contention breach on one day | `not_judged` for acceptance; solo confirmation required |
| CR-0w first start | `T_open` within minutes |
| one arm's leak test | the other arms keep running |

8. **Run sheet** (a round-1 sheet beside the stage-1 one):
   - **After each day's window:** stop every crawler, copy off and validate each archived segment with `segment`, and read the archive back.
   - **Only then:** move to the next day's exits.
   - **After day 2:** run `gate-round`.
   - **Teardown:** keep both days' raw archives until `gate-round` has been reproduced from the archive.
   - `effect` isn't used in round 1.

---

## 6. Round 1

### 6.1 Arms and their declarations
Every arm is declared, with its preset and its 12 switch values, in the CR-SPEC decisions file before the rehearsal (§8 item 1). The analyzer judges every arm against its declaration, so no arm's preset is void.

| Arm | Role | Settings for round 1 |
|---|---|---|
| **B**: all 12 on, Balanced | candidate, first choice | the explicit environment on the pinned image from `main` (the 12 switches `=true`, `LODESTAR_INTENSITY` unset) |
| **A**: all 12 on, Aggressive | candidate (fallback) if O2 authorizes it before launch; otherwise **diagnostic** | the same, with `LODESTAR_INTENSITY=aggressive`. Declared as Aggressive either way |
| **C**: explicit all-off control, Balanced | diagnostic: the box's baseline | the 12 switches `=false`, set explicitly |

- **Settings stay explicit until a candidate qualifies.**
- **`safe_restart` stays on in every arm,** as CR-0w ships it, so the start rule opens promptly (§5).

### 6.2 Two outcomes per candidate: crawler acceptance and shipping eligibility
A candidate's two daily windows give two separate verdicts:
- **Crawler acceptance:** both days valid and passing the three tests (§6.4), each meeting the contention rule.
- **Shipping eligibility:** crawler acceptance, plus the memory rule (§6.4) on both days.

| B's result | Then |
|---|---|
| shipping-eligible | **B ships** |
| crawler-accepted, but memory fails | B isn't shipped. A memory fix makes a new candidate, B′, which needs its own two days. Meanwhile, A is considered |
| a valid miss on either day | B's claim ends. A is considered |
| incomplete (an invalid day) | the retry rule (§6.3). A isn't considered until B's claim is resolved |

| A, when considered | Then |
|---|---|
| authorized before launch and shipping-eligible | **A ships** |
| authorized, but not shipping-eligible | no claim; the miss report follows (§3.4) |
| not authorized | A's result only selects. If the owner then chooses A, it needs a fresh two-day run |

- **Neither ships:** the miss report follows (§3.4).
- **Both are shipping-eligible:** B ships.
- **A middle preset** changes owner-set numbers and always needs its own run.

### 6.3 Layout, rotation, rehearsal and retries
**Image and exits:**
- **Image:** one pinned image from `main` with CR-0w merged, the same digest for every arm and day.
- **Exits:** three PIA WireGuard servers, X, Y and Z. Their distinct egress is verified at every hourly boundary (§5.2 item 4).

| Day | B | A | C |
|---|---|---|---|
| 1 | X | Y | Z |
| 2 | Y | Z | X |

- **What two days cover:** 6 of 9 arm–exit pairings. That supports an absolute pass on two observed exits per candidate. Day, exit and treatment interactions stay unresolved, and round 1 makes no comparative claim.
- **If the rehearsal drops C:** A and B swap across the same two verified exits on day 2.
- **Concurrency** is fixed after the rehearsal for both days.

**Days and timing:**
- **Every day starts fresh:** new stores, data, state and witness directories, and node identities, including after the rehearsal. No restart inside a window.
- **Start boundary:** `ws = T_open + 60 min`, with `T_open` the scrape at which the collector observes suppression reach zero (rule 1.1; `docs/soak/runsheets/host-stage1-ab-runsheet.md`, line 66).
- **Spacing:** day 1's windows start at or after the comparator's actual window end + 12 h (rule 2.2), checked by `gate-round`.
- **Disruptive steps** (every leak test, every restart drill, the exit swap) happen only while **no** arm has a measured window open:
  - leak tests before the day's starts and after all of that day's windows close;
  - restart drills after day 2's windows close.

**Rehearsal (2 h, inside the 12 h gap; not a measurement):**
- **Setup:** all three arms on their day-1 exits, each passing its own leak test before and after. Fresh stores, deleted after.
- **The drop-C rule, frozen in the decisions file before the rehearsal:**
  - **Interval:** the rehearsal's last 60 min, starting only once every arm is crawling (sampling, and stored above 0 in the previous 10 min).
  - **The rule:** if host CPU p95 exceeds 70% of 8 vCPU, or steal p95 exceeds 3%, C is dropped. Those are tighter than the contention thresholds, to leave margin.
- **It also records:** per-arm CPU and RSS, `/data` latency, tunnel handshakes, per-arm UDP rate, and connection-tracking occupancy and drops in the VM where available.

**Contention, for each candidate window:**
- **Sampling:**
  - host CPU and steal from `/proc/stat` deltas every 10 s;
  - `/data` latency as the 10 s average time per completed write from `/proc/diskstats` deltas.
  - Percentiles are nearest-rank over every 10 s sample in the window.
- **Thresholds:** CPU p95 ≤ 80% of 8 vCPU, steal p95 ≤ 5%, `/data` write latency p99 ≤ 50 ms.
- **A breach with no infrastructure fault is demonstrated contention.** The day can't count for acceptance. That candidate needs a **solo confirmation:** two days alone on the host, under the same rules.

**Events are classified in this order, and the first that applies decides:**
1. **Infrastructure fault:** any of
   - a host, VM or `/data` fault;
   - a tunnel without a handshake for more than 180 s on any arm;
   - handshake gaps on two or more tunnels at once (a shared uplink);
   - any arm down for more than 5 min;
   - a failed leak test.
   - **Then:** every arm's window for that day is invalid, the event is published, and **the whole concurrent day is repeated,** from fresh stores, with the same arms and exits. No solo confirmation.
2. **Demonstrated contention:** a threshold breach with no infrastructure fault that day. **Then:** solo confirmation for that candidate.
3. **Neither:** the day stands.

- **Windows are never stitched** (rule 6.7).
- **A valid miss** on either day ends that candidate's two-day claim (§6.4).

### 6.4 Pass rule, per candidate, fixed before day 1
**Crawler tests.** Each day's window must independently pass all three, on the carry-in-adjusted network-searchable cohort (CR-SPEC rules 1.5–1.6 and 3.1–3.5):
1. **Window mean ≥ 100/min;**
2. **every one of the 24 net hours ≥ 70/min;**
3. **p95 discovery-to-searchable ≤ 600 s,** with coverage ≥ 98% and dropped samples charged above 600 s (rules 4.1–4.5). "Not measured" fails.

**Validity and mechanism checks:**
- rules 6.5–6.6;
- §5.2 items 1–4: the declared settings, egress and the arm's own leak test;
- the query rate and outstanding within the declared preset;
- the per-IP guards and the 64-query cap unchanged.

**Memory (shipping eligibility, not a G-v06-crawler test):**
- **The statistic:** the **upper 95% confidence bound** of the `anon` slope over measured-window hours 12–24 (`ws + 12 h` to `ws + 24 h`) must be ≤ 2 MiB/h on both days. That's the Linux soak gate's statistic (`docs/build-plan.md` §0.4; bar set in `docs/decisions/2026-10-30-CR-0-acceptance.md`).
- **Why the bound, not the point:** soak r3 measured 1.88 MiB/h and failed with an upper bound of 2.33.

**What counts:**
- **A valid miss** on either day ends that candidate's two-day claim. The second day is a replicate, not a second chance.
- **A memory fix creates a new candidate,** whose two-day count starts again.
- **When a candidate ships,** its day-1 window is CR-COMB's acceptance window and G-v06-crawler's evidence (§8 item 3), and `gate-round` is the verdict of record.

### 6.5 Transfer to the shipping image (defaults only)
1. **Build from the qualified revision:** the CR-COMB defaults change is a commit on the qualified image's revision.
2. **Record both digests:** the qualified image's and the shipping image's.
3. **Check the diff:** the diff between the two revisions may only promote the tested settings to defaults (`crates/lodestar/src/config.rs` defaults, plus their tests and docs). The runner checks it, and a fresh reviewer confirms. Any behaviour change, including a dependency update, needs fresh qualification.
4. **Check the effective settings:** a fresh start of the shipping image, with no environment and no config file, must report the qualified candidate's exact preset and 12 switch values through the settings API and §5.1's gauge.

### 6.6 Diagnostics (every arm, every hour)
- Every CR-SPEC rule 6.10 series, plus the §4 ledger series.
- **The fetch-admission queue:** eligible arrivals, admissions, waiters, oldest-waiter age where exposed, evictions, idle slots, and the end-of-window backlog (rule 3.4).
- **Memory by component:** RSS and `anon`, the spill filter (allocated or not), live spill occupancy, sampler deferrals, and the spill fallback if it triggers (#1122).
- **Per exit:** timeout share and reply rate.
- **Connection tracking:** occupancy and drops in the VM where available.
  - The DHT's outstanding count measures application queries, not router state.
  - Behind a VPN, a home router sees one tunnel flow, so this test can't establish what a router without a VPN sees. The guide says so (§9).
- **Restart observation, after day 2's windows close:** a clean restart of each candidate that records the image digest, the restart snapshot's format version, the suppression reason and how long sampling stays held. r18 records that a version-1 snapshot can still hold sampling for up to about 6 h on the upgrade path.

---

## 7. Round 2, if needed: triggers
The round-2 fix comes from round 1's measurements:

| Before concluding… | first compare… |
|---|---|
| supply is short (a falling or empty admission queue) | eligible arrivals, admissions, queue age, evictions and idle slots. X-resample-fast is a candidate only if arrivals, not admission, fall short |
| more fetch attempts per hash would help (fetch success near 4%) | how many distinct hashes one extra attempt recovers, from the attempt-level counters |
| slot residence is the limit | residence by end reason, and the share spent waiting for packet tokens |
| memory is growing | the growth by component. The spill filter's heap is fixed at 33,570,816 bytes once allocated (`docs/decisions/2026-11-24-X-split-spill-horizon.md`), so a slope after allocation lies elsewhere |
| a worst hour is low | live spill occupancy and sampler deferrals in that hour |

---

## 8. CR-SPEC changes (one decisions file, merged before the rehearsal; scoped to round 1 and later rounds of this design)

1. **Declarations (rule 5.5):** a Lodestar segment runs the preset and switch values its declaration names.
   - **What a declaration holds:** round, day, arm, role (candidate or diagnostic), preset, the 12 switch values, exit label and image digest.
   - **What's declared for round 1:** Balanced for B and C. Aggressive for A, as a candidate if O2 authorizes it and otherwise as diagnostic.
   - **Enforcement:** the analyzer checks every arm against its declaration at start and at every boundary (§5.2 item 2).
2. **Parallel arms (new):** more than one Lodestar arm may run on the experiment host at once when:
   - each has its own containers, state and tunnel (§5.2 item 3);
   - each has its own verified exit, with distinct hashes checked hourly (§5.2 item 4);
   - each passes its own leak test before and after;
   - concurrency is fixed for the round.
   - The rehearsal, contention and event rules of §6.3 are part of the spec.
3. **Acceptance (rules 6, 6.1 and 6.8):** CR-COMB's acceptance evidence may be a declared candidate's two round-1 windows when `gate-round` passes them (§5.2 item 5) and the candidate is shipping-eligible (§6.2). The qualification transfers only by §6.5's defaults-only rule.
4. **Memory:** the fit uses measured-window hours 12–24 (§5.2 item 6).
5. **Rehearsal:** a non-measurement Lodestar process may run within rule 2.2's 12 h after the comparator's window, from a store deleted afterwards.
6. **Host safety on the VM** (proposal D3, approved): mover runs and parity checks on the array don't invalidate a window whose data is on `ssd_apps`. Other rule 6.11 entries still do.
7. **Unchanged:** rule 1.1 (`ws = T_open + 60 min`), rule 2.2's spacing, fresh stores, no stitching, the carry-in bound, and the latency cohort and coverage rules.
8. **Scope:** earlier runs stay judged by the spec version in force when they started. `gate` and `effect` keep their behaviour for them.

---

## 9. Phase 0 and calendar

**During cr-ab-comp-2** (to about 2026-11-28T18:00Z; nothing runs on the box or its public IP):
1. **Owner:** O1–O3 (§10).
2. **The two harness slices (§5):** X-dht-switch-identity (Rust, small), then X-cr-round1-harness (tools, Compose, leak test and fixtures). This is the critical path.
3. **Merge CR-0w (#1134).** Its hold kept the stage-1 images' 6 h start timing (runner-state, 2026-11-27T10:30Z), and those stages no longer run.
4. **Pin the round-1 image** after 2 and 3, by digest. Record it in the decisions file with every arm's declaration (§8 item 1) and the frozen drop-C rule (§6.3).
5. **Optional cloud screen at Aggressive** (16 h, about $2.70). It's blocked while the X-test-vm lanes' SSM access is denied and until the owner has checked for an AWS quarantine policy (runner inbox, 2026-11-27T08:15Z). It never delays the box.
6. **Box prep:** three per-arm stacks (§5.2 item 3) on three PIA servers with verified distinct egress hashes, each passing its own leak test; the power setting unchanged for the block.

**The CR-COMB defaults slice** is built after a candidate ships, on the qualified revision (§6.5).

**Calendar (estimate):**

| Step | When (UTC) |
|---|---|
| Comparator window end | about 11-28T18:00Z |
| Rehearsal | within the next 12 h |
| Day 1 window | starts ≥ comparator end + 12 h; ends about 11-30T07:30Z |
| Day 2 window | ends about 12-01T09:00Z |
| Gate verdict | about 12-01, if no rerun or solo confirmation is needed |

- **Compared with a separate CR-COMB run:** reusing round 1 saves about **25 h** (24 h window plus 60 min warmup). The old 6 h hold no longer applies.
- **Fallbacks:** a solo confirmation adds about 2 days. A rerun of an invalid day adds about 1 day.
- **The current sequence** reaches CR-COMB in mid-December at the earliest.

**Guide note, if A qualifies:** Aggressive keeps up to 4,096 DHT queries outstanding. Without a VPN, that can strain a home router's connection tracking. The guide says how to choose Balanced, and that this test, run behind a VPN, couldn't measure it.

---

## 10. Decisions for the owner (before launch)

| # | Decision | Recommendation |
|---|---|---|
| O1 | Adopt this sequence: round 1 replaces the serial live stages and Checkpoint 1 (§3.2); the miss report replaces Checkpoint 1's report (§3.4); attribution per switch moves to 1.1 | Yes |
| O2 | Aggressive's eligibility, decided before launch: B (Balanced) first; may A (Aggressive) ship if B isn't shipping-eligible and A is (§6.2)? | Yes, following "We must prioritize overall performance". At `combo`'s yield, Balanced misses 100/min (§4). If no, A still runs, declared as diagnostic: it only selects, and a fresh two-day run follows if the owner then chooses it |
| O3 | Confirm the CR-SPEC decisions file (§8), including rule 5.5's Balanced condition and running `preset_slots` despite CR-1c's failed simulation screen | Yes. The live screens are the first live readings with it on; the simulator under-predicts the live network (path-to-100 review, item 6) |

**The runner's calls:** recording, the harness slice, merging CR-0w, pinning the image, box prep, the rehearsal, and whether C runs.

---

## 11. Process fixes
1. **A testing lane writes its final report to runner-state and verifies it reads back before teardown.** Messages alone aren't a record (§2.3).
2. **Each day's segments are validated and read back from the archive before the exit swap; `gate-round` runs on the archive after day 2,** and the raw archives are kept until it reproduces (§5.2 item 8).
3. **Each lane's report names its counters, measured interval and restart boundaries,** as the archive README does.

---

## 12. Review log

**First review (on revision 1 up to §2.3), answered in revision 2:**
- the evidence's provenance;
- governance and Checkpoint 1;
- the model as a bound;
- one predeclared candidate;
- shared-host interference;
- diagnostics.

**Full review of revision 1, answered in revision 3:**

| Finding | Change |
|---|---|
| 1. The harness would misjudge the run: unset switches recorded as off; Aggressive voided; `gate` judges one report and can pass with a spacing violation | §5: an Opus harness slice with effective-settings recording, declared-settings checks, a two-report `gate` with spacing as `not_judged`, five rehearsal fixtures, and `gate` before teardown. Also found: the collector's start rule and an arm with `safe_restart` off (§5, §6.1) |
| 2. Aggressive's eligibility before either day; rule 5.5 and CR-1c's failed screen in O3; Balanced first; A only if preauthorized, otherwise select then confirm; a middle preset needs its own run; explicit settings until the gate passes, then verify the shipping image | §3.2, §3.3, §6.1, §8 items 1 and 3, O2 and O3. Revision 2's single owner-picked candidate (with A recommended) is replaced by B first, A as an authorized fallback |
| 3. The pass statistic: all 24 net hours; the upper 95% bound for memory; a valid miss ends the claim; a memory fix starts a new candidate | §6.4 |
| 4. Rotation and retry unit: 6 of 9 pairings; drop-C fallback; fresh stores after the rehearsal; fixed concurrency; shared outage versus tunnel loss; egress checked hourly; connection tracking and the VPN's limit | §6.2, §6.3, §6.5, §9 guide note |
| 5. Keep the observed start boundary; anchor to the comparator's actual end; the saving is about 25 h | §6.2, §8 item 6, §9. Revision 2's "`ws` = process start + 60 min" is withdrawn |
| 6. Screens are grounds for testing; archive five summaries; remove causal claims about `lookup_end` and `hint_first` | §2.2, §2.3; archive README; the causal sentence is gone |
| 7. Replace the forecast with a break-even calculation; report stored-to-searchable conversion; the fetch-socket estimate's assumption | §4 |
| 8. Keep the scope cuts; tighten round-2 triggers; memory by component (the spill heap is fixed); spill occupancy and deferrals | §7, §6.5; revision 2's "spill-table growth is the first suspect" is withdrawn |

**Review of revision 3, answered in revision 4:**

| Finding | Change |
|---|---|
| 1. Each VPN test must target one arm: Compose hard-codes names, the leak test targets `container:gluetun` and recreates the tunnel, the IP check can't see two arms on one exit | §5 table; §5.2 items 3–4 (per-arm Compose and leak test, egress hashes read from each crawler's namespace, rotation checked across arms and days, no raw addresses archived); fixtures for a repeated exit and a one-arm leak test |
| 2. The two-day gate must prove replication | §5.2 item 5: `gate-round` binds each report to round, day, candidate, exit, host and spec commit, with distinct run IDs, fresh stores, non-overlapping windows, the rotation, contention and the booked comparator; rejection fixtures; `gate` unchanged for historical runs (§8 item 8) |
| 3. The default rule after a memory-only miss; declare A's preset even when it's diagnostic; memory fit on measured-window hours | §6.2 (crawler acceptance against shipping eligibility, with outcome tables); §6.1 and §8 item 1 (A always declared); §5.2 item 6 and §6.4 |
| 4. Transfer only a defaults-only change; a runtime source for the 12 switches | §6.5; §5.1 (the switch gauge; no runtime source exists today) |
| 5. Deterministic rehearsal and retry rules | §6.3: a frozen drop-C rule over a defined interval; 10 s sampling with nearest-rank percentiles; an ordered event classification (an infrastructure fault repeats the concurrent day; only demonstrated contention leads to solo days); disruptive steps outside every window |
| CR-0 label | §2.1: stored torrents, not the CR-SPEC numerator |
| Commands that fit the design | §5.2 item 8 and §11: `segment` per day with read-back, crawlers stopped before the swap, `gate-round` after day 2, raw archives kept; `effect` not used |

## Sources
- `docs/reviews/2026-11-20-path-to-100-review.md`
- `docs/decisions/2026-11-02-CR-SPEC.md`; `docs/decisions/2026-11-20-path-to-100-cr-spec.md`
- `docs/decisions/2026-10-30-CR-0-acceptance.md`; `docs/build-plan.md` §0.4
- `docs/decisions/2026-11-21-CR-1c.md`; `docs/decisions/2026-11-24-X-split-spill-horizon.md`
- `docs/decisions/2026-11-24-runner-records-r17.md`; `docs/decisions/2026-11-27-runner-records-r18.md`
- `docs/owner-decisions.md` §2.77, §2.80, §2.82, §2.84
- `docs/soak/tools/linux/collect.py` (`config_identity`, `open_eligible`); `docs/soak/tools/analyze_cr.py` (`lodestar_checks`, `gate`)
- `docs/soak/runsheets/host-stage1-ab-runsheet.md`, line 66
- `crates/dht/src/node/mod.rs` (`Intensity`); `crates/lodestar/src/config.rs`
- `docs/decisions/2026-11-27-CR-0w.md` (branch `slice/CR-0w`, #1134)
- runner-state `state.json` and `inbox/x-test-vm.md`
- `docs/plan/evidence/2026-11-24-cloud-screens/` (reported lane reports)
