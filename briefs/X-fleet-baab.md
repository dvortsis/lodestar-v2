# X-fleet-baab (runner brief, 2026-11-23Z)

Testing-tooling slice, Opus: `scrub.py` is the privacy scrubber for copied-off evidence. It gets a fresh Opus review before merge. The runner places it in the r15 records.

## Why
CR-SPEC (as amended by r5.1, `docs/build-plan.md` §0.5) runs every treatment as two balanced blocks: **ABBA, then BAAB**. The fleet tooling can only run ABBA, so the cloud's stage 1 is running block 2 from recorded local patches on its instances. The experiment host will need BAAB for its block 2. The gaps:
- `infra/fleet/steps/vm/block-lib.sh` `arm_of` knows only ABBA (`MODE abba`) or on-only (`MODE on`);
- `infra/fleet/steps/vm/scrub.py:259` hard-codes `n in (2, 3)` as treatment in copy-off's evidence;
- `block.env` doesn't record the order.
And for the A/A noise-floor run: `docs/soak/tools/analyze_cr.py` derives arms from the recorded switches, so with both arms identical, `effect` refuses with `order_not_abba_or_baab`. The cloud lane computes it by hand with analyze_cr's own `fit_effect` on the design labels.

## Goal
1. **A BAAB mode.** `MODE baab` gives treatment, control, control, treatment. `mode_check` accepts it wherever ABBA is valid, including stacked stages, where the control is the winners. Keep on-only's rules.
2. **The order is recorded** in `block.env` (`MODE`). Every reader uses one function (`arm_of`, or a shared Python helper) instead of a hard-coded `n in (2, 3)`:
   - `scrub.py` takes treatment segments from the recorded mode;
   - any other reader in `infra/fleet` or `docs/soak/tools` does the same.
   Find every reader with grep and list them at step 0.
3. **analyze_cr:**
   - It checks that the arms derived from the switches match the recorded design (ABBA or BAAB), and refuses with a clear reason if they don't.
   - It adds an A/A mode. When both arms' switches are identical and the block is labelled A/A, it uses the design labels with the same `fit_effect` estimator and reports the result as the noise floor, never as a treatment effect.
   - It keeps existing outputs byte-identical for existing ABBA inputs, guarded by a golden or a test.
4. **Tests:**
   - `infra/fleet/tests/test_block.py` and `docs/soak/tools/test_analyze_cr.py` cover BAAB end to end: start-block args, block.env, arm-switch, copy-off/scrub evidence and the analysis;
   - the A/A path;
   - a mismatch between recorded design and switches.
   - Run `infra/fleet/run_tests.sh` and the analyze_cr tests.
5. **Docs:** `docs/operations/cloud-fleet-runbook.md` gets how to start a BAAB block and an A/A block. It's a short edit in the runbook's existing style; it's developer docs.

## Left out (don't build)
- **Any ExpiresAt or instance-lifetime change.** The 48 h guard and the runbook's expiry rule stay as they are. A longer stage expiry is decided per stage by the runner, not built in.
- **Anything that touches a cloud account,** runs terraform, or starts an instance. This slice is code and tests only.
- **Changes to the scrubber's redaction rules.** Only the treatment and control labelling changes. Show that the scrubbed output is otherwise byte-identical.

## Step 0 (send me, then wait for "go")
1. Every place that assumes ABBA (file:line), and your change at each.
2. The `block.env` field and how old blocks without it read: they must read as ABBA.
3. The analyze_cr A/A interface (flag or label) and the refusal reasons.
4. Owns, to refine:
   - `infra/fleet/steps/vm/{block-lib.sh,start-block.sh,arm-switch.sh,copy-off.sh,scrub.py}`, `infra/fleet/tests/test_block.py`;
   - `docs/soak/tools/analyze_cr.py`, `docs/soak/tools/test_analyze_cr.py`;
   - `docs/operations/cloud-fleet-runbook.md` (the BAAB and A/A sections only);
   - `docs/decisions/2026-11-23-X-fleet-baab.md`.
   - Name anything else, with a reason.
5. An estimate of the changed lines.

## Rules
- Raw soak logs, credentials, DBs and executables stay private, and nothing in tests may contain real IPs, instance ids or account names. Use 111122223333 or 999988887777 where an account-shaped number is needed.
- Condition waits in tests have deadlines of at least 10 s, with no fixed sleeps.
- Never beside X-host-ab-runsheet, which documents host runs: if it starts, the runner orders the two.

## Branch and PR
- Branch `slice/X-fleet-baab`. PR title "X-fleet-baab: BAAB blocks and an A/A mode in the fleet tooling and analyze_cr".
- Push once per round, then send "ready <pr> <full sha>". Follow `.claude/skills/steward/SKILL.md`; never merge.
