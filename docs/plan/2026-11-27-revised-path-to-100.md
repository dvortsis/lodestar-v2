# Revised path to 100/min: all-on first, on the box (2026-11-27, revision 2)

**Status:** a proposal for independent review. It is not decided. Nothing here changes an owner decision until the owner says so (`docs/owner-decisions.md`). Nothing changes CR-SPEC until a decisions file is merged before the run it governs (`docs/decisions/2026-11-02-CR-SPEC.md`, header).

**Amends:** `docs/plan/2026-11-26-fastest-1.0-proposal.md` §4 (branch `claude/amazing-darwin-vzxlgw`, commit 6c8b7cf27; not on `main`). The owner accepted that proposal on 2026-11-26 ("yes to all recommendations"; D13 "Let's go with B"). Its scope freeze (§3), 1.0 gates, D13 B scorecard and §5 politeness plan stand. This document replaces its crawler sequence (§4.1–4.3) only.

**Revision 2** answers the first independent review (§11). The reviewer's copy of revision 1 ended partway through §2.3, so §§3–8 of revision 1 were not reviewed. They are rewritten here and need review.

**Why now:** the bitmagnet comparator cr-ab-comp-2 runs on the box from 2026-11-27T17:00Z to about 2026-11-28T18:00Z. The next box slot opens 12 h after its window ends (CR-SPEC rule 2.2). The current sequence spends that slot on CR-AB-ADM, the first of about six serial live stages. This proposal spends it on a run that can pass the gate.

Numbers marked **estimate** are arithmetic, not measurements. **Reported** numbers come from a session's own report and haven't been re-derived from raw data.

---

## 1. What to review

1. **The evidence (§2):** what the cloud screens can and can't support.
2. **Governance (§3):** which decisions this supersedes, who decides, and what replaces Checkpoint 1.
3. **The capacity bound (§4):** used as a bound, not a forecast.
4. **The acceptance design (§5):** one predeclared candidate, the per-day pass rule, the contention rule, and when a solo confirmation is needed.
5. **The CR-SPEC changes (§7):** whether any weakens a protection the spec relies on.
6. **Diagnostics (§5.5), risks (§9), and what was dropped (§6).**

---

## 2. Evidence

### 2.1 CR-0: the accepted baseline (of record)
Source: `docs/reviews/2026-11-20-path-to-100-review.md`, from the CR-0 24 h run.

| Quantity | Value |
|---|---|
| Stored (CR-SPEC numerator) | 12.3/min |
| Busy lookup slots | 30.3 of 32 (30+ in 88% of hourly snapshots), never queued |
| Slot residence per admitted lookup | about 24 s |
| Wire queries per admitted lookup | ≤ 59, probably 40–55 |
| Stored per admitted lookup | 0.16 |
| DHT queries | 76/s of Balanced's 200/s |
| Metadata fetch | 17.9 of 400 sockets busy; about 3.8 s per attempt |
| Pool | `fetch_pending` pinned near 200,000; 98% of harvest events evicted before service |

The review's agreement of `60 × L × y / W` with the measured rate is largely arithmetic, because `W` and `y` were derived from the same admissions. It describes CR-0; it isn't an independent check of a model.

### 2.2 Cloud screens, 2026-11-23 to 2026-11-24 (provisional, not of record)
- **Common setup:** AWS c6a.xlarge (4 vCPU), us-east-1, Balanced preset, inside gluetun WireGuard with the leak test passing. On-only blocks with **no control arm.** Commit 6bec9388 for `combo` and `allon`.
- **Measurement differs from CR-SPEC in five ways:**
  - each lane used its own 1-minute `/metrics` scraper, not the CR-COL collector;
  - hours are counted from suppression end, with **no 60 min warmup**;
  - the rate is the hour's increase of the lane's stored counter (gross), with **no carry-in bound** and no network-searchable cohort;
  - the store was fresh;
  - each window was about 6 h.
