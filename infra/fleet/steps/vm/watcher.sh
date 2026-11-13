#!/usr/bin/env bash
# The outage watcher (OP-3b; docs/plan/2026-11-08-cloud-test-plan.md 0e), run
# detached by start-block and the outage-watcher step as an enabled systemd
# unit (RuntimeMaxSec=44h; block-lib.sh start_watcher), so it starts again
# after a reboot mid-block: watcher.sh <block dir> <stack>.
#
# It follows gluetun's container events and its log. On a VPN restart or a
# tunnel-down line it writes a `link_outage` note into the running segment's
# evidence at once (collect.py note, in that segment's collector), because
# CR-SPEC rule 6.11 counts notes only when they're written at the time. It
# never stops or restarts Lodestar: an outage makes the segment invalid, and
# the analyzer judges that.
# It also keeps the host guard (X-gluetun-guard-reboot): at its start and every
# GUARD_EVERY seconds, a guard an iptables flush removed is put back and
# recorded, and one that can't be held marks the block damaged and stops
# gluetun (block-lib.sh guard_resume). With a third argument, `boot`, it is the
# boot guard instead: one check before Docker starts, then it exits.
# It exits when the block's marker is gone.
# It prints nothing; everything it keeps stays in the block directory.
set -uo pipefail
BLOCK=${1:?block}
STACK=${2:?stack}
GAP=${GAP:-30}          # one note per burst: a restart logs several lines
MAX_NOTES=${MAX_NOTES:-500}  # bounded, whatever gluetun does; every outage is still counted
GUARD_EVERY=${GUARD_EVERY:-30}
# shellcheck source=block-lib.sh
. "${BASH_SOURCE[0]%/*}/block-lib.sh"

# watcher.sh <block> <stack> boot: the boot guard's one check, before Docker
# starts (block-lib.sh start_watcher). Any failure fails the unit, which keeps
# Docker, and so gluetun, down.
if [[ ${3:-} == boot ]]; then
  guard_resume "$STACK/secrets/wg0.conf" boot; rc=$?
  ((rc == 0 || rc == 3)) && exit 0
  exit 1
fi

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
# Between segments no collector runs: the outage is counted, and there is no window to note it in.
note() { block_record "" link_outage; }
guard() { guard_resume "$STACK/secrets/wg0.conf"; checked=$SECONDS; }

checked=0
guard

trap 'kill $(jobs -p) 2>/dev/null' EXIT
last=0
{ events & logs & wait; } | while :; do
  ((SECONDS - checked >= GUARD_EVERY)) && guard
  if IFS= read -r -t "$GUARD_EVERY" line; then
    if outage "$line"; then
      now=$(date -u +%s)
      if ((now - last >= GAP)); then note; last=$now; else echo "$now link_outage" >>"$BLOCK/outages"; fi
    fi
  elif (($? <= 128)); then
    exit 0  # both followers ended: the block's over
  fi
  ended && exit 0
done
