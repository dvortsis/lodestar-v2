# block-lib.sh (OP-3b): shared by the fleet's block steps. Sourced, never run.
# The caller sets BLOCK (tests pass a temporary one) before sourcing.
#
# The durable block marker, $BLOCK/marker: start-block writes it before Lodestar
# or any collector starts, and only the block's own end (arm-switch after the
# last segment) or copy-off removes it. Every mid-block check reads it as well
# as `docker ps`, so a block that's stopped between segments still counts.
# Fail closed: a marker that can't be read counts as a block.
BLOCK=${BLOCK:-/var/lib/lodestar-block}
# Pinned by multi-arch index digest (docs/decisions/2026-11-09-OP-3a.md and
# 2026-11-11-OP-3b.md); a change is a reviewed PR.
PROBE_IMAGE=nicolaka/netshoot@sha256:b09d9b21381f47a79b3cbcb30da25266dc17186ea00ae65e99fdc51396f48e70
PROXY_IMAGE=haproxy@sha256:5d97434a423c2533cfeb42874d45cf6d69a840b60334582dfbfd5008e94c80be  # also in compose.yaml
COLLECTOR_IMAGE=python@sha256:848ba4413eb897e225159b8fc1b02094576cbae4aa73fc13142608ae2c8c0e32
LODESTAR_REPO=ghcr.io/dvortsis/lodestar-v2
WATCHER_UNIT=lodestar-outage-watcher
API=http://127.0.0.1:8090  # Lodestar on gluetun's loopback, which the throwaway containers share
DOCKER_SOCK=${DOCKER_SOCK:-/var/run/docker.sock}  # read for its group only; only the proxy mounts it
HEX64='^[0-9a-f]{64}$'

