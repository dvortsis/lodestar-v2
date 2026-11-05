# X-filter-budget-noise decisions (#451)

Runner brief X-filter-budget-noise, 2026-11-05: make the `filter budget (§20.8)` gate robust to shared-runner noise. This is a performance-measurement call the runner decides under `docs/owner-decisions.md` §2.26, as a refinement under §2.19 that it flags to the owner. The owner's 50 µs per torrent at 10,000 rules (§1.6.6, architecture §20.8) is unchanged. So are the EPYC 7763 reference and its raw reading (§2.20), every held case, the calibration and its drift check, and the 0.4–2.0× range. Two changes follow from the diagnosis below:
1. a robust statistic (noise);
2. re-derived scaling constants (stale, not noise).

Both are in `crates/filter/tests/props.rs`, as separate commits. This supersedes:
- the "median decides" reading in `2026-10-22-filter-budget.md`;
- the `spread` rule's median in `2026-10-30-X-filter-budget-8573C.md`;
- that file's constants (`CALIB_TYPICAL_NS`, `REFERENCE_RATIO` and the four skews).

## Diagnosis
**Evidence:**
- the logs of 72 `filter budget` jobs, 2026-10-31 to 11-05: 37 on the 7763, 17 on the 9V74, 8 on the 9V45, 5 on the 8573C, 4 on the 8370C and 1 on the 6973P-C, all rustc 1.99.1;
- 20 + 25 local release runs (an unlisted 4-core Intel Xeon @ 2.30 GHz).

- **#429's failure is noise (a).** Run 41937126100, job 126950288443, EPYC 9V74, failed the neutralized case at 50.8 µs.
  - **Its seven passes:** 37.2 36.4 59.8 37.9 63.9 60.9 36.4 µs.
    - Three passes spiked near 60 µs, and two more ran 2–4% slow.
    - Sorted, the median is the fourth pass: 37.9 µs, a slowed one.
  - **Why the spread check missed it:** the median was only 4% above the fastest (36.4 µs), under `DRIFT` (15%), so no re-measure fired.
  - **Why the calibration drift check missed it:** the calibration was steady on both sides (9.1 and 9.2 µs fastest).
  - **On clean passes it passes:** the fastest clean pass (36.4 µs) reads 48.8 µs, and the third-fastest (37.2 µs) reads 49.9 µs, both under the old constants.
- **A second failure isn't noise: the scaling constants are stale.** Run 41908317816, job 126848927600, EPYC 9V45 (slice/ENR-6), failed the neutralized case at 50.2 µs.
  - **Its passes were steady:** 25.0–26.2 µs.
  - **The cause:** 7763 hosts calibrate in two bands, about 10.45–10.6 µs and 10.97–11.14 µs, at the same engine time:
    - the mixed set ran 42.3–44.6 µs raw in both bands (the 46.0 µs in the table below is one host outside both, calibrating at 11.92 µs, on 2026-10-31);
    - `CALIB_TYPICAL_NS` (11.09 µs) was the median of 29 jobs on 2026-10-29..30, mostly in the slower band;
    - by 2026-11-04 all 14 7763 jobs were in the faster band (10.46–10.69 µs).
  - **What it did:** the 7763 reads raw, so its own results didn't move, but listed models' hosts read slower than they ran. Their calibrations sped up the same way while their engine times didn't, and every listed model read about 2–10% above the 7763 at the same code.

  | CPU (jobs) | Mixed, old gate | Neutralized, old gate |
  |---|---|---|
  | 7763, raw (37) | 42.3–46.0 µs, median 43.3 | 42.8–48.2 µs, median 43.9 |
  | 9V45 (8) | 45.4–48.4 µs, median 47.7 | 44.2–50.7 µs, median 48.8 |
  | 9V74 (17) | 42.9–48.7 µs | 43.9–50.8 µs |
  | 8370C (4) | 44.7–45.2 µs | — |
- **Not a headroom gap (b).** On the 7763 reference the neutralized case reads 42.8–48.2 µs raw (median 43.9). That leaves about 12% headroom, and neutralized ÷ mixed is 0.99–1.05 (median 1.016) over the 37 jobs, inside the 1.08 tripwire set in `2026-10-30-X-filter-budget-8573C.md`.
  - The 48.2 µs is a 2026-10-31 job on a slow-calibration host (11.92 µs). It is also the 7763's slowest mixed time (46.0 µs) since #196.
  - No engine change is needed.

## Change 1: the gate decides on each case's third-fastest pass
- **The rule** (`Timing`): a case's time is its third-fastest of seven passes, where it was the median (the fourth). The rest of the gate is unchanged.
- **Why noise can't hide a regression:** noise only adds time, and adding delay to any passes can't move an order statistic down. A property test (`noise_never_makes_a_case_read_faster`) checks this, and checks that a uniform slowdown raises the time at least as much.
- **Why it absorbs the noise:**
  - It needs three passes that fast, so one lucky pass can't set it.
  - Up to four of the seven passes can be slowed without moving it. The median moves once the fourth is slowed: that was #429's job.
