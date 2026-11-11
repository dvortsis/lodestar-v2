#!/usr/bin/env bash
# leak-test (OP-3a; docs/plan/2026-11-08-cloud-test-plan.md 0d): zero egress
# outside PIA with the tunnel down, IPv6 and DNS included, judged by captures
# on gluetun's side of the Docker bridge. A failure is a stop condition: no
# crawl starts on this VM.
#
# Two tunnel-down phases (the runner's tightening of 0d):
#   A. the WireGuard endpoint dropped in the host's DOCKER-USER chain: a dead
#      tunnel, with gluetun's interface and routes still up;
#   B. the same, with gluetun's VPN interface also taken down inside the
#      namespace, so the probes' traffic is no longer routed into the tunnel
#      and only gluetun's firewall stands between them and eth0.
# A gluetun with its firewall off passes A but fails B.
#
# Run as root by the step bootstrap: step.sh <workdir> [stack] [pia-run] [block]
# (tests and start-block pass the last three); <workdir> holds the committed
# compose.yaml and block-lib.sh. Output is fixed names only (`ok|fail <name>`,
# `count <name> <int>`); the control side drops anything else. Exit 0 only when
# every check passed.
set -uo pipefail
work=${1:?workdir}
STACK=${2:-/home/lodestar/lodestar}  # tests pass a temporary stack; the bootstrap never does
PIA_RUN=${3:-/run/lodestar-pia}
BLOCK=${4:-/var/lib/lodestar-block}
CONF=$STACK/secrets/wg0.conf
IPV4='^([0-9]{1,3}\.){3}[0-9]{1,3}$'
ECHO_URL=http://checkip.amazonaws.com
V6_URL='http://[2606:4700:4700::1111]/'
# The probes run from a throwaway container in gluetun's namespace (plan 0d), so
# they never depend on the kill switch's own image: PROBE_IMAGE, netshoot v0.16
# by its multi-arch index digest, from block-lib.sh with the block marker.
# shellcheck source=block-lib.sh
. "$work/block-lib.sh"
NOT_RUN=222  # a probe whose container never ran its command: never counts as blocked
fails=0
block=""
cap_pid=""
not_run=""

