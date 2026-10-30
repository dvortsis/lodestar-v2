# Soak memory-gate fit (X-soak-memory-fit)

Runner, 2026-10-30. Follows the owner's CR-0 ruling (#189; `2026-10-30-CR-0-acceptance.md`): the 24 h Linux Docker soak is the binding memory gate for v0.5.1, judged by the same fit that settled CR-0.

- **`analyze_cr0.py` computes the gate**, so the Linux soak report doesn't depend on a hand calculation. It fits the OLS slope of `private_bytes` in MiB/h over baseline hours 12–24, counted from the first baseline sample, the way the owner's CR-0 fit did. It reports the slope, the standard error, the 95% CI and the run's peak private memory, and checks the upper bound against the 2 MiB/h bar.
- **The CI uses the normal quantile, 1.96,** and the fit needs at least 30 samples. At one sample a minute the window holds about 720, so the t-quantile difference is negligible.
- **When the gate isn't judged:**
  - **A restart inside the window:** a change of process or Lodestar start time refuses the fit. It's never fitted across a restart, matching the collector, which ends the run on one.
  - **A window that isn't covered end to end** gives a slope without a verdict. Every gap between fitted samples, and between each end of the window and its nearest sample, must be within the report's 180 s gap limit.
  - **A failed metrics read** doesn't break the identity check, because the process identity is still known.
- **The limitation, stated in the report:** samples a minute apart aren't independent, so plain OLS understates the CI width. The owner's bar was set on this same method, so the report applies it as stated, and a pass is still reviewed. A stricter method (autocorrelation-robust errors) would be a change to the owner's number, so it isn't made here.
