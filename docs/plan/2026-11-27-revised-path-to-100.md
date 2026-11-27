# Revised path to 100/min: all-on first, on the box (2026-11-27)

**Status:** a proposal for independent review. It is not decided. Nothing here changes an owner decision until the owner says so (`docs/owner-decisions.md`), and nothing changes CR-SPEC until a decisions file is merged before the run it governs (`docs/decisions/2026-11-02-CR-SPEC.md`).

**Amends:** `docs/plan/2026-11-26-fastest-1.0-proposal.md` §4 (on branch `claude/amazing-darwin-vzxlgw`, not on `main`). The owner accepted that proposal on 2026-11-26 ("yes to all recommendations"; D13 "Let's go with B"). Its scope freeze (§3), its 1.0 gates and its §5 politeness plan stand. This document replaces its crawler sequence (§4.1–4.3) only.

**Why now:** the bitmagnet comparator cr-ab-comp-2 runs on the box from 2026-11-27T17:00Z to about 2026-11-28T18:00Z. The next box slot opens 12 h after it ends (CR-SPEC rule 2.2). The current plan spends that slot on CR-AB-ADM, the first of about six serial live stages. This proposal spends it on the run that can pass the gate.

Numbers marked **estimate** are the author's arithmetic, not measurements. Every measured number names its source.

---

## 1. What to review

The reviewer is asked to check, in order of weight:

1. **The evidence (§2).** The two cloud screens it rests on (`combo`, `allon`) were never recorded in the repository (§2.3). Are the numbers used fairly, given that they are on-only screens with no control?
2. **The model (§3).** Is `stored/min = 60 · L · y / W` with a packet bound the right way to project Aggressive? Is anything binding that the model leaves out?
3. **The test design (§4).** Is round 1 reliable enough to stand as the gate's acceptance run (§4.4)? Is "pass on both days" the right rule?
4. **CR-SPEC (§6).** Does any change weaken a protection the spec relies on?
5. **What was dropped (§5).** Is any dropped item actually needed for 1.0?
6. **Risks (§8).** What's missing?

---

## 2. Evidence

### 2.1 CR-0: where the baseline loses (accepted, of record)
Source: `docs/reviews/2026-11-20-path-to-100-review.md`, from the CR-0 24 h run.

| Quantity | Value |
|---|---|
| Stored | 12.3/min |
| `L`, mean busy lookup slots | 30.3 of 32 (30+ in 88% of hours) |
| `W`, slot hold time per lookup | about 24 s |
| `q`, wire queries per lookup | ≤ 59, probably 40–55 |
| `y`, stored per admitted lookup | 0.16 |
| DHT queries | 76/s of Balanced's 200/s |
| Metadata fetch | 17.9 of 400 sockets busy, about 3.8 s per attempt |
| Pool backlog | `fetch_pending` pinned at about 200,000; 98% of harvest events evicted before service |

Check: `60 × 30.3 × 0.16 / 24` = 12.1/min, against 12.34 measured.

**Reading:** the 32 lookup slots bind. Supply (hashes heard) and metadata fetch capacity do not.

### 2.2 Cloud screens, 2026-11-23 to 2026-11-24 (provisional, not of record)
All on AWS c6a.xlarge (4 vCPU), us-east-1, through gluetun WireGuard with the leak test passing. They used the **Balanced** preset, ran 12.8–12.9 h including the 6 h start suppression, and were on-only with **no control arm**. The rates are leads, never effect estimates (`docs/owner-decisions.md` §2.82). The lanes' own reports say so.

| Lane | Commit | Switches on | Stored/min | `W` | `q` | `y` | q/s |
|---|---|---|---|---|---|---|---|
| `tbase` | a91ab090 | none (shipped defaults) | 11.5 after suppression; 19.5 unstalled (45 min), 10.4 stalled (328 min) | — | — | — | 66 |
| `tadmsoft` | a91ab090 | ADM, soft timeout | 21.4 | 23.7 s | — | — | ≤ 174 |
| `admhint` | 9ea2e855 | ADM, hint-first | 23.2 unstalled, 20.8 stalled | 23.9 s | 34.6 | 0.30 | 159 |
| `combo` | 6bec9388 | ADM, split, lookup end, preset slots | **61.7–66.2 every hour** | ≈ 20 s | ≈ 31 | ≈ 0.27 | **200 (cap)** |
| `allon` | 6bec9388 | all 12 (below) | **91–105 every hour** | — | — | — | **198–200 (cap)** |

