# X-filter-budget-drift decisions (#1175)

Runner brief X-filter-budget-drift, 2026-11-28Z. The `filter budget (§20.8)` job failed on #1167's head, which touches nothing in `crates/filter`, and the job is inside the required `ci-ok`. This is a measurement change the runner decides under `docs/owner-decisions.md` §2.26, a refinement under §2.19 that the runner flags to the owner. The runner approved the design, with conditions, on 2026-11-28Z.

**Unchanged:**
- the owner's 50 µs per torrent at 10,000 rules (§1.6.6, architecture §20.8);
- every case, held or reported, the corpus and the rule counts;
- the calibration workload, `ENGINE_SPEED`, `CALIB_*`, `REFERENCE_RATIO`;
- the 15% drift and spread checks, the 0.4–2.0× range (`SPEED`) and the busy-attempt retry;
- every attempt's reading, the reference's raw reading included;
- the CI job's invocation.

**Superseded:**
- **From `2026-11-21-X-filter-budget-remeasure.md`:** "the second measurement is scaled by the faster of the attempt's calibration and the fastest calibration beside it". That rule still holds wherever the calibrations beside a re-measure show the host less than 10% slower than in its attempt, or disagree with each other. Where they agree and show it 10% or more slower, the slowdown rule below applies instead.
- **From `2026-11-05-X-filter-budget-noise.md`:** "the 7763 reads raw, so it has no calibration in its verdict". This now holds only for the attempt. A 7763 re-measure on a host slowed 10% or more is divided by that slowdown, like any other model's (runner's condition 4).
- **Also superseded, wherever the slowdown d (below) is 1.10 or more:**
  - 2026-11-21: "so it never reads more leniently than the first";
  - 2026-11-21: "only adds a second chance under the same scaling or a stricter one";
  - 2026-11-21: "with its scale never loosened";
  - 2026-11-05: "a failing run can't pass on a slower calibration draw".
- **What that means:** a re-measure can now read more leniently than its attempt, by the measured slowdown d. #1167's case is the example: 55 µs in the attempt, 37 µs on re-measure, with d about 1.5.
- **The residual risk:** noise that slows the calibration more than the engine, by at least 10% on all seven passes on both sides of a re-measure, loosens that re-measure in proportion. This now includes the 7763.

## Diagnosis
- **The job ran two benches at once.** The filter `budget` matches two ignored tests, `budget` and `budget_worst_case`. The log says "running 2 tests", and their lines interleave. So libtest timed the budget's cases while the worst-case test ran its largest inputs on the same 4-vCPU runner: "invisible" took 73 ms per judgment, and "tiny files" has 130,048 files.
- **#1167's job (Xeon 6973P-C, unlisted, so scaled by the calibration):**
  - **The attempt:** the calibration read 7.07 µs before and after. A block of cases inside it ran about 1.48× slow: the pending 1% at commit took 36.6 µs against 24.3 for the same case without pending versions, the pending 10% took 37.2 against 25.1, and the pending mixed set 46.2 against 31.1. Each case in the block was steady (spread ≤ 1%). The block lines up with the worst-case test's "invisible" and "combining" lines. The case after it ran at normal speed (31.9).
  - **The re-measure:** the worst-case test was still running; it finished 0.9 s after `budget` failed. Every calibration beside a re-measure read 10.56–12.47 µs, about 1.5× the attempt's, and every re-measured case read about 1.5× slow too.
  - **The verdict:** a12a8e57 scaled each re-measure by `min(beside, attempt)` = 7.07 µs, so a host that stays uniformly slow through the re-measure can't pass. The four cases read 54.3, 55.2, 56.0 and 70.6 µs and failed.
- **Replayed with the calibration beside each re-measure,** the same passes read 36.3, 36.9, 37.1 and 47.3 µs. The attempt's quiet-host neighbours read 35.9, 37.1, 37.5 and 45.9 µs.

## The change
- **The benches never run side by side** (`BENCH`, `bench_alone`). This is the main fix.
  - `budget`, `budget_worst_case` and `profile` each hold one static lock for their whole run.
  - A bench that panics doesn't block the others, because a poisoned lock is taken all the same.
  - A normal test, `the_benches_run_alone`, finds every `#[ignore` attribute in the source and fails if its function doesn't take the lock as its first statement.
  - The job's wall time grows by the worst-case test's run, about 7 s. Its invocation is unchanged.
- **A re-measure is scaled by the host's own slowdown when that slowdown is uniform** (`remeasure`). This covers noise from outside the job, which the lock can't reach.
  - **Steady:** the fastest calibration passes directly before and after that re-measure agree within 15%, the attempt's own rule for its before and after. Each case uses only its own pair.
  - **The slowdown:** `slowed` = the faster of the pair ÷ the calibration of the attempt that read the case over budget. When it's 1.10 or more (`MIN_SLOWED`), the case's divisor is multiplied by it, and the log prints a `::warning::` naming the factor.
  - **Against the attempt's own calibration:** after a busy attempt, the scale is the fastest calibration of any attempt. `slowed` is measured against the attempt's own calibration, never that scale, so the gap between two attempts' calibration draws can't read as a slowdown. The scale itself stays the stricter of the two (`a_gap_between_attempts_calibrations_is_no_slowdown`).
  - **The cap:** `slowed` above 2.0 (`MAX_SLOWED`), or a host it puts outside `SPEED`, is past the cap. A case past the cap passes only if it is under budget without the slowdown. Otherwise it fails. The slowdown is never clamped.
