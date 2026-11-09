#!/usr/bin/env bash
# leak-test (OP-3a; docs/plan/2026-11-08-cloud-test-plan.md 0d): zero egress
# outside PIA with the tunnel down, IPv6 and DNS included, judged by a capture
# on gluetun's side of the Docker bridge. A failure is a stop condition: no
# crawl starts on this VM.
#
# Run as root by the step bootstrap: step.sh <workdir>; <workdir> holds the
# committed compose.yaml. Output is fixed names only (`ok|fail <name>`,
# `count <name> <int>`); the control side drops anything else. Exit 0 only when
# every check passed.
set -uo pipefail
work=${1:?workdir}
STACK=${2:-/home/lodestar/lodestar}  # tests pass a temporary stack; the bootstrap never does
PIA_RUN=${3:-/run/lodestar-pia}
CONF=$STACK/secrets/wg0.conf
IPV4='^([0-9]{1,3}\.){3}[0-9]{1,3}$'
ECHO_URL=http://checkip.amazonaws.com
V6_URL='http://[2606:4700:4700::1111]/'
# The probes run from a throwaway container in gluetun's namespace (plan 0d), so
# they never depend on the kill switch's own image. netshoot v0.16, by its
# multi-arch index digest (docs/decisions/2026-11-09-OP-3a.md).
PROBE_IMAGE=nicolaka/netshoot@sha256:b09d9b21381f47a79b3cbcb30da25266dc17186ea00ae65e99fdc51396f48e70
fails=0
block=""
cap_pid=""

pass() { echo "ok $1"; }
bad() { echo "fail $1"; fails=$((fails + 1)); }
check() { local n=$1; shift; if "$@" >/dev/null 2>&1; then pass "$n"; else bad "$n"; fi; }
refute() { local n=$1; shift; if "$@" >/dev/null 2>&1; then bad "$n"; else pass "$n"; fi; }
finish() {
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

probe() { timeout 40 docker run --rm --network container:gluetun --cap-drop ALL "$PROBE_IMAGE" "$@"; }
resolves() { probe sh -c "dig +short +time=$2 +tries=1 $1 ${3:-} | grep -q ."; }
running() { [[ "$(docker inspect --type container -f '{{.State.Running}}' "$1" 2>/dev/null)" == "true" ]]; }
healthy() { [[ "$(docker inspect --type container -f '{{.State.Health.Status}}' gluetun 2>/dev/null)" == "healthy" ]]; }
wait_healthy() {
  local i
  for ((i = 0; i < 60; i++)); do healthy && return 0; sleep 2; done
  return 1
}
echo_ip() { probe curl -4 -s -m "$1" "$ECHO_URL" 2>/dev/null | tr -d '\r\n' | head -c 64; }

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

# Never mid-block: the endpoint block would cut a live crawl, and Lodestar's own
# traffic isn't what this test judges.
if running lodestar || docker ps --filter 'name=^lodestar-collector' --format x 2>/dev/null | grep -q .; then
  bad block_running; finish
fi

# The tunnel: gluetun alone, recreated from the committed compose file and the
# current wg0.conf, so the configuration under test is the one in force.
install -m 0644 -- "$work/compose.yaml" "$STACK/compose.yaml" || { bad compose_installed; finish; }
if ! LODESTAR_IMAGE=unused docker compose -f "$STACK/compose.yaml" --project-directory "$STACK" \
    up -d --force-recreate gluetun >/dev/null 2>&1; then
  bad tunnel_started; finish
fi
check tunnel_healthy wait_healthy
docker pull "$PROBE_IMAGE" >/dev/null 2>&1 || { bad probe_image; finish; }
check probe_tools probe sh -c 'command -v curl && command -v nc && command -v dig'

# Tunnel up: IPv4 leaves through PIA (not the host's address), TCP to a literal
# address works (the positive control for the down probe), IPv6 fails, and DNS
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

# Tunnel down: drop the WireGuard endpoint in DOCKER-USER (gluetun keeps
# running, so its firewall is what's tested), capture on gluetun's veth, and
# probe everything: TCP to a literal address (no DNS needed), IPv6, DNS for a
# name not asked before (no cache can answer it), DNS straight to an outside
# resolver, and a UDP send.
idx=$(docker exec gluetun cat /sys/class/net/eth0/iflink 2>/dev/null | tr -dc '0-9')
veth=$(ip -o link 2>/dev/null | awk -F': ' -v i="$idx" '$1 == i {print $2}' | cut -d@ -f1 | head -n 1)
if [[ -z $idx || ! $veth =~ ^veth[0-9a-f]{1,12}$ ]]; then bad veth_found; finish; fi
pass veth_found
block="-d $ep_ip/32 -p udp --dport $ep_port -j DROP"
if ! iptables -I DOCKER-USER 1 $block 2>/dev/null; then block=""; bad endpoint_blocked; finish; fi
pass endpoint_blocked
tcpdump -i "$veth" -n -U -w "$work/cap.pcap" \
  "not arp and not (udp and host $ep_ip and port $ep_port)" >/dev/null 2>&1 &
cap_pid=$!
for ((i = 0; i < 20; i++)); do [[ -s $work/cap.pcap ]] && break; sleep 0.5; done
refute ipv4_blocked_down probe curl -4 -s -m 8 "$ECHO_URL"
refute tcp_literal_blocked_down probe nc -z -w 5 1.1.1.1 443
refute ipv6_blocked_down probe curl -6 -s -m 8 "$V6_URL"
refute dns_blocked_down resolves example.net 3
refute dns_direct_blocked_down resolves example.org 3 @1.1.1.1
probe sh -c 'echo probe | nc -u -w 2 1.1.1.1 53' >/dev/null 2>&1  # the capture judges it
# The capture must have run through every probe: still alive now, then readable.
if kill -0 "$cap_pid" 2>/dev/null; then alive=1; pass capture_alive; else alive=""; bad capture_alive; fi
kill "$cap_pid" 2>/dev/null; wait "$cap_pid" 2>/dev/null; cap_pid=""
if timeout 30 tcpdump -r "$work/cap.pcap" -n >"$work/cap.txt" 2>/dev/null; then
  captured=$(grep -c . "$work/cap.txt")
  pass capture_read
else
  captured=""
  bad capture_read
fi
echo "count captured_packets ${captured:-0}"
if [[ -n $alive && -s $work/cap.pcap && $captured == 0 ]]; then pass no_egress_captured; else bad no_egress_captured; fi

# Recovery: lift the block; the tunnel comes back, through the same exit.
iptables -D DOCKER-USER $block 2>/dev/null && block=""
if wait_healthy && [[ $(echo_ip 15) == "$exit_ip" && -n $exit_ip ]]; then pass tunnel_recovers; else bad tunnel_recovers; fi

# PIA delivery's work directory is gone (this step holds the VM's lock).
check run_clean test ! -e "$PIA_RUN"
finish
