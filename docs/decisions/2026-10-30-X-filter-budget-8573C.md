# X-filter-budget-8573C decisions (#PR)

The filter budget gate now scales a listed CPU model by the job's own calibration, times a per-model skew, in place of one fixed factor per model. That makes it fair on the Intel Xeon Platinum 8573C, whose hosts run at two speeds. The owner's 50 µs per torrent at 10,000 rules (`docs/owner-decisions.md` §1.6.6, architecture §20.8) and the EPYC 7763 reference (§2.20) are unchanged. The reference still reads its raw times, and every held case is still held. This supersedes the "scales listed models by the engine's own speed" bullet of `2026-10-30-F-filters-xcpu.md`, with the runner's approval under §2.19 (RUNNER-DATE); flagged to the owner.

- **The failure.** On #210 (IMP-1, store-only), job 124095631213 ran on an 8573C and read the neutralized case at 53.3 µs. The raw time was 48.0 µs ÷ the 8573C's fixed factor of 0.900, which was provisional and came from one job.
- **The data.** I read the logs of 64 `filter budget` jobs from 2026-10-28 to 10-30. For each job, R is its raw mixed-set µs ÷ its `calibration:` µs.

  | CPU | Jobs | Calibration µs | R | R ÷ the 7763's R at the same code ("skew") |
  |---|---|---|---|---|
  | EPYC 7763 (reference) | 29 | 9.83–11.34, median 11.09 | 4.09 before #196 (24), 3.93 after (5) | 1 |
  | Xeon Platinum 8573C | 9 | 8.08–8.69 and 9.46–9.86 | 4.40–4.71 | 1.126 (1.112–1.181) |
  | EPYC 9V45 | 13 | 6.71–7.34 | 3.41–3.86 | 0.894 (0.861–0.945) |
  | EPYC 9V74 (not listed) | 10 | 9.15–11.90 | 3.58–3.81 | 0.913 (0.905–0.934) |
  | Xeon Platinum 8370C (not listed) | 3 | 10.36–10.75 | 4.06–4.54 | 1.034 (1.022–1.110) |

  "Before" and "after #196" mean code without and with `1b4e8b7a`, which cut the mixed set's instructions by 4.6%. The skew is the same in both eras: 8573C 1.126 and 1.150, 9V45 0.895 and 0.889.
- **Why one factor per model couldn't work.** The 8573C's hosts fall in two bands. Their calibrations are 8.08–8.69 µs and 9.46–9.86 µs, and the engine follows them: raw mixed was 37.3–40.9 µs on the fast band and 43.2–44.5 µs on the slow one. R stays tight across both bands (4.40–4.71). The fixed 0.900 was taken from a fast-band job (41015045891), so it read every slow-band host about 13% high. Job 124095631213 is a slow-band host (calibration 9.86 µs). Re-deriving a fixed factor from all 9 jobs (median 0.91) would still read that job at 52.7 µs.
- **The method** (`tests/props.rs`, `budget`):
  - **Reference model (`REFERENCE`, EPYC 7763):** its raw times decide, as before.
  - **Model listed in `ENGINE_SPEED`:** the divisor is (this job's fastest calibration pass ÷ `CALIB_TYPICAL_NS`, the 7763's median calibration, 11.09 µs) × the model's skew. The calibration corrects for the host and the skew for the CPU, because the calibration scales the engine unevenly across CPUs, as #196 found.
  - **Why the median calibration, not the fastest:** the reference reads raw on a typical 7763 host, so a listed model's host is measured against a typical one. That is about 3.6% stricter than measuring against `CALIB_REF_NS` (10.7 µs, the fastest).
  - **Any other model:** the calibration against `CALIB_REF_NS`, unchanged.
  - **Listed now:** the 8573C (skew 1.126, 9 jobs) and the 9V45 (0.894, 13 jobs). Each entry names its run IDs and code range.
  - **Self-check:** every job prints its mixed ÷ calibration. A listed model's job also prints its own skew against `REFERENCE_RATIO` (3.93, the 7763's ratio since #196). It warns on the check when that skew is more than `MODEL_DRIFT` (20%) off the table's. The warning only informs; the table decides.
- **Replayed on every job since #196** (reference µs, mixed / neutralized):
  - 8573C 124095631213: 43.3 / 47.9 µs (it failed at 48.2 / 53.3 µs).
  - 8573C 123999363098: 45.7 / 45.4 µs.
  - 9V45, 4 jobs: 42.3–44.1 / 43.5–46.1 µs.
  - 7763: 43.1–43.4 / 43.2–43.8 µs, raw and unchanged.

  Every listed model now reads within about 6% of the 7763's raw time at the same code. Before, the 8573C read 44–48 µs.
- **Not an engine regression.** Neutralized ÷ mixed since #196 is 1.00–1.01 on the 7763 (5 jobs), 1.01–1.05 on the 9V45, 1.01–1.03 on the 9V74 and 1.03 on the 8370C. On the 8573C it was 0.99 in job 123999363098 and 1.11 in job 124095631213. In 124095631213 the neutralized passes rose steadily from 45.6 to 53.3 µs while the mixed passes just before them were flat (43.2–43.9 µs), and the calibration was steady before and after. That pattern is a busy host during one case, not the engine. Callgrind with the 8573C's cache sizes (48 KiB 12-way L1D, 2 MiB 16-way L2) agrees:
  - instructions per judgment: equal (436k neutralized against 437k mixed);
  - L1D misses: +7.5%;
  - L2 misses: about 0 in both;
  - branch mispredicts: +4–7%.

  So there is no cache cliff and no engine slice to open.
- **Considered and not taken:**
  - **(b) A per-case factor for the neutralized case:** the data doesn't support it. The neutralized case scales like the mixed set on every model.
  - **(c) Report-only on a provisional model:** with 9 jobs the 8573C isn't provisional, and report-only would only defer the slow-band failure.
  - **Using the fastest pass for each case:** that would loosen every model, and 124095631213's fastest pass (45.6 µs ÷ 0.90 = 50.7 µs) would still fail.
- **Unchanged:** the 50 µs; the 7763 as the reference and its raw reading; which cases are held (the core, 1%, 10%, the mixed set, the mixed set neutralized); the calibration, its drift check and its 0.4–2.0× range; and the fallback for unlisted models.
- **CI evidence:** the runs dispatched on this branch are listed in the PR.