- **What it costs in strictness:**
  - On steady jobs the third-fastest sits 0.22% below the median on the 7763 (p50; p90 0.60%; 185 held cases). It is 0.00% below on the 9V45, 9V74, 8370C and 8573C (p90 0.3–1.2%).
  - The fastest pass, which `2026-10-30-X-filter-budget-8573C.md` declined because it loosens every model, would cost more.
  - This is the smallest robust choice.
- **The spread check keeps its job:**
  - **The rule:** a held case whose third-fastest pass is more than `DRIFT` (15%) above its fastest is a disturbed attempt, measured again.
  - **When it fires:** now only when five or more passes slowed. X-filter-budget-8573C's job 124112699853 still trips it, at 15.6%.
- **New: an attempt over budget while busy measures again.**
  - **The rule:** if every held case over budget had a pass more than `DRIFT` over its own fastest (`jitter`), the host was busy during those cases. The gate measures again, within the same three attempts. Noise in some other case doesn't count: a steady case over budget fails the attempt at once.
  - **What still fails:**
    - any steady held case over budget, at once, as before;
    - the last attempt over budget, busy or not;
    - drift on every attempt.
  - **Why it can't pass a slow engine:** only an attempt under budget passes, and noise only adds time to a case. A slower engine slows every pass, so its case is steady and fails at once. The calibration is the one place noise loosens: a slowed calibration makes a scaled model read faster. The gate takes the fastest of its 14 passes per attempt, and the 15% drift check bounds what noise on one side can hide. Each extra attempt draws a new calibration under the same rule. That was true before this slice, and the 7763 reads raw, so it has no calibration in its verdict.
- **Replayed on the 72 jobs, under change 2's constants (in-sample: the same jobs derive the constants):** every held case is under 50 µs. #429's job reads 47.2 µs (its third-fastest pass, 37.2 µs, ÷ the divisor 9.06 ÷ 10.59 × 0.922 = 0.789). The 9V45 job reads 45.4 µs (25.6 µs ÷ 0.564).

## Change 2: re-derived constants
**The method:** the formula from `2026-10-30-X-filter-budget-8573C.md`, over all 72 jobs, on the third-fastest pass the gate now uses.
- **R:** a job's raw mixed µs ÷ its `calibration:` µs.
- **Skew:** a model's median R ÷ the 7763's median R.

**The derived values:**

| Constant | Was | Now | From |
|---|---|---|---|
| `CALIB_TYPICAL_NS` | 11.09 µs (29 jobs, 10-29..30) | **10.59 µs** | median of 37 7763 jobs, 10.45–11.92 |
| `CALIB_REF_NS` (unlisted fallback) | 10.7 µs (4 jobs, 10-22) | **10.45 µs** | lowest of the 37 |
| `REFERENCE_RATIO` (drift warning only) | 3.93 | **4.02** | median 7763 R, 3.82–4.20 |
| 8573C skew | 1.126 | **1.097** | 5 jobs, 1.070–1.143 |
| EPYC 9V45 skew | 0.894 | **0.944** | 8 jobs, 0.892–0.965 |
| EPYC 9V74 skew | 0.913 | **0.922** | 17 jobs, 0.877–0.995 |
| 8370C skew (still provisional) | 1.034 | **1.038** | 4 jobs, 1.033–1.048 |

Each `ENGINE_SPEED` entry and `REFERENCE` lists its run IDs, dates and the first and last job's head.

- **Replayed on the same jobs** (reference µs, third-fastest pass). This is in-sample. Three jobs ran after the window closed (2026-11-05T01:05Z), and they are the only independent check so far:

  | Job | CPU | R | Old gate, mixed / neutralized | New gate |
  |---|---|---|---|---|
  | 126986469384 | 7763 | 4.01 | 42.0 / 42.7 | 41.8 / 42.6 |
  | 126991634314 | 7763 | 4.04 | 42.4 / 42.5 | 42.4 / 42.4 |
  | 126993334096 | 9V45 | 3.27 | 40.6 / 42.3 | 36.7 / 38.1 |

  - **The 9V45 job reads 13% below the 7763.** Its R (3.27) is far below the 9V45's in-sample 3.71–3.90, so its engine ran fast for its calibration. That is a 9V45 host spread of about ±9%, which no single skew can follow. It is under `MODEL_DRIFT`, so it doesn't warn.
  - **What that costs:** on such a host, a regression up to about 13% would read no higher than the 7763 does today. Under the old constants the same job read 3% low, but the old constants failed steady 9V45 hosts like job 126848927600.
  - **Why it's still the better trade:** the skew is the documented median, and a median skew reads half a model's hosts low and half high by construction. The 7763 reads raw and decides most jobs, so a regression on it fails whatever the 9V45 reads.
  - **Next step (runner):** check the first ten or so 9V45 jobs after merge, and re-derive in its own PR if their R settles away from 3.79. Every listed model now reads within about 1–2 µs of the 7763's raw time:

  | CPU (jobs) | Mixed | Neutralized |
  |---|---|---|
  | 7763, raw (37) | 42.3–45.5, median 43.3 | 42.8–47.9, median 43.8 |
  | 9V45 (8) | 40.3–43.6, median 43.0 | 39.8–45.9 |
  | 9V74 (17) | 40.5–46.0, median 42.6 | 41.5–47.2 |
  | 8573C (5) | 41.6–44.4 | 43.6–45.8 |
  | 8370C (4) | 42.4–43.0 | 43.2–44.1 |
