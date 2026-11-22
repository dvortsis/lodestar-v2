# X-vm-status-mode: vm-status shows a block's mode

2026-11-22Z. Runner brief `X-vm-status-mode`; a refinement, with no owner decision changed.

## Decision
- `vm-status.sh` prints the block's mode from `block.env` as a count line, `count block_mode_<mode> 1`, with `<mode>` one of `abba`, `on` or `unknown`. A missing `MODE` is `abba` (as `rules.parse` and `block-lib.sh` treat it); any other value is `unknown`. It prints nothing when no block is recorded.
- A count line, because the control side keeps only `ok|fail|skip <name>` and `count <name> <int>` lines, and `summary()` joins the kept count lines into its answer. `mode=on` would have been dropped. `steps/run.py` and `summary()` are unchanged.
- The runbook's §3.3.2 check reads this line instead of asking for a hand check of `block.env`.

## Not changed
Rules, the arms, the leak test, copy-off and every stop condition.

## Edge cases
- A marker with no readable `block.env`, more than one `MODE=` line, or any value but `on` or `abba` reads `unknown`. Only a readable file with no `MODE` key is `abba`.
- `MODE=abba` in `block.env` reads `abba`, as the brief asks, although `block-lib.sh`'s `load_block_env` rejects it (start-block never writes it). `unknown` stays for what vm-status can't vouch for.
