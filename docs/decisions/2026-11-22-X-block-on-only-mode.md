# X-block-on-only-mode decisions

- **On-only is an explicit mode: rules `"mode": "abba" | "on"`, absent meaning abba.** `infra/fleet/steps/vm/start-block.sh` always opened segment 1 as the control, so a screen that needs the treatment on throughout, such as a fresh-database screen, couldn't run. With `"mode": "on"`, `block-lib.sh arm_of` gives the treatment in every segment. Nothing else in the block changes: the leak test, the host and boot guards, arm-switch, copy-off and every stop condition are as before. The mode changes only which arm each segment runs.
- **The need for a control is marked, and on-only fails closed (runner, 2026-11-22, §2.26).**
  - `"mode": "on"` is accepted only with `"purpose": "screen"`, and never with `winners`, because a stacked stage's control is the winners (r5.1 A2).
  - `purpose` without `"mode": "on"` is refused too.
  - `infra/fleet/steps/rules.py` refuses any other combination. start-block refuses it again in args_valid (`mode_needs_control`, `purpose_needs_on_mode`), as does `load_block_env` on every mid-block read. It's the same pattern as `stacking`.
  - Only the arguments and `block.env` give the mode; the caller's environment never does.
- **An abba block is byte-identical to before.** `rules.args` adds `MODE=on` and `PURPOSE=screen` only for on-only, and start-block writes them to `block.env` only then. An explicit `MODE=abba` argument is accepted and written as nothing. `load_block_env` accepts only `MODE=on`.
- **Analysis can't read an on-only block as an A/B result.**
  - `scrub.py collect()` takes each segment's arm from the mode and adds a `mode` field to the evidence document (`abba` or `on`, an allow-listed enum).
  - `analyze_cr.py judge_block` adds `no_control_arm` when no segment is the control. The block is invalid and never enters the effect fit. It already failed `order_not_abba_or_baab`, which stays; the new reason names the real cause for the operator.
- **Runbook** (`docs/operations/cloud-fleet-runbook.md` §3.3.2): it covers the mode and when to use it. It adds that the Lodestar image has no `env` binary, so a running process's switches are read from `/proc/<pid>/environ`, and that a fresh-database screen runs at least 12 h before its restart drill.
- **Owns widened at step 0 (runner, 2026-11-22):** `infra/fleet/steps/rules.py`, `infra/fleet/steps/vm/scrub.py`, `docs/soak/tools/analyze_cr.py` and `docs/soak/tools/test_analyze_cr.py`. The rules tests are in `infra/fleet/tests/test_block.py`.
