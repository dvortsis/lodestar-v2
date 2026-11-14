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
# A gluetun with its firewall off passes A but fails B, and so does a gluetun
# without its post-rules (compose.yaml), which lets connections opened through
# the tunnel leave by eth0 once the interface is down (X-gluetun-leak).
#
# Each phase is judged twice, and both must read zero:
#   - a capture on gluetun's veth (everything but ARP and the tunnel's own UDP
#     to the endpoint), counted by class;
#   - a packet counter in the host's DOCKER-USER chain on what the compose
#     bridge forwards out of the host's egress interface, just below the
#     endpoint block, so only the endpoint's own UDP is left out (and dropped).
#
# Run as root by the step bootstrap: step.sh <workdir> [stack] [pia-run] [block]
# (tests and start-block pass the last three). start-block adds a fifth,
# `held`: it already holds the host guard for its block, so the test checks
# that guard is there and never adds or removes one. <workdir> holds the committed
# compose.yaml and block-lib.sh. Output is fixed names only (`ok|fail|skip
# <name>`, `count <name> <int>`); the control side drops anything else. `skip`
# marks a check that can't be judged (the tunnel-up checks when the WireGuard
# handshake never completed), never a pass. Exit 0 only when every check passed.
set -uo pipefail
work=${1:?workdir}
STACK=${2:-/home/lodestar/lodestar}  # tests pass a temporary stack; the bootstrap never does
PIA_RUN=${3:-/run/lodestar-pia}
BLOCK=${4:-/var/lib/lodestar-block}
HOST_HELD=${5:-}
CONF=$STACK/secrets/wg0.conf
ECHO_URL=http://checkip.amazonaws.com
V6_URL='http://[2606:4700:4700::1111]/'
# The probes run from a throwaway container in gluetun's namespace (plan 0d), so
# they never depend on the kill switch's own image: PROBE_IMAGE, netshoot v0.16
# by its multi-arch index digest, from block-lib.sh with the block marker. The
# endpoint's validation (IPV4, endpoint_read) and the host guard are there too.
# shellcheck source=block-lib.sh
. "$work/block-lib.sh"
NOT_RUN=222  # a probe whose container never ran its command: never counts as blocked
fails=0
block=""
guard_rule=()   # the recreate guard (DOCKER-USER), once the endpoint is known
host_rule=()    # the host's own guard (INPUT, block-lib.sh), once the endpoint is known
guard_set=""
guard_held=""
released_bad=""
host_set=""
counter=""
flow=""
FLOW=lodestar-leak-flow
cap_pid=""
not_run=""