The 12 switches in `allon`, as `[dht]` keys: `coordinated_admission`, `responder_hints`, `streaming_peers`, `target_prefixes`, `utp`, `route_backoff`, `lookup_end`, `preset_slots`, `hint_first`, `sampler_cooldown_split`, `sample_backoff_escalate`, `soft_timeout`. `safe_restart` was on too.

Other `allon` readings:
- All 80 lookup slots (Balanced with `preset_slots`) were busy, and the fetch-admission queue sat flat at about 199,500 waiters for 5.8 h.
- Query rate was at Balanced's cap all run. Outstanding peaked at 521 of 1,024.
- No table-full stall: the split held samples at 2.0–2.4k/min.
- Fetch success 3.8–4.5%.
- Anonymous memory peaked at 477 MiB, with a slope of +7.4 MiB/h over the last 4 h (`combo`: +8.6 MiB/h).
- After a restart, `safe_restart` suppressed sampling again for about 4.4 h (`combo`: 18,697 s).
- 0 panics, 0 ERROR lines, 0 cap breaches.

**Reading:** with every switch on, Balanced reached about the gate rate on the cloud, and it was capped by **both** its slots (80) and its query rate (200/s). Aggressive raises both: 256 slots and 1,000/s (`crates/dht/src/node/mod.rs`, `Intensity`). No run has ever used Aggressive.

### 2.3 A records gap
- r17 (`docs/decisions/2026-11-24-runner-records-r17.md` line 136) records the launch of `combo` and `allon`, but not their results.
- The lanes' final reports could not reach the runner: their `send_message` calls were refused (`elevated_session_untrusted_device`), and their instances were terminated.
- The 2026-11-26 proposal therefore says "none has run the lookup-cost pair as a treatment". The runner's 2026-11-27 priority (owner-decisions §2.84) still starts with single-switch A/Bs.
- The figures above come from the lane sessions' final messages. The runner should copy the lane reports (`allon.md`, `combo-final.md`, `tbase.md`, `admhint.md`, `tadmsoft.md`) into its records before relying on them (§7).

### 2.4 Other recent readings
- **Stage-1 blocks (cloud, ABBA/BAAB):**
  - s1route: −40.1%, CI −97% to +1183%, inconclusive;
  - s1adm and s1utp aren't decidable (no host clock line in block 1);
  - s1aa is at the noise floor.
- **Runner call, r18 inbox, 2026-11-27T08:15Z:** with two 6 h blocks per arm, this design resolves only large effects.
- **CR-1c** failed its CR-SIM screen (`docs/decisions/2026-11-21-CR-1c.md`). The simulator under-predicts the live network (the review, item 6), and `combo`/`allon` are the first live readings with it on.

---

## 3. The model and the projection

**Throughput:** `stored/min = 60 · L · y / W`, where `L` is busy lookup slots.

**Two caps on `L`:**
- **Slot bound:** `L ≤ S`, the preset's slot count (32 off; 32 / 80 / 256 for Low / Balanced / Aggressive with `preset_slots`).
- **Packet bound:** `(L / W) · q + Q_other ≤ R`, where `R` is the preset's query rate (50 / 200 / 1,000 per second) and `Q_other` is sampling plus maintenance.

**Check against `combo` (Balanced, S = 80, W ≈ 20 s, q ≈ 31, y ≈ 0.27):**
- Slot bound: `60 · 80 · 0.27 / 20` = **64.8/min**, against 61.7–66.2 measured.
- Packet use for lookups: `80 / 20 · 31` = 124/s. With sampling at about 75/s, that's about 200/s, the cap. Both bounds bind together, as observed.

**Projection at Aggressive (estimate):**

