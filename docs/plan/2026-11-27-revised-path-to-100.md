# Revised path to 100/min: all-on first, on the box (2026-11-27, revision 3)

**Status:** a proposal for independent review. It is not decided. Nothing here changes an owner decision until the owner says so (`docs/owner-decisions.md`). Nothing changes CR-SPEC until a decisions file is merged before the run it governs (`docs/decisions/2026-11-02-CR-SPEC.md`, header).

**Amends:** `docs/plan/2026-11-26-fastest-1.0-proposal.md` §4 (branch `claude/amazing-darwin-vzxlgw`, commit 6c8b7cf27; not on `main`). The owner accepted that proposal on 2026-11-26 ("yes to all recommendations"; D13 "Let's go with B"). Its scope freeze (§3), 1.0 gates, D13 B scorecard and §5 politeness plan stand. This document replaces its crawler sequence (§4.1–4.3) only.

**Revisions:**
- **Revision 2** answered a first review that covered revision 1 up to §2.3.
- **Revision 3** answers the full review of revision 1 (§12).

**Why now:** the bitmagnet comparator cr-ab-comp-2 runs on the box from 2026-11-27T17:00Z to about 2026-11-28T18:00Z. The next box slot opens 12 h after its window ends (CR-SPEC rule 2.2). The current sequence spends that slot on CR-AB-ADM, the first of about six serial live stages. This proposal spends it on a run that can pass the gate. First it fixes the harness so it judges that run correctly (§5).

**Labels:**
- **Estimate:** arithmetic, not a measurement.
- **Reported:** from a session's own report, not re-derived from raw data.

---

## 1. What to review

1. **The harness changes (§5).** Without them the collector and analyzer misjudge this run.
2. **Governance and eligibility (§3).** What this supersedes, who decides, Aggressive's eligibility, and the replacement for Checkpoint 1.
3. **The candidates and pass rule (§6.1, §6.4).** Balanced first, Aggressive only as a fallback authorized before launch.
4. **The concurrent design (§6.2–6.3).** Rotation, retry unit and contention.
5. **The break-even arithmetic (§4).** It replaces any forecast.
6. **The CR-SPEC changes (§8),** the diagnostics (§6.5) and the round-2 triggers (§7).

---

## 2. Evidence

### 2.1 CR-0: the accepted baseline (of record)
Source: `docs/reviews/2026-11-20-path-to-100-review.md`, from the CR-0 24 h run.

| Quantity | Value |
|---|---|
| Stored (CR-SPEC numerator) | 12.3/min |
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
- **If the owner authorizes A as the fallback before launch** (O2), two valid A passes qualify A under the amended spec (§6.4).
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

## 5. Harness changes before booking (one Opus slice, on the critical path)

The current tools would misjudge round 1 (code on `main` at ebd148d4b):
- **The collector records every unset switch as `false`.** `config_identity` in `docs/soak/tools/linux/collect.py` sets a missing variable to `False` with source `"default"`. A build whose switches default on would be recorded as all off.
- **The analyzer voids any preset but Balanced.** In `lodestar_checks` in `docs/soak/tools/analyze_cr.py`, `balanced_preset` and `preset_stayed_balanced` void Aggressive at the start and at every boundary.
- **`gate` judges one 24 h `comb` report** and returns `pass` even when it reports comparator `spacing_problems`.
- **The collector's start rule** (`open_eligible`) won't accept `T_open` beside reason `off` until 6 h have passed. On a CR-0w build, a first start reports `first_start` with suppression 0, which opens at once. An arm with `safe_restart` off would report `off` and lose 6 h.

**The slice (X-cr-round1-harness; Opus, because it is the evidence path):**
1. **The collector** records the *effective* settings of the running image: the settings API and each switch's effective value, not only the environment. It keeps the environment and its source beside them.
2. **The analyzer** checks each arm against the **declared** preset and switch set from the CR-SPEC decisions file (§8), at start and at every boundary. Any mismatch or a mid-run preset change voids the window.
3. **`gate`** takes the candidate's two daily `comb` reports and passes only if:
   - both are valid;
   - both name the same declared candidate (the same image digest and the same effective settings);
   - each passes the three tests;
   - comparator spacing has no problems. Any spacing problem gives `not_judged`.
4. **Rehearsal fixtures, each with an expected verdict:**
   - an all-on build by default (recorded as on, valid);
   - an explicit all-off control (recorded as off, valid);
   - a preset changed mid-run (void);
   - a failed second day (gate `fail`);
   - a spacing violation (`not_judged`);
   - a CR-0w first start (`T_open` within minutes).
5. **Run sheet:** `gate` runs on the archived copy-off before any instance or directory is torn down, and the archive is checked readable.

---

## 6. Round 1

### 6.1 Candidates, in a fixed order

