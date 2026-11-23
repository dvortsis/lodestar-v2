# X-host-ab-runsheet decisions (#1090)

Slice session, 2026-11-23Z. It adds `docs/soak/runsheets/host-stage1-ab-runsheet.md`: the operator's step-by-step sheet for the experiment host's stage 1 (CR-AB-ADM, then CR-AB-4's uTP arm). Docs only. Nothing touched the host. The runner confirmed the choices below before the sheet was written.

- **No design change.** The sheet takes every number from CR-SPEC as amended by r5.1 and says where. Stage 1 keeps the shipped-default control and has no `--winners`.
- **The spec has no early-stop rule, and the sheet says so.** A segment runs to its end unless a collector guard trips or a host-safety rule says stop. Nobody stops because of what the data shows (no peeking). A stopped segment means a re-run of the block (rule 6.7).
- **A start hold of 6 h is assumed only as the spec states it.** CR-0w (§2.77) had not merged when the sheet was written. The sheet treats `T_open` as what the collector records, so a build with no hold gives a shorter block and no other change.
- **Parity (§2.68 with CR-SPEC rule 6.5).** A block never starts while a parity operation runs, and a running one is never cancelled. The pause covers the block. A parity, sync or rebuild inside a segment invalidates it.
- **Backup: both steps.** The container is excluded and the schedule is paused. The next-run times go to the collector with `--next-run`. The restore puts both back.
- **The §2.68 failed-restore rule is at the top of the sheet** as well as in its failure table.
- **Route backoff** runs only on the runner's word after `route-baseline`; the default expectation is a recorded skip.
- **Placeholders only.** The sheet carries no host address, share, user name or credential. The image digest, commits and hashes are filled in at run time.
- **Runner-owned:** outcomes, Checkpoint 1, extra block pairs and the route decision. Not changed: any CR-SPEC number, `cloud-fleet-runbook.md`, `docs/soak/tools/**`.
- **Exclusions (runner, 2026-11-23Z).** The collector needs `auto_update=excluded` and `backup=excluded`. Excluding a container is a settings change, which §2.68 doesn't cover. The sheet requires both exclusions to be in place already and stops to ask the owner if not. A test only pauses and restores. The restore puts the schedule back. The exclusion stays.
- **The pause is per block (runner, 2026-11-23Z).** Pause at a block's start, restore at its end or at once on any stop. A due or overdue backup or scheduled parity check runs and finishes between blocks, before the next starts. Six days without backups is too long.
- **Mover** isn't pausable under §2.68 and invalidates a segment; the sheet checks its schedule before a start.
- **Missed runs between blocks (runner, §2.26, flagged §2.19).** A backup whose schedule fell inside a block is started by hand at the block's end with Appdata Backup's own start button (it changes no setting) and finishes before the next block. A missed scheduled parity check is not started by hand: it is recorded and the runner tells the owner.
- **Block 2 phase (runner).** Start at block 1's start + 36 h, give or take 1 h. CR-SPEC says only "about 12 h" (rule 6.1), so the tolerance is the runner's; a late start is recorded as an offset.
- **Fresh instance claim.** Each block's first segment claims the instance (setup token, Legal Notice, admin password equal to the password file). No other settings are saved, because saved settings override the environment.
- **Mover (runner).** Unraid's default mover is daily. A mover run inside a block means stop before starting and ask the owner through the runner; the mover schedule is the owner's setting.
