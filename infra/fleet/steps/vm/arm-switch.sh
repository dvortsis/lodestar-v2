#!/usr/bin/env bash
# arm-switch (OP-3b; docs/plan/2026-11-08-cloud-test-plan.md 0e): the block's
# next segment. The operator runs it shortly before a segment ends.
# 1. Wait (bounded) for the running segment's collector to finish, with its
#    window complete; it has stopped Lodestar (--stop).
# 2. After the last segment, end the block: the marker goes and the watcher
#    stops. Otherwise record the next segment in the marker, start Lodestar
#    alone with the next arm (gluetun isn't recreated; safe restart makes it a
#    resumed start), and start that segment's collector with a new root.
# A segment that didn't complete its window stops here: the block stays
# recorded (mid-block for every other check) until copy-off ends it.
# Run as root by the step bootstrap: step.sh <workdir> [stack] [-] [block].
set -uo pipefail
work=${1:?workdir}
STACK=${2:-/home/lodestar/lodestar}
BLOCK=${4:-/var/lib/lodestar-block}
WAIT_SECONDS=${WAIT_SECONDS:-900}  # the step's timeout leaves room for this and the switch
# shellcheck source=block-lib.sh
. "$work/block-lib.sh"

started=""
fail() {
  [[ -n $started ]] && { stop_lodestar || echo "fail lodestar_stopped"; stop_proxy; }
  echo "fail $1"; echo "fail arm_switch"; exit 1
}

case $(block_state) in
  block) ;;
  none) fail block_recorded ;;
  *) fail block_check ;;
esac
id=$(marker_get block)
seg=$(marker_get segment)
[[ $id =~ ^[0-9]{1,20}-[0-9]{1,6}$ && $seg =~ ^[1-4]$ ]] || fail marker_valid
load_block_env || fail block_env
command -v docker >/dev/null 2>&1 || fail tool_missing_docker
result=$BLOCK/evidence/seg-$seg/result.json

# 1. The running segment ends by itself; this only waits for it.
start=$SECONDS
while :; do
  if [[ -s $result ]] && ! running "lodestar-collector-$seg"; then break; fi
  ((SECONDS - start >= WAIT_SECONDS)) && fail segment_ended
  sleep 10
done
echo "ok segment_ended"
grep -Eq '"reason": *"baseline_window_complete"' "$result" || fail segment_complete
echo "ok segment_complete"
running lodestar && fail lodestar_stopped
echo "ok lodestar_stopped"
timeout 60 docker rm "lodestar-collector-$seg" >/dev/null 2>&1 || true  # its evidence is in $BLOCK

# 2. The block's end, or the next arm.
if ((seg == 4)); then
  stop_watcher
  stop_proxy
  rm -f -- "$BLOCK/marker" && sync "$BLOCK" || fail block_ended
  echo "ok block_ended"
  echo "count block_segment 4"
  echo "ok arm_switch"
  exit 0
fi
next=$((seg + 1))
printf 'block=%s\nsegment=%s\nstate=running\n' "$id" "$next" | marker_write || fail marker_written
started=1
start_lodestar "$next" || fail lodestar_started
network_parent_ok || fail network_parent
start_proxy || fail docker_proxy  # the new container's ID; no collector runs yet
wait_api || fail api_up
echo "ok lodestar_started"
start_collector "$next" || fail collector_started
collector_ok "$next" || fail collector_started
echo "ok collector_started"
watcher_active || start_watcher || echo "fail outage_watcher"
echo "count block_segment $next"
echo "count switch_seconds $(($(date +%s) - $(stat -c %Y "$result")))"  # from the segment's end
echo "ok arm_switch"
