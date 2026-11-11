#!/usr/bin/env bash
# The outage watcher (OP-3b; docs/plan/2026-11-08-cloud-test-plan.md 0e), run
# detached by start-block and the outage-watcher step as a transient systemd
# unit (RuntimeMaxSec=44h): watcher.sh <block dir>.
#
# It follows gluetun's container events and its log. On a VPN restart or a
# tunnel-down line it writes a `link_outage` note into the running segment's
# evidence at once (collect.py note, in that segment's collector), because
# CR-SPEC rule 6.11 counts notes only when they're written at the time. It
# never stops or restarts anything: an outage makes the segment invalid, and
# the analyzer judges that. It exits when the block's marker is gone.
# It prints nothing; everything it keeps stays in the block directory.
set -uo pipefail
BLOCK=${1:?block}
GAP=${GAP:-30}          # one note per burst: a restart logs several lines
MAX_NOTES=${MAX_NOTES:-500}  # bounded, whatever gluetun does; every outage is still counted

# Only a listing that works and shows no marker ends the watcher (fail closed:
# a block it can't read is still a block).
ended() {
  local list
  list=$(ls -A -- "$BLOCK" 2>/dev/null) || return 1
  ! grep -qx marker <<<"$list"
}
events() {
  while ! ended; do
    timeout 600 docker events --filter type=container --filter container=gluetun --format 'event {{.Action}}' 2>/dev/null
    sleep 5
  done
}
logs() {
  while ! ended; do
    timeout 600 docker logs -f --since 5s gluetun 2>&1 | sed -u 's/^/log /'
    sleep 5
  done
}
outage() {
  case $1 in
    "event die" | "event kill" | "event stop" | "event restart" | "event oom" | "event health_status: unhealthy") return 0 ;;
    log\ *) grep -Eqi 'restarting vpn|unhealthy|\[vpn\] stopping' <<<"$1" ;;
    *) return 1 ;;
  esac
}
note() {
  local seg notes
  echo "$(date -u +%s) link_outage" >>"$BLOCK/outages"
  notes=$(grep -c . "$BLOCK/notes" 2>/dev/null)
  ((${notes:-0} >= MAX_NOTES)) && return 0
  seg=$(sed -n 's/^segment=//p' "$BLOCK/marker" 2>/dev/null | head -n 1)
  [[ $seg =~ ^[1-4]$ ]] || return 0
  # Between segments no collector runs: the outage is counted, and there is no window to note it in.
  if timeout 30 docker exec "lodestar-collector-$seg" python3 /tools/linux/collect.py note \
    --root "/evidence/seg-$seg" --category link_outage >/dev/null 2>&1; then
    echo "$(date -u +%s) $seg" >>"$BLOCK/notes"
  fi
}

trap 'kill $(jobs -p) 2>/dev/null' EXIT
last=0
{ events & logs & wait; } | while :; do
  if IFS= read -r -t 60 line; then
    if outage "$line"; then
      now=$(date -u +%s)
      if ((now - last >= GAP)); then note; last=$now; else echo "$now link_outage" >>"$BLOCK/outages"; fi
    fi
  elif (($? <= 128)); then
    exit 0  # both followers ended: the block's over
  fi
  ended && exit 0
done
