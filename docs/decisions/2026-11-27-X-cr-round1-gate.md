# X-cr-round1-gate decisions

Round 1's judging side (`docs/plan/2026-11-27-revised-path-to-100.md` revision 6, §5.2 items 2, 5, 6, 9, 10 and 12, §6.1–§6.4, §8; owner O1–O4, 2026-11-27). The collect slice (X-cr-round1-collect) writes the files; this slice reads them. `gate` and `effect` are unchanged for historical runs (§8 item 8).

## Commands
- **`analyze_cr.py segment RUN --declaration D`** judges one round-1 comb arm. With `--declaration`:
  - **Rule 5.5 becomes the declaration** (§8 item 1): `balanced_preset` and `preset_stayed_balanced` are replaced by `declared_preset` and `preset_stayed_declared`. `declared_switches`, `declared_image_digest` and `safe_restart_on` are checked at start, and `switches_stayed_declared` wherever a boundary read carries the switches. Each is `void` on a mismatch. `effective_switches_recorded` is `missing` (invalid) without the §5.1 gauge's 13 values.
  - **The arm is found by `run.json` `round1`** (`round`, `attempt`, `day`, `arm`). No matching entry is `invalid` (`declaration_entry_found`).
  - **Fresh store** (`fresh_store_at_start`, invalid): the first scrape's `torrents` value is 0.
  - **Bundle evidence and caps** (below), and `memory`, the measured-window fit.
  - Only a comb arm takes a declaration; anything else is an error.
- **`analyze_cr.py gate-round MANIFEST`** gives every arm's verdict and the selection from one manifest (§5.2 item 12). Output fields: `candidates` and `diagnostics` (per arm: `crawler`, `memory`, `shipping_eligible`, `reasons`, `days[]` with `status`, `reasons`, `tests`, `memory`, `contention`, `solo_confirmation_required`), `selection` (`ships`, `state` = `ships` | `pending_retry` | `round1_report`, `waiting_on`, `trail`), `inputs` (every path's SHA-256) and `manifest_sha256`. It's deterministic; a test runs it twice and compares bytes.

## The declaration (JSON; the runner writes the real one in the CR-SPEC decisions file)

| Field | Meaning |
|---|---|
| `schema` = 1, `kind` = `cr_round1_declaration`, `round` | |
| `spec_commit`, `image_digest` | every report must carry these |
| `host_identity`, `network_identity` {`uplink_hash`, `provider`, `protocol`} | salted hashes (hex), never addresses; §8 item 10 |
| `comparator` {`run_id`, optional `host_identity`, `network_identity`} | the booked comparator; identities default to the round's |
| `selection_order` | default `["B", "A"]` (Balanced first, §6.2) |
| `contention` | optional; if present it must equal the frozen thresholds below |
| `attempts[]` {`id`, `layout` (L3, L2, L2b, S), `candidate_digests` {arm: digest}} | §6.3 |
| `arms[]` {`round`, `attempt`, `day` (1 or 2), `arm`, `role` (candidate or diagnostic), `preset`, `switches` (exactly the 12, booleans), `exit` (X, Y or Z), `image_digest`} | one entry per arm per day per attempt; an arm has one role for the round |

A malformed declaration is an error (exit 2), not a verdict: it's frozen before the rehearsal, so it's fixed, not judged.

## Draft collector schema (the interface with X-cr-round1-collect; replace with theirs when it arrives)
- **`run.json`:** `run_id`; `round1` {`round`, `attempt`, `day`, `arm`, optional `repeat`}; `host_identity`; `network_identity`; `config.effective_switches` (the §5.1 gauge's 13 values at start). Boundary rows may carry `api.switches`. The first scrape's `values.torrents`. `result.json` `ended` is carried into the report's `binding.ended_utc`.
- **Day archive** (one per day, from the coordinator): `exits` {label: hash}, written when the stacks come up; `arms` {arm: {`egress`: [{`utc`, `hash`}], `active_from_utc`, `active_through_utc`, `leak_test` {`before`, `after`}}}; `events` [{`class`: `infrastructure` | `product`, `arms`: [...] (empty = all)}].
- **Host samples** (one per day): `interval_seconds` = 10, `rehearsal_rtt_median_ms` {arm: ms}, `samples` [{`utc`, `cpu` (busy share of all 8 vCPU, 0–1), `steal` (0–1), `disk_write_ms` (null with no completed write), `disk_inflight`, `uplink_drops_total` (cumulative), `tunnels` {arm: {`rtt_ms`, `lost`}}}].
- **Manifest:** `schema` 1, `kind` `cr_round1_manifest`, `declaration`, `comparator`, `days[]` {`attempt`, `day`, `repeat`, `reports` {arm: ref}, `archive`, `contention`}; each ref is {`path` (relative, inside the archive), `sha256`}.

## §6.1's evidence table, as coded (the runner freezes it; step-0 note sent 2026-11-27)
- **Existing checks, run whenever the switch is on, whatever the run kind:** coordinated admission and uTP (rules 5.1–5.2, `mechanism`); `lookup_end` runs the lookup-cost pair (`lookup_cost_pair`: ended on values, slots above 32 once, query rate); `hint_first` (`hint_first`); `sampler_cooldown_split` (`sampler_split`; a table that never filled is reported `not_exercised`; its series absent is void); `soft_timeout` (the counter rendered; 0 is a result).
- **New rows, one series above 0 (absent or zero voids):** `responder_hints` → `dht_hints_captured_total`; `streaming_peers` → `fetch_stream_batches_total` (the nearest to "streamed peer events": batches the pool read); `target_prefixes` → `dht_target_prefix_queries_total` (all kinds); `sample_backoff_escalate` → the gauge `dht_sample_cooling_reason{reason="first_failure"}`, its maximum over the window's scrapes (no counter exists).
- **`preset_slots`:** `dht_lookup_slots_capacity` equals the declared preset's count (32, 80, 256) at every window scrape.
- **Caps, in every arm:** an interval over the declared preset's query rate (r5.1 A1's bound) or a scrape over its outstanding cap (`dht_outstanding` against 256, 1024, 4096) voids the window.
- **`route_backoff`** is declared off (O3): rule 5.3's idle-when-off check is unchanged.