| Order | Candidate | Settings for round 1 | Qualifies if |
|---|---|---|---|
| 1st | **B**: all 12 switches on, Balanced | explicit environment on the pinned image from `main` (the 12 switches `=true`, `LODESTAR_INTENSITY` unset) | two valid passes (§6.4) |
| 2nd, only if authorized before launch (O2) | **A**: all 12 on, Aggressive | the same, with `LODESTAR_INTENSITY=aggressive` | B doesn't qualify, and A has two valid passes |
| — | **C**: explicit all-off control, Balanced, diagnostic only | the 12 switches `=false`, set explicitly | never: it's the box's baseline |

- **Both candidates qualify:** B ships.
- **If A wasn't authorized before launch,** A's result only selects, and the chosen default needs a fresh two-day run (§3.3).
- **Settings are explicit until the gate passes.** CR-COMB then makes the qualifying settings the shipping defaults. Before it merges, the shipping image is started fresh and the collector's effective-settings record (§5 item 1) must match the qualified candidate exactly. This replaces CR-SPEC rule 6's "set as the build under test ships it" (§8 item 3).
- **`safe_restart` stays on in every arm,** as CR-0w ships it, so the start rule opens promptly (§5).

### 6.2 Layout, rotation and spacing
- **Image:** one pinned image from `main` with CR-0w merged, the same digest for every arm and day.
- **Exits:** three PIA WireGuard servers, X, Y and Z, with distinct public IPs.

| Day | B | A | C |
|---|---|---|---|
| 1 | X | Y | Z |
| 2 | Y | Z | X |

  - Each candidate meets two exits.
  - Two days cover 6 of the 9 arm–exit pairings. That supports an absolute pass on two observed exits per candidate. It leaves day, exit and treatment interactions unresolved, and round 1 makes no comparative claim.
- **If the rehearsal drops C,** A and B swap across the same two verified exits on day 2.
- **Concurrency is fixed** after the rehearsal for both days. No arm is added or removed between days.
- **Every day starts fresh:** new stores, data directories, witnesses and node identities, including after the rehearsal. No restart inside a window.
- **Start boundary:** `ws = T_open + 60 min`, where `T_open` is the scrape at which the collector observes suppression reach zero (rule 1.1; run sheet `docs/soak/runsheets/host-stage1-ab-runsheet.md`, line 66). Process start never replaces `T_open`: that could count suppressed time or hide a regression.
- **Spacing:** each round-1 window starts at or after the comparator's actual window end + 12 h (rule 2.2). `gate` checks it (§5 item 3).
- **Egress:** the collector verifies at every hourly boundary that each arm's public IP is the one assigned and that the three differ. A change is missing evidence for that arm.

### 6.3 Rehearsal, retry unit and contention
- **Rehearsal (inside the 12 h gap):** rule 2.2 bars Lodestar *windows*, not processes, after the comparator's window.
  - **What runs:** all three arms at once for 2 h, each passing `leak-test.sh` before and after, with fresh stores that are deleted after.
  - **What it records:** host CPU and steal, per-arm CPU and RSS, `/data` latency, tunnel handshakes and drops, per-arm UDP rate, and connection-tracking occupancy and drops in the VM where available.
  - **What it decides:** whether C runs. A threshold change after it needs a new decisions file before day 1.
- **Contention rule, for each candidate window:**
  - host CPU p95 ≤ 80% of 8 vCPU;
  - steal p95 ≤ 5%;
  - `/data` write p99 ≤ 50 ms;
  - no tunnel without a handshake for over 180 s.
  - These are proposed values. A breach doesn't fail the candidate, but the day can't count for acceptance. The candidate then needs a **solo confirmation:** two days alone on the host, under the same rules.
- **Retry unit:**
  - **A shared host outage** (VM, host, `/data` or uplink) invalidates every affected arm's window. The whole concurrent day is repeated.
  - **A tunnel failure confined to one arm** invalidates that arm. Losing an arm also changes its peers' contention, so if any arm is down for more than 15 min of a window, the whole concurrent day is repeated.
  - **An invalid day is published with its reason and rerun whole,** from fresh stores. Windows are never stitched (rule 6.7).

### 6.4 Pass rule, per candidate, fixed before day 1
**Each day's window must independently satisfy all three tests,** on the carry-in-adjusted network-searchable cohort (CR-SPEC rules 1.5–1.6 and 3.1–3.5):
1. **window mean ≥ 100/min;**
2. **every one of the 24 net hours ≥ 70/min;**
3. **p95 discovery-to-searchable ≤ 600 s,** with coverage ≥ 98% and dropped samples charged above 600 s (rules 4.1–4.5). "Not measured" fails.

**What counts:**
- **Validity:** rules 6.5–6.6 decide validity. The mechanism checks of §5 items 1–2 apply, plus the query rate and outstanding within the declared preset, the per-IP guards and the 64-query cap unchanged, and leak tests before and after.
- **A valid miss** on either day ends that candidate's two-day claim. The second day is a replicate, not a second chance.
- **Qualification:** two valid passes on two exits that also meet the contention rule. Day 1's window is then CR-COMB's acceptance window and G-v06-crawler's evidence (§8 item 3).

