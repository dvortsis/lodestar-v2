# X-gluetun-guard-reboot decisions (#845)

This closes the gap that `2026-11-13-X-gluetun-host-guard.md` (#841) lists under "A reboot or an iptables flush mid-block". Either one drops the host guard while the marker still records a running block, and Docker restarts gluetun by itself (`restart: unless-stopped`). Before this change, nothing re-held the guard until the next start-block.

## The change
- **One check, `guard_resume`, in `block-lib.sh`.** While a guard is expected, it makes sure the guard for the endpoint in `wg0.conf` is in INPUT. Expected means a recorded block; at boot it also means a block that ended (arm-switch after the last segment removes the marker) but isn't copied off yet, because copy-off is what releases the guard.
  - **Present:** nothing happens.
  - **Gone:** it's put back first, then recorded.
  - **Can't be held** (no valid endpoint, iptables refuses, or the lock file can't be opened): gluetun's restart policy is turned off and gluetun is stopped (killed if it won't stop), then the block is marked damaged and it's recorded. With the policy off, neither a kill nor a reboot brings gluetun back. The next leak test recreates it from `compose.yaml`, which restores the policy.
  - **Every iptables call in the check waits for the xtables lock** (`-w 5`), so Docker editing rules at the same moment doesn't read as an error.
  - **Only exit 1 from `-C` means gone,** as in `drop_all`. Any other exit (the xtables lock while Docker edits rules, say) is tried three times, 2 s apart, and then fails closed.
  - **Nothing expected:** nothing is held. copy-off's release of a leftover guard is unchanged.
  - **Locked:** a `flock` on `$BLOCK/.guard.lock` keeps the checks from running at the same moment. A check that waits `GUARD_LOCK_WAIT` (15 s) without the lock doesn't run, records nothing and says so; the next one tries again. Only real contention means "try again": a lock file that can't be opened fails closed.
- **It only adds, so it doesn't call `host_guard_hold` as it stands.** `host_guard_hold` removes every other tagged copy. Mid-block, a VPN renewal can replace `wg0.conf` while gluetun keeps its old session (#841's follow-up). Removing the old endpoint's copy then would drop the guard for the session actually in use. So a resumed block inserts this endpoint's rule at the top of INPUT and checks it with `-C`, the same way `host_guard_hold` does, and leaves any other copy alone. copy-off still removes every tagged copy at the block's end.
  - **A renewal isn't a drop.** When only another endpoint's copy is left, the guard for the session in use is still there, so the new copy is added as a precaution and nothing is recorded. Only a flush or a reboot leaves no copy at all, and only that is recorded.
- **Where it runs:**
  - **At boot, before Docker.** `lodestar-block-guard.service` is a oneshot.
    - **Ordering:** `DefaultDependencies=no`, `After=local-fs.target netfilter-persistent.service ufw.service firewalld.service` (a host firewall that loads rules at boot goes first; a unit that doesn't exist is ignored), and `Before=network-pre.target docker.service`.
    - **What it runs:** `watcher.sh <block> <stack> boot`, which does the one check without any Docker call.
    - **Docker requires it.** A drop-in, `docker.service.d/lodestar-block-guard.conf`, adds `Requires=` and `After=`. So a guard that can't be held at boot fails the unit, and Docker, and with it gluetun, stays down. Docker starting again mid-block (a restart, or socket activation) runs the check again.
    - **When it runs:** whenever the block has a marker or evidence (trigger conditions, `ConditionPathExists=|<block>/marker` and `|<block>/evidence`). A block whose staged script is missing then fails the unit rather than being skipped, because a skipped unit would satisfy `Requires=` and let Docker start. With nothing expected (no block, or one already copied off), it exits 0 and Docker starts as usual. It's never disabled.
    - **Installed by start-block, before the marker** (`boot_guard_install`, which stages `watcher.sh` and `block-lib.sh` into `$BLOCK/tools`). It's in place for the whole block. A failed install fails start-block with `fail boot_guard_installed` and leaves no marker, so nothing is recorded and the next start-block sets that directory aside. (The runner asked for right after the marker; just before it keeps the same coverage without leaving a recorded block that copy-off can't end.) `start_watcher` installs it again each time.
  - **The outage watcher, at its start, every `GUARD_EVERY` seconds (30), and at once on every outage line.** This covers an iptables flush mid-block. The watcher already looped on a 60 s read timeout. It now waits only for what's left of the interval, so a busy log can't push the check back. A gluetun stop or restart is the moment a missing guard matters, so an outage line checks the guard before it's noted.
  - **The `outage-watcher` step,** before it starts the watcher. It prints `ok host_guarded`, `ok host_guard_reheld`, `fail host_guarded` or `fail host_guard_checked`, then `fail block_intact` for a damaged block and `count host_guard_drops <n>`. A guard that can't be held, a check that didn't run or a damaged block makes the step fail. It now also needs `flock`.
  - **vm-status reports it, read-only.** For a recorded block it adds `ok|fail block_intact` and `count host_guard_drops <n>`.