## `gate-round`'s rules, as coded
- **Per candidate, from its latest attempt** (days pair only within one attempt, §6.3). A repeated day (`repeat` > 0) replaces the day it repeats. More than two repeated days in the round (solo confirmations' days not counted) is `retry_budget_exhausted`.
- **Candidate-level, any problem is `not_judged`:** not authorized (diagnostic), the attempt doesn't declare days 1 and 2, a day missing, a comparator problem (missing, unbooked, not `kind=comparator`, not `valid`, not a 24 h non-rehearsal window, another host or network identity, or `ended` before `timeline.window_end_utc`), duplicate run IDs, overlapping windows, a repeated exit (the same label or the same hash on both days), rule 2.2 spacing against every Lodestar report in the round (from the comparator's `timeline.window_end_utc`).
- **Day-level, any problem makes that day `not_judged`:** the layout isn't the attempt's (exits exactly as §6.3's table; S is one arm on two different exits), the report isn't a valid 24 h comb window or isn't bound to its declaration (placement, spec commit, host and network identity, run ID, image digest, fresh store), the roster differs from the declaration, a peer's report is missing, an arm wasn't active from the candidate's `ws` through its final boundary, a leak test didn't pass, an egress reading is missing at a boundary (± 90 s) or differs from the arm's exit, two exits share a hash, an infrastructure fault or a product failure, or contention isn't `ok`.
- **Verdict:** any candidate-level problem → `not_judged`; else a judged day that misses → `fail` (a valid miss ends the claim, §6.4); both days judged and passing → `pass`; otherwise `not_judged`.
- **Memory:** `pass` only with both days' `meets_bar` true; `fail` if either is false; otherwise `not_judged`. Shipping eligibility is crawler `pass` and memory `pass`.
- **Selection (§6.2):** in `selection_order`, a diagnostic arm only selects; the first authorized candidate whose disposition isn't terminal stops the selection at `pending_retry`; the first shipping-eligible one ships; otherwise `round1_report`. Terminal dispositions: shipping-eligible, crawler-accepted with memory failing, a valid miss, a product failure, out of retries (budget, or a failed solo confirmation).

## Contention (§6.3), thresholds as proposed, frozen by the runner
- CPU p95 ≤ 0.80 of 8 vCPU, steal p95 ≤ 0.05, `/data` write latency p99 ≤ 50 ms; per window hour, the candidate's tunnel loss ≤ 1 %, RTT p95 ≤ 2 × its rehearsal median, no uplink drops. Nearest-rank over every 10 s interval of `[ws, window end)`.
- **Coverage:** each series (CPU, steal, disk, the candidate's tunnel, uplink) below 98 % of intervals is `not_judged`. A missing interval ranks over every value. A disk interval with no completed write is excluded with nothing in flight and counts over with writes in flight.
- **A breach** makes the day `not_judged` with `solo_confirmation_required`; in layout S it's `solo_confirmation_failed` instead (terminal).

## Readings of open rules, for the runner (not decided as owner questions)
1. **Uplink drops are a cumulative counter, read per hour as a delta.** §6.3 says a missing interval counts over the threshold; for a "no drops" rule that would make any lost sample a breach. A counter can't hide drops in a gap, so the delta is used.
2. **A missing tunnel probe counts as lost** (and its RTT ranks over), per "a missing interval counts as over the threshold".
3. **Peers must be active from each candidate's `ws`,** literally (§5.2 item 11). Arms that start minutes apart pass, since `ws` is `T_open + 60 min`.
4. **`lookup_end` on runs the whole lookup-cost pair,** including slots above 32 once. With `preset_slots` off that can't pass; every declared arm has the two together.
5. **A candidate in several attempts is judged from its latest one;** earlier attempts are listed in `earlier_attempts`.
6. **A failed solo confirmation** is reported as `not_judged` with `solo_confirmation_failed` and a terminal `out_of_retries` disposition (§6.3: "recorded as failed and the round-1 report follows"), not as `fail`: it isn't a valid miss.
7. **The collector's own bound check** (`collect.py`, about line 2016) treats `dht_outstanding` > 1024 as a bad bound. On an Aggressive arm (cap 4096) that would trip; it's the collect slice's file.
8. **`dht_hints_captured_total`, `fetch_stream_batches_total`, `dht_target_prefix_queries_total{kind}` and `dht_sample_cooling_reason{reason}`** aren't in the collector's allowlist today, so a real snapshot drops them and the rows void. The collect slice adds them.
