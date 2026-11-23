# X-fleet-baab decisions

CR-SPEC, as amended by r5.1 (`docs/build-plan.md` §0.5), runs every treatment as balanced blocks: ABBA, then BAAB. Until now the fleet tooling could only run ABBA, and `analyze_cr.py` couldn't analyse an A/A run. The runner approved the design at step 0 (message of 2026-11-23Z). The A/A block is the runner's call (§2.26), flagged to the owner, who can veto it (§2.19).

- **BAAB is a block mode (`MODE=baab`).** It runs treatment, control, control, treatment. It's valid wherever ABBA is, stacked stages included (the control is the winners, r5.1 A2). On-only keeps its rules. The rules file takes `"mode": "baab"`. `rules.py` stays closed: only the enum values and `args()` changed.
- **The order is recorded in `block.env`'s `MODE` line, and ABBA is its absence.** `MODE=abba` is never written, so an ABBA block's `block.env`, and its start-block arguments, are byte-identical to before. A block from before this change, with no `MODE` line, reads as ABBA. `load_block_env` accepts `MODE` on or baab only.
- **One arm function per language.** In the shell it's block-lib.sh `arm_of`, which arm_env, arm-switch and start-block all go through. In Python it's `scrub.arm_of`, which mirrors it, because copy-off mounts `scrub.py` alone into its container. `analyze_cr.DESIGNS` gives the same orders as arm letters. A test (`BaabAndAA.test_one_order_for_every_reader`) checks the three agree for every mode and segment. `vm-status.sh` prints the validated `MODE` (`block_mode_baab`). No other reader in `infra/fleet` or `docs/soak/tools` assumed ABBA.
- **An A/A block is `PURPOSE=aa`, with ABBA or BAAB (runner, §2.26; flagged §2.19).**
  - arm_of keeps the order's labels, but arm_env keeps the treatment's switches off in both arms, with the winners on in both.
  - So a stacked A/A measures the stacked control's noise floor, never a treatment.
  - mode_check allows `screen` only with on-only and `aa` only with abba or baab; the new reason is `purpose_needs_ab_mode`.
  - The scrubbed document gains `"purpose": "aa"` for A/A blocks only.
- **The scrubber's redaction rules are unchanged.** Only the `arm` labels and the `mode` vocabulary (abba, baab, on) changed, plus the optional `purpose: aa`. The scrubbed bytes of an ABBA block, a stacked ABBA block and an on-only block match hashes captured from `scrub.py` before this change (`SCRUB_GOLDEN`). A BAAB document differs from its ABBA twin only in `mode` and the labels.
- **`analyze_cr.py effect` takes the recorded design.**
  - `--design abba|baab` is repeatable, one per `--block`.
  - A block whose switch-derived order differs from its design is invalid: `order_differs_from_design`.
  - `--aa`, which needs `--design`, labels the run A/A. Each block's four segments must have identical switches (`aa_switches_differ`), and the arms are the design labels.
  - Every other block check still applies. Rule 6.2's `fit_effect` runs unchanged on those labels.
  - The result is reported as `"analysis": "noise_floor"`, `"outcome": "noise_floor"`, `"decided": false`, with the fit under `noise_floor`. There's never a `fit` key and never a win, loss or no-effect outcome.
  - One valid block is enough (df 1); A/A blocks needn't come in ABBA/BAAB pairs.
  - `gate` and `checkpoint1` refuse a noise-floor report.
- **Without `--design`, `effect` is byte-identical to before.** `fixtures/cr_effect_abba_golden.json` holds the sha256 and length of ten reports captured from the analyzer before this change: wins, a loss, inconclusive, on-only, a bad order, all-off, unpaired, adm and route. A matching `--design` doesn't change a byte either.
- **Out of scope, as the brief says:** instance lifetime and ExpiresAt, cloud calls, and the scrubber's redaction rules.