| Configuration | Slot bound | Packet use | Estimate |
|---|---|---|---|
| `combo` switches, S = 256, same W, q, y | `60 · 256 · 0.27 / 20` ≈ 207/min | `256 / 20 · 31` ≈ 397/s + sampling, under 1,000 | ≈ 200/min upper bound |
| `allon` switches, S = 256 | about 3.2 × 98 ≈ 310/min, if `allon` was slot-bound like `combo` | under 1,000/s | 150–300/min |

**Why the real figure will be lower than the bound, and by how much, is the open question:**
- **`y` may fall** as lookups reach deeper into the backlog (older, less popular hashes).
- **`W` and timeouts may rise** at 5× the query rate: through one VPN exit, against remote rate limits, and with PIA's own UDP handling. About 38% of queries timed out in CR-0.
- **Metadata fetch:** at about 13 lookups/s and CR-0's 3.7 attempts per lookup at 3.8 s, about 180 sockets are busy, under Aggressive's 1,000. It should not bind.
- **CPU and memory** on 8 vCPU shared with other arms.

**What would falsify the plan quickly:** if Aggressive with all switches on stores well under 100/min on the box, the slot-and-packet model is missing a binding stage. CR-LEDGER's counters (`W` by end reason, `q` by purpose, `y`, time to first values, fetch success) then say which one (§4.6).

**Correction to an earlier estimate:** the first draft of this review put "all switches on Balanced" at 30–50/min from CR-0's `W` and `y`. `combo` and `allon` show that `lookup_end` and `hint_first` raised `y` (0.16 → 0.27–0.30) and cut `W` (24 → 20 s). The measured screens supersede that estimate.

---

## 4. The revised sequence

### 4.1 Phase 0: while cr-ab-comp-2 runs (to about 2026-11-28T18:00Z)
Nothing may run on the box or its public IP (CR-SPEC rule 2.2). Work in parallel:

