#!/usr/bin/env bash
# vm-status (OP-3a): fixed names and counters only. The control side keeps
# only lines in its grammar (`ok|fail|skip <name>`, `count <name> <int>`), so
# nothing here may print an address, a name or an ID, and nothing does.
# Run as root by the step bootstrap: step.sh <workdir>.
set -uo pipefail
STACK=${2:-/home/lodestar/lodestar}  # tests pass a temporary stack; the bootstrap never does
PIA_RUN=${3:-/run/lodestar-pia}

running() {  # exactly that container name, running
  [[ "$(docker inspect -f '{{.State.Running}}' "$1" 2>/dev/null)" == "true" ]]
}
say() { if "$@" >/dev/null 2>&1; then echo "ok $NAME"; else echo "fail $NAME"; fi; }
digits() { tr -dc '0-9' | head -c 15; }  # counters are bare integers, whatever a tool printed

NAME=docker_up; say docker info --format '{{.ServerVersion}}'
NAME=wg_conf_present; say test -s "$STACK/secrets/wg0.conf"
NAME=gluetun_running; say running gluetun
NAME=lodestar_running; say running lodestar
# PIA delivery's work directory is gone (this step holds the VM's lock, so no
# delivery can be running now).
NAME=run_clean; say test ! -e "$PIA_RUN"

collectors=$(docker ps --filter 'name=^lodestar-collector' --format '{{.ID}}' 2>/dev/null | grep -c . | digits)
echo "count collectors_running ${collectors:-0}"
free_kib=$(df -Pk / 2>/dev/null | awk 'NR==2 {print $4}' | digits)
echo "count disk_free_gib $(( 10#${free_kib:-0} / 1048576 ))"
avail_kib=$(awk '/^MemAvailable:/ {print $2}' /proc/meminfo 2>/dev/null | digits)
echo "count mem_available_mib $(( 10#${avail_kib:-0} / 1024 ))"
up=$(cut -d. -f1 /proc/uptime 2>/dev/null | digits)
echo "count uptime_hours $(( 10#${up:-0} / 3600 ))"
exit 0
