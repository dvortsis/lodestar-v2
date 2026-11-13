# X-gluetun-guard-reboot decisions (#PR)

This closes the gap that `2026-11-13-X-gluetun-host-guard.md` (#841) lists under "A reboot or an iptables flush mid-block". Either one drops the host guard while the marker still records a running block, and Docker restarts gluetun by itself (`restart: unless-stopped`). Before this change, nothing re-held the guard until the next start-block.

## The change
- **One check, `guard_resume`, in `block-lib.sh`.** While a block is recorded, it makes sure the guard for the endpoint in `wg0.conf` is in INPUT:
  - **Present:** nothing happens.
  - **Gone:** it's put back and recorded.
  - **Can't be held** (no valid endpoint, or iptables refuses): the block is marked damaged and gluetun is stopped (killed if it won't stop). Docker keeps a stopped `unless-stopped` container down after a reboot too, so gluetun never runs unguarded.
  - **No marker:** nothing is held. copy-off's release of a leftover guard is unchanged.
  - **Locked:** a `flock` on `$BLOCK/.guard.lock` stops the watcher and the step from checking at the same moment. A check that can't get the lock within 30 s doesn't run and says so; the next one tries again.
- **It only adds, so it doesn't call `host_guard_hold` as it stands.** `host_guard_hold` removes every other tagged copy. Mid-block, a VPN renewal can replace `wg0.conf` while gluetun keeps its old session (#841's follow-up). Removing the old endpoint's copy then would drop the guard for the session actually in use. So a resumed block inserts this endpoint's rule at the top of INPUT and checks it with `-C`, the same way `host_guard_hold` does, and leaves any other copy alone. copy-off still removes every tagged copy at the block's end.
- **Where it runs:**
  - **The outage watcher, at its start and every `GUARD_EVERY` seconds (30).** This covers an iptables flush mid-block. The watcher already loops on a 60 s read timeout; the timeout is now `GUARD_EVERY`.
  - **After a reboot.** The watcher now runs as an enabled unit (below), so it starts again at boot when the marker is present and checks first.
  - **The `outage-watcher` step,** before it starts the watcher. It prints `ok host_guarded`, `ok host_guard_reheld`, `fail host_guarded` or `fail host_guard_checked`, then `fail block_intact` for a damaged block and `count host_guard_drops <n>`. A guard that can't be held, or a damaged block, makes the step fail.
  - **vm-status reports it, read-only.** For a recorded block it adds `ok|fail block_intact` and `count host_guard_drops <n>`.
- **The watcher's unit is enabled, not transient.** This supersedes the "transient systemd unit" bullet of `2026-11-11-OP-3b.md`. A transient unit doesn't survive a reboot, so after one nothing ran until an operator restarted the watcher.
  - **The unit:** `start_watcher` writes `/etc/systemd/system/lodestar-outage-watcher.service` and runs `systemctl enable --now`.
  - **Unchanged from before:** the same name, the same `RuntimeMaxSec=44h`, no restart policy. `watcher_active` still uses `systemctl is-active`.
  - **New:** `ConditionPathExists=<block>/marker`, so it starts only while a block is recorded, and `After=docker.service`.
  - **Its paths are checked** (`^/[A-Za-z0-9._/-]+$`) before they're written into the unit.
  - **It runs from the block's own copies.** It runs `$BLOCK/tools/watcher.sh <block> <stack>`, and `start_watcher` now installs `block-lib.sh` beside it, because the watcher sources it.
  - **The block's end disables it.** `stop_watcher` is now `systemctl disable --now`, so arm-switch after the last segment and copy-off leave nothing that starts at boot. The unit file stays, inert without a marker.
  - **start-block's tool check** now needs `flock` and no longer needs `systemd-run`.
- **The watcher never stops Lodestar.** As before, an outage makes the segment invalid and the analyzer judges it. The one thing the watcher now stops is gluetun, and only when the guard can't be held.

## The evidence
- **Every record is a line in `$BLOCK/outages`.** copy-off's scrubbed document counts those lines as `outages`, so a block with a dropped guard never reads as clean. The lines are:
  - `host_guard_dropped`: no copy was left;
  - `host_guard_endpoint_changed`: only another endpoint's copy was left;
  - `host_guard_lost`: it couldn't be held. This is recorded once, together with `$BLOCK/damaged`.
- **The running segment gets a `link_outage` note,** through its collector, the same note an outage gets (CR-SPEC rule 6.11 counts notes written at the time). The note code moved from `watcher.sh` into `block-lib.sh` (`block_record`, `segment_note`), so the step and the watcher share it. It's still bounded by `MAX_NOTES`. Between segments, or after a reboot (the collectors don't restart), there's no collector to take the note, and the outage line still counts.
- **Damage is `$BLOCK/damaged`,** a file and not a marker state. arm-switch rewrites the marker at each segment, so a state in it wouldn't last; the file does. The scrubber reads only fixed files (outside this slice), so the damage reaches the evidence through the `host_guard_lost` outage line. vm-status and the step report the file.
- **Nothing prints or records the endpoint.** Output is fixed names and counts only. The tests check that the address isn't in any output.

## Tests
- **`test_block.py`, `GuardResume`:**
  - marker present and guard gone: re-held, recorded once, the segment noted, gluetun left alone;
  - another endpoint's copy is kept and the change is recorded;
  - a failed re-hold (iptables refuses, gluetun won't stop, a bad endpoint, no `wg0.conf`): damaged, gluetun stopped or killed, recorded once over repeated checks, the marker kept for copy-off;
  - no marker: the step and the watcher hold nothing;
  - the watcher after a reboot (it checks first), after a flush mid-block (a later check), when it can't hold the guard, and with a guard already held.
- **No fixed sleeps.** The flush test lets the watcher's own 1 s read timeout come round, under the test's 60 s deadline. `sleep` is stubbed as before.
- **Existing tests:** the systemd stubs now check the unit file, `daemon-reload`, `enable --now` and `disable --now`.
- **`test_steps.py`:** vm-status's new lines.

## Left as it is (for the runner)
- **A short window at boot.** The unit is ordered after `docker.service`, so gluetun starts a few seconds before the watcher's first check. A gluetun stop in those seconds isn't covered. The reboot itself also drops the host's state with no guard, and the endpoint's leftover UDP for the old session can be answered during boot, before any of this runs. That's recorded as a drop on the first check, so the block doesn't read as clean.
- **copy-off's order.** copy-off releases the guard before it stops the watcher. A watcher check that lands in that gap of milliseconds puts a guard back and records a drop after the evidence was taken. The leftover guard can't touch the tunnel, and the next start-block or copy-off removes it. Stopping the watcher before the release, in copy-off (outside this slice's paths), would close that gap.
- **The runbook** (`docs/operations/cloud-fleet-runbook.md` §3.6 and §3.5, outside this slice's paths) should name the new lines. The step's "works" result becomes `done; 3 ok; host_guard_drops <n>, outages_seen <n>`. `ok host_guard_reheld` means a guard was put back and the block isn't clean. `fail host_guarded` with `fail block_intact` means gluetun was stopped and the block is damaged: run copy-off.

This changes a slice decision (OP-3b's transient unit), not an owner decision. It's a runner §2.19 flag to the owner.