- **The watcher's unit is enabled, not transient.** This supersedes the "transient systemd unit" bullet of `2026-11-11-OP-3b.md`. A transient unit doesn't survive a reboot, so after one nothing ran until an operator restarted the watcher.
  - **The unit:** `start_watcher` writes `/etc/systemd/system/lodestar-outage-watcher.service` (and the boot guard) and runs `systemctl enable --now`.
  - **Unchanged from before:** the same name, the same `RuntimeMaxSec=44h`, no restart policy. `watcher_active` still uses `systemctl is-active`.
  - **New:** `ConditionPathExists=<block>/marker`, so it starts only while a block is recorded, and `After=docker.service`.
  - **Its paths are checked** (`^/[A-Za-z0-9._/-]+$`) before they're written into any unit.
  - **It runs from the block's own copies.** It runs `$BLOCK/tools/watcher.sh <block> <stack>`, and `start_watcher` now installs `block-lib.sh` beside it, because the watcher sources it.
  - **The block's end disables it.** `stop_watcher` is now `systemctl disable --now`. The unit file stays, inert without a marker.
  - **start-block's tool check** now needs `flock` and no longer needs `systemd-run`.
- **The watcher never stops Lodestar.** As before, an outage makes the segment invalid and the analyzer judges it. The one thing the watcher now stops is gluetun, and only when the guard can't be held.

## The evidence
- **Every record is a line in `$BLOCK/outages`.** copy-off's scrubbed document counts those lines as `outages`, so a block with a dropped guard never reads as clean. The lines are:
  - `host_guard_dropped`: no copy was left;
  - `host_guard_lost`: it couldn't be held. This is recorded once, together with `$BLOCK/damaged`. A `host_guard_dropped` from the same failure is recorded with it, also once; later failed checks stop gluetun again but record nothing more.
- **The running segment gets one `link_outage` note per check,** through its collector, the same note an outage gets (CR-SPEC rule 6.11 counts notes written at the time). It's written after the guard is back, or after gluetun is stopped, so a slow collector can't delay either. The note code moved from `watcher.sh` into `block-lib.sh` (`block_record`, `segment_note`), so the step and the watcher share it. It's still bounded by `MAX_NOTES`. There's no note at boot, when Docker isn't up, or between segments, when no collector runs; the outage line still counts.
- **Damage is `$BLOCK/damaged`,** a file and not a marker state. arm-switch rewrites the marker at each segment, so a state in it wouldn't last; the file does. The scrubber reads only fixed files (outside this slice), so the damage reaches the evidence through the `host_guard_lost` outage line. vm-status and the step report the file.
- **Nothing prints or records the endpoint.** Output is fixed names and counts only, and the boot check prints nothing at all. The tests check that the address isn't in any output.

## Tests
- **`test_block.py`, `GuardResume`:**
  - marker present and guard gone: put back, recorded once, the segment noted, gluetun left alone;
  - another endpoint's copy is kept, and the new copy is added without a record;
  - a failed re-hold (iptables refuses, gluetun won't stop, a bad endpoint, no `wg0.conf`, a lock file that can't be opened): gluetun's restart policy is off and gluetun is stopped or killed, the block is damaged, `host_guard_lost` is recorded once over repeated checks, and the marker is kept for copy-off;
  - an iptables error on `-C` is tried again, and fails closed if it persists;
  - a lock held by another process (`fcntl.flock` from the test) gives `fail host_guard_checked`, with nothing recorded or stopped;
  - unit paths with a space, `%` or a newline are refused before any unit is written;
  - no marker: the step and the watcher hold nothing;
  - the boot check: it holds the guard with no Docker call, a failure exits 1 (which keeps Docker down), and it covers the gap before copy-off but does nothing once the block is copied off or there's no block;
  - the watcher after a reboot (it checks first), after a flush mid-block (a later check), when it can't hold the guard, and with a guard already held.