# The host guard (X-gluetun-host-guard): an INPUT DROP for the VPN endpoint's
# UDP, tagged lodestar-leak-host. When gluetun stops or is recreated, Docker
# flushes the container's conntrack entries, so the endpoint's leftover UDP for
# the old session goes to the host's own INPUT and the host's kernel would
# answer it ("port unreachable") from its public address, outside the tunnel.
# The guard can't touch the tunnel: a live session's replies are de-NATed and
# forwarded, never delivered to the host. start-block holds it for the whole
# block (from before the leak test recreates gluetun), copy-off removes it; a
# leak test run on its own holds its own copy for its run.
HOST_TAG=lodestar-leak-host
IPV4='^((25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])\.){3}(25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])$'
# endpoint_read conf: the WireGuard endpoint from the root-only wg0.conf, into
# ep_ip and ep_port, validated before it reaches iptables: IPv4 only (the guards
# are IPv4 rules) and a port of 1-65535. It never leaves this VM or is printed.
endpoint_read() {
  local ip port
  ep_ip="" ep_port=""
  ip=$(sed -n 's/^Endpoint = \([^:]*\):\([0-9]*\)$/\1/p' "$1" 2>/dev/null | head -n 1)
  port=$(sed -n 's/^Endpoint = \([^:]*\):\([0-9]*\)$/\2/p' "$1" 2>/dev/null | head -n 1)
  [[ $ip =~ $IPV4 && $port =~ ^[0-9]{1,5}$ ]] || return 1
  ((10#$port >= 1 && 10#$port <= 65535)) || return 1
  ep_ip=$ip ep_port=$((10#$port))
}
host_guard_rule() {  # sets host_rule, for the endpoint endpoint_read read
  host_rule=(-s "$ep_ip/32" -p udp --sport "$ep_port" -m comment --comment "$HOST_TAG" -j DROP)
}
# host_guard_hold: exactly one host guard, this endpoint's, at the top of INPUT.
# The new copy goes in first and the rest (a stuck copy, a duplicate, or an
# earlier endpoint's) come out after, so there's never a moment without one.
# Re-running it leaves the same single rule.
host_guard_hold() {
  iptables -I INPUT 1 "${host_rule[@]}" 2>/dev/null || return 1
  clear_tagged INPUT "$HOST_TAG" 1 || return 1
  iptables -C INPUT "${host_rule[@]}" 2>/dev/null
}
host_guard_release() { clear_tagged INPUT "$HOST_TAG"; }  # every copy, whatever endpoint it names
# drop_all chain rule...: remove every copy of a rule (a failed delete can
# leave more than one). Only `-C` exit 1 means absent (iptables also uses 1
# for some generic errors, such as a missing chain); any other exit is an
# error, never read as "gone". Fails when a copy may still be there.
drop_all() {
  local chain=$1 i rc
  shift
  for ((i = 0; i < 5; i++)); do
    iptables -C "$chain" "$@" 2>/dev/null; rc=$?
    ((rc == 1)) && return 0
    ((rc == 0)) || return 1
    iptables -D "$chain" "$@" 2>/dev/null || return 1
  done
  iptables -C "$chain" "$@" 2>/dev/null; rc=$?
  ((rc == 1))
}
# clear_tagged chain tag [keep]: remove every rule in the chain that carries
# the comment tag, whatever endpoint it names, but the first `keep` (default
# 0). Each line of `iptables -S` is turned from -A into -D and run as words,
# never through a shell. Deleting by a duplicate's spec removes the first
# identical copy, which leaves the same rule in place.
clear_tagged() {
  local chain=$1 tag=$2 keep=${3:-0} i out line words
  for ((i = 0; i < 10; i++)); do
    out=$(iptables -S "$chain" 2>/dev/null) || return 1
    line=$(printf '%s\n' "$out" | grep -e "--comment $tag " | sed -n "$((keep + 1))p")
    [[ -z $line ]] && return 0
    read -ra words <<<"$line"
    [[ ${words[0]} == -A && ${words[1]} == "$chain" ]] || return 1
    words[0]=-D
    iptables "${words[@]}" 2>/dev/null || return 1
  done
  return 1
}

# none | block | unknown
block_state() {
  local list
  if [[ ! -e $BLOCK && ! -L $BLOCK ]]; then echo none; return; fi
  if [[ -L $BLOCK || ! -d $BLOCK ]]; then echo unknown; return; fi
  list=$(ls -A -- "$BLOCK" 2>/dev/null) || { echo unknown; return; }
  if grep -qx marker <<<"$list"; then echo block; else echo none; fi
}
# The marker's value for $1, or nothing. Callers validate it.
marker_get() { sed -n "s/^$1=//p" "$BLOCK/marker" 2>/dev/null | head -n 1; }
# Replace the marker atomically with the key=value lines on stdin.
marker_write() {
  cat >"$BLOCK/.marker.tmp" && sync "$BLOCK/.marker.tmp" && mv -f -- "$BLOCK/.marker.tmp" "$BLOCK/marker" \
    && sync "$BLOCK"
}
# The block's own processes are running: Lodestar or a collector. Exit 2 when
# Docker can't say (fail closed: the caller treats it as running).
block_procs() {
  local names
  names=$(timeout 30 docker ps --format '{{.Names}}' 2>/dev/null) || return 2
  grep -Eq '^(lodestar|lodestar-collector.*)$' <<<"$names"  # never `printf | grep -q` under pipefail (SIGPIPE)
}
collectors_running() {  # 0 yes, 1 no, 2 Docker can't say
  local names
  names=$(timeout 30 docker ps --format '{{.Names}}' 2>/dev/null) || return 2
  grep -Eq '^lodestar-collector' <<<"$names"
}
stop_lodestar() { timeout 90 docker stop -t 60 lodestar >/dev/null 2>&1; }
running() { [[ "$(timeout 30 docker inspect --type container -f '{{.State.Running}}' "$1" 2>/dev/null)" == "true" ]]; }
# A throwaway container in gluetun's namespace (no capabilities), for Lodestar's API.
api() { timeout 30 docker run --rm -i --network container:gluetun --cap-drop ALL "$PROBE_IMAGE" curl -s -m 20 "$@"; }
api_up() { [[ "$(api -o /dev/null -w '%{http_code}' "$API/healthz" 2>/dev/null)" == "200" ]]; }
wait_api() {  # at least one try, then up to API_WAIT seconds (tests shorten it)
  local until=$((SECONDS + ${API_WAIT:-180}))
  while :; do
    api_up && return 0
    ((SECONDS >= until)) && return 1
    sleep 3
  done
}
# Lodestar's arm for segment $1 of the ABBA block: control, treatment, treatment, control.
arm_of() { case $1 in 2 | 3) echo treatment ;; *) echo control ;; esac; }
# Start Lodestar (alone: gluetun isn't recreated) with segment $1's arm. Every
# switch but the treatment's stays at its shipped default; safe restart is on
# for the whole block, so each later start is a resumed one.
start_lodestar() {  # $1 segment, $2 "fresh" to recreate whatever compose thinks
  local value=false recreate=()
  [[ ${2:-} == fresh ]] && recreate=(--force-recreate)
  [[ $(arm_of "$1") == treatment ]] && value=true
  (
    unset LODESTAR_DHT_COORDINATED_ADMISSION LODESTAR_DHT_UTP LODESTAR_DHT_ROUTE_BACKOFF
    export LODESTAR_IMAGE="$LODESTAR_REPO@sha256:$IMAGE_DIGEST" LODESTAR_DHT_SAFE_RESTART=true \
      "$TREATMENT_ENV=$value" DOCKER_GID=unused
    timeout 240 docker compose -f "$STACK/compose.yaml" --project-directory "$STACK" \
      --profile block up -d --no-deps "${recreate[@]}" lodestar >/dev/null 2>&1
  )
}
# Lodestar shares gluetun's namespace (plan Lane L: checked before each collector starts).
network_parent_ok() {
  local id mode
  id=$(timeout 30 docker inspect --type container -f '{{.Id}}' gluetun 2>/dev/null) || return 1
  mode=$(timeout 30 docker inspect --type container -f '{{.HostConfig.NetworkMode}}' lodestar 2>/dev/null) || return 1
  [[ -n $id && $mode == "container:$id" ]]
}
# The collector's Docker proxy (docker-proxy.cfg), rendered with the IDs of the
# Lodestar container just started, its image and gluetun, read here on the
# host. It's rendered only while no collector runs: at start-block, and at each
# arm switch, whose new Lodestar container has a new ID.
start_proxy() {
  local lid img gid sockgid
  lid=$(timeout 30 docker inspect --type container -f '{{.Id}}' lodestar 2>/dev/null)
  img=$(timeout 30 docker inspect --type container -f '{{.Image}}' lodestar 2>/dev/null)
  img=${img#sha256:}
  gid=$(timeout 30 docker inspect --type container -f '{{.Id}}' gluetun 2>/dev/null)
  [[ $lid =~ $HEX64 && $img =~ $HEX64 && $gid =~ $HEX64 ]] || return 1
  sockgid=$(stat -c %g -- "$DOCKER_SOCK" 2>/dev/null)
  [[ $sockgid =~ ^[0-9]{1,10}$ ]] || return 1
  mkdir -p -m 0755 -- "$STACK/proxy" || return 1
  sed -e "s/@LODESTAR_ID@/$lid/g" -e "s/@IMAGE_ID@/$img/g" -e "s/@GLUETUN_ID@/$gid/g" \
    "$work/docker-proxy.cfg" >"$STACK/proxy/haproxy.cfg" && chmod 0644 -- "$STACK/proxy/haproxy.cfg" || return 1
  # The socket's folder: the proxy's user (the image's haproxy, uid 99) makes the
  # socket; the collector can only pass through.
  rm -rf -- "$STACK/proxy-sock" && install -d -o "${PROXY_UID:-99}" -m 0711 -- "$STACK/proxy-sock" || return 1
  DOCKER_GID=$sockgid LODESTAR_IMAGE=unused timeout 120 docker compose -f "$STACK/compose.yaml" \
    --project-directory "$STACK" --profile block up -d --no-deps --force-recreate docker-proxy >/dev/null 2>&1 \
    || return 1
  local until=$((SECONDS + ${PROXY_WAIT:-30}))
  while [[ ! -S $STACK/proxy-sock/docker.sock ]]; do
    ((SECONDS >= until)) && return 1
    sleep 1
  done
}
# Segment $1's collector, detached, in gluetun's namespace (CR-SPEC rehearsal
# kind `ab`; plan Lane L and the cloud screen). It stops Lodestar when its
# window ends (--stop). Raw evidence stays in $BLOCK/evidence on this VM. No
# credentials but Lodestar's password; every mount is read-only except its own
# evidence folder. Docker's API only through the proxy's socket (start_proxy).
start_collector() {
  local seg=$1 next guard=()
  next=$(date -u -d '+3 days' +%Y-%m-%dT%H:%M:%SZ)  # after any fleet's teardown (44 h at most)
  [[ $ROLE == worker ]] && guard=(--min-free-bytes 12884901888 --warn-free-bytes 8589934592
    --stop-free-bytes 4294967296)  # the 25 GB disk (plan Lane K)
  mkdir -m 0700 -- "$BLOCK/evidence/seg-$seg" || return 1
  timeout 120 docker run -d --name "lodestar-collector-$seg" --network container:gluetun --restart no \
    --oom-score-adj -500 --cap-drop ALL --security-opt no-new-privileges:true --read-only --tmpfs /tmp \
    -v "$STACK/proxy-sock:/sock:ro" -v /proc:/host/proc:ro -v /sys/fs/cgroup:/sys/fs/cgroup:ro \
    -v "$BLOCK/tools:/tools:ro" -v "$BLOCK/password:/secrets/password:ro" \
    -v "$BLOCK/evidence/seg-$seg:/evidence/seg-$seg" -v "$STACK/data:/data:ro" -v "$STACK/witness:/witness:ro" \
    "$COLLECTOR_IMAGE" python3 /tools/linux/collect.py --mode docker --docker-socket /sock/docker.sock \
    --container lodestar --url "$API" \
    --run-kind ab --treatment "$TREATMENT" --intensity balanced --stop --rehearsal \
    --baseline-hours "$SEGMENT_HOURS" --commit "$LODESTAR_COMMIT" --spec-commit "$SPEC_COMMIT" \
    --password-file /secrets/password --root "/evidence/seg-$seg" --proc-root /host/proc \
    --storage-path /data --storage-path /witness "${guard[@]}" \
    --checklist auto_update=excluded --next-run "auto_update=$next" \
    --checklist backup=excluded --next-run "backup=$next" >/dev/null 2>&1
}
# The collector really started: its run.json, with Lodestar in gluetun's
# namespace (plan Lane L), while it still runs. A preflight refusal exits.
collector_ok() {
  local seg=$1 run=$BLOCK/evidence/seg-$1/run.json until=$((SECONDS + ${COLLECT_WAIT:-120}))
  while :; do
    running "lodestar-collector-$seg" || return 1
    if [[ -s $run ]]; then grep -Eq '"gluetun_network_parent": *true' "$run"; return; fi
    ((SECONDS >= until)) && return 1
    sleep 3
  done
}
# The values start-block recorded, re-validated on every read.
load_block_env() {
  local f=$BLOCK/block.env k v
  [[ -f $f ]] || return 1
  while IFS='=' read -r k v; do
    case $k in
      IMAGE_DIGEST) [[ $v =~ ^[0-9a-f]{64}$ ]] || return 1 ;;
      LODESTAR_COMMIT | SPEC_COMMIT | RULES_COMMIT) [[ $v =~ ^[0-9a-f]{40}$ ]] || return 1 ;;
      TREATMENT) [[ $v =~ ^(adm|utp|route)$ ]] || return 1 ;;
      SEGMENT_HOURS) [[ $v =~ ^[23]$ ]] || return 1 ;;
      ROLE) [[ $v =~ ^(main|worker)$ ]] || return 1 ;;
      *) return 1 ;;
    esac
    printf -v "$k" '%s' "$v"
  done <"$f"
  [[ -n ${IMAGE_DIGEST:-} && -n ${LODESTAR_COMMIT:-} && -n ${SPEC_COMMIT:-} && -n ${TREATMENT:-} \
    && -n ${SEGMENT_HOURS:-} && -n ${ROLE:-} ]] || return 1
  case $TREATMENT in
    adm) TREATMENT_ENV=LODESTAR_DHT_COORDINATED_ADMISSION ;;
    utp) TREATMENT_ENV=LODESTAR_DHT_UTP ;;
    route) TREATMENT_ENV=LODESTAR_DHT_ROUTE_BACKOFF ;;
    *) return 1 ;;
  esac
}
watcher_active() { systemctl is-active --quiet "$WATCHER_UNIT" 2>/dev/null; }
# The outage watcher, detached as a transient unit, bounded by RuntimeMaxSec.
start_watcher() {
  watcher_active && return 0
  install -m 0700 -- "$work/watcher.sh" "$BLOCK/tools/watcher.sh" || return 1
  systemd-run --quiet --collect --unit="$WATCHER_UNIT" --property=RuntimeMaxSec=44h \
    bash "$BLOCK/tools/watcher.sh" "$BLOCK" >/dev/null 2>&1
}
# At the block's end: nothing is left that can reach Docker's socket.
stop_proxy() { timeout 60 docker rm -f lodestar-docker-proxy >/dev/null 2>&1 || true; }
stop_watcher() { systemctl stop "$WATCHER_UNIT" >/dev/null 2>&1 || true; }
