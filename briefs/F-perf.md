# F-perf: Filter engine speed-ups, and the budget on typical hardware
**Model:** Opus (untrusted input, hot path, security). **Branch:** `slice/F-perf`, started from `origin/slice/F-near` (F-near #85 is approved and waits only on the new gate, #97). Merge `origin/main` once F-near and #97 land.

**Why:** the owner's 50 µs per torrent at 10,000 rules (§20.8) should hold on hardware like users run (NAS and desktop CPUs), not only on GitHub's fastest runners (owner, 2026-10-23: "Go with whatever makes for the best user experience"). Today the 10% case reads about 52 µs on the most common hosted runner (EPYC 7763) and 57–60 µs on slower Intel VMs. #97 measures the budget in fastest-runner units as a stopgap; **this slice makes the engine fast enough to move the reference to the EPYC 7763** and does so.

**Spec:**
- `runner/standards.md`; `docs/owner-decisions.md` (§1.6.6 the 50 µs budget, §2.19).
- Architecture §20 (the rule engine), §20.8 (budget), and the #97 decisions file (`docs/decisions/2026-10-22-filter-budget.md`) once on main.
- The code: `crates/filter/src/{prefilter.rs,matcher.rs,core.rs,engine.rs,normalize.rs}` and `crates/filter/tests/props.rs`.

## Own
- `crates/filter/**`, `docs/decisions/<date>-F-perf.md`, and the §20.8 text in `docs/architecture.md` only where the reference changes.
- **Don't edit** other crates or CI wiring beyond `crates/filter/tests/props.rs`.

## Build
1. **Quadratic paths (security: hostile names must stay linear or near-linear):**
   - `prefilter.rs` `Scan::other` `at_pos` is quadratic on non-ASCII text;
   - `matcher.rs` `sequence()` is quadratic for phrases and multi-part words;
   - NEAR's per-place window scan: precompute per-word place lists and binary-search them; cache sorted tokens per text.
   Add adversarial-input tests (long non-ASCII names, many repeated tokens, many NEAR places) asserting work bounds (operation counters, not wall-clock), plus property tests that results are unchanged versus the old implementation.
2. **General speed:** profile the 1%/10%/25% cases and cut the dominant costs. Results must be identical (property test against a reference implementation kept in tests).
3. **Move the reference:** once the 10% case is under 50 µs scaled to the EPYC 7763, set `CALIB_REF_NS` to the 7763's calibration (using #97's estimation rule and re-derivation recipe), update the decisions file and §20.8, and say what the gate now means in user terms.
4. If the 10% case can't reach 50 µs on the 7763 reference without changing semantics, stop at the best result, report the numbers, and propose options rather than changing what's held.

## Done when
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` pass; `cargo test --release -p lodestar-filter --test props -- --ignored --nocapture budget` passes with the new reference; the filter fuzz targets run 60 s clean.
- A PR titled `F-perf: Filter speed-ups and the budget on typical hardware`, with before/after raw and scaled numbers for core/1%/10%/25% on the CI runner types seen, the quadratic fixes with their bounds, and the reference change.