- **No fixed sleeps.** The flush test lets the watcher's own 1 s read timeout come round, under the test's 60 s deadline. `sleep` is stubbed as before, which also covers the retry pause.
- **Existing tests:** the systemd stubs now check the units, the drop-in, `daemon-reload`, `enable --now` and `disable --now`.
- **`test_steps.py`:** vm-status's new lines, including `fail boot_guard_held` beside `fail docker_up`, and the step's 180 s timeout.
- **`test_block.py`, `CopyOff`:** the watcher stops before the guard comes off.

## The runner's calls on the outside-owns items (2026-11-13)
The runner approved all four for this PR (§2.26, §2.33):
1. **`infra/fleet/steps/run.py`:** the `outage-watcher` step's timeout goes from 60 s to 180 s. In the worst case, `guard_resume` takes about 15 s for the lock, 4 s of retries, then 15 + 30 + 15 + 30 s to keep gluetun down (update, stop, kill, inspect) and 30 s for the note. A test pins the value.
2. **`copy-off.sh`:** the watcher now stops before the guard is released. A watcher check can no longer put a guard back after copy-off has removed it. A test checks the order.
3. **`docs/operations/cloud-fleet-runbook.md` §3.5–§3.7:** now names the new lines and what to do about each.
4. **Both §2.19 flags were accepted.** The runner passes them to the owner at merge:
   - **(a) The watcher is an enabled unit, not transient.** This supersedes the transient-unit choice in `2026-11-11-OP-3b.md`. The reason: a transient unit is gone after a reboot. Mid-block, nothing then watched gluetun or re-held the guard until someone ran the step by hand. An enabled unit with `ConditionPathExists=<block>/marker` starts again by itself and does nothing once the block has ended.
   - **(b) The boot guard that Docker requires.** If the guard can't be held at boot, Docker stays down on that VM, which fails closed. vm-status reports it as `fail boot_guard_held` (from `systemctl is-failed`), so a tester sees why `docker_up` fails.

## Left as it is (for the runner)
- **The boot window is closed, apart from boot itself.** From power-on until the boot guard runs, the host has no guard, and the endpoint's leftover UDP for the old session can be answered then. That happens before the network is configured, and the first check records it as a drop, so the block doesn't read as clean.
- **After a failed boot guard, the evidence is stranded, and that's accepted.** Docker stays down on that VM until the guard can be held, so copy-off, which runs the scrubber in a container, can't run there either. No fleet step recovers it: the block is lost, and the runbook says to tear the fleet down. A failed boot guard means the VM can't keep its own leak guard, so its evidence wouldn't count anyway.
- **A short window after a flush.** Between an iptables flush and the watcher's next check (at most 30 s, or at once on a gluetun outage line) the host has no guard. A gluetun stop in that window is checked when its event arrives, but the host may already have answered the endpoint's leftover UDP by then. The check records the drop, so the block doesn't read as clean.
- **A VPN renewal and a flush together.** After a renewal mid-block, a flush removes the guard for both endpoints. The check then puts back only the endpoint in `wg0.conf`, the new one, while gluetun's session still uses the old one. A gluetun stop after that can leak to the old endpoint. The drop is recorded, so the block isn't clean. The fix belongs to the renewal side (#841's follow-up: renew-vpn refusing mid-block, or recording the session's endpoint).
- **The scrubbed document has no `host_guard_drops` or `damaged` field.** The drops and the loss reach it as outage lines only. Adding the fields to `scrub.py` is outside this slice; the runner logged it as a follow-up.

This changes a slice decision (OP-3b's transient unit), not an owner decision. The runner accepted it as a §2.19 flag to the owner.
