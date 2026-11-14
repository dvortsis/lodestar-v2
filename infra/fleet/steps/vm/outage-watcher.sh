#!/usr/bin/env bash
# outage-watcher (OP-3b): make sure the block's outage watcher is running, and
# that the host guard is held (X-gluetun-guard-reboot). start-block starts the
# watcher; this step restarts it if it has stopped. First it checks the host
# guard, which a reboot or an iptables flush mid-block removes: a missing guard
# is put back and recorded in the block's evidence, and one that can't be held
# marks the block damaged and stops gluetun (block-lib.sh guard_resume). It
# needs a recorded block. Idempotent. Run as root by the step bootstrap:
# step.sh <workdir> [stack] [-] [block].
set -uo pipefail
work=${1:?workdir}
STACK=${2:-/home/lodestar/lodestar}
BLOCK=${4:-/var/lib/lodestar-block}
# shellcheck source=block-lib.sh
. "$work/block-lib.sh"

case $(block_state) in
  block) ;;
  none) echo "fail block_recorded"; exit 1 ;;
  *) echo "fail block_check"; exit 1 ;;
esac
for tool in iptables flock; do
  command -v "$tool" >/dev/null 2>&1 || { echo "fail tool_missing_$tool"; exit 1; }
done
failed=""
guard_resume "$STACK/secrets/wg0.conf"; rc=$?
case $rc in
  0) echo "ok host_guarded" ;;
  3) echo "ok host_guard_reheld" ;;
  1) echo "fail host_guarded"; failed=1 ;;
  *) echo "fail host_guard_checked"; failed=1 ;;
esac
damaged_marked && { echo "fail block_intact"; failed=1; }
n=$(guard_drops)
echo "count host_guard_drops ${n:-0}"
if watcher_active; then
  echo "skip outage_watcher_started"
elif start_watcher && watcher_active; then
  echo "ok outage_watcher_started"
else
  echo "fail outage_watcher_started"; exit 1
fi
n=$(grep -c . "$BLOCK/outages" 2>/dev/null)
echo "count outages_seen ${n:-0}"
[[ -n $failed ]] && { echo "fail outage_watcher"; exit 1; }
echo "ok outage_watcher"