**Memory, which decides whether the qualified settings can ship as defaults (not a G-v06-crawler test):**
- **The statistic:** the **upper 95% confidence bound** of the `anon` slope over measured hours 12–24 must be ≤ 2 MiB/h on both days. That's the Linux soak gate's statistic (`docs/build-plan.md` §0.4; bar set in `docs/decisions/2026-10-30-CR-0-acceptance.md`).
- **Why the bound, not the point:** soak r3 measured 1.88 MiB/h and failed with an upper bound of 2.33.
- **A memory fix creates a new candidate,** whose two-day count starts again.

### 6.5 Diagnostics (every arm, every hour)
- Every CR-SPEC rule 6.10 series, plus the §4 ledger series.
- **The fetch-admission queue:** eligible arrivals, admissions, waiters, oldest-waiter age where exposed, evictions, idle slots, and the end-of-window backlog (rule 3.4).
- **Memory by component:** RSS and `anon`, the spill filter (allocated or not), live spill occupancy, sampler deferrals, and the spill fallback if it triggers (#1122).
- **Per exit:** timeout share and reply rate.
- **Connection tracking:** occupancy and drops in the VM where available.
  - The DHT's outstanding count measures application queries, not router state.
  - Behind a VPN, a home router sees one tunnel flow, so this test can't establish what a router without a VPN sees. The guide says so (§9).
- **Restart observation, after each candidate window:** a clean restart that records the image digest, the restart snapshot's format version, the suppression reason and how long sampling stays held. r18 records that a version-1 snapshot can still hold sampling for up to about 6 h on the upgrade path.
- **Copy-off:** keeps `process_start` and `last_scrape` (runner inbox, 2026-11-27T08:15Z). `effect` and `gate` run before any raw directory is removed.

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

## 8. CR-SPEC changes (one decisions file, merged before the rehearsal)

1. **Preset (rule 5.5):** a Lodestar segment runs the preset its declaration names. Round 1 declares Balanced for B and C, and Aggressive for A only if O2 authorizes it. The analyzer checks the declared preset at start and at every boundary (§5).
2. **Parallel arms (new):** more than one Lodestar arm may run on the experiment host at once when:
   - each has its own verified public IP, checked hourly;
   - each passes the leak test before and after;
   - concurrency is fixed for the round;
   - a candidate window used for acceptance meets the contention rule (§6.3).
3. **Acceptance (rules 6, 6.1 and 6.8):** CR-COMB's acceptance evidence may be a declared candidate's round-1 windows when both days pass (§6.4).
   - The candidate runs on explicit settings.
   - Its qualification transfers to the shipping image only if a fresh start of that image records the same effective settings (§6.1).
   - `gate` judges both reports together (§5 item 3).
4. **Rehearsal:** a non-measurement Lodestar process may run within rule 2.2's 12 h after the comparator's window, from a store deleted afterwards.
5. **Host safety on the VM** (proposal D3, approved): mover runs and parity checks on the array don't invalidate a window whose data is on `ssd_apps`. Other rule 6.11 entries still do.
6. **Unchanged:** rule 1.1 (`ws = T_open + 60 min`), rule 2.2's spacing, fresh stores, no stitching, the carry-in bound, and the latency cohort and coverage rules.

---

## 9. Phase 0 and calendar

**During cr-ab-comp-2** (to about 2026-11-28T18:00Z; nothing runs on the box or its public IP):
1. **Owner:** O1–O3 (§10).
2. **The harness slice (§5)** and its fixtures. This is the critical path.
3. **Merge CR-0w (#1134).** Its hold kept the stage-1 images' 6 h start timing (runner-state, 2026-11-27T10:30Z), and those stages no longer run.
4. **Pin the round-1 image** after 2 and 3, by digest. Record it in the decisions file with each arm's declared settings.
5. **Optional cloud screen at Aggressive** (16 h, about $2.70). It's blocked while the X-test-vm lanes' SSM access is denied and until the owner has checked for an AWS quarantine policy (runner inbox, 2026-11-27T08:15Z). It never delays the box.
6. **Box prep:** three gluetun containers on three PIA servers with verified distinct IPs, each passing `leak-test.sh`; the power setting unchanged for the block.

**The CR-COMB defaults slice** is built after qualification, not before (§6.1).

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
| O2 | Aggressive's eligibility, decided before launch: B (Balanced) first; may A (Aggressive) qualify as the shipped default if B doesn't? | Yes, following "We must prioritize overall performance". At `combo`'s yield, Balanced misses 100/min (§4). If no, A only selects, and a fresh two-day run follows |
| O3 | Confirm the CR-SPEC decisions file (§8), including rule 5.5's Balanced condition and running `preset_slots` despite CR-1c's failed simulation screen | Yes. The live screens are the first live readings with it on; the simulator under-predicts the live network (path-to-100 review, item 6) |

**The runner's calls:** recording, the harness slice, merging CR-0w, pinning the image, box prep, the rehearsal, and whether C runs.

---

## 11. Process fixes
1. **A testing lane writes its final report to runner-state and verifies it reads back before teardown.** Messages alone aren't a record (§2.3).
2. **`gate` and `effect` run on the archived copy-off before teardown** (§5 item 5).
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
