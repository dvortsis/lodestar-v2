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
COLLECTOR_IMAGE=python@sha256:848ba4413eb897e225159b8fc1b02094576cbae4aa73fc13142608ae2c8c0e32
LODESTAR_REPO=ghcr.io/dvortsis/lodestar-v2
WATCHER_UNIT=lodestar-outage-watcher
API=http://127.0.0.1:8090  # Lodestar on gluetun's loopback, which the throwaway containers share

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
  printf '%s\n' "$names" | grep -Eq '^(lodestar|lodestar-collector.*)$'
}
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
start_lodestar() {
  local value=false
  [[ $(arm_of "$1") == treatment ]] && value=true
  (
    unset LODESTAR_DHT_COORDINATED_ADMISSION LODESTAR_DHT_UTP LODESTAR_DHT_ROUTE_BACKOFF
    export LODESTAR_IMAGE="$LODESTAR_REPO@sha256:$IMAGE_DIGEST" LODESTAR_DHT_SAFE_RESTART=true \
      "$TREATMENT_ENV=$value"
    timeout 240 docker compose -f "$STACK/compose.yaml" --project-directory "$STACK" \
      --profile block up -d --no-deps lodestar >/dev/null 2>&1
  )
}
# Lodestar shares gluetun's namespace (plan Lane L: checked before each collector starts).
network_parent_ok() {
  local id mode
  id=$(timeout 30 docker inspect --type container -f '{{.Id}}' gluetun 2>/dev/null) || return 1
  mode=$(timeout 30 docker inspect --type container -f '{{.HostConfig.NetworkMode}}' lodestar 2>/dev/null) || return 1
  [[ -n $id && $mode == "container:$id" ]]
}
# Segment $1's collector, detached, in gluetun's namespace (CR-SPEC rehearsal
# kind `ab`; plan Lane L and the cloud screen). It stops Lodestar when its
# window ends (--stop). Raw evidence stays in $BLOCK/evidence on this VM. No
# credentials but Lodestar's password; every mount is read-only except its own
# evidence folder and the Docker socket (--mode docker reads Docker's API).
start_collector() {
  local seg=$1 next guard=()
  next=$(date -u -d '+3 days' +%Y-%m-%dT%H:%M:%SZ)  # after any fleet's teardown (44 h at most)
  [[ $ROLE == worker ]] && guard=(--min-free-bytes 12884901888 --warn-free-bytes 8589934592
    --stop-free-bytes 4294967296)  # the 25 GB disk (plan Lane K)
  mkdir -m 0700 -- "$BLOCK/evidence/seg-$seg" || return 1
  timeout 120 docker run -d --name "lodestar-collector-$seg" --network container:gluetun --restart no \
    --oom-score-adj -500 --cap-drop ALL --security-opt no-new-privileges:true --read-only --tmpfs /tmp \
    -v /var/run/docker.sock:/var/run/docker.sock -v /proc:/host/proc:ro -v /sys/fs/cgroup:/sys/fs/cgroup:ro \
    -v "$BLOCK/tools:/tools:ro" -v "$BLOCK/password:/secrets/password:ro" -v "$BLOCK/evidence:/evidence" \
    -v "$STACK/data:/data:ro" -v "$BLOCK/witness:/witness:ro" \
    "$COLLECTOR_IMAGE" python3 /tools/linux/collect.py --mode docker --container lodestar --url "$API" \
    --run-kind ab --treatment "$TREATMENT" --intensity balanced --stop --rehearsal \
    --baseline-hours "$SEGMENT_HOURS" --commit "$LODESTAR_COMMIT" --spec-commit "$SPEC_COMMIT" \
    --password-file /secrets/password --root "/evidence/seg-$seg" --proc-root /host/proc \
    --storage-path /data --storage-path /witness "${guard[@]}" \
    --checklist auto_update=excluded --next-run "auto_update=$next" \
    --checklist backup=excluded --next-run "backup=$next" >/dev/null 2>&1
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
stop_watcher() { systemctl stop "$WATCHER_UNIT" >/dev/null 2>&1 || true; }