pass() { echo "ok $1"; }
bad() { echo "fail $1"; fails=$((fails + 1)); }
check() { local n=$1 rc; shift; "$@" >/dev/null 2>&1; rc=$?; if ((rc == 0)); then pass "$n"; else bad "$n"; fi; }
refute() { local n=$1 rc; shift; "$@" >/dev/null 2>&1; rc=$?; if ((rc != 0 && rc != NOT_RUN)); then pass "$n"; else bad "$n"; fi; }
finish() {
  [[ -n $not_run ]] && bad probe_ran
  release_guards || { bad guards_released; released_bad=1; }  # before the verdict: a guard left behind fails the run
  if [[ $fails -eq 0 ]]; then echo "ok leak_test"; exit 0; fi
  echo "fail leak_test"; exit 1
}
cleanup() {
  trap '' HUP INT TERM  # a second signal must not cut the release short
  [[ -n $cap_pid ]] && kill "$cap_pid" 2>/dev/null
  # The endpoint block never outlives the test, whatever ended it.
  [[ -n $flow ]] && timeout 30 docker rm -f "$FLOW" >/dev/null 2>&1
  # A failed delete in a phase can leave a counter behind; remove every copy.
  if [[ -n $counter ]]; then
    for _ in 1 2 3; do iptables -D DOCKER-USER $counter 2>/dev/null || break; done
  fi
  [[ -n $block ]] && iptables -D DOCKER-USER $block 2>/dev/null
  # finish has released the guards; a signal skips finish, so they're released
  # here too, and a failure is reported (the run's exit code is already non-zero).
  release_guards || [[ -n $released_bad ]] || echo "fail guards_released"
  return 0
}
release_guards() {  # remove both guards, every copy; fails when one won't come off
  local rc=0
  if [[ -n $guard_set ]]; then
    if drop_all DOCKER-USER "${guard_rule[@]}"; then guard_set=""; else rc=1; fi
  fi
  if [[ -n $host_set ]]; then
    if drop_all INPUT "${host_rule[@]}"; then host_set=""; else rc=1; fi
  fi
  return "$rc"
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
read_iface() {  # the VPN interface gluetun's config names; the image's own default is tun0
  local i
  i=$(docker inspect --type container -f '{{range .Config.Env}}{{println .}}{{end}}' gluetun 2>/dev/null \
    | sed -n 's/^VPN_INTERFACE=//p' | tail -n 1)
  printf '%s\n' "${i:-tun0}"
}
# tunnel_up: recreate gluetun alone from the committed compose file and the
# current wg0.conf. Two windows around it can draw a "port unreachable" to the
# VPN endpoint, which keeps sending the old session for a while:
#   - the old container shutting down, or a new one before gluetun has turned
#     its firewall on: the endpoint's packets that still reach it. The
#     DOCKER-USER guard holds them at the host until gluetun's post-rules are
#     in place in both families. Rules that never load leave the guard on
#     (cleanup lifts it) and fail post_rules_before_egress.
#   - the host itself: Docker flushes the container IP's conntrack entries
#     when the old container leaves the network and the new one joins, so the
#     endpoint's leftover packets for the old session go to the host's own
#     INPUT, never FORWARD, and the host's kernel would answer from its public
#     address. The host guard (INPUT) drops them, from before the first
#     recreate until cleanup. It can't touch the tunnel: the new session's
#     replies are de-NATed and forwarded, never delivered to the host.
tunnel_up() {
  local rc i ready=""
  guard_set=1  # before the insert: a signal in between still has cleanup look for it
  iptables -I DOCKER-USER 1 "${guard_rule[@]}" 2>/dev/null || { bad recreate_guarded; return 1; }
  LODESTAR_IMAGE=unused DOCKER_GID=unused docker compose -f "$STACK/compose.yaml" --project-directory "$STACK" \
    up -d --force-recreate gluetun >/dev/null 2>&1
  rc=$?
  if ((rc == 0)); then
    for ((i = 0; i < 30; i++)); do  # gluetun turns its firewall on within seconds of starting
      if [[ $(read_iface) =~ ^[a-z][a-z0-9]{0,14}$ ]] \
        && post_rules_on iptables "$(read_iface)" >/dev/null 2>&1 \
        && post_rules_on ip6tables "$(read_iface)" >/dev/null 2>&1; then
        ready=1; break
      fi
      sleep 1
    done
  fi
  if ((rc == 0)) && [[ -z $ready ]]; then
    bad post_rules_before_egress  # the guard stays on: nothing reaches a gluetun without its rules
    guard_held=1
  elif drop_all DOCKER-USER "${guard_rule[@]}"; then
    guard_set="" guard_held=""
  else
    bad recreate_guard_lifted
  fi
  return "$rc"
}
# post_rules_on family iface: gluetun loaded its post-rules
# (compose.yaml) for this interface, and each of the three comes before the
# first accept in OUTPUT. gluetun skips a missing file without a word, so this
# is checked, not assumed. Its image has both iptables backends; the rules are
# in whichever it uses. POST_RULES_CHECK runs inside gluetun (the unit tests
# run it against a fake iptables).
# shellcheck disable=SC2016  # expanded by the inner shell
POST_RULES_CHECK='for t in "$1" "$1-nft" "$1-legacy"; do
  command -v "$t" >/dev/null 2>&1 || continue
  "$t" -C OUTPUT -o "$2" -j CONNMARK --set-xmark 0x800000/0x800000 2>/dev/null || continue
  "$t" -C OUTPUT ! -o "$2" -m connmark --mark 0x800000/0x800000 -j DROP 2>/dev/null || continue
  if [ "$1" = iptables ]; then
    "$t" -C OUTPUT ! -o "$2" -p icmp -m addrtype ! --dst-type LOCAL -j DROP 2>/dev/null || continue
  else
    "$t" -C OUTPUT ! -o "$2" -p ipv6-icmp ! -d fe80::/10 -m addrtype ! --dst-type LOCAL,MULTICAST -m hl ! --hl-eq 255 -j DROP 2>/dev/null || continue
  fi
  rules=$("$t" -S OUTPUT 2>/dev/null) || continue
  first() { printf "%s\n" "$rules" | grep -n -m 1 -e "$1" | cut -d: -f1; }
  acc=$(first "-j ACCEPT")
  mark=$(first "-o $2 -j CONNMARK --set-xmark 0x800000/0x800000")
  drop=$(first "! -o $2 -m connmark --mark 0x800000/0x800000 -j DROP")
  icmp=$(printf "%s\n" "$rules" | grep -n -e "! -o $2 " | grep -e "--dst-type LOCAL" | grep -m 1 -e "-j DROP" | cut -d: -f1)
  [ -n "$acc" ] && [ -n "$mark" ] && [ -n "$drop" ] && [ -n "$icmp" ] || continue
  [ "$mark" -lt "$acc" ] && [ "$drop" -lt "$acc" ] && [ "$icmp" -lt "$acc" ] && exit 0
done; exit 1'
post_rules_on() { timeout 10 docker exec gluetun sh -c "$POST_RULES_CHECK" rules "$1" "$2"; }
# A connection opened through the tunnel that keeps sending through both
# down phases (an unprivileged ping), so the leak gluetun's post-rules close
# is exercised every run, not only when gluetun happens to hold one open.
flow_start() {
  timeout 30 docker rm -f "$FLOW" >/dev/null 2>&1  # one left by a run that died
  timeout 60 docker run -d --rm --name "$FLOW" --network container:gluetun --cap-drop ALL "$PROBE_IMAGE" \
    ping -q -i 0.5 1.1.1.1 >/dev/null 2>&1
}
# The host-side counter: packets the compose bridge forwards out of the egress
# interface. It sits just below the endpoint block, so the endpoint's own UDP
# (dropped there) is the only thing it leaves out.
start_counter() {
  counter="-i $bridge -o $egress -m comment --comment lodestar-leak-egress"
  iptables -I DOCKER-USER 2 $counter 2>/dev/null || counter=""
}
read_counter() {  # the counter's packets, or nothing when it can't be read
  iptables -L DOCKER-USER -v -x -n 2>/dev/null | awk '/\/\* lodestar-leak-egress \*\// {print $1; exit}' | grep -E '^[0-9]{1,12}$'
}
judge_counter() {  # $1: phase
  local n=""
  if [[ -n $counter ]]; then
    n=$(read_counter)
    iptables -D DOCKER-USER $counter 2>/dev/null && counter=""
  fi
  [[ -n $n ]] && echo "count host_egress_packets_$1 $n"  # no line when it couldn't be read
  if [[ -n $n && $n == 0 ]]; then pass "no_host_egress_$1"; else bad "no_host_egress_$1"; fi
}
start_capture() {  # $1: phase
  tcpdump -i "$veth" -n -U -w "$work/cap-$1.pcap" \
    "not arp and not (udp and host $ep_ip and port $ep_port)" >/dev/null 2>&1 &
  cap_pid=$!
  for ((i = 0; i < 20; i++)); do [[ -s $work/cap-$1.pcap ]] && break; sleep 0.5; done
}
# CAPTURE_PARSE (python3 -I, from stdin): reads a phase's pcap and prints
# grammar lines only. A packet counts as sent by the container when its
# Ethernet source is the container's own MAC; packets towards the container
# are counted apart (captured_inbound) and never judged. For every ICMP or
# ICMPv6 packet it prints its type, code, direction and the far end's class
# (endpoint, linklocal, multicast, other: the destination of what the
# container sends, the source of what it receives): a class, never the address.
# Exit 2 when the file isn't a readable Ethernet pcap.
CAPTURE_PARSE='
import struct, sys
path, mac, ep, phase = sys.argv[1:5]
mac = bytes.fromhex(mac.replace(":", ""))
ep = bytes(int(x) for x in ep.split("."))
data = open(path, "rb").read()
magics = {b"\xd4\xc3\xb2\xa1": "<", b"\xa1\xb2\xc3\xd4": ">", b"\x4d\x3c\xb2\xa1": "<", b"\xa1\xb2\x3c\x4d": ">"}
if len(data) < 24 or data[:4] not in magics:
    sys.exit(2)
e = magics[data[:4]]
if struct.unpack(e + "I", data[20:24])[0] != 1:
    sys.exit(2)
def dclass(fam, d):
    if fam == 4 and d == ep:
        return "endpoint"
    if (fam == 4 and d[:2] == bytes([169, 254])) or (fam == 6 and d[0] == 0xFE and d[1] & 0xC0 == 0x80):
        return "linklocal"
    if (fam == 4 and (d[0] & 0xF0 == 0xE0 or d == bytes([255] * 4))) or (fam == 6 and d[0] == 0xFF):
        return "multicast"
    return "other"
out = inb = 0
cls = {"tcp": 0, "udp": 0, "icmp": 0, "other": 0}
icmp = {}
off = 24
while off < len(data):
    if off + 16 > len(data):
        sys.exit(2)
    n = struct.unpack(e + "I", data[off + 8:off + 12])[0]
    pkt = data[off + 16:off + 16 + n]
    off += 16 + n
    if len(pkt) != n or n < 14:
        sys.exit(2)
    sent = pkt[6:12] == mac
    et, p = struct.unpack(">H", pkt[12:14])[0], pkt[14:]
    if et == 0x8100 and len(p) >= 4:
        et, p = struct.unpack(">H", p[2:4])[0], p[4:]
    proto = fam = d = l4 = None
    if et == 0x0800 and len(p) >= 20:  # the far end: the destination of what it sends, the source of the rest
        fam, proto, d, l4 = 4, p[9], p[16:20] if sent else p[12:16], p[(p[0] & 15) * 4:]
    elif et == 0x86DD and len(p) >= 40:
        fam, proto, d, l4 = 6, p[6], p[24:40] if sent else p[8:24], p[40:]
    kind = {6: "tcp", 17: "udp", 1: "icmp", 58: "icmp"}.get(proto, "other")
    if sent:
        out += 1
        cls[kind] += 1
    else:
        inb += 1
    if kind == "icmp" and len(l4) >= 2:
        key = "icmp_%s_%s_t%d_c%d_%s" % (phase, "out" if sent else "in", l4[0], l4[1], dclass(fam, d))
        icmp[key] = icmp.get(key, 0) + 1
print("count captured_packets_%s %d" % (phase, out))
print("count captured_inbound_%s %d" % (phase, inb))
for k in ("tcp", "udp", "icmp", "other"):
    print("count captured_%s_%s %d" % (k, phase, cls[k]))
for k in sorted(icmp)[:40]:
    print("count %s %d" % (k, icmp[k]))
'
judge_capture() {  # $1: phase. The capture must have run through every probe, then be readable, and the container sent nothing.
  local alive="" captured="" parsed
  if kill -0 "$cap_pid" 2>/dev/null; then alive=1; pass "capture_alive_$1"; else bad "capture_alive_$1"; fi
  kill "$cap_pid" 2>/dev/null; wait "$cap_pid" 2>/dev/null; cap_pid=""
  if parsed=$(timeout 30 python3 -I -c "$CAPTURE_PARSE" "$work/cap-$1.pcap" "$cmac" "$ep_ip" "$1" 2>/dev/null); then
    captured=$(printf '%s\n' "$parsed" | sed -n "s/^count captured_packets_$1 \([0-9]*\)$/\1/p")
    pass "capture_read_$1"
    printf '%s\n' "$parsed"
  else
    bad "capture_read_$1"  # no count line: an unreadable capture is never read as zero
  fi
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
for tool in docker iptables tcpdump ip python3; do
  command -v "$tool" >/dev/null 2>&1 || { bad "tool_missing_$tool"; finish; }
done
[[ -z $HOST_HELD || $HOST_HELD == held ]] || { bad args_valid; finish; }
[[ -s $CONF ]] || { bad wg_conf_present; finish; }
endpoint_read "$CONF" || { bad endpoint_valid; finish; }
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
guard_rule=(-s "$ep_ip/32" -p udp --sport "$ep_port" -m comment --comment lodestar-leak-guard -j DROP)
host_guard_rule
# A killed run can have left a recreate guard behind, for this endpoint or an
# earlier one: every rule with its tag goes first.
clear_tagged DOCKER-USER lodestar-leak-guard || { bad recreate_guard_lifted; finish; }
if [[ -n $HOST_HELD ]]; then
  # start-block's guard for its block, for this same endpoint: checked, never
  # duplicated, and left in place when the test ends.
  iptables -C INPUT "${host_rule[@]}" 2>/dev/null || { bad host_guarded; finish; }
else
  # On its own, the test holds its own copy for its run. Leftovers (a killed
  # run's, an earlier endpoint's) come out once the new copy is in.
  host_set=1  # before the insert: a signal in between still has cleanup look for it
  host_guard_hold || { bad host_guarded; finish; }
fi
tunnel_up || { bad tunnel_started; finish; }
wait_healthy; up=$?
unhealthy_stop() { ((up == 0)) || bad tunnel_healthy; bad "$1"; finish; }  # an early stop still says so
docker pull "$PROBE_IMAGE" >/dev/null 2>&1 || unhealthy_stop probe_image
check probe_tools probe sh -c 'command -v curl && command -v nc && command -v dig && command -v ip'
iface=$(read_iface)  # compose.yaml sets wg0, the name the post-rules use
[[ $iface =~ ^[a-z][a-z0-9]{0,14}$ ]] || unhealthy_stop vpn_interface
# The rules name the interface; a name gluetun didn't use would make them inert.
check vpn_interface_live iface_up
check post_rules_ipv4 post_rules_on iptables "$iface"
check post_rules_ipv6 post_rules_on ip6tables "$iface"

# An unhealthy tunnel whose interface has received nothing never completed its
# WireGuard handshake: an expired or wrong PIA key, or an endpoint that can't be
# reached. Check the VPN credentials, then the endpoint. It's its own outcome,
# so the report says why; it's still a failure, and the kill-switch phases
# below still run and judge any leak on their own. The counter is read in
# gluetun's namespace with no capabilities (`wg show` would need NET_ADMIN, and
# gluetun may run WireGuard in userspace). gluetun's VPN restarts recreate the
# interface (a fresh counter, or none for a moment), so it's read several
# times: "no handshake" needs at least one read, and every read 0. A counter
# that can't be read is never read as "no handshake".
rx_zero() {
  local i rx zero=""
  for ((i = 0; i < 5; i++)); do
    ((i > 0)) && sleep 2
    rx=$(probe cat "/sys/class/net/$iface/statistics/rx_bytes" 2>/dev/null | tr -d '\r\n')
    [[ -z $rx ]] && continue
    [[ $rx == 0 ]] || return 1
    zero=1
  done
  [[ -n $zero ]]
}
no_handshake=""
if ((up == 0)); then
  pass tunnel_healthy
elif [[ -n $guard_held || -n $guard_set ]]; then
  bad recreate_guard_held  # gluetun never loaded its post-rules, so the endpoint stays held: not a credentials problem
elif rx_zero; then
  no_handshake=1
  bad wireguard_handshake_never_completed  # check the VPN credentials
else
  bad tunnel_healthy
fi

# Tunnel up: IPv4 leaves through PIA (not the host's address), TCP to a literal
# address works (the positive control for the down probes), IPv6 fails, and DNS
# answers through gluetun's own resolver. With no handshake the first three,
# and the ping, can't be judged: they're skipped, never passed.
host_ip=$(ip -4 route get 1.1.1.1 2>/dev/null | sed -n 's/.* src \([0-9.]*\).*/\1/p' | head -n 1)
exit_ip=""
if [[ -n $no_handshake ]]; then
  echo "skip ipv4_through_tunnel"; echo "skip tcp_literal_up"; echo "skip dns_up"; echo "skip ping_up"
else
  exit_ip=$(echo_ip 10)
  if [[ $exit_ip =~ $IPV4 && $host_ip =~ $IPV4 && $exit_ip != "$host_ip" ]]; then
    pass ipv4_through_tunnel
  else
    bad ipv4_through_tunnel
  fi
  check tcp_literal_up probe nc -z -w 5 1.1.1.1 443
  check dns_up resolves example.com 5
  check ping_up probe ping -c 2 -W 3 1.1.1.1  # ICMP through the tunnel, no capabilities
fi
refute ipv6_blocked_up probe curl -6 -s -m 10 "$V6_URL"
check dns_local probe grep -qx 'nameserver 127.0.0.1' /etc/resolv.conf
# The flow starts either way: with no handshake its packets still go into wg0.
if flow_start; then flow=1; pass flow_started; else bad flow_started; fi

# Down, phase A: the endpoint dropped in DOCKER-USER (gluetun keeps running).
idx=$(docker exec gluetun cat /sys/class/net/eth0/iflink 2>/dev/null | tr -dc '0-9')
veth=$(ip -o link 2>/dev/null | awk -F': ' -v i="$idx" '$1 == i {print $2}' | cut -d@ -f1 | head -n 1)
if [[ -z $idx || ! $veth =~ ^veth[0-9a-f]{1,12}$ ]]; then bad veth_found; finish; fi
pass veth_found
# The container's own MAC: what it sends carries it as the Ethernet source.
cmac=$(docker exec gluetun cat /sys/class/net/eth0/address 2>/dev/null | tr -d '\r\n')
[[ $cmac =~ ^[0-9a-f]{2}(:[0-9a-f]{2}){5}$ ]] || { bad container_mac; finish; }
# The compose bridge (the veth's master) and the host's egress interface.
bridge=$(ip -o link show dev "$veth" 2>/dev/null | sed -n 's/.* master \([^ ]*\) .*/\1/p' | head -n 1)
egress=$(ip -4 route get 1.1.1.1 2>/dev/null | sed -n 's/.* dev \([^ ]*\) .*/\1/p' | head -n 1)
IFNAME='^[A-Za-z0-9_.][A-Za-z0-9_.-]{0,14}$'
if [[ ! $bridge =~ $IFNAME || ! $egress =~ $IFNAME || $bridge == "$egress" ]]; then bad egress_found; finish; fi
pass egress_found
block="-d $ep_ip/32 -p udp --dport $ep_port -j DROP"
if ! iptables -I DOCKER-USER 1 $block 2>/dev/null; then block=""; bad endpoint_blocked; finish; fi
pass endpoint_blocked
start_counter
start_capture down
down_probes down example.net
judge_capture down
judge_counter down
check flow_alive_down running "$FLOW"

# Down, phase B: also take gluetun's VPN interface down inside the namespace, so
# nothing is routed into the tunnel and only gluetun's firewall holds. (The
# endpoint stays blocked, so a reconnecting gluetun can't bring the tunnel back.)
# The counter and capture start first, so the open flows' first packets after
# the interface goes are seen too.
start_counter
start_capture ifdown
check vpn_interface_down probe_in --cap-add NET_ADMIN -- ip link set dev "$iface" down
down_probes ifdown example.edu
# gluetun's own VPN restart could bring the interface back mid-phase: fail
# closed unless it's still down (or gone) after the last probe.
refute vpn_interface_stayed_down iface_up
judge_capture ifdown
judge_counter ifdown
# The blocked endpoint makes gluetun restart its VPN in phase B: its post-rules
# must still head OUTPUT afterwards.
check post_rules_after_restart_ipv4 post_rules_on iptables "$iface"
check post_rules_after_restart_ipv6 post_rules_on ip6tables "$iface"
check flow_alive_ifdown running "$FLOW"
timeout 30 docker rm -f "$FLOW" >/dev/null 2>&1 && flow=""

# Recovery: lift the block, recreate gluetun (its interface comes back), and the
# tunnel must be healthy again, through the same exit.
iptables -D DOCKER-USER $block 2>/dev/null && block=""
if [[ -n $no_handshake ]]; then
  echo "skip tunnel_recovers"  # a tunnel that never came up can't recover
elif tunnel_up && wait_healthy && [[ -n $exit_ip && $(echo_ip 15) == "$exit_ip" ]]; then
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
