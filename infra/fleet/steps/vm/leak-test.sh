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

in_ns() { docker exec gluetun "$@"; }  # gluetun's network namespace (Lodestar shares it)
healthy() { [[ "$(docker inspect -f '{{.State.Health.Status}}' gluetun 2>/dev/null)" == "healthy" ]]; }
wait_healthy() {
  local i
  for ((i = 0; i < 60; i++)); do healthy && return 0; sleep 2; done
  return 1
}

# Inputs. The endpoint is read from the root-only wg0.conf and validated; it
# never leaves this VM.
for tool in docker iptables tcpdump ip; do
  command -v "$tool" >/dev/null 2>&1 || { bad "tool_missing_$tool"; finish; }
done
[[ -s $CONF ]] || { bad wg_conf_present; finish; }
ep_ip=$(sed -n 's/^Endpoint = \([^:]*\):\([0-9]*\)$/\1/p' "$CONF" | head -n 1)
ep_port=$(sed -n 's/^Endpoint = \([^:]*\):\([0-9]*\)$/\2/p' "$CONF" | head -n 1)
if ! [[ $ep_ip =~ $IPV4 && $ep_port =~ ^[0-9]{1,5}$ ]] || ((ep_port < 1 || ep_port > 65535)); then
  bad endpoint_valid; finish
fi
pass endpoint_valid

# The tunnel: gluetun alone, from the committed compose file.
install -m 0644 -- "$work/compose.yaml" "$STACK/compose.yaml"
if ! LODESTAR_IMAGE=unused docker compose -f "$STACK/compose.yaml" --project-directory "$STACK" \
    up -d gluetun >/dev/null 2>&1; then
  bad tunnel_started; finish
fi
check tunnel_healthy wait_healthy

# Tunnel up: IPv4 leaves through PIA (not the host's address), IPv6 fails, and
# DNS answers through gluetun's own resolver.
host_ip=$(ip -4 route get 1.1.1.1 2>/dev/null | sed -n 's/.* src \([0-9.]*\).*/\1/p' | head -n 1)
exit_ip=$(in_ns wget -qO- -T 10 "$ECHO_URL" 2>/dev/null | tr -d '\r\n' | head -c 64)
if [[ $exit_ip =~ $IPV4 && $host_ip =~ $IPV4 && $exit_ip != "$host_ip" ]]; then
  pass ipv4_through_tunnel
else
  bad ipv4_through_tunnel
fi
refute ipv6_blocked_up in_ns wget -qO- -T 10 "$V6_URL"
check dns_up in_ns nslookup example.com
check dns_local in_ns grep -qx 'nameserver 127.0.0.1' /etc/resolv.conf

# Tunnel down: drop the WireGuard endpoint in DOCKER-USER (gluetun keeps
# running, so its firewall is what's tested), capture on gluetun's veth, and
# probe everything.
idx=$(in_ns cat /sys/class/net/eth0/iflink 2>/dev/null | tr -dc '0-9')
veth=$(ip -o link 2>/dev/null | awk -F': ' -v i="$idx" '$1 == i {print $2}' | cut -d@ -f1 | head -n 1)
if [[ -z $idx || ! $veth =~ ^veth[0-9a-f]{1,12}$ ]]; then bad veth_found; finish; fi
pass veth_found
block="-d $ep_ip/32 -p udp --dport $ep_port -j DROP"
if ! iptables -I DOCKER-USER 1 $block 2>/dev/null; then block=""; bad endpoint_blocked; finish; fi
pass endpoint_blocked
tcpdump -i "$veth" -n -U -w "$work/cap.pcap" \
  "not arp and not (udp and host $ep_ip and port $ep_port)" >/dev/null 2>&1 &
cap_pid=$!
for ((i = 0; i < 20; i++)); do [[ -e $work/cap.pcap ]] && break; sleep 0.5; done
refute ipv4_blocked_down in_ns wget -qO- -T 8 "$ECHO_URL"
refute ipv6_blocked_down in_ns wget -qO- -T 8 "$V6_URL"
refute dns_blocked_down in_ns nslookup example.com
refute dns_direct_blocked_down in_ns nslookup example.com 1.1.1.1
in_ns sh -c 'echo probe | nc -u -w 2 1.1.1.1 53' >/dev/null 2>&1  # the capture judges it
kill "$cap_pid" 2>/dev/null; wait "$cap_pid" 2>/dev/null; cap_pid=""
captured=$(timeout 30 tcpdump -r "$work/cap.pcap" -n 2>/dev/null | grep -c . || true)
echo "count captured_packets ${captured:-0}"
if [[ -s $work/cap.pcap && ${captured:-1} -eq 0 ]]; then pass no_egress_captured; else bad no_egress_captured; fi

# Recovery: lift the block, and the tunnel comes back.
iptables -D DOCKER-USER $block 2>/dev/null && block=""
check tunnel_recovers wait_healthy

# PIA delivery's work directory is gone (this step holds the VM's lock).
check run_clean test ! -e "$PIA_RUN"
finish
