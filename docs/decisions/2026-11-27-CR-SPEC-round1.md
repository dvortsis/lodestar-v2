# CR-SPEC-R1 decisions: the measurement rules for round 1

This amends `docs/decisions/2026-11-02-CR-SPEC.md` (CR-SPEC) for round 1 of `docs/plan/2026-11-27-revised-path-to-100.md` (revision 6, "the plan"). It's the single decisions file the plan's §8 asks for, and it's merged before the rehearsal. Rule numbers such as 1.1, 2.2, 5.3, 5.5, 6, 6.1, 6.8 and 6.11 are CR-SPEC's. Each rule says what the tools enforce and where:
- `docs/soak/tools/analyze_cr.py` ("analyzer");
- `docs/soak/tools/linux/collect.py` ("collector");
- `docs/soak/tools/linux/host_samples.py` ("sampler");
- `infra/fleet/steps/vm/round1-coordinator.py` ("coordinator");
- `docs/soak/runsheets/host-round1-runsheet.md` ("run sheet").

**Line numbers** refer to `main` at e174bcf7 (the merge of #1151); run sheet line numbers refer to this PR's version.
- A rule tagged **(X-cr-round1-collect-fixes, #1151)** was added or changed by that slice to meet this file. `docs/decisions/2026-11-27-X-cr-round1-collect.md` (lines 100–120) records its items A–L.
- A rule tagged **(X-cr-round1-spec-fixes)** lands in that slice, which answers this PR's review. Its reason names are used here.
- The runner ruled on every mismatch the draft found (2026-11-27). §14 records each ruling.

## 1. Status and scope
- **The owner approved this design.** Recorded in `docs/owner-decisions.md` §2.86 (lines 834–848), 2026-11-27T17:52:16Z: "I approve", for O1–O4 as the plan's §10 recommends.
  - **O1:** round 1 replaces CR-AB-ADM, CR-AB-4, CR-CP1, CR-AB-LC, CR-AB-1 and CR-5 before the gate. The round-1 report replaces Checkpoint 1.
  - **O2:** Aggressive (A) is the authorized fallback. Balanced (B) goes first.
  - **O3:** the owner confirmed the plan's §8, including:
    - declarations in place of "every Lodestar segment on Balanced";
    - running `preset_slots` despite CR-1c's failed screen;
    - the §6.1 evidence table, with `route_backoff` off in both candidates;
    - rule 6.8's network identity.
  - **O4:** single-node performance. A miss under demonstrated contention gets a solo confirmation instead of ending the claim.
- **§2.85 D3** (owner-decisions.md:828) changed CR-SPEC's host design: parallel arms on separate VPN exits, swapped daily, and mover and parity runs don't void a VM segment. The owner confirmed it explicitly at 2026-11-27T18:11Z. §2.85 D4 (politeness) doesn't bear on this file.
- **§2.87** changes the AWS stop and the Legal Notice line. Neither touches these rules.
- **Unchanged (§2.86):** G-v06-crawler's three tests, rule 2.2's spacing, the carry-in rules, one crawler per public IP, and §2.27 (the runner never loosens the gate).
- **Refinements of owner-approved plan text, which the runner flags to the owner (§2.19):**
  - the comparator binding after the window (§10);
  - the leak-test ruling (§4);
  - L2c (§5);
  - the bound on S attempts: after `solo_confirmation_required`, or when its arm is the only one left, and at most one per candidate per round (§5);
  - the repeat exceptions (§5);
  - the rehearsal's 30 min warm-up and 3 h window, ended by the first decisive `drop-c` (§6);
  - dropping C when `drop-c` is still `not_judged` at the rehearsal's end (§5);
  - handshake classification resting on archived gluetun logs (§8);
  - the RTT base taken on the day-1 exit (§7);
  - the per-IP guards resting on the digest (§9);
  - coverage counted window-wide (§7);
  - the stricter `disk_inflight` (§7).
- **Scope:** round 1, and any later round of this design whose declaration names this file.
  - Runs that started earlier are judged by the spec in force when they started (plan §8 item 8, line 570).
  - `gate` and `effect` keep their behaviour for those runs. `segment` without `--declaration` is unchanged.

## 2. Declarations (rule 5.5 as amended)
A Lodestar segment runs the preset and switch values its declaration names (plan §8 item 1).

**The declaration:** a JSON file, `schema` 1, `kind` `cr_round1_declaration`, validated by `check_declaration` (analyzer:1824–1922). A malformed declaration is an error (exit 2), never a verdict. It holds:
- `round` (an integer ≥ 1), `spec_commit` and `image_digest` (§12);
- `host_identity`, and `network_identity` {`uplink_hash`, `provider`, `protocol`}. The hashes match `HASH` = `^[0-9a-f]{16,128}$` (1806);
- `backing_device` (the `/data` device's kernel name);
- `vcpus`, which must be 8 (`DECLARED_VCPUS`, 1858, 2016), and the optional pool map `pools` (§11; 1849–1856) (X-cr-round1-collect-fixes, #1151);
- `comparator.run_id`;
- optionally `selection_order` and `contention`, which may only restate the frozen values (1845–1848);
- `attempts[]` {`id`, `layout`, `candidate_digests`};
- `arms[]` {`round`, `attempt`, `day`, `arm`, `role`, `preset`, `switches` (exactly the 12), `exit`, `image_digest`} (1868–1885).

**The three arms**, as `check_declaration`'s table enforces them (1886–1895) and as plan §6.1 (lines 321–333) sets them:

| | **B** | **A** | **C** |
|---|---|---|---|
| Role | candidate, first choice | candidate (O2 authorized it; the code also allows `diagnostic`, 1889) | diagnostic: the box's baseline |
| Preset | `balanced` | `aggressive` | `balanced` |
| `LODESTAR_INTENSITY` | unset | `aggressive` | unset |
| `coordinated_admission`, `responder_hints`, `streaming_peers`, `target_prefixes`, `utp`, `lookup_end`, `preset_slots`, `hint_first`, `sampler_cooldown_split`, `sample_backoff_escalate`, `soft_timeout` | true, each | true, each | false, each |
| `route_backoff` | **false** (O3) | **false** (O3) | false |
| Selection order | 1st (`SELECTION_ORDER`, 1798) | 2nd | never selected (diagnostic) |
| `safe_restart` | true | true | true |

**Where the values come from:**
- **The 12 names** are `config::Dht::switches` (`crates/lodestar/src/config.rs`:295–311). They're also the collector's `SWITCH_ENV` without `safe_restart` (collect.py:1178–1196; analyzer `ROUND_SWITCHES`, 1785).
- **Compiled defaults:** all 12 are off and `safe_restart` is on since CR-0w (config.rs:267–280). Settings stay explicit until a candidate qualifies (plan line 332).
- **The coordinator sets every switch explicitly** (`arm_switch_env`, coordinator:537–544), in a scrubbed environment:
  - each switch as `LODESTAR_DHT_*=true|false`;
  - `LODESTAR_DHT_SAFE_RESTART=true`;
  - `LODESTAR_INTENSITY` only for a non-Balanced preset.
- **Preset numbers** (`crates/dht/src/node/mod.rs`:66–98):

  | Preset | Query rate | Outstanding cap | Slots with `preset_slots` | Analyzer constants |
  |---|---|---|---|---|
  | Balanced | 200 queries/s | 1,024 queries | 80 | `PRESET_QUERY_RATE` (73), `PRESET_OUTSTANDING` (80, from `analyze_cr0.OUTSTANDING_RPCS`, analyze_cr0.py:33), `PRESET_SLOTS` (81) |
  | Aggressive | 1,000 queries/s | 4,096 queries | 256 | the same |

- **One role per arm for the whole round** (1896–1898). No arm other than B, A and C is accepted (1892).

**Append-only (runner's ruling):**
- **This file freezes the rules and the L3 attempt's entries.** The attempt is `r1-L3` (runner-confirmed), layout `L3`, round 1. It has six entries: B, A and C on days 1 and 2.
  - Roles, presets and switches are as in the table above.
  - The exits are L3's (§5).
  - `image_digest` is §12's, and `candidate_digests` names it for B and A.
- **The live declaration** is `docs/soak/evidence/cr-r1/declaration.json`.
  - It's filled once, before the rehearsal, with `host_identity`, `network_identity`, `backing_device`, the pool map, `vcpus`, `image_digest` and `comparator.run_id` (§10).
  - That's done by a runner records PR or the CR-R1 evidence PR. The CR-R1 slice owns that folder.
- **Later attempts** (L2, L2b, L2c, S) are appended before they start.
- **An existing entry never changes.** Any change to one means a new round.

**Enforcement:**
- **At start:** `segment RUN --declaration D` finds the entry by `run.json` `round1` (`declared_entry`, 674–680). An entry that isn't found makes the segment `invalid`. `declared_checks` (683–698) then checks:
  - `effective_switches_recorded`: the 13 values of `lodestar_dht_switch{switch}` (`docs/metrics.md`:883), read at the first scrape that has all 13. If they're missing, the segment is invalid;
  - `declared_preset`, the settings API's preset;
  - `declared_switches`;
  - `safe_restart_on`;
  - `declared_image_digest`, compared on the part after the last `@` (698; X-cr-round1-collect-fixes, #1151).
  - The last four void the window on a mismatch. `gate-round` also requires every one of these checks to be present (§6 item 5).
- **At every boundary:** `preset_stayed_declared` and `switches_stayed_declared` (651–657) read the boundary rows' settings and `api.switches`. A mismatch voids the window. The gauge is set once at start (metrics.md:879), so the start read binds.
- **Placement:** `gate-round` requires each report's embedded entry to equal the manifest's declaration entry (`report_judged_against_another_declaration`, 2251–2252). Because entries never change, an appended attempt leaves earlier reports matching.

## 3. The frozen §6.1 evidence table
Bundle dispatch replaces the per-treatment dispatch for every round-1 window (plan §8 item 9):
- `measure` calls `bundle_evidence` and `mechanism` whenever a declaration entry exists, whatever the run kind (analyzer:880–884).
- A row that fails, or reads an absent series, voids the window (D7). The exception is where the "Absent" column says `missing`.
- `gate-round` then gives `not_judged` (`segment_void` or `segment_invalid`, 2244–2245).
- Deltas run over the window, from `t_s` to the end scrape (813–816).

| Switch (on) | Series read | Test and threshold | Absent | Function (analyzer) |
|---|---|---|---|---|
| `coordinated_admission` | `fetch_admission_grants_total`; the permit identity (metrics.md:742) | Δgrants > 0, and the identity gap never stays non-zero for 3,600 s (`PERMIT_STRETCH`, 58) | `missing`: the segment is invalid, published as `segment_invalid` (`required_series`, 300–301) | `mechanism`, 1171–1196 (rule 5.1) |
| `responder_hints` | `dht_hints_captured_total` | Δ > 0 | void | `bundle_evidence`, 758–764; `BUNDLE_COUNTERS`, 86 |
| `streaming_peers` | `fetch_stream_batches_total` | **`fetch_stream_batches_total` > 0** (Δ over the window) | void | same, 87 |
| `target_prefixes` | `dht_target_prefix_queries_total`, summed over `kind` | Δ > 0 | void | same, 88 |
| `utp` | `dht_utp_in_total`, `fetch_attempts_total{transport="utp"}` | both Δ > 0 | `missing`, published as `segment_invalid` (302–304) | `mechanism`, 1201–1205 (rule 5.2) |
| `lookup_end` | `dht_lookup_ended_total{reason="enough_values"}` and `{reason="values"}`, `dht_lookup_slots_occupied`, `dht_queries_out_total` | • **Δended on `enough_values` > 0** (`lookups_end_on_enough_values`, 768–772; X-cr-round1-collect-fixes, #1151);<br>• **the lookup-cost pair, whenever `lookup_end` is on** (called with `judged=lookup_end`, 767): Δended on values > 0, where values now counts `values` + `enough_values` (ledger, 951–955); the most occupied slots > 32 (`A1_SLOTS`, 76) at least once; and every scrape interval's queries ≤ rate × 1.05 × (interval s + 2 s) (`VERIFY_SHARE`, `RATE_SLACK_SECONDS`, 74–75) | void | `lookup_cost_pair`, 968–995 |
| `preset_slots` | `dht_lookup_slots_capacity` | **capacity equals the preset count** (80 for Balanced, 256 for Aggressive) at every window scrape | void | `bundle_evidence`, 773–780 (`preset_slots_capacity`) |
| `hint_first` | `dht_hint_first_marked_total`, `fetch_hint_lookups_total`, `fetch_hint_ignored_total`, `fetch_hint_keys_total` | Δmarked > 0, Δlookups > 0, and Δignored ≤ Δkeys | void | `hint_first`, 998–1023, called at 781 |
| `sampler_cooldown_split` | `dht_sample_spill_slots`, `dht_sample_cooling` + `dht_sample_inflight`, `dht_sample_full_seconds_total` | If the table filled (≥ 200,000 entries, `SAMPLER_TABLE`, collect.py:222, or spill > 0), spill slots > 0 at some scrape from then on. If it never filled: `not_exercised`, reported | void (`sampler_split_series_present`, 783–784) | `sampler_split`, 1026–1063 |
| `sample_backoff_escalate` | the gauge `dht_sample_cooling_reason{reason="first_failure"}` | **the max over the window's scrapes > 0** | void (if absent at any scrape) | `counter_row`, 715–721; `BUNDLE_COUNTERS`, 89 |
| `soft_timeout` | `dht_soft_timeouts_total` | The counter is rendered in every interval. A count of 0 is a valid result | void | `bundle_evidence`, 785–789; `soft_timeout`, 1079–1117 |
| `route_backoff` (off) | `fetch_route_skipped_total`, `fetch_route_probe_down_total` | When rendered at `t_s`: Δskipped = 0, and with `utp` off, Δprobe_down = 0 | not checked | `mechanism`, 1210–1218 (rule 5.3, unchanged) |
| **Caps, every arm** | `dht_queries_out_total`, `dht_outstanding` | An interval over rate × 1.05 × (interval + 2 s) voids. So does **any scrape with `dht_outstanding` above the preset max** (1,024 or 4,096) | void (an unread value counts as over) | `preset_caps`, 724–750 |

- **The collector keeps `enough_values`** among the ledger's end reasons (`LEDGER_END_REASONS`, collect.py:164–165; `LEDGER_EARLY_VALUES`, 167) (X-cr-round1-collect-fixes, #1151). The ledger's W, q and y count those lookups where the series is rendered. Older binaries don't render it, so it counts 0 there and their numbers are unchanged.
- **The CR-ADM and uTP rows are stricter than the plan's wording,** and the stricter checks stand (runner's ruling). They're CR-SPEC's existing rules 5.1–5.2.
- **C's off switches** are checked idle where CR-SPEC already checks them: `admission_idle_when_off`, `utp_idle_when_off` and `route_backoff_idle_when_off` (1197–1218). The other off rows are reported, not judged.

## 4. Parallel arms (plan §8 item 2)
Up to three Lodestar arms run on the experiment host at once when every condition below holds.

1. **Own containers, state, tunnel and Compose project:**
   - project `lodestar-<arm>`, with its own bridge;
   - containers `gluetun-<arm>`, `lodestar-<arm>` and `lodestar-docker-proxy-<arm>`;
   - its own stack directory, holding `secrets/wg0.conf`, `data` and `witness`;
   - its own loopback port (coordinator:243–266).
   - Every arm has a different PIA WireGuard server (`check-endpoints`, coordinator:374–385).
2. **Distinct, verified exits, hashed hourly:**
   - `bring-up` reads each arm's egress inside its gluetun namespace, as `HMAC-SHA256(salt, "ip:"+address)`. It refuses two arms on one hash, or an arm on the uplink's own hash (coordinator:568–604).
   - Each exit check goes into the archive's `exit_checks`, stamped with its first reading's time (597–598).
   - The collector reads egress at the first scrape and at every boundary. It retries the final boundary up to 3 times, 20 s apart; each retry is stamped when its read began.
   - **Judged against the exit in force at each reading's time** (`exit_in_force`, analyzer:2185–2195; `egress_problems`, 2197–2225) (X-cr-round1-collect-fixes, #1151):
     - **In the window:** every reading must match the exit checked last at or before the window start. A reading within ±90 s of every boundary is required.
     - **Before the window:** each reading must match the exit in force at its own time. So a reading of the server used before a repair is history, not a fault.
     - **An exit check after the window start** fails the day (`egress_assignment_missing`, 2205–2207) only when the current exits differ from those in force at the window start. A lone first check after the window start passes: with no earlier check, it's the one in force (2192–2193).
   - The day's exits must not share a hash (2277–2278).
3. **A leak test before and after each day,** run by the coordinator, one arm at a time:
   - `leak` holds a lock in the round's folder (`leak_lock`, 477–486) and refuses while any collector measures (498–500).
   - A leak test that times out fails, whatever it printed (`run_leak_test`, 453–474).
   - `gate-round` needs `before` = `pass` and `after` = `pass` for every declared arm (`leak_test_not_passed`, analyzer:2287–2289).
   - `before` is the latest `before` or `retest` result before the start (`leak_summary`, coordinator:334–338).
4. **Concurrency fixed for the round.** Within an attempt, the roster must equal the declaration (`roster_differs_from_declaration`, 2271–2272). It changes only through a new attempt that drops an arm (§5).

**A failed leak test** (runner's ruling, X-cr-round1-collect line 59, flagged to the owner under §2.19):
- It stops that arm's Lodestar, or every arm's with `--host-level`.
- It blocks `check-start`, `up` and `collect` for those arms until a `--phase retest` passes (coordinator:518–532, 415–416, 639–641).
- **Before any collector has started:** nothing was measured. The failure goes in the day's `notes` in the archive (529), and the day runs as planned once a retest passes. It isn't repeated.
  - **If the repair changed a server,** `bring-up` runs again before the start.
  - `check-start` refuses an exit check older than the latest leak test (423–430).
  - Egress is then judged against the new check (item 2).
- **After the start, or in an `after` test:** the failure is an `infrastructure` event (525–527), and §8 applies.
- The run sheet says the same (lines 59, 160–162, 319) (X-cr-round1-collect-fixes, #1151).

**Reboot rule:**
- Round-1 stacks never restart by themselves. The coordinator sets `LODESTAR_GLUETUN_RESTART=no` (coordinator:266), and Lodestar's own policy is `"no"` (`compose.yaml`:111).
- After a host reboot, the stacks stay down and the day is invalid.
- The repeated day starts as any day does: leak tests, then `bring-up`, then `up` (run sheet line 322).

## 5. Layouts and attempts
**The layouts** (`LAYOUTS`, analyzer:1788–1796; plan §6.3, lines 394–401; plan §8 item 11). `layout_problems` (2166–2177) requires each day's declared arms and exits to match exactly:

| Layout | Day 1 | Day 2 | When |
|---|---|---|---|
| L3 | B–X, A–Y, C–Z | B–Y, A–Z, C–X | the rehearsal keeps C |
| L2 | B–X, A–Y | B–Y, A–X | the drop-C rule fires, C times out in the rehearsal, or C fails |
| L2b | B–X, C–Y | B–Y, C–X | A is dropped: a diagnostic A that times out or fails, or an authorized A out pending diagnosis |
| L2c | A–X, C–Y | A–Y, C–X | B fails in L3 (plan line 470); A candidate, C diagnostic (1794; X-cr-round1-collect-fixes, #1151; run sheet lines 46–49) |
| S | one arm, one exit | the same arm, a different exit | only as §5's attempts rules allow: a solo confirmation (O4), or the only arm left. At most one per candidate per round (X-cr-round1-spec-fixes) |

**Attempts:**
- **IDs:** each attempt has an `id` matching `^[A-Za-z0-9_.:-]{1,80}$` (`RUN_ID`, 1816), distinct within the declaration (1862–1865).
- **Pairing:** a candidate's two days pair only within its latest attempt (`judge_candidate`, 2369–2374). The attempt must declare days 1 and 2 (`attempt_does_not_declare_two_days`, 2427–2429).
- **Repeats:** each (attempt, day) runs repeats 0..k with no gap (`repeats_not_contiguous`, 2386–2388; coordinator `manifest`, 983–985).
  - A repeated day is the folder `day<n>-repeat<k>`.
  - The repeat number comes from its first `leak` (run sheet line 256).
- **The retry budget:** at most 2 repeated days per round (`RETRY_BUDGET`, 1800). It's counted as the sum over every (attempt, day) of its highest repeat (2466–2473).
  - A third repeat is `retry_budget_exhausted`. That's `not_judged`, with the disposition `out_of_retries` (2514–2515).
  - Fresh attempts the layout chain bounds (L2, L2b and L2c after a drop) are repeat 0, so they never count against the budget.
  - An S attempt's two days don't count either (plan line 474), since S is bounded on its own (below).
- **A layout change must drop an arm:** each later attempt that isn't S must be a strict subset of the chain's last attempt (1906–1921).
  - L2 and L2b follow L3. L2c may follow only when the chain's last attempt that isn't S is L3 (1917–1918; X-cr-round1-collect-fixes, #1151). So L3 → S → L2c is allowed.
- **An S attempt is allowed only** in these cases (X-cr-round1-spec-fixes):
  - (a) for a candidate whose day in the attempt just before was `solo_confirmation_required`;
  - (b) when its arm is the only arm left (plan line 469, "S if both are gone"). Every other arm of the attempt before it has dropped, at the rehearsal or by a class 2 product failure, and that attempt's `attempt_ends` reason records the drop (`rehearsal_drop` or `product_failure:<arm>`).
  - Each candidate gets at most one S attempt per round.
  - Anything else makes the candidate `not_judged` with `solo_not_permitted`.
  - Today `check_declaration` accepts any one-arm S attempt within the chain (1915–1916), so S could act as an unbudgeted retry.
- **What a repeat may replace** (plan §6.3's "the retry rule", lines 153, 364 and 444; the budget counts repeats "whatever the cause", line 474) (X-cr-round1-spec-fixes):
  - any `not_judged` day, except:
    - one with `solo_confirmation_required`, which goes to S;
    - one that is `not_judged` because of a product failure, its own or a peer's, which goes to a fresh attempt (class 2, §8).
  - Repeating either of those is `repeat_of_a_non_repeatable_day`. It replaces today's `repeat_without_infrastructure_fault` (2420–2424), which allows only an infrastructure fault.
  - Every repeat counts against the budget of 2.
  - Repeating a passing day is `repeat_of_a_judged_day` (2418–2419).
  - **A valid miss stands:** a judged `fail` in any attempt, or in a replaced day, makes the candidate `fail` (2491–2497).
- **`attempt_ends`** records an attempt that ended short (`load_manifest`, 1969–1974, 1990–1994; coordinator `end-attempt`, 912–924).
  - Its format is {attempt: {`reason`, `days_run`}}. `days_run` is exactly `[]`, `[1]` or `[1, 2]` as plain integers, and each day is listed in the manifest.
  - `[]` means the attempt never started. It's accepted only when no day of that attempt is listed. An example is `r1-L3` when the rehearsal drops C.
  - An earlier attempt's declared days must all be listed, or its end recorded. Otherwise it's `earlier_attempt_incomplete` (2391–2394).

**The drop-C rule,** frozen here (plan lines 430–432), computed by `analyze_cr.py drop-c SAMPLES --arm X=DIR …` (`drop_c`, 2585–2630; `DROP_C`, 2566) (X-cr-round1-collect-fixes, #1151):
- **Crawling:** an arm is crawling from its first scrape that is sampling (`dht_sample_suppression_seconds` = 0) and whose store grew since a scrape at least 10 min earlier (`crawling_from`, 2569–2582).
- **Interval:** the last 60 min of the rehearsal's host samples, starting no earlier than the latest arm's crawling start (2609).
- **The rule:** C is dropped, and the layout is L2, if either of these holds:
  - host CPU p95 is above 0.70 of the declared 8 vCPU;
  - steal p95 is above 0.03.
- **The percentile:** nearest rank over every 10 s slot, with a missing slot ranked over every value, as in `contention` (runner-confirmed).
- **The output:** `keep` or `drop`, with the numbers and each arm's `crawling_from_utc`. It's `not_judged` when:
  - the samples' header `vcpus` isn't exactly 8 (`vcpus_differ`);
  - there are no host samples;
  - an arm never crawled;
  - fewer than 60 crawling minutes remain (2598–2613).
- **After a `not_judged`** (runner's ruling; run sheet step 8c, lines 239–251):
  - **`vcpus_differ`:** the rehearsal fails. The box or the declaration is fixed, and the rehearsal is repeated.
  - **Any other reason:** the rehearsal keeps running, and `drop-c` runs again every 15 min (§6).
  - **Still `not_judged` at the rehearsal's end** (§6): C is dropped (L2, or S with B alone if A is dropped too). C is only diagnostic, and keeping it unjudged would risk B's measurement. The reason is recorded in the round's archive.

**Solo confirmation (S), O4:**
- **When it's required:** a contention breach outside S makes that day `not_judged` with `solo_confirmation_required`, unless the day has a class 1 or class 2 event or an unknown event (§8; analyzer:2328–2337).
  - The candidate's next attempt is then its one S attempt (case (a) above). Every later verdict stays `not_judged` until that S attempt has run without a breach of its own (2479–2482).
- **A failed solo is terminal:**
  - A breach in S, on a day with no such event, is `solo_confirmation_failed` (2337, 2483–2485), with the disposition `out_of_retries` (2514–2515).
  - It isn't a valid miss, and there's no further retry.
- **A product failure blocks shipping on that digest,** diagnosed or pending (`product_failure_on_this_digest`, 2486–2490). A fix is a new candidate on a new digest, so a new round (§2, §12).

## 6. Spacing and the rehearsal (plan §8 item 5)
**Spacing:**
- **The rule:** every arm's process start and window start is at or after the comparator's `timeline.window_end_utc` + 12 h.
  - In the analyzer: `lodestar_before_comparator_end_plus_12h` (2450–2455), beside rule 2.2's `spacing` (1728–1750; `COMPARATOR_CLEAR` = 43,200 s, line 62).
  - It's measured from `timeline.window_end_utc`, never from `result.json` `ended`.
- **The coordinator refuses an earlier start:** `check-start`, which `up` also runs, refuses before end + 12 h (`SPACING`, coordinator:63, 401–402).

**The rehearsal:**
- **When it starts:** only after X-cr-round1-spec-fixes has merged. That slice lands the 30 min warm-up and 3 h window, the gateway check (§7) and the gluetun logs (§8).
  - It also waits until the comparator's window has ended and the comparator is bound (§10). `check-start` reads `ended_utc` from the report's binding or from `comparator-binding.json` (coordinator:396–400). Rule 2.2's 12 h doesn't apply to it.
- **What it runs:**
  - Every arm of `r1-L3` runs on its day-1 exit, with leak tests before and after.
  - The collectors run `--rehearsal` with a 30 min warm-up and a 3 h window (X-cr-round1-spec-fixes, as `--warmup-minutes 30 --baseline-hours 3`, for the rehearsal only; measurement days keep rule 1.1's 60 min). Today coordinator:616–617 passes `--baseline-hours 2` and no warm-up, so rule 1.1's 60 min applies (collect.py:1787–1789). Run IDs end in `-rehearsal`.
- **How it ends (runner's ruling; plan lines 421–429; run sheet step 6, lines 202–213):**
  - **Active minutes:** an arm's run from its own `crawling_from_utc` (from `drop-c`, §5) to the rehearsal's end.
  - **`drop-c` is run first** when every arm has at least 60 active minutes. There's no optional stopping.
  - **The first `keep` or `drop` decides the layout,** and the rehearsal then stops with the coordinator's `stop` (`cmd_stop`, coordinator:866).
  - **Only after a `not_judged`** other than `vcpus_differ` is it run again, every 15 min, until the 3 h window's end or the 4 h cap from the first start, whichever comes first. With staggered starts the last arm's window ends about 3 h 35 min after the first start.
  - **If no run has given `keep` or `drop` by then,** C is dropped (§5).
  - **`vcpus_differ`** fails the rehearsal, as in §5.
  - **The rehearsal report** states each arm's active minutes, derived from its `crawling_from_utc`.
  - **An arm short of 60 active minutes at the end fails the rehearsal:**
    - C is dropped (L2), and the drop-C rule is applied to A and B.
    - A diagnostic A is dropped (L2b).
    - An authorized A is out of round 1 pending diagnosis, and B proceeds in L2b.
    - If B fails, round 1 doesn't start. B's setup is diagnosed and fixed, and the rehearsal is repeated.
    - If both C and A fail, B proceeds alone in an S attempt declared at the rehearsal.
- **Its store is deleted afterwards** (run sheet step 9, line 252).
  - Its day is never in the manifest (`list_days`, coordinator:932–933).
  - `check-start` refuses a day whose `data` or `witness` isn't empty (419–422).
- **Before day 1, it must show:**
  1. **The probe target answers,** per arm, from inside its tunnel (run sheet step 4, lines 188–194). Without it, contention coverage fails (`coverage_below_98pct:tunnel`).
  2. **The drops series' hourly deltas:**
     - `host_samples.py drops` prints the rise per hour of `uplink_rx_dropped`, `uplink_tx_dropped`, `rx_missed_errors` and `rx_fifo_errors` (run sheet step 8a, lines 222–228).
     - The output decides the open item in §7.
  3. **Distinct exits:** `bring-up` passes, with no shared hash and none equal to the uplink.
  4. **Leak tests passing,** before and after, for every arm.
  5. **The gauge showing the declared values** (run sheet step 8b, lines 229–238) (X-cr-round1-collect-fixes, #1151):
     - `segment <round>/rehearsal/<arm> --declaration` runs for each arm.
     - `declaration_entry_found` (417), every `declared_*` check, `effective_switches_recorded`, `safe_restart_on`, `preset_stayed_declared` and `switches_stayed_declared` (analyzer:651, 657) must each be present and read ok.
     - `gate-round` makes a day with any of them missing `not_judged` (`declaration_checks_missing`) (X-cr-round1-spec-fixes). Today a missing entry emits none of the others.
  6. **The drop-C verdict:** the output of `analyze_cr.py drop-c` (run sheet step 8c, lines 239–251).
- **It also records** each arm's median tunnel RTT (`host_samples.py median`, run sheet step 8, line 215), which is the network rule's base.

## 7. Contention, network included
**`CONTENTION`** (analyzer:1803–1805). A declaration may only restate it (1847–1848), and every verdict echoes it.

| Key | Value | Unit and meaning |
|---|---|---|
| `interval_seconds` | 10 | s per sample slot |
| `coverage_min` | 0.98 | share of the window's slots each series must cover |
| `cpu_p95_max` | 0.80 | busy share of the declared 8 vCPU; nearest-rank p95 |
| `steal_p95_max` | 0.05 | steal share of the declared 8 vCPU; p95 |
| `disk_write_p99_ms_max` | 50.0 | ms per completed write on `backing_device`; p99 |
| `tunnel_loss_max` | 0.01 | share of the candidate's probes lost, in each window hour |
| `tunnel_rtt_p95_over_rehearsal_max` | 2.0 | each hour's RTT p95 ÷ the arm's rehearsal median |
| `uplink_drops_max` | 0 | frames dropped per window hour, rx + tx |

**How each input is measured** (sampler). There's one line per 10 s UTC slot. A late slot is skipped, never backfilled.
- **vCPU** (X-cr-round1-collect-fixes, #1151):
  - The header records `vcpus` (host_samples.py:217).
  - `assemble` refuses a count that's missing or changed between sampler starts (315–317).
  - `contention` gives `not_judged` (`vcpus_differ`) when it isn't the declared 8 (analyzer:2029–2030).
- **CPU:** (user + nice + system + irq + softirq) ÷ all jiffies since the previous slot, from the aggregate `/proc/stat` line (`cpu_times`, 67–78; 175–181).
- **Steal:** steal ÷ all jiffies, from the same line.
- **Disk:**
  - Latency is Δms writing ÷ Δwrites completed on the declared device, from `/proc/diskstats` (`disk_times`, 81–91; 183–189).
  - `disk_inflight` is the device's I/Os in progress, reads included. The plan's line 445 says "writes in flight", so this is stricter, and accepted (runner's ruling).
- **Uplink:** `uplink_drops_total` = `rx_dropped` + `tx_dropped` from `/sys/class/net/<uplink>/statistics`, cumulative (93–102, 192–197).
- **Tunnel:** one ICMP echo (`-c 1`, 2 s timeout) to the arm's declared `tunnel_target`, sent from inside its gluetun namespace (`probe`, 159–170).
  - `tunnel_target` must be the arm's WireGuard gateway (plan line 441) (X-cr-round1-spec-fixes). `bring-up` reads the gateway from inside the namespace, refuses a target that differs, and records `tunnel_target_is_gateway: true` per arm in run state and the day archive.
  - A day whose archive lacks it, for any arm, is `not_judged` (`probe_target_unchecked`).
  - The address itself is never in the repo or the archive. Today the coordinator only checks that it's an IPv4 address (coordinator:227–228).
- **RTT base:** each arm's rehearsal median of answered RTTs, taken on its day-1 exit (`rehearsal_medians`, 262–269; plan line 450).
  - The same base is used for day 2 and for S days, on other exits.
  - The noise from that rotation is accepted for round 1, and a per-exit base is a 1.1 item (runner's ruling).

**How `contention` reads them** (analyzer:2020–2110), for the candidate's window `[window_start_utc, window_end_utc)`:
- **Slots:** every slot that starts inside the window counts. A missing slot ranks over every value (`nearest_rank`, 2007–2013), and a missing probe counts as lost.
- **Disk slots with no completed write:** excluded when nothing is in flight, counted as over when something is.
- **Coverage:** each series (CPU, steal, disk, the candidate's tunnel, uplink) must cover at least 98 % of the window's slots (plan line 444). Below that, contention is `not_judged` (`coverage_below_98pct:<series>`, 2071–2078), and the day goes to the retry rule: a repeat (§5).
  - The sampler's per-hour `coverage_by_hour` is reported only (host_samples.py:272–304). Its first and last hours are prorated to the part covered.
- **Network:** checked per window hour. A final partial slot joins hour 23 (2088–2108).
- **An uplink counter decrease** within the window makes contention `not_judged` (`uplink_counter_reset`, 2057–2059). The day is then `not_judged`, and the segment stays `valid`.
- **Device:** samples from a device other than the declared one are `not_judged` (2027–2028).
- **A breach** is handled as §5 and §8 say.

**Open item, recorded as written:** "gate-round sums rx and tx uplink drops (`uplink_drops_total`). virtio NICs can count unknown-protocol frames in rx_dropped. The rehearsal's `host_samples.py drops` output decides, before day 1, whether the drop rule reads rx+tx as now or a narrower series. If it must change, the analyzer change lands, and this file is amended, before day 1."
- **A change needs the owner.** Any narrower series narrows the plan's "no uplink drops" (line 450), so it's a loosening. It needs the owner's approval before day 1, and until then the rule reads rx + tx as written.

## 8. Event classification (plan §6.3, lines 455–472)
- **Where events are recorded:** in the day archive's `events` [{`class`, `arms`, `cause`, `diagnosis`}]. `arms: []` means every arm.
- **Classes and diagnoses:**
  - `gate-round` accepts the classes `infrastructure` and `product`. `diagnosis` is absent, `pending`, `product` or `infrastructure` (`DIAGNOSES`, 1799).
  - Anything else, including the coordinator's `unclassified`, is `unknown_event`. The day fails closed until the runner classes it (2307–2309; coordinator `classify_close`, 793–806).
  - A `product` event diagnosed `infrastructure` is reclassified (2310–2311).
  - The runner sets the diagnosis, with a fresh reviewer.
- **Classify first:** the first class that applies decides (X-cr-round1-collect-fixes, #1151).
  - A breach on a day that also has an infrastructure fault, a product failure (pending or diagnosed, the arm's own or a peer's) or an unknown event sets no `solo_confirmation_required`, nor `solo_confirmation_failed` in S.
  - The day records `contention_class` (2328–2337).
- **Handshake events** rest on the runner's evidence (runner's ruling).
  - The evidence is each arm's gluetun log for the day, archived as `<arm>/gluetun.log` with IPv4 and IPv6 addresses and WireGuard keys redacted, and listed with its SHA-256 in the manifest (X-cr-round1-spec-fixes).
  - The runner's event classification is committed to the round's archive before `gate-round` runs, and before any segment report's rates are read for acceptance.
  - A reclassification after that needs a fresh reviewer, and it's published.
  - A collector for handshakes is a 1.1 item.

| Class | Evidence | That day | The round |
|---|---|---|---|
| **1. Infrastructure** | Evidence outside the Lodestar process:<br>• host, VM, `/data` or uplink faults;<br>• a tunnel without a handshake for more than 180 s, or handshake gaps on 2 or more tunnels;<br>• a failed leak test after the start (coordinator:525–527);<br>• a collector storage guard (`HOST_REASONS`, 76) | Every affected arm's day is invalid. The candidate's day is `not_judged` with `infrastructure_fault`, or `peer_infrastructure_fault` when only a peer was hit (2312–2313) | The whole concurrent day is repeated within the attempt, from fresh stores, with the same layout and exits. There's no solo confirmation. The repeat counts against the budget, and so does a recurrence after a repair |
| **2. Product** | A panic, OOM kill, exit or failed health checks with no evidenced infrastructure fault. The collector reasons are `PRODUCT_REASONS` (coordinator:73–75), recorded as `pending` | Failing candidate: `product_failure_under_diagnosis`, then `product_failure` once diagnosed (2314–2316). Peers: `peer_product_failure` (2317–2318) | • The failing candidate's qualification is withheld on that digest, pending or diagnosed (2486–2490).<br>• `product` is terminal (`disposition`, 2512–2513); `pending` keeps the selection at `pending_retry`.<br>• A failed diagnostic arm (C, or a diagnostic A) is dropped on its first failure. The attempt ends, and the next attempt uses L2 without C, L2b without A, or S when one arm is left.<br>• A failed candidate: the other candidate continues in a fresh attempt with the remaining arms (L2b after A, L2c after B) |
| **3. Demonstrated contention** | A CPU, steal, disk or network breach with neither of the above | `not_judged` and `solo_confirmation_required`; `solo_confirmation_failed` in S | Solo confirmation. A failed solo means `out_of_retries` and the round-1 report |
| **4. None** | — | The day stands: `pass` or `fail` on the tests (2343) | A `fail` is a valid miss and ends the claim |

## 9. Acceptance and transfer (plan §8 items 3 and 4, §6.4, §6.5)
**Acceptance (rules 6, 6.1 and 6.8 as amended):**
- CR-COMB's acceptance evidence may be a declared candidate's two round-1 windows, when `gate-round` gives that candidate `crawler` = `pass` and `shipping_eligible` = true.
- When it ships, its day-1 window is CR-COMB's acceptance window and G-v06-crawler's evidence. `gate-round`'s output is the verdict of record (plan lines 505, 565).
- The selection is `select` (2525–2544):
  - B's disposition must be terminal before A is read.
  - A diagnostic arm is never selected.
  - If both candidates are shipping-eligible, B ships.

**Transfer: defaults only** (plan §6.5, lines 507–513):
1. The CR-COMB defaults change is a commit on the qualified image's revision.
2. Both digests are recorded.
3. The diff only promotes the tested settings to defaults: `config.rs` defaults, their tests and docs.
   - The runner checks it, and a fresh reviewer confirms.
   - Any behaviour change, a dependency update included, needs fresh qualification.
4. A fresh start of the shipping image, with no environment and no config file, must report the qualified preset, all 12 switch values and `safe_restart`, through the settings API and `lodestar_dht_switch`.
- The order is: qualify, build and verify, then merge the defaults change.
- Digests Q, D and R, and the remeasurement at R, follow the plan's items 5–7.

**Memory:**
- **The fit:** `memory_window` (794–799) calls `analyze_cr0.measured_window_fit` (analyze_cr0.py:656–677) over measured-window hours 12–24, clocked from `t_s` (rule 1.4). It fits the `anon` series, which Docker mode records as `private_bytes`.
- **The bar:**
  - `meets_bar` is set only for a complete window: no gap over 180 s, and at least 30 samples (`MEMORY_FIT`, analyze_cr0.py:50–53).
  - It's true when the upper 95 % bound (slope + 1.96 SE) is at most 2.0 MiB/h (650).
- **An incomplete or refused fit,** or missing `anon`, leaves `meets_bar` unset, which is `not_judged` (`memory_verdict`, 2123–2132).
- **The candidate's memory** is `pass` only when both days' `meets_bar` is true (2504–2505).

**The pass rule per candidate, fixed before day 1** (plan §6.4, lines 481–505). §6.4 governs where the plan's §3.4 differs; §3.4 describes reporting (runner's ruling):

| Plan clause | Analyzer field or reason |
|---|---|
| Window mean ≥ 100/min, net of the carry-in bound | `tests.mean_passes` (`gate`, 1762–1764; `GATE_MEAN`, 64) |
| Every one of the 24 net hours ≥ 70/min | `tests.every_hour_passes` (1765: exactly 24 hours, each ≥ `GATE_HOUR`) |
| p95 ≤ 600 s, coverage ≥ 98 %, dropped samples charged. "Not measured" fails, as a valid miss | `tests.p95` = `pass`, `fail` or `not_measured` (1766), from `latency` (343–357: `COVERAGE_MIN` 0.98; Δbucket{le="600"} ≥ 0.95 × (Δcount + Δdropped)) |
| All three pass on a day | day `status` = `pass` (2343). Otherwise `fail`, a valid miss (`valid_misses`, 2491–2492) |
| A valid, non-rehearsal, 24 h comb window | `not_a_24h_comb_window`, `segment_<verdict>` (2242–2245) |
| Rules 6.5–6.6 | `segment_invalid` |
| The declared settings; §6.1's evidence; query rate and outstanding within the preset | `segment_void` (§2, §3) |
| Egress and leak tests | `egress_*`, `leak_test_not_passed` |
| Bound to the declared round, day, candidate, exit, host and spec commit, with distinct runs | `report_not_bound_to_its_declaration`, `spec_commit_differs`, `host_identity_differs`, `network_identity_differs`, `image_digest_differs`, `no_fresh_store_evidence`, `duplicate_run`, `run_id_reused` |
| Fresh and separate | `windows_overlap_or_out_of_order`; `fresh_store_at_start` (701–703, 562–563) |
| The layout held, and peers were active through the candidate's window | `layout_*`, `roster_differs_from_declaration`, `peer_report_missing`, `peer_not_active_through_candidate_window` |
| Two different exits | `repeated_exit` (2441–2444) |
| Contention `ok` on each day | `contention_not_judged`, `contention_breach` |
| Spacing | `spacing` (2447–2458) |
| Only an authorized candidate qualifies | `not_authorized` (2376–2377) |
| Memory ≤ 2 MiB/h on both days, as an upper 95 % bound | `memory` = `pass`; `shipping_eligible` = crawler `pass` and memory `pass` (2506) |

**The per-IP guards and the 64-query cap** (plan line 494) aren't checked by the analyzer. They rest on the pinned digest (runner's ruling).

**Precedence** (2494–2501):
1. `UNJUDGEABLE` reasons (2358–2359) or a comparator problem give `not_judged`.
2. Otherwise, a valid miss gives `fail`.
3. Otherwise, any other reason gives `not_judged`.
4. Two passing days give `pass`.

## 10. Comparator and network identity (plan §8 item 10)
**Network identity:**
- `uplink_hash`: the salted hash of the uplink's public IPv4, read outside any tunnel at each collector's launch (collect.py:2051–2058);
- `provider` = `pia` and `protocol` = `wireguard`: the collector's defaults, which the coordinator never overrides (`collector_argv`, coordinator:607–622).
  - `bind-comparator` reads them from the coordinator file's `vpn_provider` and `vpn_protocol`, with the same defaults (1078–1079). Any other value there gives a binding that differs from the declaration, so the rule fails safe.
- The declaration must name exactly these values. Every report's binding must equal them (`network_identity_differs`, 2258–2259), and so must the comparator's (`comparator_network_identity`, 2154–2155).

**Changes:**
- **Rotation between declared exits isn't a network change.** Egress hashes aren't part of the network identity.
- **Any other change needs a new comparator** before the next acceptance window: a different uplink hash, provider or protocol. That includes an ISP changing the public address. The rule stays strict (runner's ruling).
- **The hash is read at each collector's launch,** not hourly. An hourly check is a 1.1 item.

**The comparator binding, after the window** (runner's ruling, flagged to the owner under §2.19) (`bind-comparator`, coordinator:1025–1083; X-cr-round1-collect-fixes, #1151):
- **Why:** cr-ab-comp-2 began at 2026-11-27T17:00Z under the old collector, which writes no binding. Re-running it would cost about two days.
- **The step:** once the comparator has finished, `bind-comparator --comparator-run DIR` writes `comparator-binding.json` into the round's folder, once (run sheet step 5a, lines 137–144). It holds:
  - `run_id` = `comparator-<run directory name>`, which the declaration's `comparator.run_id` must name;
  - the host and network identity, computed at bind time with the round's salt, as the arms compute theirs;
  - `ended_utc`, from the comparator's `result.json` `ended`;
  - `bound_utc`, and `bound_after_window: true`.
- **What it refuses:**
  - a run that isn't a comparator, or hasn't finished;
  - one that ended before its report's `timeline.window_end_utc` (1049);
  - a second binding;
  - a run directory that isn't the one the round's `comparator.json` came from. It re-segments the directory (`analyze_segment`) and requires the same `kind`, `timeline`, `window_hours` and `identity.binary_sha256` (1055–1064).
- **How it's used:**
  - The manifest carries it (coordinator:992).
  - `load_manifest` gives it to a comparator report with no binding of its own, and a report's own binding wins (analyzer:1953–1967). `comparator_problems` then reads it unchanged.
  - The verdict echoes `comparator_bound_after_window` (2557).
- **The cost, recorded as a refinement of plan §5.2 item 10 (lines 295–302):**
  - Identity is checked once, at bind time, not hourly through the comparator's window.
  - A public-address change during that window is still caught where rule 6.11's `public_ip_change` note is recorded, since that note invalidates the comparator's segment.

**The comparator itself must be** (`comparator_problems`, 2135–2159):
- the booked run ID;
- `kind` = `comparator`;
- `valid`, not a rehearsal, with a 24 h window;
- on the declared host and network identities;
- uninterrupted, with `ended_utc` not before `timeline.window_end_utc`.
- Anything else is `not_judged`, and the box slot isn't treated as open.

## 11. Host safety (plan §8 item 6, D3)
X-cr-round1-collect-fixes, #1151 (analyzer:518–539; collect.py `note`, 1844–1881).
- **The note:** an array operation is recorded with `collect.py note --category array_operation --kind mover|parity|other --pool P …`. That gives {`kind`, `pools`: [..]}, where `pools` lists every pool the operation touches; a mover run names both the cache pool and the array.
  - A round-1 run's `array_operation` note must carry `--kind` and `--pool` (collect.py:1870–1872).
  - The run sheet's line 323 says how.
- **The map:** the declaration maps each pool to the devices whose storage sits on it, named as the VM's kernel sees them (the names `identity` records, such as `vdb`), not the Unraid host's.
  - The operator reports the map (run sheet step 5, line 136), and the records PR writes it into the declaration.
  - Pool names must match `[a-z0-9_]{1,32}`, the form `note --pool` accepts (collect.py:1861). `check_declaration` doesn't enforce this yet. A pool named otherwise can never match a note, so it can never spare a window: the rule fails safe.
  - `check_declaration` refuses a declaration with a non-empty map whose `backing_device` is in no pool's list, or in more than one (analyzer:1853–1856).
- **Exempt:** a `mover` or `parity` entry, only when the arm's `backing_device` is in the map and none of the entry's `pools` holds it (`spares`, 526–531). A pool missing from the map counts as holding it. Exempt entries are listed in the report's `activity_exempt_d3`.
  - With the arm on `ssd_apps`, a parity check of the array is exempt; a mover run from `ssd_apps` is not.
- **Still invalidating (rule 6.11):**
  - every `other` array operation, an array rebuild included;
  - any array operation whose `pools` include the pool that holds the arm's device;
  - every other rule 6.11 entry: `import`, `other_crawler`, `host_reboot`, `plugin_container_action`, `link_outage`, `router_outage`, `public_ip_change` and `power_event` (`ACTIVITY_CATEGORIES`, collect.py:1207–1209).
- **Contention still applies.** Any I/O a mover run puts on the backing device shows in the disk p99 (§7).

## 12. The pinned image digest
**Which build:**
- The `Release` workflow (`.github/workflows/release.yml`) builds and pushes `ghcr.io/dvortsis/lodestar-v2:nightly` from `main` (lines 321–322, 359).
  - It runs on the daily schedule (`17 3 * * *`, 03:17Z, line 21) or on a `workflow_dispatch` on `main`.
  - The image is a multi-arch index (amd64 and arm64, line 366).
  - Its OCI label `org.opencontainers.image.revision` is the run's commit (release.yml:372, `packaging/docker/Dockerfile`:111).
- **The round's image** is one such build whose revision is a `main` commit at or after `spec_commit`. It therefore holds CR-0w (#1134), X-dht-switch-identity (#1146), the harness (#1147, #1150) and X-cr-round1-collect-fixes (#1151).
- **A locally built image can't be used.** It has no repo digest for the collector to record (collect.py:725–726).

**How the digest is recorded, before the rehearsal:**
1. The operator pulls the `nightly` tag once on the box. They check that the revision label is the chosen commit, and report `RepoDigests[0]` (runner-confirmed; run sheet step 1, lines 70–80). The tag moves daily, so the label check is what ties the digest to the commit.
2. That value, in the form `ghcr.io/dvortsis/lodestar-v2@sha256:<64 hex>`, goes into the live declaration's `image_digest`, every arm entry's `image_digest`, and every attempt's `candidate_digests` for B and A (§2's records PR).
3. From then on, the box runs by digest only. The coordinator file's `image` is the same value, and `commit` is the revision (run sheet lines 80, 104).

**Enforcement** (X-cr-round1-collect-fixes, #1151, item K):
- **The form:** `check_declaration` requires `name@sha256:<64 hex>` for `image_digest` and every entry's (`IMAGE_DIGEST`, analyzer:1809; 1832–1833, 1884–1885). It refuses any entry or `candidate_digests` value that differs from `image_digest` (1899–1905).
- **Compared on the digest part:** the collector records the digest without the repository (collect.py:725–726).
  - So reports are compared on the part after the last `@`: `declared_image_digest` (698), `gate-round` (`digest_part`, 1812–1813; `image_digest_differs`, 2262–2265) and the coordinator's `manifest` (971–972).
  - `candidate_digests` are compared in full.
- **`up`** refuses a coordinator `image` that isn't exactly the declaration's `image_digest` (coordinator:549–552). `manifest` also refuses candidate digests that aren't the round's image (952–957).

**A new digest means a new round,** declared before its first run (§2): a fix after a product failure, a memory fix (B′) or any rebuild. Days under different declarations never pair, and B′ starts its two-day count again (plan §6.2).

**Naming the spec:**
- `spec_commit` is the merge commit of this file's PR on `main`. The collector writes it (coordinator:611), and `gate-round` checks it (`spec_commit_differs`, 2254–2255).
- Round-1 segment reports (`round1.round1_spec`, 415–416) and the verdict (`round1_spec`, 2552) name `docs/decisions/2026-11-27-CR-SPEC-round1.md` (`ROUND1_SPEC`, analyzer:45) (X-cr-round1-collect-fixes, #1151). `spec` stays CR-SPEC (`SPEC`, 44).

## 13. Unchanged (plan §8 item 7)
- **Rule 1.1:** `ws = T_open + 60 min` (`WARMUP`, 48; analyze_segment 446–452). With `safe_restart` on, CR-0w opens `T_open` within seconds of the first start.
- **Rule 2.2's spacing** (§6), from `timeline.window_end_utc`.
- **Fresh stores** every day, the rehearsal included, with new data, state, witness and node identities. The coordinator (419–422) and the analyzer (`fresh_store_at_start`) both check this.
- **No stitching** (rule 6.7): one process per window (`one_process_identity`, 503–504), and no restart inside a window.
- **The carry-in bound** (rule 3.2): `carry_in_bound` (323–331), summed per hour and per window (833–839), with `MARGIN` 2 s and `DROPPED_CARRY` 7,200 s.
- **The latency cohort and coverage rules** (rules 3.3, 4.1–4.5): carry-in samples are kept, dropped samples are charged above 600 s, and coverage must be ≥ 0.98.

## 14. Mismatches for the runner
The draft listed 25. The runner ruled on all of them on 2026-11-27. Numbers are the draft's.

**Fixed in code** (locations are `main` at e174bcf7):
1. **The comparator had no binding:** a pre-round-1 collector writes none, and `gate-round` and the rehearsal gate needed one. → fixed in #1151: `bind-comparator` (coordinator:1025–1083), read by `load_manifest` (analyzer:1953–1967) and `check-start` (coordinator:396–400). §10.
2. **D3's exemption:** every `array_operation` invalidated, against plan line 568 and owner-decisions.md:828. → fixed in #1151: `spares` (analyzer:518–539), `note --kind --pool` (collect.py:1844–1881), the pool map (analyzer:1849–1856). §11.
3. **An exit re-check after a repair voided the day.** → fixed in #1151: `exit_in_force` and `egress_problems` (analyzer:2185–2225), and the check's stamp (coordinator:597–598). §4.
4. **The run sheet's leak-test wording.** → fixed in #1151: run sheet lines 59 and 319. §4.
5. **The classification order:** any breach set `solo_confirmation_required`, against plan lines 455 and 464. → fixed in #1151: `judge_day` (analyzer:2328–2337). §8.
7. **`lookup_end`'s end reason:** only `values` was read, and the collector dropped `enough_values` (metrics.md:809; `crates/dht/src/node/ledger.rs`:52). → fixed in #1151: collect.py:164–167; analyzer:768–772, 951–955. §3.
10. **The drop-C rule:** no tool computed it (plan lines 430–432). → fixed in #1151: `drop_c` (analyzer:2566–2630), run sheet step 8c (lines 239–251). §5.
12. **The rehearsal's gauge check:** no `segment --declaration` step. → fixed in #1151: run sheet step 8b (lines 229–238). §6.
14. **No layout for A with C** (plan line 470). → fixed in #1151: L2c (analyzer:1794, 1917–1918). §5.
19. **The vCPU count** was never checked (plan lines 432, 447). → fixed in #1151: `vcpus` (analyzer:1858, 2029–2030, 2598–2599; host_samples.py:315–317). §7.
23. **The digest's form and source:** any non-empty string passed, and `release.yml` records no digest (lines 361–372). → fixed in #1151: `IMAGE_DIGEST` and `digest_part` (analyzer:1809–1813), run sheet step 1 (lines 70–80). §12.
24. **Reports didn't name this file.** → fixed in #1151: `ROUND1_SPEC` (analyzer:45, 415–416, 2552). §12.

**Resolved by the runner** (no code change):
- **6.** §6.4 governs: "not measured" fails, as a valid miss. §3.4's line 153 describes reporting.
- **8.** Accepted. An absent CR-ADM or uTP series is `missing`, published as `segment_invalid`. Either way the result is `not_judged`.
- **9.** Accepted. The stricter rule 5.1–5.2 checks stand.
- **11.** Each arm needs ≥ 60 active minutes. The first `keep` or `drop` from `drop-c` decides and ends the rehearsal. After a `not_judged`, `drop-c` reruns every 15 min until the 3 h window's end or the 4 h cap from the first start. The report states each arm's active minutes (§6).
- **13.** The declaration is append-only (§2).
- **15.** L2b is permitted (plan §8 item 11; item 2's list is incomplete). A diagnostic arm is dropped on its first product failure (§8).
- **16.** Coverage is ≥ 98 % over the window's slots. Per-hour coverage is reported only (§7).
- **17.** An uplink counter decrease makes contention, and so the day, `not_judged`. The segment stays `valid` (§7).
- **18.** Handshake events rest on the runner's evidence from gluetun's logs, archived per day (X-cr-round1-spec-fixes). A collector for them is a 1.1 item (§8).
- **20.** `disk_inflight` counts reads too. That's accepted as stricter than the plan's wording (§7).
- **21.** The RTT base is each arm's rehearsal median on its day-1 exit, also used for day 2 and S. The rotation noise is a 1.1 item (§7).
- **22.** The per-IP guards and the 64-query cap rest on the pinned digest (§9).
- **25.** Kept strict: a changed uplink hash needs a new comparator. The launch-only read is a 1.1 item (§10).

**1.1 items from the check of #1151:**
- check_declaration should hold pool names to the note pattern (§11).
