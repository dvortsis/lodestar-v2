# F-filters-xcpu decisions (#196)

The budget gate now holds the owner's 50 µs per torrent at 10,000 rules (`docs/owner-decisions.md` §1.6.6, architecture §20.8) on every hosted runner type seen, with the neutralized case held too. The engine is faster on the shared path, and the gate scales each runner model by the engine's own measured speed on it, not by a proxy workload. The 50 µs and the EPYC 7763 reference (§2.20) are unchanged.

- **Why the gate moved (measured, not guessed).** `main`'s `filter budget` went red at `a84c3681` (run 41015045891) on an Intel Xeon Platinum 8573C: the mixed set read 50.4 reference-µs, though the same code read 39.3 on an EPYC 9V45. Fifteen successful jobs from 2026-10-26 to 2026-10-30 give the engine's time ÷ the calibration's time per CPU model:

  | CPU | Jobs | Calibration, raw µs (median) | Mixed set, raw µs (median) | Mixed ÷ calibration (range) |
  |---|---|---|---|---|
  | AMD EPYC 7763 (reference) | 9 | 11.12 | 45.3 | 4.09 (3.96–4.43) |
  | AMD EPYC 9V45 | 5 | 6.94 | 25.5 | 3.66 (3.52–3.69) |
  | Intel Xeon Platinum 8573C | 1 | 8.67 | 40.9 | 4.72 |

  Within a model the ratio is tight; between models it differs by up to 29%. The calibration (#97, std hashing, sorting and small allocations) tracks how fast a CPU runs that work, not how fast it runs the engine: it read the 8573C as 0.81× the reference's time where the engine ran 0.90×, and the 9V45 as 0.65× where the engine ran 0.56×. So the same code read 12% apart between the two AMD types, and passed or failed by which runner a job drew.
- **Speed first, all exact** (callgrind instructions per judgment, which don't move between CPUs; `profile` in `tests/props.rs`):

  | Case | Before | After |
  |---|---|---|
  | Mixed set (held) | 458k | 437k (−4.6%) |
  | Mixed, 5 words neutralized | 487k | 436k (−10%) |

  - **Each distinct gate answered once per torrent.** A rule list now numbers its gates (`Gates`), and the marking pass remembers each gate's answer. Before, only gates reading a clause were remembered; size, file-size, tag and pair gates were asked again for every rule that had them (393 asks per torrent in the mixed set, about 25 distinct). A gate's answer depends only on the torrent and the neutral words, both fixed for one pass.
  - **The core reuses keyword entries' read** (`normalize::Read`, `Core::pattern_hits_read`). With keyword entries, the name and every path were tokenized twice by the same normalizer: whole for the entries, segment by segment for the core. The two give the same tokens unless a spelled-out run is open at a `/` (`l/e/x/a`), which goes on over it in one text and ends at it read apart. The keyword pass now notes that per path (`paths_apart`, fed one folder at a time), and the core reuses every path not so marked and reads the marked ones itself. The name reads the same either way. `marked_paths_read_the_same_apart` checks the tokens against the core's own read (look-alikes, combining marks, letters and `@`/`$` on both sides of a `/`, folders shared and forgotten); `reusing_the_whole_read_finds_the_same` checks the core's hits, padding files and neutral words included; `a_run_over_a_slash_is_marked` checks the case that differs. Marking every path reusable makes the first two token tests fail.
  - **`paths_apart`'s folder buffers live in the thread's tokenizer**, so they carry over between torrents (the #192 author's note).
  - The rest of the profile is flat: the key scan about 186k, rule candidates about 112k, the core's tokenizer about 94k, nothing else above about 13%. A further fifth would need the key scan restructured, which this slice doesn't do.
- **The gate scales listed models by the engine's own speed** (runner's decision under §2.19, 2026-10-30, as the brief allowed; flagged to the owner). `ENGINE_SPEED` in `tests/props.rs` lists, per CPU model name, the engine's mixed-set time there ÷ its time on the EPYC 7763 over the same code range and toolchain (rustc 1.99.1). Each entry records its jobs' run IDs and the code they ran; one from fewer than three jobs is provisional, and the gate's output says so:

  | Model | Factor | Jobs (run IDs) | Code |
  |---|---|---|---|
  | EPYC 7763 | 1.0 | 9: 41027904941, 41024264027, 41022335477, 41018406044, 41016836536, 41011318136, 41004752665, 40845396409, 40289941026 | `058d9937..8d7a24f6` |
  | EPYC 9V45 | 0.563 (25.5 ÷ 45.3 µs) | 5: 41028071535, 41025649623, 41024152492, 41014655186, 40660530001 | `058d9937..8d7a24f6` |
  | Xeon Platinum 8573C | 0.90 (40.9 ÷ 45.3 µs), provisional | 1: 41015045891 | `a84c3681` |

  The 7763 and 9V45 factors come from the same code range (`058d9937..8d7a24f6`), not identical code: filter and search code changed inside it (#192's neutral path, phrase-scorer fixes), but the mixed set's raw time is flat across it on each model.
  - **Tracking:** the 8573C factor (n = 1) is re-derived, with a decisions entry, once two more 8573C `filter budget` jobs exist. This PR's job 123999363098 is one of them (raw mixed 39.6 µs, at this slice's code).
  - The gate posts annotations (`::warning::`) on the check for an unlisted model, a provisional factor and a factor that disagrees with the calibration by more than 20%. The CPU line was read for three of the 7763 jobs and two of the 9V45's. The rest were assigned by their calibration, which falls in two clean bands (9.8–11.3 µs and 6.9–7.3 µs); each job's log prints its CPU line to confirm.
  - A job on a listed model divides each case by its factor. Any other model falls back to the calibration, as before, with a log line saying so.
  - Every case's line prints the raw µs, the divisor and where it came from, and the 7763-equivalent µs, and the scaling line names the CPU, so anyone can recheck a run.
  - **Adding or changing a factor needs a `docs/decisions/` entry** with the runs and the arithmetic (runner, 2026-10-30), so the table never moves silently to make a run pass. The comment above the table says so.
  - **Why this method:** it's grounded in the workload the budget measures. On the reference type itself the gate reads the raw time, and elsewhere it reads the time the reference would have taken by the engine's measured ratio. The cost: on a listed model nothing corrects for one runner being slower than another of the same model (the 7763's calibrations range 9.8–11.3 µs, about 15%), so a slow host reads slower. The runner accepted that; the drift check still catches a host whose speed changes during the test. The calibration still runs on every job, to catch a runner whose speed changes under the test (drift) and a measurement that can't be trusted (the 0.4–2.0× range).
  - **Keeping it true:** when a listed model's calibration and its factor disagree by more than 20%, the gate prints a warning that the factor may need re-deriving (a toolchain change, or a change in the engine's mix of work). It only warns: the table decides. The procedure sits beside the table.
  - **Considered and not taken:** an instruction-count gate under valgrind is deterministic, but it needs valgrind in the job, converts instructions to µs with a fixed rate, and misses cache and branch regressions; it stays a profiling tool. A ratio to an in-run reference workload is what the calibration already is.
- **The neutralized case is held now**, as #192's decisions file said it would be once the gate held across CPU types. At #192's merge it read 51.1 µs raw on a 7763 (job 123970548225; 49.8 µs scaled by that job's 1.03× calibration). Under the new method that raw time is what the gate reads, so this slice's cut is what brings it under: by instructions (436k ÷ 487k) it projects to about 45.7 µs, a margin of about 9%, against a spread of about 11% between the 7763's jobs. That's the tightest held case; the PR's `filter budget` jobs show where it lands.
- **Unchanged:** the owner's 50 µs per torrent at 10,000 rules (§1.6.6), and the reference, an AMD EPYC 7763 (§2.20). The instruction counts above are reported here and by the `profile` test, never gated.
- **Wall time on the build VM couldn't settle the two cases' relative cost**: passes there ranged from 37 to 88 µs within one run. Under cache and branch simulation the two cases match (D1 misses within 8%, mispredicts within 4%). The PR's `filter budget` job reports both on a hosted runner.