- **These numbers justify host time.** They establish neither a 24 h mean nor a gain attributable to any switch (`docs/owner-decisions.md` §2.82; the lanes' own reports say the same).

| Lane | Switches on | Stored/min | Source |
|---|---|---|---|
| `combo`, first hour after suppression | ADM, split, lookup end, preset slots | **66.2**, no stall, 200 q/s | **of record:** r18 (`docs/decisions/2026-11-27-runner-records-r18.md` §"X-test-vm: the combo screen's first hour") |
| `combo`, hours 2–6 | same | 63.4, 63.6, 64.4, 65.2, 61.7 | **reported**, `docs/plan/evidence/2026-11-24-cloud-screens/combo.md` |
| `allon`, hours 0–5 (h6 is a 9 min partial) | all 12 (below) | 100.5, 91.3, 91.1, 105.0, 97.5, 98.7 | **reported**, `docs/plan/evidence/2026-11-24-cloud-screens/allon.md` |
| `admhint` | ADM, hint-first | 23.2 unstalled, 20.8 stalled | reported (lane summary only) |
| `tadmsoft` | ADM, soft timeout | 21.4 | reported (lane summary only) |
| `tbase` | none (shipped defaults) | 11.5; 19.5 unstalled (45 min), 10.4 stalled (328 min) | reported (lane summary only) |

**Window boundaries in the two reports:**
- **`allon`:** started 2026-11-23T22:22:51Z; suppression ended 2026-11-24T04:23Z; hour 0 began then. Six full hours plus 9 minutes. The restart drill came after the window (10:32:51Z).
- **`combo`:** started 22:23Z; suppression ended 04:24Z. Six full hours. The drill was at 10:31:48Z.
- **Neither window contains a restart.**

**The 12 switches in `allon`** (`[dht]` keys): `coordinated_admission`, `responder_hints`, `streaming_peers`, `target_prefixes`, `utp`, `route_backoff`, `lookup_end`, `preset_slots`, `hint_first`, `sampler_cooldown_split`, `sample_backoff_escalate`, `soft_timeout`. `safe_restart` was on too.

**Other reported readings that matter for the design:**
- **`combo` per hour:** slot residence 19.4–20.5 s, 29.0–31.0 queries per lookup, 0.262–0.274 stored per lookup, 80 of 80 slots occupied in every scrape, query rate 199.9–200.0/s.
- **`allon`:**
  - 80 of 80 permits in use;
  - `fetch_admission_waiters` pinned at about 199,500 for 5.8 h, which the lane estimated as about 97% of arrivals waiting;
  - 45–59k queries throttled per hour, so the rate limiter was engaged;
  - outstanding peaked at 521 of 1,024.
- **Memory:**
  - `allon`: anonymous memory peaked at 477 MiB, with a 4 h slope of +7.4 MiB/h;
  - `combo`: anonymous memory rose from 389 to 519 MiB, with a 4 h slope of +8.6 MiB/h;
  - in both, spill slots grew 115–250k an hour.
- **After the post-window restart, sampling was suppressed again:**
  - `allon`: reason `locked`, then 15,907 s remaining;
  - `combo`: reason `resumed`, 18,697 s.
  - Both builds predate X-split-resume-gate (#1118) and CR-0w (#1134). See §5.5 on the witness format.

### 2.3 Records gap
- **What r17 and r18 hold:** r17 records the launch of `combo` and `allon` (`docs/decisions/2026-11-24-runner-records-r17.md`, line 136). r18 records `combo`'s first hour (§2.2).
- **What's missing:** `combo`'s later hours and every `allon` result. The lanes' messages to the coordinator were refused (`elevated_session_untrusted_device`), and their instances, with the results files, were terminated.
- **What's attached:** the `allon` and `combo` reports, copied from the lane sessions' own report text, marked as reported.
- **What's not attached:** the `tbase`, `admhint` and `tadmsoft` full reports aren't in this branch. Their figures come from the lanes' final summaries.

---

## 3. Governance: what this supersedes and who decides

### 3.1 The conflict
- **The plan the owner accepted:** on 2026-11-26 the owner accepted the fastest-1.0 proposal, whose §4 is a top-down test.
- **The runner's reading:** on 2026-11-27 the runner read the owner's "prioritize the new plan" as the r5/r5.1 sequence (`docs/owner-decisions.md` §2.84): comparator → 12 h → CR-AB-ADM and CR-AB-4 → Checkpoint 1 → CR-AB-LC.
- **Only the owner can settle which plan governs** (O1, §10).

### 3.2 What this amendment supersedes, if the owner agrees

| Item | Owner-set? | Under this amendment |
|---|---|---|
| CR-AB-ADM, CR-AB-4 as live stages before the gate | sequence: §2.80, §2.84 | Not run before the gate. The slices stay for 1.1 attribution |
| Checkpoint 1 (CR-CP1) | yes: added by the owner on 2026-11-02 ("keep the gate, add this checkpoint") | Replaced by the miss report (§3.3) |
| CR-AB-LC, CR-AB-1, CR-5 as stages before CR-COMB | sequence: §2.80 | Not run before the gate; CR-LEDGER chooses round 2's fix |
| CR-COMB's dependencies (CR-AB-1, CR-5, `ext:owner-checkpoint-1`) | runner's plan, from owner decisions | Round 1 (§5) |
| CR-SPEC rules 5.5 (Balanced only), 6.1 (one treatment), 6.8 (one CR-COMB window) | owner-confirmed spec (`ext:owner-cr-spec`) | Changed by a decisions file before launch (§7) |

**Unchanged:** G-v06-crawler's three tests, rule 2.2's comparator spacing, the carry-in rules, one crawler per public IP, and §2.27 ("the runner never loosens the gate").

**Who decides:** the owner decides O1–O3. The runner records them in `docs/owner-decisions.md`, in the CR-SPEC decisions file, and in `runner/slices.yaml` and the dashboard, before launch.

### 3.3 If round 1 misses: the miss report (replaces Checkpoint 1)
Within 24 h of the last window, the runner reports to the owner what CR-SPEC rule 6.9 asks of Checkpoint 1:
- the measured rates, with per-hour and per-day spread;
- the binding limiter from CR-LEDGER (§4);
- the comparator's rate on the box;
- the round-2 fix and a projection with stated uncertainty.

If the projection for round 2 is short of 100/min, the owner re-decides before round 2 runs, as Checkpoint 1 provided.

---

## 4. The capacity bound (not a forecast)

Lookup completions per second, `C`, are bounded by the smaller of two limits:

```
C ≤ min( S / W_slot ,  (R − Q_other) / q_lookup )
```

where:
- `S` is the slot count: 32 with `preset_slots` off; with it on, 32 / 80 / 256 for Low / Balanced / Aggressive (`crates/dht/src/node/mod.rs`);
- `R` is the preset's query rate: 50 / 200 / 1,000 per second;
- `Q_other` is non-lookup queries per second (sampling, maintenance);
- `q_lookup` is lookup queries per completion;
- `W_slot` is slot residence per completion.

Stored per minute is at most `60 · C · y`, with `y` = stored per completion.

**What the two terms do and don't say:**
- **They aren't independent.** A lookup waiting for a packet token holds its slot, so a full slot pool with the rate limiter engaged (`allon`: 80 of 80 and 45–59k throttled per hour) doesn't show two separate bottlenecks. Part of `W_slot` may be token waiting.
- **`combo`, as reported:**
  - slot term: `80 / 20.3` ≈ 3.9/s;
  - packet term: `(200 − ~70) / 31` ≈ 4.2/s, taking `Q_other` ≈ samples per minute ÷ 60;
  - `60 · 3.9 · 0.27` ≈ 63/min, matching the reported rate.
  - The two terms are too close to tell which binds. The agreement is again partly arithmetic.
- **`allon`:** the report gives no per-hour slot residence, queries per lookup or `y`. They are **unknown**, and no bound is computed for `allon`.
- **Aggressive** raises `S` by 3.2× and `R` by 5×. Whether completions rise, and by how much, depends on `W_slot`, `q_lookup` and `y` under 5× the query rate, through one VPN exit. No run has measured that. **No projection is used for any decision in this plan.** Round 1 measures it.

**Measured per hour, per arm, from one consistent cohort (the CR-SPEC window and CR-LEDGER):** admissions; slot residence by end reason; queries by purpose (lookup, sampling, maintenance); completions; `y`; fetch attempts, successes and duration; index lag (discovery to searchable); the fetch-admission queue and its evictions. A round-2 fix is chosen from these, not from the bound.

---

## 5. Round 1

### 5.1 One predeclared candidate
- **Before launch, the owner picks the candidate** (O2, §10). Only the candidate can count toward acceptance. Every other arm is diagnostic.

| Option | Candidate | Shipping settings | Trade-off |
|---|---|---|---|
| **A** (recommended under the owner's "performance first" instruction) | round-1 image, all 12 switches on by default, **Aggressive as the shipped default** | `LODESTAR_INTENSITY=aggressive`, the 12 `[dht]` switches at their new defaults, `safe_restart` as CR-0w ships it | More headroom over 100/min. The owner must accept 1,000 queries/s and up to 4,096 outstanding queries (home-router connection tracking) as the default |
| **B** | the same image, **Balanced** default | `LODESTAR_INTENSITY` unset (Balanced) | No change to an owner-set preset. `allon`'s reported 91–105/min at Balanced is marginal against a 24 h mean ≥ 100 |

- **The candidate's image digest and full effective settings** are recorded in the CR-SPEC decisions file before launch.
- **If round 1's results lead to a different preset or any configuration change,** that configuration gets a fresh 24 h acceptance run (§5.4). Round 1 doesn't count for it.

### 5.2 Arms

| Arm | Role | Configuration | When it runs |
|---|---|---|---|
| **K** (the candidate) | acceptance | as declared in §5.1 | always |
| **C** | baseline | the round-1 image with all 12 switches off and Balanced, by config | always; low load (CR-0: 76 q/s) |
| **D** | diagnostic | the other preset of §5.1 with all switches on | only if the rehearsal (§5.3) shows headroom: host CPU p95 under 60% with K and C running |

- **Exits:** a different PIA WireGuard server for each arm. They rotate on day 2, so the candidate meets two exits. Each arm has its own public IP (one crawler per IP).
- **Each day starts fresh:** a new store, data directory and node identity, with no restart inside a window (CR-SPEC rule 2.6). With CR-0w merged there's no first-start hold, so `ws` = process start + 60 min (rule 1.1).
- **Spacing:** the comparator's actual end anchors everything. Every round-1 window starts at or after the comparator `result.json` `ended` + 12 h, so each process starts at or after `ended` + 11 h 30 min: 60 min warmup and a 30 min margin, as the runner's existing rule for CR-AB-ADM uses.
- **Bitmagnet doesn't run beside round 1.** cr-ab-comp-2 calibrates the box. The paired bitmagnet scorecard stays on the RC's 24 h run (proposal §4.8, D13 B, G-v1-vs-bitmagnet).

### 5.3 Concurrency rehearsal (inside the 12 h gap)
- **When:** rule 2.2 bars Lodestar *windows*, not processes, for 12 h after the comparator's window. So a rehearsal can run after the comparator ends without delaying round 1.
- **What runs:** all intended arms at once for 2 h, each in its own gluetun container on its own exit, each passing `leak-test.sh` before start and after stop. Fresh stores, deleted after. Not a measurement.
- **What it records:** host CPU and steal, per-arm CPU seconds and RSS, `/data` disk latency (p99), each tunnel's handshake age and drops, per-arm UDP packets per second, and loss.
- **What it decides:** whether arm D runs, and whether the contention thresholds (§5.4 rule 6) are realistic. Changing a threshold after the rehearsal is a CR-SPEC change and needs a new decisions file before day 1.

### 5.4 Pass rules, fixed before day 1
**For each day, the candidate's window must independently satisfy all of the following.** Counts are on the carry-in-adjusted network-searchable cohort (CR-SPEC rules 1.5–1.6 and 3.1–3.5):
1. **Window mean ≥ 100/min** (rule 1.6).
2. **Every hour ≥ 70/min** (rule 1.5).
3. **p95 discovery-to-searchable ≤ 600 s,** with latency coverage ≥ 98% and dropped samples charged above 600 s (rules 4.1–4.5). "Not measured" fails.
4. **Validity:**
   - **An invalid window** (rule 6.5) or missing evidence (rule 6.6) is published with its reason and rerun whole, from a fresh store. Windows are never stitched (rule 6.7).
   - **A valid miss fails.** One valid miss on either day fails the two-day rule. The second day is a replicate, not a second chance.
5. **Mechanism checks:**
   - each declared switch is effective, from the process environment and its counter moving every hour (CR-SPEC §5, extended to the 12 switches);
   - the query rate stays within the preset;
   - outstanding stays within its cap;
   - the per-IP guards and the 64-query cap are unchanged (bounded load per node);
   - the leak test passes before and after.
6. **Contention:** over the candidate's window:
   - host CPU p95 ≤ 80% of the VM's 8 vCPU;
   - steal p95 ≤ 5%;
   - `/data` write latency p99 ≤ 50 ms;
   - no tunnel without a handshake for more than 180 s.
   - Proposed values, for review. A breach doesn't fail the candidate, but the day can't count for acceptance: it is reported, and the candidate needs a **solo confirmation** (one 24 h window with no other arm on the host, same rules 1–5).

**Outcome:**
- **Both days pass:** day 1 is CR-COMB's acceptance window and G-v06-crawler's evidence (§7 item 3).
- **Otherwise:** no claim. The miss report follows (§3.3).

**Memory, which decides whether the candidate can ship as the default (not part of G-v06-crawler):** the upper 95% confidence bound of the `anon` private-memory slope over measured hours 12–24 must be ≤ 2 MiB/h. That's the Linux soak gate's statistic (`docs/build-plan.md` §0.4; the bar was set in `docs/decisions/2026-10-30-CR-0-acceptance.md`). If the crawler tests pass but this fails, CR-COMB's defaults don't merge until the slope is fixed. Spill-table growth is the first suspect.

### 5.5 Diagnostics recorded (every arm, every hour)
- **Required:** every CR-SPEC rule 6.10 series, plus the CR-LEDGER series of §4.
- **The fetch-admission queue:** waiters, the age of the oldest waiter where exposed, evictions, and the end-of-window backlog (rule 3.4).
- **Memory:** RSS and anonymous memory each scrape; spill slots and whether the spill fallback triggered (#1122).
- **Per exit:** timeout share and reply rate, so throttling at Aggressive shows from the first hour.
- **Restart observation (after each candidate window, not inside it):** a clean restart that records the image digest, the restart snapshot's format version, the suppression reason and how long sampling stays held.
  - r18 records that a version-1 snapshot still resumes with its gate, which can hold sampling for up to about 6 h on the upgrade path.
  - `allon` and `combo` predate the fixes.
  - The drill shows whether the candidate meets `docs/owner-decisions.md` §2.77 on a v2 snapshot.
- **Copy-off:** keeps `process_start` and `last_scrape` (the step-a tooling finding, runner inbox 2026-11-27T08:15Z), and `effect` runs before any raw directory is removed.

---

## 6. Changes from the 2026-11-26 proposal

| Proposal item | Here | Why |
|---|---|---|
| X-resample-fast built before round 1 (§4.1 item 1) | Deferred to round 2, only if the ledger shows supply binding | `allon`'s admission queue sat full and CR-0 evicted 98% of harvest events unserved. Re-sampling sooner adds supply and costs politeness |
| Wider per-IP hint guard (§4.1 item 2) | Dropped | The review (item 2) found one hash per responder enough |
| Base, loose, loose + Aggressive arms | One predeclared candidate, a baseline, and an optional diagnostic arm | Acceptance needs one candidate; contention needs fewer arms |
| Bitmagnet beside round 1 | On the RC's run only (D13 B) | cr-ab-comp-2 calibrates the box |
| A separate CR-COMB run after the winner is picked | Round 1's day 1 counts, if the candidate was declared before launch and both days pass | Saves about 26 h; selection after the fact needs its own run (§5.1) |

**Unchanged:** the safety rules (private hashes never reach `get_peers`, VPN leak protection, content filters), no Sybil techniques, and politeness work in 1.1.

---

## 7. CR-SPEC changes (one decisions file, merged before the rehearsal)

1. **Preset (rule 5.5):** a Lodestar segment may use the preset the CR-SPEC decisions file declares for it, not only Balanced. The acceptance candidate's preset must be its shipping default.
2. **Parallel arms (new):** more than one Lodestar arm may run on the experiment host at once when:
   - each has its own VPN exit and public IP;
   - each passes the leak test before and after;
   - the contention rule (§5.4 rule 6) is met for any window used for acceptance.
3. **Acceptance window (rules 6.1 and 6.8):** CR-COMB's one valid 24 h window may be a round-1 window of the candidate declared before launch, when:
   - the same candidate's second-day window on a different exit also passes rules 1–3;
   - both windows meet rules 4–6.
4. **No start hold:** for a build without start suppression, `ws` = process start + 60 min (rule 1.1 unchanged in form). Run-sheet line 66 already covers this.
5. **Host safety on the VM** (proposal D3, approved): mover runs and parity checks on the array don't invalidate a window whose data is on `ssd_apps`. Other host-activity entries (rule 6.11) still do.
6. **Unchanged:**
   - rule 2.2's comparator spacing, anchored to its `result.json` `ended`;
   - fresh stores;
   - no stitching;
   - the carry-in bound;
   - the latency cohort and coverage rules.

---

## 8. Phase 0: during cr-ab-comp-2 (to about 2026-11-28T18:00Z)
Nothing runs on the box or its public IP (rule 2.2).
1. **Owner:** O1–O3 (§10).
2. **Runner, records:** the owner's answers, and the lane reports in §2.2 (already attached here).
3. **CR-COMB build (one Opus slice):** the 12 switches default on in `crates/lodestar/src/config.rs`; the declared preset as the default only if the owner chose candidate A; every switch still individually off-able.
4. **CR-0w (#1134):** merge. Its hold exists only to keep the stage-1 images' 6 h start timing (runner-state note, 2026-11-27T10:30Z), and the serial stages no longer run. §2.77 requires it for any shipped default.
5. **Pin the round-1 image** from `main` after 3 and 4, by digest, and record it in the decisions file.
6. **Optional cloud screen at Aggressive:** one 16 h on-only screen, about $2.70. It can only stop a clear failure and never delays the box. It's blocked while the X-test-vm lanes' SSM access is denied, and until the owner has checked for an AWS quarantine policy (runner inbox, 2026-11-27T08:15Z). Skip it if it isn't ready.
7. **Box prep:** three gluetun containers on three PIA servers, each passing `leak-test.sh`; fresh data directories on `/data`; the power setting unchanged for the whole block.

**Calendar (estimate):**

| Step | Time (UTC) |
|---|---|
| Comparator `ended` | about 11-28T18:00Z |
| Rehearsal | in the following 12 h |
| Day 1 window start | `ended` + 12 h |
| Day 1 window end | about 11-30T07:00Z |
| Day 2 window end | about 12-01T08:30Z |
| Gate verdict | about 12-01, if no solo confirmation is needed |
| Solo confirmation, if needed | about 26 h more |

The current sequence reaches CR-COMB in mid-December at the earliest.

---

## 9. Risks

| Risk | Sign | Response |
|---|---|---|
| Cloud readings don't transfer (exits, CPU, cohort definition) | the candidate well under `allon`'s reported range | C gives the box's own baseline; the miss report (§3.3) follows |
| Aggressive is throttled by PIA or remote nodes | timeout share rising, reply rate per exit falling | per-exit records (§5.5); round 2 or a middle preset (owner) |
| Memory slope over the bar | the hours 12–24 upper bound above 2 MiB/h | blocks shipping the defaults (§5.4); spill table first |
| Spill fallback inside 24 h | spill slots near 7.86M | #1122's fallback; record when it triggers |
| Shared-host contention | rule 6 breach | solo confirmation |
| Home-router connection tracking at Aggressive | outstanding near 4,096 | the owner weighs it in O2; the candidate's outstanding peak is recorded; the guide says how to choose Balanced |
| Invalid windows (VPN drop, host event) | rule 6.5 or 6.6 | publish, rerun whole |
| CR-0w changes more than the start hold | regressions against the cloud screens | C and the ledgers show it; CR-0w passed review and CI |
| Security in the crawler path at higher load | — | Track Q and the 1.0 security reviews still gate 1.0 |

---

## 10. Decisions for the owner (before launch)

| # | Decision | Recommendation |
|---|---|---|
| O1 | Adopt this sequence: round 1 replaces the serial live stages and Checkpoint 1 (§3.2); the miss report replaces Checkpoint 1's report (§3.3); attribution per switch moves to 1.1 | Yes |
| O2 | The candidate: **A**, Aggressive as the shipped default, or **B**, Balanced (§5.1) | A, following "We must prioritize overall performance", with the router note in the guide. B if the owner doesn't want 1,000 queries/s by default |
| O3 | Confirm the CR-SPEC decisions file (§7) | Yes |

**The runner's calls:** recording, the CR-COMB slice, merging CR-0w, pinning the image, box prep, the rehearsal, and whether arm D runs.

---

## 11. Review log

**First review (2026-11-27), on revision 1 up to §2.3:**

| Finding | Change in revision 2 |
|---|---|
| 1. Correct the evidence record; cite r18's `combo` hour; attach lane summaries with counter, denominator, hours, warmup and restart boundaries; label reported numbers | §2.2 rebuilt: r18's hour cited as of record; `allon` and `combo` reports attached and labelled; the measurement differences and window boundaries stated; §2.3 corrected |
| 2. Resolve which decision governs; list superseded steps; who decides; replace Checkpoint 1; Aggressive eligibility, parallel validity, reuse of round 1; anchor the 12 h gap | §3 (governance, superseded steps, miss report); §7 items 1–3; §5.2 spacing from `result.json` `ended` |
| 3. Use the model as a capacity bound; the terms aren't independent; leave `allon`'s inputs unknown; measure on the host | §4 rewritten; no projection drives a decision |
| 4. Define the per-day pass; one candidate; a fresh run if results select the preset | §5.1 and §5.4 |
| 5. Shared-host interference; rehearse concurrency; predeclare solo confirmation; keep the RC's bitmagnet scorecard | §5.3, §5.4 rule 6, §5.2 |
| 6. Keep diagnostics; queue age, evictions and latency; full memory fit with the 95% bound; restart witness format | §5.5; memory rule in §5.4 |

## Sources
- `docs/reviews/2026-11-20-path-to-100-review.md`
- `docs/decisions/2026-11-02-CR-SPEC.md`, `docs/decisions/2026-11-20-path-to-100-cr-spec.md`
- `docs/decisions/2026-10-30-CR-0-acceptance.md`
- `docs/decisions/2026-11-21-CR-1c.md`
- `docs/decisions/2026-11-24-runner-records-r17.md`, `docs/decisions/2026-11-27-runner-records-r18.md`
- `docs/owner-decisions.md` §2.77, §2.80, §2.82, §2.84
- `crates/dht/src/node/mod.rs` (`Intensity`), `crates/lodestar/src/config.rs`
- runner-state `state.json` and `inbox/x-test-vm.md` (comparator booking, CR-0w hold, SSM blocker)
- `docs/plan/evidence/2026-11-24-cloud-screens/` (reported lane reports)
