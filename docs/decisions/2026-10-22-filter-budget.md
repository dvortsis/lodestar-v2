# Filter budget gate decisions (#97)

- **The 50 µs filter budget is held on the reference runner (owner, 2026-10-22).** The owner chose to normalise the gate when asked to pick from engine − core, engine/core, an absolute retry or a larger runner. The build runner later passed on their go-ahead to change the gate if something fits the stack better. The budget and what it holds (core, 1%, 10%; 25% reported) are unchanged; only where it's measured changed (§1.6.6, §20.8).
  - **Why:** hosted runners differ by 1.5–1.9×, uniformly across core and engine. PR #48 measured 11.9 / 42.1 / 52.8 µs (core, 1%, 10%) on the same code main measured at 6.2 / 22.2 / 28.8 µs, and failed. The gate failed PRs, and `main`, at random.
  - **How:** `crates/filter/tests/props.rs` times `calibrate`, which runs no Lodestar code, before any filter code runs and after the engine is dropped. It divides each case's time by the runner's calibration time over `CALIB_REF_NS` (7.4 µs per torrent). Raw times, the runner's speed and the CPU are printed beside the scaled ones. A speed outside 0.4–3.0× fails as an untrusted measurement rather than being scaled away.
  - **The reference is the fastest hosted runners seen** (Xeon 6973P-C, EPYC 9V45). There the gate reads what the unscaled gate did. On the slower runners measured, the calibration slowed 1.45× where the filter slowed 1.47×, so they came out slightly stricter than the reference, not looser.
  - **Not chosen:**
    - **engine − core** grows with a slow runner (22.6 µs on main's run, 40.9 on #48's).
    - **engine / core** is stable, but makes a core speed-up read as an engine regression.
    - **A larger runner** needs a Team or Enterprise plan, and its hosts still vary.
    - **#66's 60 ms synthetic workload** moved only 1.10× where the filter moved 1.25×, and its 1.6× cap would fail a 7763-class runner.
    - **Calibrating between passes** (#97's first version) let an engine pass slow the next calibration by up to 19%, so a regression could hide part of itself.
  - **Evidence** (PR #97's temporary sample workflow, run 39868473532, 10% case): EPYC 7763 ×4, 52.0–52.5 µs raw, 35.8–36.0 scaled; EPYC 9V45 (first round), 32.1 raw, 31.7 scaled; Xeon 6973P-C, 35.5 raw, 35.4 scaled. A full match run 24 times scaled to 70.4 µs, and turning the cheap check off scaled to 113.2 µs; both failed the gate.
- **Supersedes #66** (slice/F1, runner-speed scaling with a clamped 60 ms workload).