- **Why this isn't moving the table to make a run pass:**
  - The trigger is the one the constants' own docs name: "when the calibration's time moves". The 7763's calibration moved by 4.5% with its engine unchanged, and that is what made the listed models read high.
  - The 7763 reads raw before and after, so the reference's verdict doesn't change.
  - **What the change does:** listed models get about 2–9% looser, back toward the 7763's raw reading at the same code. The 8573C and 9V74 move 1–3%.
  - **Unlisted models** get 2.4% looser through `CALIB_REF_NS`.
- **Not listed: the Xeon 6973P-C,** which has one job (run 41212909255). It is still scaled by the calibration alone. Its R gives a skew of 1.18, so it reads high: mixed 49.7 µs under the new constants, 51.1 µs under the old ones. That is 0.6% from failing on a hosted model, and it is the next flake this gate is likely to hand an unrelated PR.
  - **Next step (runner's queue):** list it, provisional, once it has a few jobs. Until then a 6973P-C failure close to 50 µs is this gap, not the PR's code.
- **Considered and declined: a warning when a 7763 job's calibration is more than 5% off `CALIB_TYPICAL_NS`.** The two bands are about 5% apart, so it would fire on whichever band is the minority.
  - **What tells instead:** every job prints its `calibration:` line and its mixed ÷ calibration.
  - **What the bands mean:** they are a weakness of a std-only calibration as a speed probe on the 7763, which this slice doesn't change. If the fleet swings back to the 11.0 band for good, listed models read about 4% low until the next re-derivation.

## Proof
**Local release runs:**
- **Machine:** an unlisted Intel Xeon @ 2.30 GHz, 4 cores, shared, scaled by the calibration alone.
- **Command:** `cargo test --release -p lodestar-filter --test props -- --ignored --nocapture budget`.

**Reference µs per held row, over the runs' deciding attempts:**

| Row | Before: 20 runs, median pass, old constants | After: 25 runs (28 deciding attempts), third-fastest, new constants |
|---|---|---|
| core | 9.5 / 9.7 / 15.4 | 9.3 / 9.6 / 10.0 |
| 1% | 35.1 / 36.1 / 37.8 | 34.6 / 35.8 / 38.8 |
| 10% | 37.0 / 37.9 / 39.7 | 36.1 / 37.5 / 39.9 |
| mixed | 45.4 / 47.1 / 53.4 | 42.9 / 46.3 / 51.8 |
| mixed, neutralized | 47.8 / 48.4 / 55.3 | 47.0 / 47.8 / 51.8 |
| result | 12 passed, 8 failed | 23 passed, 2 failed |

Each cell is min / median / max. The 8 failures before mixed slowed passes the median kept with steady attempts at 50.0–55.3 µs.

**The two failures after** were steady attempts at 50.0 and 50.2 µs.
- This CPU isn't listed, and its R is about 4.8 against the 7763's 4.02 (skew about 1.19). The calibration alone reads it about 19% high.
- So it sits at the edge whatever statistic decides. It isn't a hosted runner model, and it isn't what CI runs.
- Its busy attempts re-measured as intended: three "over budget while busy" and five calibration-drift retries in 25 runs.

**A deliberate slowdown still fails.** This was a test-only busy-wait inside the neutralized case's judgment closure, applied locally and not committed:
- **+8 µs raw per torrent:** 10 of 10 runs failed, at 59.6–67.0 µs, including attempts that re-measured while busy.
- **+3 µs raw (about +9%):** 10 of 10 runs failed, at 50.7–54.5 µs.

**Tests** (debug, with the rest of `props`):
- `Timing` on #429's passes: time 37.2, spread under 3%, jitter over 15%;
- job 124112699853's disturbed set still has a spread over 15%;
- a uniform slowdown scales the time and keeps spread and jitter;
- the noise property;
- the 8573C hand-computed divisor, now 9.86 ÷ 10.59 × 1.097 = 1.021381;
- the unlisted fallback at 8.36 ÷ 10.45 = 0.8.
