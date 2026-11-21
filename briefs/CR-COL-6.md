# CR-COL-6

The collector and analyzer for stage 3 (CR-AB-1). Follows up CR-COL-5. Opus, core evidence tooling. The runner's slice (2026-11-21), from CR-COL-5's own follow-up list.

## Spec
- docs/decisions/2026-11-20-CR-COL-5.md, "Follow-ups": stage 3's treatments need a `TREATMENT_SWITCHES` and a `STACKED` entry each, plus their own mechanism checks. Their switches are already recorded (`hint_first`, `sampler_cooldown_split` in `SWITCH_ENV`), and the stacking checks need no change.
- docs/decisions/2026-11-20-path-to-100-cr-spec.md, item 3: CR-HINT-FIRST, then X-sampler-cooldown-split, each stacked on the winners so far. The sampler split is also reported on sampler-full time and on its segments' worst hours. That's rule 6.4: reported, never decided on.
- docs/decisions/2026-11-02-CR-SPEC.md rules 5, 6.1, 6.2, 7 and 8, unchanged.
- The series:
  - CR-HINT-FIRST (#1003, names approved by the runner): `dht_hint_first_marked_total`, `dht_hint_first_skipped_total{reason=responder|full|rejected}`, `fetch_hint_keys_total`, `fetch_hint_ignored_total`, `fetch_hint_t1_waits_total`, `fetch_hint_t1_left_total{reason=full|aged}`, `fetch_hint_lookups_total`, the gauge `fetch_hint_t1_waiting`, and `fetch_admission_grants_total{class="hinted"}`. Take the definitions from #1003's docs/metrics.md section, "Hint-first (CR-HINT-FIRST, 2026-11-21)". If #1003 hasn't merged yet, read them from its branch.
  - X-sampler-cooldown-split (merged): `dht_sample_full_seconds_total` and `dht_sample_spill_slots` (docs/metrics.md). Its decisions file is docs/decisions/2026-11-20-X-sampler-cooldown-split.md.

## Do
1. **Treatments.** Add `--treatment hint` (switch `hint_first`) and `--treatment split` (switch `sampler_cooldown_split`) to `TREATMENT_SWITCHES` and `STACKED`, so both need `--winners`. Their own switch must never be among the winners; the existing check covers that.
2. **Mechanism checks, treatment arm only.** A failure voids the segment, as A1's check does under rule 5.
   - **hint:** over the window, the deltas of `dht_hint_first_marked_total` and `fetch_hint_lookups_total` are both above 0, and `fetch_hint_ignored_total` doesn't exceed `fetch_hint_keys_total`. If the latter happens, the hint marks are coming from somewhere unexpected, so report it.
     - Stage 3's control must have CR-ADM on: #1003's screen shows T1 does little without it. If the named winners don't include coordinated admission, the effect report carries a plain warning. It never changes the verdict; this is the runner's call.
   - **split:** the treatment arm shows `dht_sample_spill_slots` above 0 at some scrape after the table first fills. If the table never filled in the window, report "not exercised"; that isn't a failure.
3. **Rule 6.4 report for split.** In both arms, report the sampler-full time (the delta of `dht_sample_full_seconds_total`) and the segment's worst hours of stored per hour. Report only; it never changes a verdict.
4. **Allowlist.** Add the new series to the collector snapshot's allowlist with fixed label enums, as CR-COL-5 did for the ledger. An unknown label value is dropped, never a crash.
5. **Missing evidence.** The hint series are required in both arms of a hint segment, and the split series in both arms of a split segment, under rule 6.10. Every other treatment lists them under `absent` and never crashes.

## Tests and gates
- Passing and failing fixtures for each new check, including:
  - hint with no marks (void);
  - hint with marks but no lookups (void);
  - a hint control without CR-ADM (the warning, with the verdict unchanged);
  - split that is "not exercised";
  - split with spill slots;
  - missing series in each arm.
- The stage-1 golden (`fixtures/cr_col5_stage1_golden.json`) and the stage-2 rehearsal are unchanged.
- A short synthetic stage-3 rehearsal: two blocks, ABBA then BAAB, with the CR-ADM and lc winners named.
- Run the existing tools' test suite (see docs/soak/tools) plus the status-connector test.

## Owns
- docs/soak/tools/**
- docs/decisions/<date>-CR-COL-6.md
- your own slices.yaml entry and dashboard.yaml placement line

No running slice touches these. Never edit crates/**.