- **Why a 10% floor:**
  - On a steady build VM, the calibrations beside each re-measure read 1–4% over the attempt's.
  - The 7763 calibrates in two bands about 5% apart at the same engine time.
  - Below the floor, the re-measure keeps the strict scale, so it reads up to 10% stricter than quiet, never looser. #1167's 1.5× is well clear of it.
- **A re-measure whose calibrations disagree is void.** Disagreeing means more than 15% apart (#1167's first case: 7.07 µs before, 10.56 µs after), or a calibration that can't be read.
  - A void case is still read at the strict scale, with no slowdown. If it's under budget even so, it passes, because that reading is never more lenient than a12a8e57's.
  - Otherwise, the run's first void is measured once more, with a `::warning::`. One void re-run is allowed per run in total, not per case (runner's condition 3).
  - Any later void fails the case, with every reason.
- **The reference model:** `slowed` is relative to the attempt's calibration, so the 7763 gets the same factor on re-measure. Its attempt still reads raw.
  - **The 2026-11-21 file's worry was a calibration run beside filter state,** which a regression could slow so that it hides itself. Here, `calibrate` runs no Lodestar code, and the engine is dropped before each calibration beside a re-measure (`measure(Some(case))`).
  - **Shared caches and the allocator are the only path left.** A regression that slowed the calibration through them would need to slow it at least 10%, steadily on both sides, before any scaling applies.
- **Why not interleave calibrations with every case** (the brief's other candidate):
  - It would put a calibration next to the engine in every case.
  - It would cost seven calibration passes per case on every run.
  - The re-measure already sits between two calibrations, at the one point where a verdict turns on them.
- **What noise can still do:**
  - **Loosen a re-measure in proportion:** a slowdown that hits the calibration harder than the engine loosens it by the ratio of the two factors (`a_case_and_its_calibrations_slowed_apart_read_in_proportion`).
  - **Only past the floor:** that takes a calibration slowed at least 10% on all seven passes on both sides, for a case already over budget in its attempt.
  - **The same trust the attempt already places** in its own pair of calibrations.

## Tests (`crates/filter/tests/props.rs`)
- **`a_host_slowed_through_the_re_measure_reads_as_it_would_quiet`:** #1167's figures.
  - Two cases pass at 36–38 µs with `slowed` about 1.5, where a12a8e57's scale read them over 55 µs.
  - The other two, whose calibrations moved during the re-measure, are void.
- **`a_steady_slowdown_fails_on_re_measure_however_slow_the_host`:** a case 1.1× over budget on the reference, a listed model and an unlisted one, re-measured on a host 1.0–2.0× slower, reads 1.1× over every time.
- **`a_slowdown_past_the_speed_cap_fails_rather_than_clamps`:** past `SPEED` and past `MAX_SLOWED`, for all three models.
- **`a_void_re_measure_under_budget_at_the_strict_scale_passes`**, and `a_calibration_that_moves_voids_the_re_measure`.
- **`a_gap_between_attempts_calibrations_is_no_slowdown`:** the fresh review's case. The scale is 7.0 from a busy attempt, this attempt and the re-measure calibrate at 7.6, and the case still fails.
- **Property tests:** each runs 256 cases (pinned) of pure arithmetic, in milliseconds.
  - `a_steady_host_never_passes_a_case_over_budget`: with any noise added to the passes, and noise under 10% on the calibrations beside the re-measure.
  - `a_uniformly_slowed_host_gives_the_quiet_verdict`: for the scaled models, a host 1–2× slower in the attempt and another 1–2× in the re-measure gives the same reading and verdict as a quiet host. Under the floor, it reads up to 10% stricter, never looser. A host that slows further during the re-measure (within 15%) never reads faster than quiet.
  - `a_case_and_its_calibrations_slowed_apart_read_in_proportion`.
  - `the_reference_reads_a_drift_after_its_attempt_as_the_attempt`.
  - `re_measuring_never_reads_more_leniently`: kept, for calibrations no slower than the attempt's.
- **Mutants that fail a normal test:**
  - measuring `slowed` against the scale rather than the attempt;
  - no floor;
  - no cap;
  - never voiding;
  - never scaling;
  - a bench that doesn't take the lock.
- **Local mutation run** (an unlisted Xeon @ 2.30 GHz, release, pinned to one core):
  - **The mutation:** a fixed loop added to each judgment, slowing the mixed set about 10% (50.8 → 54.5 µs on the reference scale). It was reverted and never committed.
  - **At k = 1:** every held mixed case read over budget in the attempt and again on re-measure (51–57 µs), at `slowed` 1.00, and the gate failed.
  - **At k = 1.5:** this container couldn't slow the host uniformly. A time-sliced competing process left some calibration passes unslowed, a CPU quota throttled in 3× bursts, and memory-bandwidth load on the other cores didn't move it. Every attempt was void, unmutated or mutated, so the gate failed as untrusted, never passed. The uniform k = 1.5 case is covered by the property tests and `a_steady_slowdown_fails_on_re_measure_however_slow_the_host`.
  - **This host fails unmutated too,** about 1% over at 50.8 µs. Its CPU skews about 1.25× against the calibration, as the 2026-11-21 file found, so it says nothing about CI.

## Owns
`crates/filter/tests/props.rs` (the budget gate and its helpers) and this file.
