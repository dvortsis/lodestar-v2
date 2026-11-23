# X-host-ab-runsheet decisions

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
