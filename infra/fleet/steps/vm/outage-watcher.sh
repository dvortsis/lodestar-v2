#!/usr/bin/env bash
# outage-watcher (OP-3b): make sure the block's outage watcher is running.
# start-block starts it; this step restarts it if it has stopped. It needs a
# recorded block. Idempotent. Run as root by the step bootstrap:
# step.sh <workdir> [-] [-] [block].
set -uo pipefail
work=${1:?workdir}
BLOCK=${4:-/var/lib/lodestar-block}
# shellcheck source=block-lib.sh
. "$work/block-lib.sh"

case $(block_state) in
  block) ;;
  none) echo "fail block_recorded"; exit 1 ;;
  *) echo "fail block_check"; exit 1 ;;
esac
if watcher_active; then
  echo "skip outage_watcher_started"
elif start_watcher && watcher_active; then
  echo "ok outage_watcher_started"
else
  echo "fail outage_watcher_started"; exit 1
fi
n=$(grep -c . "$BLOCK/outages" 2>/dev/null)
echo "count outages_seen ${n:-0}"
echo "ok outage_watcher"
