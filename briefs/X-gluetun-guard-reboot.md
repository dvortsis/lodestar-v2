# X-gluetun-guard-reboot (brief, runner 2026-11-13)

**Model:** Opus. This is a leak guard for the cloud test VM. **Track:** cloud testing first (§2.62).

## Why
#841 (X-gluetun-host-guard) added a host guard for gluetun while a measured block runs (`infra/fleet/steps/vm/block-lib.sh`, `start-block.sh`, `copy-off.sh`). Its decisions file, `docs/decisions/2026-11-13-X-gluetun-host-guard.md`, lists a gap under "A reboot or an iptables flush mid-block":
- either one drops the guard while the marker still records a running block;
- gluetun then restarts by itself, unguarded;
- nothing re-holds the guard until the next start-block.

## What to build
1. **Re-hold the guard** wherever a running block is resumed or detected, using the existing `host_guard_hold` path. That covers at least:
   - boot or resume with the block marker present;
   - the outage watcher (`outage-watcher.sh`, or `watcher.sh` if that is the right owner) noticing the guard is missing while the marker says a block runs.

   Choose whichever design is simplest and fails closed: **if the guard can't be re-held, the block is marked damaged and gluetun is stopped, never left running unguarded.**
2. **Record it in the evidence.** A guard dropped and re-held mid-block is recorded in the block's evidence (the existing damage or log lines). The block must not read as clean.
3. **Tests** in `infra/fleet/tests/test_block.py` and/or `test_steps.py`, in the existing style:
   - marker present plus guard missing → re-held;
   - a failed re-hold → damaged and gluetun stopped;
   - no marker → nothing held (copy-off's leftover-guard release still works);
   - no fixed sleeps.
4. **Decisions file:** `docs/decisions/2026-11-13-X-gluetun-guard-reboot.md` (README form). It says the gap from #841's file is closed.

## Owns
- `infra/fleet/steps/vm/{block-lib.sh,start-block.sh,outage-watcher.sh,watcher.sh,vm-status.sh}`
- `infra/fleet/tests/{test_block.py,test_steps.py,helpers.py}`
- the decisions file above.

## Rules
- **No cloud or AWS calls, no terraform, no VM access.** This is code and local tests only (`infra/fleet/run_tests.sh`).
- Never print or record the test VM's IP, firewall details or VPN endpoint addresses in files.
- UTC only. No 12-digit numbers. No model identifiers.
- PR title: `X-gluetun-guard-reboot: re-hold the gluetun host guard on a resumed block`. Branch: `slice/X-gluetun-guard-reboot`.
- Follow `.claude/skills/steward/SKILL.md`. When CI is green, message the runner `ready <PR> <full sha>`.