1. **Records (runner):** copy the five lane reports into the runner's records. Record this plan's adoption, if the owner says yes (§9).
2. **CR-COMB build (one Opus slice, small):** all 12 switches default on in `crates/lodestar/src/config.rs`, `safe_restart` as CR-0w ships it, every switch still individually off-able. The preset is a setting (`LODESTAR_INTENSITY`), not a default change in this slice.
3. **Merge CR-0w (#1134).** The runner holds it only so that the stage-1 images keep CR-SPEC's 6 h first-start timing (runner-state note, 2026-11-27T10:30Z). With the serial stage-1 runs gone, that reason lapses. CR-0w is required by §2.77 for any shipped default, and it removes the post-restart suppression seen in `allon` and `combo`. The round-1 image includes it. Run-sheet line 66 already covers a build with no start hold.
4. **Pin the round-1 image** from `main` after 2 and 3 merge, by digest. Record the commit.
5. **Aggressive pre-screen (cloud, optional):** one 16 h on-only screen of the round-1 image at Aggressive, about $2.70, for crashes, caps, memory, CPU and VPN throughput at 1,000/s.
   - It can only stop a clear failure and never delays the box booking (the standing cloud-screen rule).
   - **Blocked today:** the X-test-vm lanes hit SSM AccessDenied on 2026-11-27, and the runner asked the owner to check for an AWS quarantine policy before any new cloud work. The two Linodes are an alternative. If neither is ready, skip it.
6. **CR-SPEC decisions file (§6),** merged before round 1 starts, with the owner's confirmation.
7. **Box prep:** three gluetun containers on three different PIA servers, each passing `leak-test.sh`. Three fresh data directories on `/data`. The power cap stays as it is for the whole block.

### 4.2 Round 1: three arms, two days (from about 2026-11-29T06:00Z)

| Arm | Build | Intensity | Switches | Purpose |
|---|---|---|---|---|
| **A** | round-1 image | Aggressive | all 12 on (defaults) | the candidate expected to clear the gate |
| **B** | round-1 image | Balanced | all 12 on (defaults) | the candidate that needs no preset decision; checks the cloud's `allon` reading on the box |
| **C** | round-1 image | Balanced | all 12 off by config | the box's baseline; the denominator for the scorecard and the 1.1 attribution work |

- **Exits:** three PIA servers, X / Y / Z. Day 1: A–X, B–Y, C–Z. Day 2: A–Y, B–Z, C–X. Each arm meets two exits, and each exit carries two arms.
- **Each day is a fresh start:** a new data directory and no restart in the window, as CR-COMB requires (rule 6). With CR-0w there's no first-start hold, so `ws` = process start + 60 min. The window is 24 measured hours, and each day takes about 25.5 h with setup.
- **Spacing:** day 1's windows start no earlier than 12 h after cr-ab-comp-2's window ends (rule 2.2).
- **Capacity check before day 1:** if a 30 min warm-up shows host CPU over 80% or steal time over 5%, drop arm C, not A or B.
- **Bitmagnet doesn't run in round 1.** cr-ab-comp-2 is the box's comparator reading. Bitmagnet runs beside the RC for the 1.0 scorecard (G-v1-vs-bitmagnet, proposal §4.8).

### 4.3 What is recorded (per arm, per day)
- Everything CR-SPEC rules 1–6 require: window mean, every hour, the carry-in bound and p95 discovery-to-searchable.
- CR-LEDGER: `W` by end reason, `q` by purpose, `y`, time to first values, hint outcomes.
- Query rate, outstanding peak, timeout share, fetch success, slot occupancy, the fetch-admission queue.
- Memory: RSS, anonymous memory, and its slope over hours 12–24 (the soak gate's window), plus spill slots.
- Host: CPU, steal, load, disk, each gluetun's throughput and packet loss.
- Copy-off keeps `process_start` and `last_scrape` (the step-a tooling finding, r18 inbox 2026-11-27T08:15Z), and `effect` runs before any raw segment directory is removed.

### 4.4 Decision rules, fixed before day 1
For each arm, **pass** means, on **both** days:
1. 24 h window mean ≥ 100/min;
2. no hour < 70/min;
3. p95 discovery-to-searchable ≤ 10 min;
4. anonymous-memory slope over hours 12–24 ≤ 2.0 MiB/h;
5. 0 panics, 0 cap breaches, and leak tests passing before and after.

Rules 1–3 are G-v06-crawler's (`runner/slices.yaml`). Rule 4 is the Linux soak bar. Requiring both days on different exits guards against one lucky exit.

**Outcomes:**
- **B passes:** B is the shipped default (Balanced, all on). Day 1 of B stands as CR-COMB's acceptance run and G-v06-crawler's evidence, if §6 item 3 is adopted. No preset decision is needed.
- **Only A passes:** the owner decides the default (§9 O2). If the owner accepts Aggressive as the default, day 1 of A stands as the acceptance run. If not, the gate isn't claimed, and round 2 targets Balanced.
- **Neither passes:** go to §4.6. C's numbers and A/B's ledgers name the loss.
- **A or B fails only rule 4 (memory):** don't claim the gate. Fix the slope (spill-table growth is the first suspect) and rerun that arm alone.

### 4.5 After a pass
- Tag v0.6 when its other gates pass.
- G-v1-crawler and G-v1-soak-linux are judged later, on the RC's single 24 h run (proposal §6.1 week 5), with bitmagnet beside it.
- The 1.1 work (attribution per switch, crawl-rate parity with bitmagnet, politeness tuning) reuses this three-exit setup.

### 4.6 If round 1 misses: round 2 (about 3–6 days)
Pick the fix from the ledger, not from a list. In order of expected size (estimate):

| Ledger sign | Likely fix |
|---|---|
| Slots full and `W` high, timeouts rising | X-soft-lane-free (parked, `runner/slices.yaml`), shorter per-query timeout |
| Query rate at cap, slots not full | more queries per second, or fewer per lookup |
| `y` low, fetch success ~4% | fetch: more peers per hash, shorter per-peer deadlines. 96% of fetch attempts fail, the largest unexploited ratio |
| Fetch-admission queue empty or falling | supply: X-resample-fast (proposal §4.1 item 1) |
| Hours under 70 during sampler stalls | sampler coverage and spill behaviour |

Round 2 uses the same three-exit design: the best round-1 arm against it plus the fix.

---

## 5. Changes from the 2026-11-26 proposal

| Proposal item | Here | Why |
|---|---|---|
| X-resample-fast built before round 1 (§4.1 item 1) | **Deferred** to round 2, only if supply binds | Supply isn't the limit: CR-0 evicts 98% of harvest events unserved; `allon`'s admission queue sat full. It costs politeness for no measured gain |
| Wider per-IP hint guard (§4.1 item 2) | **Dropped** | The review (item 2) found one hash per responder enough: about 1,500+ responders a minute against about 750 lookups a minute needed at 120/min |
| Aggressive as one arm among several | **Primary arm A** | Both of `allon`'s caps bind at Balanced; Aggressive raises both |
| Base, loose and loose + Aggressive arms | A, B, C as in §4.2 | Same idea, without the code that isn't needed |
| Bitmagnet beside round 1 | **Moved** to the RC run | cr-ab-comp-2 already calibrates the box; saves CPU for arms |
| 2-exit daily swap | **3 exits, rotated** | Each arm meets two exits; exit effects are visible |
| Separate CR-COMB run after round 1 | **Round 1 can be the acceptance run** (§6 item 3) | Saves about 31 h |
| Throttle telemetry (§4.1 item 4) | Kept as an observation only: per-exit reply rate and timeout share | No code needed for round 1 |

**Unchanged:** scope freeze, the gates, the safety rules (private hashes never reach `get_peers`, VPN leak protection, content filters), no Sybil techniques, and politeness work in 1.1.

**Removed from the critical path:** CR-AB-ADM, CR-AB-4, Checkpoint 1 as a live stage, CR-AB-LC, CR-AB-1 and CR-5. Their switches are all in arm A. Attributing each switch moves to 1.1, as the proposal's §1 already says.

---

## 6. CR-SPEC changes needed (one decisions file, before round 1)

1. **Parallel arms on one host, each on its own VPN exit,** with the exits rotated as in §4.2. This is proposal D3, which the owner approved. It needs writing down as a rule.
2. **Fresh-start days without a first-start hold:** `ws` = process start + 60 min when the build has no start suppression (CR-0w). Run-sheet line 66 already anticipates this.
3. **The acceptance run may be a round's arm:** CR-COMB's acceptance run (rule 6: once, fresh directory, no restart, 24 measured hours) may be day 1 of a round-1 arm when the decision rules (§4.4) were fixed before day 1 and the arm passes on both days. The second day is a replicate, not a second chance: a fail on either day means no claim.
4. **Host-safety rules on the VM** (proposal D3): mover runs and parity checks on the array don't void a segment on `ssd_apps`. Other host events still do.
5. **Unchanged:** rule 2.2's comparator spacing, the carry-in bound, the latency cohort, and one crawler per public IP.

---

## 7. Process fixes

1. **Lane results go into the records, not only into messages.** A testing lane writes its final report to runner-state (`inbox/` or `results/`) in the same step as its teardown. `combo` and `allon` show what's lost otherwise.
2. **The runner's 2026-11-27 reading of "the new plan"** (§2.84: the r5/r5.1 sequence) differs from the proposal the owner had accepted hours before. The owner should say which plan governs (§9 O1).
3. **Any later lane at Aggressive** records per-exit timeout share and reply rate from the first hour, so throttling shows early.

---

## 8. Risks

| Risk | Sign | Response |
|---|---|---|
| Cloud readings don't transfer to the box (different exits, CPU, time of day) | B well under `allon`'s 91–105 on day 1 | C anchors the box's own scale; A still tests the headroom |
| Aggressive is throttled (PIA, remote rate limits) | timeout share rising over hours; per-exit reply rate falling | compare A's exits; consider a middle setting (§9 O2 option b) |
| Memory slope over the 2.0 MiB/h bar | `allon` +7.4 and `combo` +8.6 MiB/h late in a 6 h window | rule 4 catches it; spill-table growth is the first suspect; X-split-spill-horizon (#1122) bounds the spill table |
| Spill fallback reached inside 24 h | spill slots near 7.86M | #1122's fallback; record when it triggers |
| Box CPU contention between three arms | CPU over 80% or steal over 5% | drop C (§4.2) |
| Aggressive harms home routers (conntrack) | outstanding near 4,096 | the default decision (§9 O2) weighs it; A's outstanding peak is recorded |
| A run fails for a reason outside the crawler (VPN drop, host event) | a void by CR-SPEC rules | rerun that arm and day only |
| CR-0w changes behaviour beyond the start hold | regressions in A, B or C against the cloud screens | CR-0w passed review and CI; the C arm and the ledgers show it |
| Security-relevant code in the crawler path unreviewed at Aggressive load | — | Track Q and the 1.0 security reviews are unchanged and still gate 1.0 |

---

## 9. Decisions for the owner

| # | Decision | Recommendation |
|---|---|---|
| O1 | Adopt this sequence: round 1 (§4.2) replaces the serial live stages and Checkpoint 1; attribution per switch moves to 1.1 | Yes. Checkpoint 1 was the owner's own addition (2026-11-02), so this needs the owner's word |
| O2 | If only Aggressive clears 100/min: (a) Aggressive becomes the default; (b) a new middle preset, which changes owner-set numbers (Balanced's 200/s); or (c) keep Balanced and run round 2 for it | Decide after round 1. Lean (c) if B is within about 20% of 100; otherwise (a), with the router note in the guide |
| O3 | Confirm the CR-SPEC decisions file (§6) before round 1 | Yes |
| O4 | Merge CR-0w now, ahead of round 1 | Yes (runner's call once O1 is yes; listed for visibility) |

**Runner's calls, not the owner's:** recording the lane reports, launching the CR-COMB slice, pinning the image, and the box prep.

---

## 10. Calendar (estimate)

| When (UTC) | Current sequence (owner-decisions §2.84) | This proposal |
|---|---|---|
| 11-27 17:00Z → 11-28 ~18:00Z | cr-ab-comp-2 | cr-ab-comp-2; Phase 0 in parallel |
| 11-29 ~06:00Z | CR-AB-ADM starts (ABBA, 6 h segments × 2 blocks) | Round 1 day 1 |
| 11-30 ~07:30Z | CR-AB-ADM continues, then CR-AB-4 back to back | Round 1 day 2 |
| 12-01 ~09:00Z | | **Gate verdict** (pass, or round 2 starts) |
| about 12-03 to 12-06 | Checkpoint 1, then CR-AB-LC | Round 2 if needed |
| mid-December or later | CR-AB-1, CR-5, CR-COMB 24 h, gate | — |

Box time in this proposal: about 2.2 days to a gate verdict, against about two weeks or more. The rest of the 1.0 calendar (security reviews, M8, the RC) is unchanged.

---

## Sources
- `docs/reviews/2026-11-20-path-to-100-review.md`: CR-0's `L`, `W`, `q`, `y` and the fetch figures.
- `docs/decisions/2026-11-21-CR-1c.md`: slot counts per preset, caps, memory per slot.
- `docs/decisions/2026-11-24-runner-records-r17.md`: the cloud screens' launch; the sampler split's effect.
- `docs/decisions/2026-11-27-runner-records-r18.md` and `docs/owner-decisions.md` §2.84: the current priority.
- `docs/decisions/2026-11-02-CR-SPEC.md`, `docs/decisions/2026-11-20-path-to-100-cr-spec.md`: the measurement rules.
- `crates/dht/src/node/mod.rs` (`Intensity`), `crates/lodestar/src/config.rs`: presets and switches.
- runner-state `state.json` and `inbox/x-test-vm.md`: cr-ab-comp-2's booking, CR-0w's hold, the import result, the SSM blocker.
- The lane reports `tbase.md`, `tadmsoft.md`, `admhint.md`, `combo-final.md` and `allon.md` (2026-11-24), read from the lanes' final messages; not yet in the repository (§2.3).
