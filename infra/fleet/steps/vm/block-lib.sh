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

# The host guard: two rules, both tagged lodestar-leak-host, held and released
# together.
# - INPUT (X-gluetun-host-guard): a DROP for the VPN endpoint's UDP. When
#   gluetun stops or is recreated and Docker flushes the container's conntrack
#   entries, the endpoint's leftover UDP for the old session goes to the host's
#   own INPUT, and the host's kernel would answer it ("port unreachable").
# - mangle POSTROUTING (X-gluetun-guard-stop): a DROP for any ICMP to the
#   endpoint, whoever sends it. While conntrack entries outlive a stopped
#   gluetun, the endpoint's leftover UDP is still forwarded towards the gone
#   container, and the host answers "host unreachable"; a new gluetun can send
#   its own "port unreachable" before its firewall is up. Both leave from the
#   host's public address. mangle POSTROUTING sees what the host sends and
#   what it forwards, after any DNAT and before SNAT (the destination, the
#   endpoint, is the same either way), and Docker never writes to the mangle table, so the
#   rule holds before Docker starts (the boot guard) and stays at the top.
# Neither can touch the tunnel: a live session's replies are de-NATed and
# forwarded, never delivered to the host, and the tunnel itself is UDP; nothing
# legitimate sends ICMP to the VPN endpoint outside the tunnel. start-block
# holds the guard for the whole block (from before the leak test recreates
# gluetun), copy-off removes it; a leak test run on its own holds its own copy
# for its run.
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
host_guard_rule() {  # sets host_rule and icmp_rule, for the endpoint endpoint_read read
  host_rule=(-s "$ep_ip/32" -p udp --sport "$ep_port" -m comment --comment "$HOST_TAG" -j DROP)
  icmp_rule=(-d "$ep_ip/32" -p icmp -m comment --comment "$HOST_TAG" -j DROP)
}
ICMP_TABLE=mangle
ICMP_CHAIN=POSTROUTING
# host_guard_hold: exactly one host guard, this endpoint's, in each chain. The
# new copy goes in at the top first and the rest (a stuck copy, a duplicate, or
# an earlier endpoint's) come out after, so there's never a moment without one.
# Re-running it leaves the same single rules. (With an identical copy already
# there, deleting by spec removes the new one, so the old copy keeps its place;
# nothing else on the VM adds rules above it.)
host_guard_hold() {
  iptables -I INPUT 1 "${host_rule[@]}" 2>/dev/null || return 1
  clear_tagged INPUT "$HOST_TAG" 1 || return 1
  iptables -t "$ICMP_TABLE" -I "$ICMP_CHAIN" 1 "${icmp_rule[@]}" 2>/dev/null || return 1
  clear_tagged_t "$ICMP_TABLE" "$ICMP_CHAIN" "$HOST_TAG" 1 || return 1
  iptables -C INPUT "${host_rule[@]}" 2>/dev/null \
    && iptables -t "$ICMP_TABLE" -C "$ICMP_CHAIN" "${icmp_rule[@]}" 2>/dev/null
}
host_guard_held() {  # both rules are there, for this endpoint
  iptables -C INPUT "${host_rule[@]}" 2>/dev/null \
    && iptables -t "$ICMP_TABLE" -C "$ICMP_CHAIN" "${icmp_rule[@]}" 2>/dev/null
}
host_guard_release() {  # every copy in both chains, whatever endpoint it names
  local rc=0
  clear_tagged INPUT "$HOST_TAG" || rc=1
  clear_tagged_t "$ICMP_TABLE" "$ICMP_CHAIN" "$HOST_TAG" || rc=1
  return "$rc"
}

