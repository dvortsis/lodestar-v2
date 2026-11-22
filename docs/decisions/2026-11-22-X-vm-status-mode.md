# X-vm-status-mode decisions (#1070)

2026-11-22Z. Runner brief `X-vm-status-mode`; a refinement, with no owner decision changed.

## Decision

- `vm-status.sh` prints the block's mode from `block.env` as a count line, `count block_mode_<mode> 1`, with `<mode>` one of `abba`, `on` or `unknown`. A missing `MODE` is `abba` (as `rules.parse` and `block-lib.sh` treat it); any other value is `unknown`. It prints nothing when no block is recorded.
- A count line, because the control side keeps only `ok|fail|skip <name>` and `count <name> <int>` lines, and `summary()` joins the kept count lines into its answer. `mode=on` would have been dropped. `steps/run.py` and `summary()` are unchanged.
- The runbook's §3.3.2 check reads this line instead of asking for a hand check of `block.env`.

## Not changed

Rules, the arms, the leak test, copy-off and every stop condition.

## Edge cases

- The mode comes from `load_block_env` (`block-lib.sh`), the reader arm-switch and copy-off use, run in a subshell. So status can't disagree with them: a record it rejects for any reason (a missing or unreadable file, a bad `ROLE`, a NUL byte, `MODE=abba`, a padded or lower-case line, CRLF) is `unknown`; a valid record with `MODE=on` is `on`; a valid one with no `MODE` is `abba`.
- `MODE=abba` in `block.env` is therefore `unknown`: start-block never writes it, and the reader rejects it.

## Scope

The runbook's §3.5 (the vm-status reference) also gains the new counter and what `unknown` means. The runner approved that edit beyond the brief's §3.3.2 check.
