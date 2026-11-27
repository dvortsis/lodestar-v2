# X-soft-absent-void decisions

The runner launched this slice on 2026-11-27 (runner records r17, `docs/decisions/2026-11-24-runner-records-r17.md`). A soft block run on an image without CR-SOFT-TIMEOUT (#1091) is an A/A run, because the switch does nothing there, yet `analyze_cr.py effect` still decided it. This slice makes such a treatment segment void. It changes no crawler code and no switch default.

## A soft treatment arm with no soft counter is void (§2.19 refinement)
- **This reverses X-soft-key's recorded "no check, no warning" call** (`docs/decisions/2026-11-23-X-soft-key.md`, "Reported, never decided on") for one case. It's a refinement of a runner decision, not of an owner decision: no owner-set number moves and nothing is loosened. The runner flags it to the owner (`docs/owner-decisions.md` §2.19).
- **The rule.** In a segment whose treatment is `soft` and whose `soft_timeout` switch is on, `dht_soft_timeouts_total` absent at any scrape of the window fails rule 5's check `soft_timeout_counter_rendered`, and the segment is void with that reason.
  - A binary with #1091 renders the counter at every scrape, 0 included (`render_soft_timeout` in `crates/lodestar/src/metrics.rs`). So absence means the binary predates the switch, and the arm ran as the control.
  - **A count of 0 over the window is a result, never a void.** The switch was set; it found nothing to act on, or it acted and the count says so.
  - The control arm (switch off) is never checked: a binary from before #1091 renders nothing there, and that's what the control is.
  - Other treatments never get the check, so their output is unchanged.
  - Only the soft counter decides. `dht_soft_timeout_late_replies_total` and sampler-full time stay reported only, as X-soft-key set them.
- **Void, not a warning.** CR-SPEC rule 5 (`docs/decisions/2026-11-02-CR-SPEC.md`) says a segment whose treatment didn't take effect is void, and D7 defines void as exactly that. A missing counter with the switch on is the plainest case of it, the same shape as uTP's `utp_in_use`. A warning would leave `effect` deciding an A/A run as an A/B, the very failure r17 names.
  - **Not "missing evidence" (rule 6.10)** either: that would read as an invalid segment, whose evidence can't be trusted. Here the evidence is fine; the treatment just never ran.
  - **Not "no opportunity" (rule 5.3).** That exception is for a switch that ran and found nothing to do, which is the zero case above. An absent counter is a void like any other, so rule 6.7 makes the whole block invalid, to be re-run on an image with #1091.
- **`effect` refuses the pair through the existing path.** The void segment makes its block invalid (`segment_void`, rule 6.7), so the run lacks two valid paired blocks and `effect` reads `not_decidable` with no fit. The block's segment view names the reason.

## Owns
- **The decisions file X-soft-key wrote is `2026-11-23-X-soft-key.md`**, not `2026-11-24-…` as the brief named it. Its heading now carries "(#1102)", the brief's one-line edit, and nothing else in it changed.

## Tests
- **The analyzer** (`test_analyze_cr_soft.py`), on segments written as the collector writes them:
  - the soft counter missing in the treatment arm voids with `soft_timeout_counter_rendered`, with winners and alone;
  - a count of 0 in the treatment arm is valid;
  - the counter missing in the control arm (switch off) is valid and never checked;
  - hint, split and utp segments never get the check;
  - `effect` refuses a pair when a B segment of either block is void for this reason, and decides the same pair without it;
  - X-soft-key's "absent series never change the verdict" test now drops only the late counter and sampler-full time, which still don't.
- **The fleet** (`test_block.py`): the winners-length test hit the repeat rule (`["adm"] * 8`), not the length limit (#1102's review). Distinct keys can never outnumber the key table, so the test now shortens scrub's `TREATMENT_KEYS` to two keys and shows three distinct, known keys dropped by the length alone. This supersedes X-soft-key's "8 entries" line.