pass() { echo "ok $1"; }
bad() { echo "fail $1"; fails=$((fails + 1)); }
check() { local n=$1 rc; shift; "$@" >/dev/null 2>&1; rc=$?; if ((rc == 0)); then pass "$n"; else bad "$n"; fi; }
refute() { local n=$1 rc; shift; "$@" >/dev/null 2>&1; rc=$?; if ((rc != 0 && rc != NOT_RUN)); then pass "$n"; else bad "$n"; fi; }
finish() {
  [[ -n $not_run ]] && bad probe_ran
  if [[ $fails -eq 0 ]]; then echo "ok leak_test"; exit 0; fi
  echo "fail leak_test"; exit 1
}
cleanup() {
  [[ -n $cap_pid ]] && kill "$cap_pid" 2>/dev/null
  # The endpoint block never outlives the test, whatever ended it.
  [[ -n $block ]] && iptables -D DOCKER-USER $block 2>/dev/null
  return 0
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

# probe_in [docker args] -- command...: the command in a throwaway container in
# gluetun's namespace, with its own exit code reported after a marker. No
# marker (the container never ran it) returns NOT_RUN.
probe_in() {
  local extra=() out rc
  while [[ $1 != -- ]]; do extra+=("$1"); shift; done
  shift
  out=$(timeout 40 docker run --rm --network container:gluetun --cap-drop ALL "${extra[@]}" "$PROBE_IMAGE" \
    sh -c '"$@"; printf "\nprobe_rc=%s\n" "$?"' probe "$@" 2>/dev/null)
  rc=$(printf '%s\n' "$out" | sed -n 's/^probe_rc=\([0-9]\{1,3\}\)$/\1/p' | tail -n 1)
  if [[ -z $rc ]]; then not_run=1; return "$NOT_RUN"; fi
  printf '%s\n' "$out" | sed '/^probe_rc=/d'
  return "$rc"
}
probe() { probe_in -- "$@"; }
# resolves name timeout [@server]: an address came back. Only an address line
# counts: on a timeout dig prints ";; ... timed out" on stdout too.
resolves() { probe sh -c "dig +short +time=$2 +tries=1 $1 ${3:-} | grep -Eq '^[0-9]{1,3}(\.[0-9]{1,3}){3}\$'"; }
healthy() { [[ "$(docker inspect --type container -f '{{.State.Health.Status}}' gluetun 2>/dev/null)" == "healthy" ]]; }
wait_healthy() {
  local i
  for ((i = 0; i < 60; i++)); do healthy && return 0; sleep 2; done
  return 1
}
echo_ip() { probe curl -4 -s -m "$1" "$ECHO_URL" 2>/dev/null | tr -d '\r\n' | head -c 64; }
tunnel_up() {  # recreate gluetun alone from the committed compose file and the current wg0.conf
  LODESTAR_IMAGE=unused docker compose -f "$STACK/compose.yaml" --project-directory "$STACK" \
    up -d --force-recreate gluetun >/dev/null 2>&1
}
start_capture() {  # $1: phase
  tcpdump -i "$veth" -n -U -w "$work/cap-$1.pcap" \
    "not arp and not (udp and host $ep_ip and port $ep_port)" >/dev/null 2>&1 &
  cap_pid=$!
  for ((i = 0; i < 20; i++)); do [[ -s $work/cap-$1.pcap ]] && break; sleep 0.5; done
}
judge_capture() {  # $1: phase. The capture must have run through every probe, then be readable and empty.
  local alive="" captured=""
  if kill -0 "$cap_pid" 2>/dev/null; then alive=1; pass "capture_alive_$1"; else bad "capture_alive_$1"; fi
  kill "$cap_pid" 2>/dev/null; wait "$cap_pid" 2>/dev/null; cap_pid=""
  if timeout 30 tcpdump -r "$work/cap-$1.pcap" -n >"$work/cap-$1.txt" 2>/dev/null; then
    captured=$(grep -c . "$work/cap-$1.txt")
    pass "capture_read_$1"
  else
    bad "capture_read_$1"
  fi
  echo "count captured_packets_$1 ${captured:-0}"
  if [[ -n $alive && -s $work/cap-$1.pcap && $captured == 0 ]]; then pass "no_egress_captured_$1"; else bad "no_egress_captured_$1"; fi
}
down_probes() {  # $1: phase; $2: a name not asked before (no cache can answer it)
  refute "ipv4_blocked_$1" probe curl -4 -s -m 8 "$ECHO_URL"
  refute "tcp_literal_blocked_$1" probe nc -z -w 5 1.1.1.1 443  # no DNS needed
  refute "ipv6_blocked_$1" probe curl -6 -s -m 8 "$V6_URL"
  refute "dns_blocked_$1" resolves "$2" 3
  refute "dns_direct_blocked_$1" resolves "$2" 3 @1.1.1.1
  probe sh -c 'echo probe | nc -u -w 2 1.1.1.1 53' >/dev/null 2>&1  # the capture judges it
}
iface_up() { probe sh -c 'ip -o link show up dev "$1" | grep -q .' iface "$iface"; }

# Inputs. The endpoint is read from the root-only wg0.conf and validated; it
# never leaves this VM.
for tool in docker iptables tcpdump ip; do
  command -v "$tool" >/dev/null 2>&1 || { bad "tool_missing_$tool"; finish; }
done
[[ -s $CONF ]] || { bad wg_conf_present; finish; }
ep_ip=$(sed -n 's/^Endpoint = \([^:]*\):\([0-9]*\)$/\1/p' "$CONF" | head -n 1)
ep_port=$(sed -n 's/^Endpoint = \([^:]*\):\([0-9]*\)$/\2/p' "$CONF" | head -n 1)
if ! [[ $ep_ip =~ $IPV4 && $ep_port =~ ^[0-9]{1,5}$ ]] || ((10#$ep_port < 1 || 10#$ep_port > 65535)); then
  bad endpoint_valid; finish
fi
ep_port=$((10#$ep_port))
pass endpoint_valid

# Never mid-block, and fail closed: a block is recorded (the marker, which
# also covers a block stopped between segments) or running; if either can't
# be read, a block may be running.
case $(block_state) in
  none) ;;
  block) bad block_running; finish ;;
  *) bad block_check; finish ;;
esac
block_procs; rc=$?
if ((rc == 2)); then bad block_check; finish; fi
if ((rc == 0)); then bad block_running; finish; fi

# The tunnel.
install -m 0644 -- "$work/compose.yaml" "$STACK/compose.yaml" || { bad compose_installed; finish; }
tunnel_up || { bad tunnel_started; finish; }
check tunnel_healthy wait_healthy
docker pull "$PROBE_IMAGE" >/dev/null 2>&1 || { bad probe_image; finish; }
check probe_tools probe sh -c 'command -v curl && command -v nc && command -v dig && command -v ip'
iface=$(docker inspect --type container -f '{{range .Config.Env}}{{println .}}{{end}}' gluetun 2>/dev/null \
  | sed -n 's/^VPN_INTERFACE=//p' | tail -n 1)
iface=${iface:-wg0}  # gluetun's WireGuard default; compose.yaml sets it explicitly
[[ $iface =~ ^[a-z][a-z0-9]{0,14}$ ]] || { bad vpn_interface; finish; }

# Tunnel up: IPv4 leaves through PIA (not the host's address), TCP to a literal
# address works (the positive control for the down probes), IPv6 fails, and DNS
# answers through gluetun's own resolver.
host_ip=$(ip -4 route get 1.1.1.1 2>/dev/null | sed -n 's/.* src \([0-9.]*\).*/\1/p' | head -n 1)
exit_ip=$(echo_ip 10)
if [[ $exit_ip =~ $IPV4 && $host_ip =~ $IPV4 && $exit_ip != "$host_ip" ]]; then
  pass ipv4_through_tunnel
else
  bad ipv4_through_tunnel
fi
check tcp_literal_up probe nc -z -w 5 1.1.1.1 443
refute ipv6_blocked_up probe curl -6 -s -m 10 "$V6_URL"
check dns_up resolves example.com 5
check dns_local probe grep -qx 'nameserver 127.0.0.1' /etc/resolv.conf

# Down, phase A: the endpoint dropped in DOCKER-USER (gluetun keeps running).
idx=$(docker exec gluetun cat /sys/class/net/eth0/iflink 2>/dev/null | tr -dc '0-9')
veth=$(ip -o link 2>/dev/null | awk -F': ' -v i="$idx" '$1 == i {print $2}' | cut -d@ -f1 | head -n 1)
if [[ -z $idx || ! $veth =~ ^veth[0-9a-f]{1,12}$ ]]; then bad veth_found; finish; fi
pass veth_found
block="-d $ep_ip/32 -p udp --dport $ep_port -j DROP"
if ! iptables -I DOCKER-USER 1 $block 2>/dev/null; then block=""; bad endpoint_blocked; finish; fi
pass endpoint_blocked
start_capture down
down_probes down example.net
judge_capture down

# Down, phase B: also take gluetun's VPN interface down inside the namespace, so
# nothing is routed into the tunnel and only gluetun's firewall holds. (The
# endpoint stays blocked, so a reconnecting gluetun can't bring the tunnel back.)
check vpn_interface_down probe_in --cap-add NET_ADMIN -- ip link set dev "$iface" down
start_capture ifdown
down_probes ifdown example.edu
# gluetun's own VPN restart could bring the interface back mid-phase: fail
# closed unless it's still down (or gone) after the last probe.
refute vpn_interface_stayed_down iface_up
judge_capture ifdown

# Recovery: lift the block, recreate gluetun (its interface comes back), and the
# tunnel must be healthy again, through the same exit.
iptables -D DOCKER-USER $block 2>/dev/null && block=""
if tunnel_up && wait_healthy && [[ -n $exit_ip && $(echo_ip 15) == "$exit_ip" ]]; then
  pass tunnel_recovers
else
  bad tunnel_recovers
fi

# The host's own IPv6 (0d's separate check): the host doesn't route IPv6, the
# stack's Docker network has none, and gluetun's namespace holds no global
# IPv6 address. With these, the public IPv6 every Linode has can't carry
# Lodestar's traffic around the tunnel.
check host_ipv6_not_forwarded grep -qx 0 "${IPV6_FORWARDING:-/proc/sys/net/ipv6/conf/all/forwarding}"
check bridge_ipv6_off test "$(docker network inspect -f '{{.EnableIPv6}}' lodestar_default 2>/dev/null)" = false
refute namespace_no_global_ipv6 probe sh -c 'ip -6 -o addr show scope global | grep -q .'

# PIA delivery's work directory is gone (this step holds the VM's lock).
check run_clean test ! -e "$PIA_RUN"
finish
