# F-filters-xcpu: hold the 50 µs filter budget on every hosted runner type
**Model:** Opus (filter engine hot path; the budget gate is owner-set). Runner brief, 2026-10-30.

## Base and timing
Branch `slice/F-filters-xcpu` from `origin/main`, **after F-filters-neutral-perf has merged** (it changes the same engine code). The runner launches it then.

## Evidence
- `main`'s `filter budget (§20.8)` job went red at `a84c3681` (run 41015045891, job 123930410931). The held mixed rule set read 50.4 µs reference (40.9 µs raw) on an Intel Xeon Platinum 8573C, calibrated at 0.81×.
- The same code passed at `b4d80387` at 39.3 µs on an AMD EPYC 9V45, calibrated at 0.65×. `git diff b4d80387 a84c3681` is empty in filter, search, classify and release-parser.
- So the engine runs about 28% slower relative to calibration on that Intel type. It's runner variance, not a regression.
- F-filters-neutral-perf brought the neutralized case to 1.08–1.11× the mixed set. It stays reported, not held, until this slice: it projects to ~43 µs on EPYC and ~55 µs on the 8573C.

## Rules (owner)
- `docs/owner-decisions.md` §1.6.6 sets the 50 µs per torrent. §2.20 says it should hold on typical hardware (reference EPYC 7763), and #97's calibrated gate is a stopgap.
- The owner allowed changing the gate's method "if there is something more efficient to our tech stack" (§2.19), recorded and flagged. **The 50 µs number doesn't change.**

## Do
1. **Speed first.** Profile the mixed set and the neutralized set (callgrind instruction counts, which are CPU-stable, plus wall time) and cut the shared per-judgment cost so both clear 50 µs with margin on the 8573C's calibration. Matching stays exact: no rule's semantics change, and the property tests stay green.
2. **Then the gate.** If calibration still under-corrects across CPU types, propose a better-grounded method: an instruction-count gate, per-CPU-model calibration, or a ratio to an in-run reference workload. Put it in `docs/decisions/<date>-F-filters-xcpu.md` and message the runner before changing the gate.
3. **Hold the neutralized case** once it clears the budget everywhere.
4. Report the numbers per runner type seen in CI history (the `filter budget` job logs print the CPU and factor).

## Own
`crates/filter/**` (the engine hot path and its benches and props budget), `.github/workflows/ci.yml` (the `filter budget` job only, if the gate method changes), `docs/decisions/<date>-F-filters-xcpu.md`.

## Done when
fmt, clippy (`-D warnings`) and the workspace tests are clean; `filter budget` is green on the PR head, holding both cases; the fresh-reviewer self-review is done; and any gate change has the runner's OK.