# A guard dropped mid-block (X-gluetun-guard-reboot): a reboot or an iptables
# flush removes it while the marker still records a block, and gluetun, which
# Docker restarts by itself, would then run unguarded. Three places check:
# - at boot, before Docker starts: the boot unit (boot_guard_install), which Docker
#   requires, so a guard that can't be held at boot keeps Docker down;
# - the outage watcher, at its start and every GUARD_EVERY seconds;
# - the outage-watcher step.
#
# guard_resume conf [boot]: while a guard is expected, this endpoint's guard
# is in both its chains, or what's missing is put back. Expected means a recorded block; at boot also
# a block that ended but isn't copied off yet (copy-off releases the guard).
# It only adds: a copy for another endpoint (a VPN renewal mid-block replaces
# wg0.conf but not gluetun's session) is left in place, and copy-off removes
# every tagged copy. It acts first and records after. Returns
#   0  the guard is there, or nothing is expected; with only another
#      endpoint's copy left, this endpoint's is added (a precaution, not a
#      drop, so it isn't recorded);
#   3  it had gone and is back: recorded as `host_guard_dropped`;
#   1  it can't be held (no valid endpoint, iptables refuses or keeps
#      erroring, or the lock can't be opened): gluetun is stopped (not at
#      boot, when Docker isn't up), the block is marked damaged and
#      `host_guard_lost` is recorded once;
#   2  another check held the lock for GUARD_LOCK_WAIT seconds; the caller
#      tries again.
# Records are lines in $BLOCK/outages, which copy-off's evidence counts, plus
# one link_outage note in the running segment, so the block can't read as
# clean. It prints nothing.
GUARD_LOCK_WAIT=${GUARD_LOCK_WAIT:-15}
guard_expected() {
  [[ $(block_state) != none ]] && return 0
  [[ ${1:-} == boot && -d $BLOCK/evidence && ! -e $BLOCK/copied ]]
}
guard_resume() {
  local conf=$1 mode=${2:-} fd rc=1 i tagged dropped=""
  if ! { exec {fd}>>"$BLOCK/.guard.lock"; } 2>/dev/null; then
    guard_expected "$mode" || return 0
    guard_lost "$mode" ""; return 1
  fi
  if ! flock -w "$GUARD_LOCK_WAIT" "$fd" 2>/dev/null; then exec {fd}>&-; return 2; fi
  if ! guard_expected "$mode"; then exec {fd}>&-; return 0; fi
  if endpoint_read "$conf"; then
    host_guard_rule
    local slot table chain ok=1 rule=()
    for slot in "filter INPUT" "$ICMP_TABLE $ICMP_CHAIN"; do
      read -r table chain <<<"$slot"
      if [[ $chain == INPUT ]]; then rule=("${host_rule[@]}"); else rule=("${icmp_rule[@]}"); fi
      # Only exit 1 means gone (as in drop_all). Any other exit (the xtables
      # lock held past -w's 5 s, say) is tried again, and fails closed if it
      # persists.
      for ((i = 0; i < 3; i++)); do
        ipt_w "$table" -C "$chain" "${rule[@]}" 2>/dev/null; rc=$?
        ((rc <= 1)) && break
        sleep 2
      done
      ((rc == 0)) && continue
      if ((rc == 1)); then
        tagged=$(ipt_w "$table" -S "$chain" 2>/dev/null | grep -c -e "--comment $HOST_TAG ")
        # A block start-block began before the ICMP rule existed never held
        # one: it's added, not recorded as a drop. ($BLOCK/guard-icmp says
        # the block's start held it.)
        if ((${tagged:-0} == 0)) && [[ $chain == INPUT || -e $BLOCK/guard-icmp ]]; then dropped=1; fi
        if ipt_w "$table" -I "$chain" 1 "${rule[@]}" 2>/dev/null \
          && ipt_w "$table" -C "$chain" "${rule[@]}" 2>/dev/null; then
          continue
        fi
      fi
      ok=""
      break
    done
    if [[ -n $ok ]]; then
      exec {fd}>&-
      [[ -n $dropped ]] || return 0
      block_record "$mode" host_guard_dropped
      return 3
    fi
  fi
  guard_lost "$mode" "$dropped"
  exec {fd}>&-
  return 1
}
# The damaged mark is there: a file, or a link of any kind, even a dangling
# one (X-damaged-mark-links). Every check of the mark uses this, as copy-off
# does, so they all agree.
damaged_marked() { [[ -e $BLOCK/damaged || -L $BLOCK/damaged ]]; }
# mark_create: the damaged mark, created without following any link
# (X-scrub-fields-readers). It's written to a new temp file in $BLOCK, then
# hard-linked into place with `ln -T`, which fails if anything (a file or a
# link of any kind) is already there and never writes through it.
mark_create() {
  local tmp
  tmp=$(mktemp -- "$BLOCK/.damaged.XXXXXX" 2>/dev/null) || return 1
  echo host_guard_lost 2>/dev/null >"$tmp" && ln -T -- "$tmp" "$BLOCK/damaged" 2>/dev/null
  local rc=$?
  rm -f -- "$tmp" 2>/dev/null
  return "$rc"
}
# guard_lost mode dropped: stop gluetun first, then record. The records and
# the damaged mark are written once, by the first failed check; gluetun is
# stopped on every one, in case it came back.
guard_lost() {
  local kinds=()
  [[ $1 == boot ]] || stop_gluetun
  if ! damaged_marked; then
    [[ -n $2 ]] && kinds+=(host_guard_dropped)
    mark_create
    kinds+=(host_guard_lost)
  fi
  ((${#kinds[@]})) && block_record "$1" "${kinds[@]}"
  return 0
}
# block_record mode kind...: a line in $BLOCK/outages per kind, then one
# link_outage note in the running segment's evidence when its collector runs
# (never at boot, when Docker isn't up; bounded by MAX_NOTES).
block_record() {
  local mode=$1 k
  shift
  for k in "$@"; do echo "$(date -u +%s) $k" 2>/dev/null >>"$BLOCK/outages"; done
  [[ $mode == boot ]] || segment_note
}
segment_note() {  # 0 noted, 1 not (no collector, no segment, or the bound reached)
  local seg notes
  notes=$(grep -c . "$BLOCK/notes" 2>/dev/null)
  ((${notes:-0} >= ${MAX_NOTES:-500})) && return 1
  seg=$(marker_get segment)
  [[ $seg =~ ^[1-4]$ ]] || return 1
  timeout 30 docker exec "lodestar-collector-$seg" python3 /tools/linux/collect.py note \
    --root "/evidence/seg-$seg" --category link_outage >/dev/null 2>&1 || return 1
  echo "$(date -u +%s) $seg" >>"$BLOCK/notes"
}
# gluetun, kept down: its restart policy off first (so neither a kill nor a
# reboot brings it back; the next leak test recreates it from compose.yaml),
# then stopped, or killed if it won't stop. 0 when it's no longer running.
stop_gluetun() {
  timeout 15 docker update --restart=no gluetun >/dev/null 2>&1
  timeout 30 docker stop -t 10 gluetun >/dev/null 2>&1 || timeout 15 docker kill gluetun >/dev/null 2>&1
  ! running gluetun
}
guard_drops() { grep -cE ' host_guard_dropped$' "$BLOCK/outages" 2>/dev/null || true; }
# ipt_w table args...: iptables waiting up to 5 s for the xtables lock, in
# `table` (no -t for filter, the default).
ipt_w() {
  local table=$1
  shift
  if [[ $table == filter ]]; then iptables -w 5 "$@"; else iptables -w 5 -t "$table" "$@"; fi
}
# ipt table args...: iptables in `table` (no -t for filter).
ipt() {
  local table=$1
  shift
  if [[ $table == filter ]]; then iptables "$@"; else iptables -t "$table" "$@"; fi
}
# drop_all chain rule...: remove every copy of a rule (a failed delete can
# leave more than one). Only `-C` exit 1 means absent (iptables also uses 1
# for some generic errors, such as a missing chain); any other exit is an
# error, never read as "gone". Fails when a copy may still be there.
drop_all() { drop_all_t filter "$@"; }
drop_all_t() {  # table chain rule...
  local table=$1 chain=$2 i rc
  shift 2
  for ((i = 0; i < 5; i++)); do
    ipt "$table" -C "$chain" "$@" 2>/dev/null; rc=$?
    ((rc == 1)) && return 0
    ((rc == 0)) || return 1
    ipt "$table" -D "$chain" "$@" 2>/dev/null || return 1
  done
  ipt "$table" -C "$chain" "$@" 2>/dev/null; rc=$?
  ((rc == 1))
}
# clear_tagged chain tag [keep]: remove every rule in the chain that carries
# the comment tag, whatever endpoint it names, but the first `keep` (default
# 0). Each line of `iptables -S` is turned from -A into -D and run as words,
# never through a shell. Deleting by a duplicate's spec removes the first
# identical copy, which leaves the same rule in place.
clear_tagged() { clear_tagged_t filter "$@"; }
clear_tagged_t() {  # table chain tag [keep]
  local table=$1 chain=$2 tag=$3 keep=${4:-0} i out line words
  for ((i = 0; i < 10; i++)); do
    out=$(ipt "$table" -S "$chain" 2>/dev/null) || return 1
    line=$(printf '%s\n' "$out" | grep -e "--comment $tag " | sed -n "$((keep + 1))p")
    [[ -z $line ]] && return 0
    read -ra words <<<"$line"
    [[ ${words[0]} == -A && ${words[1]} == "$chain" ]] || return 1
    words[0]=-D
    ipt "$table" "${words[@]}" 2>/dev/null || return 1
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
# The image Lodestar runs: the registry's, pinned by digest (the default), or,
# for a test run, a locally built image pinned by its image ID (LOCAL_IMAGE_ID, the
# 64 hex of `docker image inspect -f {{.Id}}`). Either way it's content-pinned.
lodestar_image() {
  if [[ -n ${LOCAL_IMAGE_ID:-} ]]; then echo "sha256:$LOCAL_IMAGE_ID"; else echo "$LODESTAR_REPO@sha256:$IMAGE_DIGEST"; fi
}
# lodestar_image_ready: the registry image pulled, or the local one present
# with exactly that ID (a local image is never pulled).
lodestar_image_ready() {
  if [[ -n ${LOCAL_IMAGE_ID:-} ]]; then
    [[ $(timeout 30 docker image inspect -f '{{.Id}}' "sha256:$LOCAL_IMAGE_ID" 2>/dev/null) == "sha256:$LOCAL_IMAGE_ID" ]]
  else
    timeout 300 docker pull "$LODESTAR_REPO@sha256:$IMAGE_DIGEST" >/dev/null 2>&1
  fi
}
# Lodestar's account (the image's default PUID/PGID; compose sets neither).
# Tests, which don't run as root, pass their own.
LODESTAR_UID=${DATA_UID:-65532}
# data_dirs_ready: the data and witness folders belong to Lodestar's account,
# mode 0755, inside a stack folder others can't enter.
# Lodestar starts as root with only CHOWN, SETGID and SETUID (compose
# cap_drop: ALL), so root can't override file modes: it opens /data to check
# its owner, which a 0700 folder owned by 65532 refuses on every start after
# the first. Others' read and execute let it open the folder; closing the
# stack folder to others (o-rwx) keeps every host account but root and the
# stack's owner out, and only the containers that mount the folders see them
# (a bind mount doesn't need its host parents). What Lodestar writes inside
# keeps its own modes. Created empty when missing (`fresh` replaces them).
data_dirs_ready() {  # [fresh]
  local d
  chmod o-rwx -- "$STACK" || return 1
  [[ $(stat -c %A -- "$STACK" 2>/dev/null) == *--- ]] || return 1
  if [[ ${1:-} == fresh ]]; then rm -rf -- "$STACK/data" "$STACK/witness" || return 1; fi
  for d in "$STACK/data" "$STACK/witness"; do
    [[ -L $d ]] && return 1
    if [[ -d $d ]]; then
      chown -h "$LODESTAR_UID:$LODESTAR_UID" -- "$d" && chmod 0755 -- "$d" || return 1
    else
      install -d -o "$LODESTAR_UID" -g "$LODESTAR_UID" -m 0755 -- "$d" || return 1
    fi
    # What's there now: a real folder, Lodestar's, 0755 (nothing swapped in meanwhile).
    [[ ! -L $d && $(stat -c '%u %a' -- "$d" 2>/dev/null) == "$LODESTAR_UID 755" ]] || return 1
  done
}
start_lodestar() {  # $1 segment, $2 "fresh" to recreate whatever compose thinks
  local value=false recreate=() image
  [[ ${2:-} == fresh ]] && recreate=(--force-recreate)
  [[ $(arm_of "$1") == treatment ]] && value=true
  image=$(lodestar_image)
  (
    unset LODESTAR_DHT_COORDINATED_ADMISSION LODESTAR_DHT_UTP LODESTAR_DHT_ROUTE_BACKOFF
    export LODESTAR_IMAGE="$image" LODESTAR_DHT_SAFE_RESTART=true \
      "$TREATMENT_ENV=$value" DOCKER_GID=unused
    timeout 240 docker compose -f "$STACK/compose.yaml" --project-directory "$STACK" \
      --profile block up -d --pull never --no-deps "${recreate[@]}" lodestar >/dev/null 2>&1
  )
}
# Lodestar shares gluetun's namespace (plan Lane L: checked before each collector starts).
network_parent_ok() {
  local id mode
  id=$(timeout 30 docker inspect --type container -f '{{.Id}}' gluetun 2>/dev/null) || return 1
  mode=$(timeout 30 docker inspect --type container -f '{{.HostConfig.NetworkMode}}' lodestar 2>/dev/null) || return 1
  [[ -n $id && $mode == "container:$id" ]]
}
# netns_of container: its network namespace's inode, from its main process.
netns_of() {
  local pid
  pid=$(timeout 30 docker inspect --type container -f '{{.State.Pid}}' "$1" 2>/dev/null)
  [[ $pid =~ ^[1-9][0-9]{0,9}$ ]] || return 1
  stat -L -c %i -- "${PROC_ROOT:-/proc}/$pid/ns/net" 2>/dev/null  # tests pass their own /proc
}
# same_netns: running Lodestar is in the namespace gluetun has now. A gluetun
# that restarted (same container, new namespace) leaves a running Lodestar in
# the old one, where its API can't be reached, until Lodestar restarts.
same_netns() {
  local a b
  a=$(netns_of gluetun) && b=$(netns_of lodestar) && [[ -n $a && $a == "$b" ]]
}
# lodestar_follows_gluetun: after gluetun has come back, a running Lodestar in
# an old namespace is restarted, so it joins gluetun's (test scripts only; the
# product's own behaviour is a follow-up, X-gluetun-guard-stop decisions).
# 0 when it's in gluetun's namespace, restarted or not.
lodestar_follows_gluetun() {
  running lodestar || return 0
  same_netns && return 0
  timeout 120 docker restart -t 60 lodestar >/dev/null 2>&1 || return 1
  same_netns
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
  unset IMAGE_DIGEST LOCAL_IMAGE_ID  # only the file's pin counts, never the environment's
  while IFS='=' read -r k v; do
    case $k in
      IMAGE_DIGEST | LOCAL_IMAGE_ID) [[ $v =~ ^[0-9a-f]{64}$ ]] || return 1 ;;
      LODESTAR_COMMIT | SPEC_COMMIT | RULES_COMMIT) [[ $v =~ ^[0-9a-f]{40}$ ]] || return 1 ;;
      TREATMENT) [[ $v =~ ^(adm|utp|route)$ ]] || return 1 ;;
      SEGMENT_HOURS) [[ $v =~ ^[23]$ ]] || return 1 ;;
      ROLE) [[ $v =~ ^(main|worker)$ ]] || return 1 ;;
      *) return 1 ;;
    esac
    printf -v "$k" '%s' "$v"
  done <"$f"
  # Exactly one image pin: the registry digest or a local image ID.
  [[ -z ${IMAGE_DIGEST:-} && -n ${LOCAL_IMAGE_ID:-} || -n ${IMAGE_DIGEST:-} && -z ${LOCAL_IMAGE_ID:-} ]] || return 1
  [[ -n ${LODESTAR_COMMIT:-} && -n ${SPEC_COMMIT:-} && -n ${TREATMENT:-} \
    && -n ${SEGMENT_HOURS:-} && -n ${ROLE:-} ]] || return 1
  case $TREATMENT in
    adm) TREATMENT_ENV=LODESTAR_DHT_COORDINATED_ADMISSION ;;
    utp) TREATMENT_ENV=LODESTAR_DHT_UTP ;;
    route) TREATMENT_ENV=LODESTAR_DHT_ROUTE_BACKOFF ;;
    *) return 1 ;;
  esac
}
watcher_active() { systemctl is-active --quiet "$WATCHER_UNIT" 2>/dev/null; }
# The boot guard: a oneshot that Docker requires (a drop-in), run before the
# network and Docker come up, which holds the host guard while one is expected
# (guard_resume boot) and does nothing otherwise. boot_guard_install stages
# watcher.sh and this library into $BLOCK/tools and writes both; start-block
# calls it before the marker, so the guard is in place for the whole block.
# The unit runs whenever the block has a marker or evidence (trigger
# conditions): a block whose script is missing then fails the unit and keeps
# Docker down, rather than being skipped (a skipped unit satisfies Requires=).
# It's ordered after the usual host firewalls, which may load rules at boot.
UNIT_DIR=${UNIT_DIR:-/etc/systemd/system}
GUARD_UNIT=lodestar-block-guard
UNIT_PATH='^/[A-Za-z0-9._/-]+$'  # the units' paths are written into them unquoted
unit_write() {  # path, then its lines
  local f=$1
  shift
  printf '%s\n' "$@" >"$f.tmp" && mv -f -- "$f.tmp" "$f"
}
boot_guard_install() {
  [[ $BLOCK =~ $UNIT_PATH && $STACK =~ $UNIT_PATH ]] || return 1
  mkdir -p -m 0700 -- "$BLOCK/tools" \
    && install -m 0700 -- "$work/watcher.sh" "$BLOCK/tools/watcher.sh" \
    && install -m 0600 -- "$work/block-lib.sh" "$BLOCK/tools/block-lib.sh" \
    && mkdir -p -- "$UNIT_DIR/docker.service.d" || return 1
  unit_write "$UNIT_DIR/$GUARD_UNIT.service" "[Unit]" "Description=Lodestar block host guard, before Docker" \
    "DefaultDependencies=no" "After=local-fs.target netfilter-persistent.service ufw.service firewalld.service" \
    "Before=network-pre.target docker.service" "Wants=network-pre.target" \
    "ConditionPathExists=|$BLOCK/marker" "ConditionPathExists=|$BLOCK/evidence" \
    "[Service]" "Type=oneshot" "ExecStart=/bin/bash $BLOCK/tools/watcher.sh $BLOCK $STACK boot" || return 1
  unit_write "$UNIT_DIR/docker.service.d/$GUARD_UNIT.conf" "[Unit]" "Requires=$GUARD_UNIT.service" \
    "After=$GUARD_UNIT.service" || return 1
  systemctl daemon-reload >/dev/null 2>&1
}
# The outage watcher, as an enabled unit bounded by RuntimeMaxSec, so it starts
# again after a reboot mid-block; with no marker it doesn't start. stop_watcher
# disables it at the block's end. It runs from $BLOCK/tools with its own copy
# of this library; starting it re-installs the boot guard too.
start_watcher() {
  watcher_active && return 0
  boot_guard_install || return 1
  unit_write "$UNIT_DIR/$WATCHER_UNIT.service" "[Unit]" "Description=Lodestar block outage watcher and host guard" \
    "Wants=docker.service" "After=docker.service" "ConditionPathExists=$BLOCK/marker" \
    "[Service]" "ExecStart=/bin/bash $BLOCK/tools/watcher.sh $BLOCK $STACK" "RuntimeMaxSec=44h" \
    "[Install]" "WantedBy=multi-user.target" || return 1
  systemctl daemon-reload >/dev/null 2>&1 && systemctl enable --now "$WATCHER_UNIT" >/dev/null 2>&1
}
# At the block's end: nothing is left that can reach Docker's socket.
stop_proxy() { timeout 60 docker rm -f lodestar-docker-proxy >/dev/null 2>&1 || true; }
stop_watcher() { systemctl disable --now "$WATCHER_UNIT" >/dev/null 2>&1 || true; }
