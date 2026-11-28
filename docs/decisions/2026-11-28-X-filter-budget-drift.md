# X-filter-budget-drift decisions (#PR)

Runner brief X-filter-budget-drift, 2026-11-28Z. The `filter budget (§20.8)` job failed on #1167's head, which touches nothing in `crates/filter`, and the job is inside the required `ci-ok`. This is a measurement change the runner decides under `docs/owner-decisions.md` §2.26, a refinement under §2.19 that the runner flags to the owner.

**Unchanged:**
- the owner's 50 µs per torrent at 10,000 rules (§1.6.6, architecture §20.8);
- every case, held or reported, the corpus and the rule counts;
- the calibration workload, `ENGINE_SPEED`, `CALIB_*`, `REFERENCE_RATIO`;
- the 15% drift and spread checks, the 0.4–2.0× range and the busy-attempt retry;
- the CI job's invocation.

This supersedes one rule in `2026-11-21-X-filter-budget-remeasure.md`: "the second measurement is scaled by the faster of the attempt's calibration and the fastest calibration beside it". The rule now covers only a re-measure whose host is no slower than in the attempt. The new rules below cover the rest.

## Diagnosis
- **The job ran two benches at once.** The filter `budget` matches two ignored tests, `budget` and `budget_worst_case`. The log says "running 2 tests", and their lines interleave. So libtest timed the budget's cases while the worst-case test ran its largest inputs on the same 4-vCPU runner: "invisible" took 73 ms per judgment, and "tiny files" has 130,048 files.
- **#1167's job (Xeon 6973P-C, unlisted, so scaled by the calibration):**
  - **The attempt:** the calibration read 7.07 µs before and after. A block of cases inside it ran about 1.48× slow: the pending 1% at commit took 36.6 µs against 24.3 for the same case without pending versions, the pending 10% took 37.2 against 25.1, and the pending mixed set 46.2 against 31.1. Each case in the block was steady (spread ≤ 1%). The block lines up with the worst-case test's "invisible" and "combining" lines. The case after it ran at normal speed (31.9).
  - **The re-measure:** the worst-case test was still running; it finished 0.9 s after `budget` failed. Every calibration beside a re-measure read 10.56–12.47 µs, about 1.5× the attempt's, and every re-measured case read about 1.5× slow too.
  - **The verdict:** a12a8e57 scaled each re-measure by `min(beside, attempt)` = 7.07 µs, so a host that stays uniformly slow through the re-measure can't pass. The four cases read 54.3, 55.2, 56.0 and 70.6 µs and failed.
- **Replayed with the calibration beside each re-measure,** the same passes read 36.3, 36.9, 37.1 and 47.3 µs. The attempt's quiet-host neighbours read 35.9, 37.1, 37.5 and 45.9 µs.

## The change
- **The benches never run side by side** (`BENCH`, `bench_alone`).
  - `budget`, `budget_worst_case` and `profile` each hold one static lock for their whole run. A bench that panics doesn't block the others, because a poisoned lock is taken all the same.
  - A normal test, `the_benches_run_alone`, reads the source and fails if any ignored test doesn't take the lock as its first line.
  - The job's wall time grows by the worst-case test's run, about 7 s. Its invocation is unchanged.
- **A re-measure is scaled by the host's own slowdown when that slowdown is uniform** (`remeasured`).
  - **Steady:** the fastest calibration passes before and after the re-measure agree within 15%, the attempt's own rule for its before and after. If they also show the host at least 5% slower than in the attempt (`MIN_SLOWED`), by `slowed` = the faster of the two ÷ the attempt's calibration, the case's divisor is multiplied by `slowed`, and the log prints a `::warning::` naming the factor.
  - **Why a 5% floor:** a local run on a steady build VM saw the calibrations beside each re-measure read 1–4% over the attempt's. Scaling by that would let calibration noise loosen the gate. Below 5% the re-measure keeps the attempt's scale, so it reads up to 5% stricter than quiet, never looser.
  - **No looser on a quiet host:** a host less than 5% slower than in the attempt keeps a12a8e57's stricter scale, so the property `re_measuring_never_reads_more_leniently` still holds there. A slower engine slows the case and not the calibration, so it still reads over. A 10% regression over budget fails at any slowdown.
  - **The reference model is covered too:** `slowed` is relative to the attempt's calibration, so the 7763, which reads raw, gets the same correction. Its attempt still reads raw.
- **A re-measure whose calibrations disagree is void.** Disagreeing means more than 15% apart (#1167's first case: 7.07 µs before, 10.56 µs after), or showing the host more than 2× slower than in the attempt (`MAX_SLOWED`).
  - A void re-measure is measured once more, with a `::warning::`.
  - A second void fails the case, with both reasons.
- **Why not interleave calibrations with every case** (the brief's other candidate):
  - It would put a calibration beside filter state in every case. The 2026-11-21 file's reasons against that still hold.
  - It would cost seven calibration passes per case on every run.
  - The re-measure already sits between two calibrations, at the one point where a verdict turns on them.
- **What noise can still do:** loosen a re-measure only by slowing all seven passes of the calibration on both sides alike while leaving the case's third-fastest pass alone, and only for a case already over budget in its attempt. The attempt already trusts its own calibration pair in the same way.

## Tests (`crates/filter/tests/props.rs`)
- **`a_host_slowed_through_the_re_measure_reads_as_it_would_quiet`:** #1167's figures. Two cases pass at 36–38 µs with `slowed` about 1.5, where a12a8e57's scale read them over 55 µs. The other two, whose calibrations moved during the re-measure, are void.
- **`a_steady_slowdown_fails_on_re_measure_however_slow_the_host`:** a case 1.1× over budget on the reference, a listed model and an unlisted one, re-measured on a host 1.0–2.0× slower, reads 1.1× over every time.
- **`a_calibration_that_moves_or_slows_too_far_voids_the_re_measure`:** the void bounds at 15% apart and 2×, either way round, including calibrations that can't be read.
- **Property tests:**
  - `a_steady_host_never_passes_a_case_over_budget`: with any noise added to the passes, and noise under 5% on the calibrations beside the re-measure.
  - `a_uniformly_slowed_host_gives_the_quiet_verdict`: for the scaled models, a host 1–2× slower in the attempt and another 1–2× in the re-measure gives the same reading and verdict as a quiet host. Where the re-measure is under 5% slower than the attempt, it reads up to 5% stricter, never looser. A host that slows further during the re-measure (within 15%) never reads faster than quiet.
  - `the_reference_reads_a_drift_after_its_attempt_as_the_attempt`.
  - `re_measuring_never_reads_more_leniently`: kept, for calibrations no slower than the attempt's.
- The earlier re-measure tests are kept, adapted to the two calibrations.
- **Mutants that fail a normal test:** never scaling by the slowdown (#1167's replay fails), scaling below the 5% floor (the steady-host property fails), never voiding (the void bounds test fails), and a bench that doesn't take the lock (`the_benches_run_alone` fails).
- **Local run of the gate** (an unlisted Xeon @ 2.30 GHz, release): the two benches ran one after the other. Every re-measure was read at `slowed` 1.00–1.02 before the floor was added, which is how the floor came about. The gate fails on this host on `main` as well, because this CPU skews about 1.25× against the calibration, as the 2026-11-21 file found. That says nothing about CI.

## Owns
`crates/filter/tests/props.rs` (the budget gate and its helpers) and this file.
