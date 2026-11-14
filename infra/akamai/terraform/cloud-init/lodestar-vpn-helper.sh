#!/bin/bash
# Lodestar's PIA VPN helper (X-pia-vm-helper, from #632). Root-owned, run
# through sudo with one verb from the fixed list in main(); anything else is
# refused. Every awk program here is POSIX (no {m,n} intervals), because
# Ubuntu 24.04's awk is mawk 1.3.4, which panics on nested intervals.
set -Eeuo pipefail
umask 027
cd /

CONFIG_DIR="/run/lodestar/vpn"
CONFIG_FILE="/run/lodestar/vpn/wg0.conf"
COMPOSE_DIR="/etc/lodestar/vpn"
COMPOSE_FILE="/etc/lodestar/vpn/compose.yaml"
COMPOSE_PROJECT="lodestar-vpn"
PIA_CA_FILE="/etc/lodestar/vpn/ca.rsa.4096.crt"
PIA_SERVERLIST_URL="https://serverlist.piaservers.net/vpninfo/servers/v4"
# gluetun v3.41.3 by its multi-arch index digest, as in compose.yaml and the
# fleet's stack (a test checks the three agree).
GLUETUN_DIGEST="sha256:fa19cc76b2af13d57a8d3dc3066f2ada061b1c761b8aecf989b3877c0486e027"
MAX_CONFIG_BYTES=4096

# A dotted IPv4 address: four decimal octets of one to three digits, each <= 255.
AWK_IPV4='
function is_ipv4(s,   n, o, i) {
  n = split(s, o, ".")
  if (n != 4) return 0
  for (i = 1; i <= 4; i++) {
    if (o[i] !~ /^[0-9][0-9]?[0-9]?$/ || o[i] + 0 > 255) return 0
  }
  return 1
}
function is_wg_key(s) { return length(s) == 44 && s ~ /^[A-Za-z0-9+\/]+=$/ }
function is_port(s) { return s ~ /^[0-9][0-9]?[0-9]?[0-9]?[0-9]?$/ && s + 0 >= 1 && s + 0 <= 65535 }
'

no_proxy_curl() {
  env -u http_proxy -u https_proxy -u HTTP_PROXY -u HTTPS_PROXY -u all_proxy -u ALL_PROXY \
    -u no_proxy -u NO_PROXY curl --noproxy '*' "$@"
}

query_wg() {
  local subcmd="${1:-}"
  local gluetun_pid=""
  gluetun_pid="$(docker inspect -f '{{if .State.Running}}{{.State.Pid}}{{else}}0{{end}}' gluetun 2>/dev/null || true)"
  if [ -z "$gluetun_pid" ] || ! [ "$gluetun_pid" -gt 0 ] 2>/dev/null; then
    echo "ERROR: Gluetun container is not running; cannot inspect WireGuard $subcmd" >&2
    return 1
  fi
  if ! command -v nsenter >/dev/null 2>&1 || ! command -v wg >/dev/null 2>&1; then
    echo "ERROR: Required host utilities (nsenter, wg) unavailable for WireGuard $subcmd inspection" >&2
    return 1
  fi
  local out=""
  # Kernel WireGuard: the host's wg reads wg0 in gluetun's network namespace
  # (-n) over netlink. Never enter its mount namespace: the host's wg and libc
  # aren't in gluetun's image.
  out="$(nsenter -t "$gluetun_pid" -n wg show wg0 "$subcmd" 2>/dev/null || true)"
  if [ -z "$out" ]; then
    echo "ERROR: Authoritative kernel WireGuard $subcmd inspection unavailable for wg0" >&2
    return 1
  fi
  echo "$out"
}

extract_cfg_value() {
  local key="${1:-}"
  local cfg_file="${2:-}"
  awk -v target="$key" '
    {
      pos = index($0, "=")
      if (pos > 0) {
        k = substr($0, 1, pos - 1)
        v = substr($0, pos + 1)
        gsub(/^[[:space:]]+|[[:space:]]+$/, "", k)
        gsub(/^[[:space:]]+|[[:space:]]+$/, "", v)
        if (k == target) {
          print v
          exit
        }
      }
    }
  ' "$cfg_file" 2>/dev/null || true
}

# The typed schema for wg0.conf: exactly one [Interface] (Address, PrivateKey,
# DNS) then one [Peer] (PublicKey, a public Endpoint, AllowedIPs 0.0.0.0/0,
# PersistentKeepalive 25). Nothing else, so no PostUp or similar hook gets in.
validate_wg_config() {
  local f="${1:-}"
  local size=""
  size="$(wc -c < "$f")"
  if [ "$size" -gt "$MAX_CONFIG_BYTES" ]; then
    echo "ERROR: WireGuard configuration payload is too large" >&2
    return 1
  fi
  # Printable ASCII, tabs and newlines only; no shell metacharacters.
  if [ -n "$(LC_ALL=C tr -d '\t\n -~' < "$f")" ] || LC_ALL=C grep -q -E '[`$\\"'\'';|&<>()]' "$f"; then
    echo "ERROR: Forbidden characters in configuration payload" >&2
    return 1
  fi
  if ! awk "$AWK_IPV4"'
    BEGIN { sec = ""; iface = 0; peer = 0; addr = 0; priv = 0; dns = 0; pub = 0; ep = 0; aips = 0; ka = 0; ok = 1 }
    /^[[:space:]]*$/ || /^[[:space:]]*#/ { next }
    /^[[:space:]]*\[Interface\][[:space:]]*$/ {
      if (sec != "") { ok = 0; exit 1 }
      sec = "I"; iface++; next
    }
    /^[[:space:]]*\[Peer\][[:space:]]*$/ {
      if (sec != "I") { ok = 0; exit 1 }
      sec = "P"; peer++; next
    }
    {
      pos = index($0, "=")
      if (pos == 0) { ok = 0; exit 1 }
      k = substr($0, 1, pos - 1)
      v = substr($0, pos + 1)
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", k)
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", v)
      if (sec == "I") {
        if (k == "Address" && v ~ /^10\./ && v ~ /\/32$/ && is_ipv4(substr(v, 1, length(v) - 3))) { addr++ }
        else if (k == "PrivateKey" && is_wg_key(v)) { priv++ }
        else if (k == "DNS" && v ~ /^10\./ && is_ipv4(v)) { dns++ }
        else { ok = 0; exit 1 }
      } else if (sec == "P") {
        if (k == "PublicKey" && is_wg_key(v)) { pub++ }
        else if (k == "Endpoint") {
          if (split(v, ep_parts, ":") != 2 || !is_ipv4(ep_parts[1]) || !is_port(ep_parts[2])) { ok = 0; exit 1 }
          split(ep_parts[1], o, ".")
          o1 = o[1] + 0; o2 = o[2] + 0
          # Public unicast only: not 0/8, loopback, private, link-local, CGNAT, multicast or reserved.
          if (o1 == 0 || o1 > 223 || o1 == 10 || o1 == 127 || (o1 == 169 && o2 == 254) || (o1 == 192 && o2 == 168) || (o1 == 172 && o2 >= 16 && o2 <= 31) || (o1 == 100 && o2 >= 64 && o2 <= 127)) { ok = 0; exit 1 }
          ep++
        }
        else if (k == "AllowedIPs" && v == "0.0.0.0/0") { aips++ }
        else if (k == "PersistentKeepalive" && v == "25") { ka++ }
        else { ok = 0; exit 1 }
      } else {
        ok = 0; exit 1
      }
    }
    END {
      if (!ok || iface != 1 || peer != 1 || addr != 1 || priv != 1 || dns != 1 || pub != 1 || ep != 1 || aips != 1 || ka != 1) {
        exit 1
      }
    }
  ' "$f"; then
    echo "ERROR: Invalid WireGuard configuration payload" >&2
    return 1
  fi
}

# The network of an IPv4 CIDR (172.18.0.2/16 -> 172.18.0.0/16), in plain
# arithmetic so mawk's doubles give the same answer as gawk.
cidr_network() {
  printf '%s\n' "${1:-}" | awk -F'[./]' '
    NF == 5 {
      ip = (($1 * 256 + $2) * 256 + $3) * 256 + $4
      plen = $5 + 0
      if (plen < 0 || plen > 32) exit 1
      block = 1
      for (i = plen; i < 32; i++) block *= 2
      net = ip - (ip % block)
      printf "%d.%d.%d.%d/%d\n", int(net / 16777216) % 256, int(net / 65536) % 256, int(net / 256) % 256, net % 256, plen
    }
  '
}

# gluetun's OUTPUT chain: Lodestar's three post-rules first, in order
# (compose.yaml), then only allowed shapes; anything else fails closed. A
# rule counts as DROP or REJECT by its final target alone.
check_output_rules() {
  local rules="${1:-}" vpn_ip="${2:-}" vpn_port="${3:-}" non_vpn_if="${4:-}" container_ip="${5:-}" bridge_subnet="${6:-}"
  printf '%s\n' "$rules" | awk \
    -v vpn_ip="$vpn_ip" \
    -v vpn_port="$vpn_port" \
    -v non_vpn_if="$non_vpn_if" \
    -v container_ip="$container_ip" \
    -v bridge_subnet="$bridge_subnet" '
    BEGIN {
      # The post-rules of Lodestar, inserted at 1, 2 and 3: they must lead the
      # chain, in this order, or an established flow can still leave by eth0.
      post[1] = "-A OUTPUT -o wg0 -j CONNMARK --set-xmark 0x800000/0x800000"
      post[2] = "-A OUTPUT ! -o wg0 -m connmark --mark 0x800000/0x800000 -j DROP"
      post[3] = "-A OUTPUT ! -o wg0 -p icmp -m addrtype ! --dst-type LOCAL -j DROP"
      n = 0
    }
    {
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", $0)
      if ($0 == "" || $0 == "-P OUTPUT DROP") next
      if ($0 ~ /^-A OUTPUT /) n++
      if (n >= 1 && n <= 3) {
        if ($0 != post[n]) {
          print "ERROR: Lodestar'\''s gluetun post-rules are missing or out of order (rule " n " is: " $0 ")" > "/dev/stderr"
          failed = 1; exit 1
        }
        next
      }
      # A comment or a goto could disguise a rule'\''s real target.
      if ($0 ~ /(^| )(--comment|-g|--goto) /) {
        print "ERROR: Unexpected or insecure firewall directive in OUTPUT chain: " $0 > "/dev/stderr"
        failed = 1; exit 1
      }
      # Judged by the final target only: these narrow egress.
      if ($0 ~ /^-A OUTPUT .* -j DROP$/ || $0 ~ /^-A OUTPUT .* -j REJECT( --reject-with [a-z0-9-]+)?$/) next
      if ($0 ~ /-j ACCEPT$/) {
        if ($0 == "-A OUTPUT -o lo -j ACCEPT") next
        if ($0 == "-A OUTPUT -o wg0 -j ACCEPT") next
        if ($0 ~ /^-A OUTPUT -m (conntrack --ctstate|state --state) (RELATED,ESTABLISHED|ESTABLISHED,RELATED) -j ACCEPT$/) next
        # The VPN server: exact IP, interface, UDP and port.
        if (vpn_ip != "" && vpn_port != "") {
          e1 = "-A OUTPUT -d " vpn_ip "/32 -o " non_vpn_if " -p udp -m udp --dport " vpn_port " -j ACCEPT"
          e2 = "-A OUTPUT -d " vpn_ip "/32 -o " non_vpn_if " -p udp --dport " vpn_port " -j ACCEPT"
          e3 = "-A OUTPUT -d " vpn_ip " -o " non_vpn_if " -p udp -m udp --dport " vpn_port " -j ACCEPT"
          if ($0 == e1 || $0 == e2 || $0 == e3) next
        }
        # gluetun'\''s own Docker network: this container'\''s /32 to its bridge subnet.
        if (container_ip != "" && bridge_subnet != "") {
          if ($0 == "-A OUTPUT -s " container_ip "/32 -d " bridge_subnet " -o " non_vpn_if " -j ACCEPT") next
        }
        print "ERROR: Unauthorized egress rule in Gluetun firewall: " $0 > "/dev/stderr"
        failed = 1; exit 1
      }
      print "ERROR: Unexpected or insecure firewall directive in OUTPUT chain: " $0 > "/dev/stderr"
      failed = 1; exit 1
    }
    END {
      if (!failed && n < 3) {
        print "ERROR: Lodestar'\''s gluetun post-rules are missing (" n " of 3 rules in OUTPUT)" > "/dev/stderr"
        exit 1
      }
    }
  '
}

check_ca_file() {
  # Fail closed when the PIA root CA is missing, a symlink, unreadable or not PEM.
  if [ -L "$PIA_CA_FILE" ] || [ ! -f "$PIA_CA_FILE" ] || [ ! -r "$PIA_CA_FILE" ] || [ ! -s "$PIA_CA_FILE" ]; then
    echo "ERROR: PIA Root CA certificate ($PIA_CA_FILE) is missing, unreadable, or a symlink" >&2
    return 1
  fi
  if ! grep -q -- "-----BEGIN CERTIFICATE-----" "$PIA_CA_FILE" 2>/dev/null || ! grep -q -- "-----END CERTIFICATE-----" "$PIA_CA_FILE" 2>/dev/null; then
    echo "ERROR: PIA Root CA certificate ($PIA_CA_FILE) is invalid or corrupted" >&2
    return 1
  fi
  if command -v openssl >/dev/null 2>&1 && ! openssl x509 -in "$PIA_CA_FILE" -noout >/dev/null 2>&1; then
    echo "ERROR: PIA Root CA certificate ($PIA_CA_FILE) failed X.509 parsing" >&2
    return 1
  fi
}

verify_pia_endpoint_binding() {
  local cfg_file="${1:-}"
  local cfg_ep="" ep_ip="" ep_port="" cfg_pub=""
  cfg_ep="$(extract_cfg_value "Endpoint" "$cfg_file")"
  ep_ip="${cfg_ep%:*}"
  ep_port="${cfg_ep#*:}"
  cfg_pub="$(extract_cfg_value "PublicKey" "$cfg_file")"
  if [ -z "$ep_ip" ] || [ -z "$ep_port" ] || [ -z "$cfg_pub" ] || ! [[ "$ep_port" =~ ^[0-9]+$ ]]; then
    echo "ERROR: Missing Endpoint or PublicKey in WireGuard configuration" >&2
    return 1
  fi
  check_ca_file || return 1
  local sl_raw="" curl_sl_rc=0 sl_json=""
  sl_raw="$(no_proxy_curl -fsSL --max-time 10 --max-filesize 4194304 "$PIA_SERVERLIST_URL" 2>/dev/null)" || curl_sl_rc=$?
  sl_json="$(printf '%s\n' "$sl_raw" | head -n 1 | tr -d '\r')"
  if [ "$curl_sl_rc" -ne 0 ] || [ -z "$sl_json" ]; then
    echo "ERROR: Failed to fetch authoritative PIA server list for endpoint authorization (curl exit $curl_sl_rc)" >&2
    return 1
  fi
  # The UDP port must be one the server list gives for WireGuard (1337 if it lists none).
  local port_allowed=""
  port_allowed="$(printf '%s' "$sl_json" | jq -r --argjson p "$ep_port" 'if (.groups.wg | type) == "array" and ([.groups.wg[]?.ports[]?] | length) > 0 then (if ([.groups.wg[]?.ports[]?] | index($p)) != null then "yes" else "no" end) else (if $p == 1337 then "yes" else "no" end) end' 2>/dev/null | tr -d '\r[:space:]' || true)"
  if [ "$port_allowed" != "yes" ]; then
    echo "ERROR: Unauthorized WireGuard UDP port $ep_port: not permitted by PIA server list metadata" >&2
    return 1
  fi
  local server_cn=""
  server_cn="$(printf '%s' "$sl_json" | jq -r --arg ip "$ep_ip" '.regions[]?.servers.wg[]? | select(.ip == $ip) | .cn' 2>/dev/null | tr -d '\r' | head -n 1 || true)"
  if [ -z "$server_cn" ] || [ "$server_cn" = "null" ]; then
    echo "ERROR: Unauthorized WireGuard endpoint $ep_ip: not present in authoritative PIA WireGuard server list" >&2
    return 1
  fi
  if ! printf '%s\n' "$server_cn" | grep -q -x -E '[a-zA-Z0-9.-]+'; then
    echo "ERROR: Invalid server Common Name format from PIA metadata" >&2
    return 1
  fi
  # The endpoint must present a certificate for that name signed by PIA's CA.
  local http_code="" curl_tls_rc=0
  http_code="$(no_proxy_curl -sS --max-time 10 --cacert "$PIA_CA_FILE" --resolve "${server_cn}:${ep_port}:${ep_ip}" -o /dev/null -w '%{http_code}' "https://${server_cn}:${ep_port}/addKey" 2>/dev/null)" || curl_tls_rc=$?
  http_code="$(printf '%s' "$http_code" | tr -d ' \t\r\n')"
  if [ "$curl_tls_rc" -ne 0 ] || [ -z "$http_code" ] || [ "$http_code" = "000" ]; then
    echo "ERROR: Endpoint $ep_ip ($server_cn:$ep_port) failed PIA Root CA TLS verification (curl exit $curl_tls_rc, http_code ${http_code:-000})" >&2
    return 1
  fi
  return 0
}

require_compose_file() {
  if [ ! -f "$COMPOSE_FILE" ] || [ -L "$COMPOSE_FILE" ]; then
    echo "ERROR: $COMPOSE_FILE missing or symlink" >&2
    return 1
  fi
}

require_config_file() {
  if [ ! -f "$CONFIG_FILE" ] || [ -L "$CONFIG_FILE" ]; then
    echo "ERROR: $CONFIG_FILE missing or symlink" >&2
    return 1
  fi
}

compose() {
  docker compose -p "$COMPOSE_PROJECT" --project-directory "$COMPOSE_DIR" -f "$COMPOSE_FILE" "$@"
}

# The container named gluetun must be this stack's, never the fleet's or
# another project's; every verb that reads or enters it checks first.
require_stack_gluetun() {
  local svc_label="" proj_label=""
  svc_label="$(docker inspect -f '{{index .Config.Labels "com.docker.compose.service"}}' gluetun 2>/dev/null || true)"
  proj_label="$(docker inspect -f '{{index .Config.Labels "com.docker.compose.project"}}' gluetun 2>/dev/null || true)"
  if [ "$svc_label" != "gluetun" ] || [ "$proj_label" != "$COMPOSE_PROJECT" ]; then
    echo "ERROR: Container 'gluetun' is not this stack's gluetun service (service: $svc_label, project: $proj_label)" >&2
    return 1
  fi
}

verify_tunnel_isolation_gate() {
  require_compose_file || return 1
  require_config_file || return 1
  # 1. gluetun reports healthy.
  local health_status=""
  health_status="$(docker inspect -f '{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' gluetun 2>/dev/null || echo "inspect_failed")"
  if [ "$health_status" != "healthy" ]; then
    echo "ERROR: Gluetun health check failed (status: $health_status, required: healthy)" >&2
    return 1
  fi
  # 2. It is this stack's gluetun, not the fleet's or another project's.
  require_stack_gluetun || return 1
  # 3. The pinned image.
  local img_ref=""
  img_ref="$(docker inspect -f '{{.Config.Image}}' gluetun 2>/dev/null || true)"
  case "$img_ref" in
    *"@$GLUETUN_DIGEST") ;;
    *)
      echo "ERROR: Container 'gluetun' is not running approved pinned image digest" >&2
      return 1
      ;;
  esac
  # 4. A fresh handshake (<= 120 s).
  local hs_out="" hs_peer="" hs_epoch="" now="" hs_age=0
  hs_out="$(query_wg latest-handshakes)" || return 1
  hs_peer="$(echo "$hs_out" | awk '{print $1}')"
  hs_epoch="$(echo "$hs_out" | awk '{print $2}')"
  if ! [[ "$hs_epoch" =~ ^[0-9]+$ ]] || [ "$hs_epoch" -le 0 ]; then
    echo "ERROR: WireGuard tunnel handshake missing or inactive" >&2
    return 1
  fi
  now="$(date +%s)"
  hs_age=$(( now - hs_epoch ))
  if [ "$hs_age" -lt 0 ] || [ "$hs_age" -gt 120 ]; then
    echo "ERROR: WireGuard handshake is stale (${hs_age}s > 120s limit)" >&2
    return 1
  fi
  # 5. Traffic both ways.
  local tx_out="" rx_b="" tx_b=""
  tx_out="$(query_wg transfer)" || return 1
  rx_b="$(echo "$tx_out" | awk '{print $2}')"
  tx_b="$(echo "$tx_out" | awk '{print $3}')"
  if ! [[ "$rx_b" =~ ^[0-9]+$ && "$tx_b" =~ ^[0-9]+$ ]] || [ "$rx_b" -le 0 ] || [ "$tx_b" -le 0 ]; then
    echo "ERROR: WireGuard transfer inactive (rx=${rx_b:-0}, tx=${tx_b:-0})" >&2
    return 1
  fi
  # 6. The endpoint is PIA's, and the live peer and endpoint are wg0.conf's.
  verify_pia_endpoint_binding "$CONFIG_FILE" || return 1
  local cfg_pub="" cfg_ep="" ep_out="" obs_peer="" obs_ep=""
  cfg_pub="$(extract_cfg_value "PublicKey" "$CONFIG_FILE")"
  cfg_ep="$(extract_cfg_value "Endpoint" "$CONFIG_FILE")"
  ep_out="$(query_wg endpoints)" || return 1
  obs_peer="$(echo "$ep_out" | awk '{print $1}')"
  obs_ep="$(echo "$ep_out" | awk '{print $2}')"
  if [ -z "$cfg_pub" ] || [ "$hs_peer" != "$cfg_pub" ] || [ "$obs_peer" != "$cfg_pub" ]; then
    echo "ERROR: Observed WireGuard peer key ($hs_peer) does not match configured PublicKey ($cfg_pub)" >&2
    return 1
  fi
  if [ -z "$cfg_ep" ] || [ "$obs_ep" != "$cfg_ep" ]; then
    echo "ERROR: Observed WireGuard peer endpoint ($obs_ep) does not match configured Endpoint ($cfg_ep)" >&2
    return 1
  fi
  # 7. Firewall, IPv6 off and the DNS route through wg0.
  verify_dns_firewall >/dev/null || return 1
  # 8. DNS over TLS answers inside gluetun.
  local dns_out=""
  dns_out="$(docker exec gluetun nslookup -timeout=5 example.com 127.0.0.1 2>/dev/null || true)"
  if ! echo "$dns_out" | grep -q "Address"; then
    echo "ERROR: Internal DoT DNS resolution failed inside Gluetun container" >&2
    return 1
  fi
  # 9. The tunnel's exit address is neither the host's nor a Linode/Akamai one.
  local vpn_ip="" host_ip="" ipinfo_json="" org_val=""
  vpn_ip="$(query_egress)" || return 1
  if [ "$(printf '%s\n' "$vpn_ip" | awk "$AWK_IPV4"'{ print is_ipv4($0) }')" != "1" ]; then
    echo "ERROR: Invalid VPN egress IP format ($vpn_ip)" >&2
    return 1
  fi
  host_ip="$(no_proxy_curl -fsSL --max-time 5 https://checkip.amazonaws.com 2>/dev/null | tr -d '[:space:]' || true)"
  if [ "$(printf '%s\n' "$host_ip" | awk "$AWK_IPV4"'{ print is_ipv4($0) }')" != "1" ]; then
    host_ip="$(ip -4 route get 192.0.2.1 2>/dev/null | awk '{for(i=1;i<=NF;i++) if($i=="src") {print $(i+1); exit}}' || true)"
  fi
  if [ -z "$host_ip" ]; then
    echo "ERROR: Host IP unavailable; cannot rule out an egress leak" >&2
    return 1
  fi
  if [ "$vpn_ip" = "$host_ip" ]; then
    echo "ERROR: CRITICAL LEAK: VPN egress IP ($vpn_ip) matches host IP ($host_ip)" >&2
    return 1
  fi
  ipinfo_json="$(docker exec gluetun wget -qO- -T 5 https://ipinfo.io/json 2>/dev/null || true)"
  org_val="$(printf '%s' "$ipinfo_json" | jq -r '.org // empty' 2>/dev/null | tr '[:upper:]' '[:lower:]' | tr -d '\r' || true)"
  if [ -z "$org_val" ]; then
    echo "ERROR: Egress ISP lookup failed: missing org attribute in ipinfo response" >&2
    return 1
  fi
  if echo "$org_val" | grep -q -E '(linode|akamai)'; then
    echo "ERROR: CRITICAL LEAK: Container egress is routing through host ISP/cloud ($org_val)" >&2
    return 1
  fi
  # 10. A crawler service, if compose.yaml has one, lives in gluetun's namespace.
  local crawler_net=""
  crawler_net="$(compose --profile crawler config --format json 2>/dev/null | jq -r 'if .services.crawler then (.services.crawler.network_mode // "unset") else "absent" end' 2>/dev/null || echo "config_failed")"
  if [ "$crawler_net" != "absent" ] && [ "$crawler_net" != "service:gluetun" ] && [ "$crawler_net" != "container:gluetun" ]; then
    echo "ERROR: Crawler is not restricted to Gluetun network namespace in $COMPOSE_FILE ($crawler_net)" >&2
    return 1
  fi
  printf '{"status":"protected","vpn_ip":"%s","host_ip":"%s","endpoint":"%s","handshake":"active","handshake_age":%d,"rx_bytes":%d,"tx_bytes":%d,"dns_leak_protected":true,"kill_switch_active":true}\n' \
    "$vpn_ip" "$host_ip" "$obs_ep" "$hs_age" "$rx_b" "$tx_b"
}

query_egress() {
  # Ask over the live tunnel each time; never a cached address.
  local egress_ip=""
  egress_ip="$(docker exec gluetun wget -qO- -T 5 https://checkip.amazonaws.com 2>/dev/null | tr -d '[:space:]' || true)"
  if [ -z "$egress_ip" ]; then
    echo "ERROR: Active tunneled egress check failed" >&2
    return 1
  fi
  echo "$egress_ip"
}

verify_dns_firewall() {
  local non_vpn_if="" if_cidr="" container_ip="" bridge_subnet="" fw_rules="" raw_ep_full="" vpn_server_ip="" vpn_server_port="" route_out=""
  non_vpn_if="$(docker exec gluetun ip -o link show 2>/dev/null | awk -F': ' '{sub(/@.*/, "", $2); if ($2 != "lo" && $2 != "wg0") print $2}' | head -n1)"
  if [ -z "$non_vpn_if" ]; then
    echo "ERROR: Failed to discover non-VPN interface in Gluetun container" >&2
    return 1
  fi
  if_cidr="$(docker exec gluetun ip -o -4 addr show dev "$non_vpn_if" 2>/dev/null | awk '{for(i=1;i<=NF;i++) if($i=="inet") {print $(i+1); exit}}' || true)"
  if [ -n "$if_cidr" ]; then
    container_ip="${if_cidr%/*}"
  fi
  bridge_subnet="$(docker exec gluetun ip -o -4 route show dev "$non_vpn_if" proto kernel scope link 2>/dev/null | awk '{print $1; exit}' || true)"
  if [ -z "$bridge_subnet" ] && [ -n "$if_cidr" ]; then
    bridge_subnet="$(cidr_network "$if_cidr" || true)"
  fi
  if [ -z "$container_ip" ] || [ -z "$bridge_subnet" ]; then
    echo "ERROR: Failed to determine Gluetun container IP or Docker bridge subnet for $non_vpn_if" >&2
    return 1
  fi
  fw_rules="$(docker exec gluetun iptables -S OUTPUT 2>/dev/null || true)"
  if [ -z "$fw_rules" ]; then
    echo "ERROR: Failed to inspect Gluetun iptables OUTPUT rules" >&2
    return 1
  fi
  if ! printf '%s\n' "$fw_rules" | grep -q -x -- '-P OUTPUT DROP'; then
    echo "ERROR: Default OUTPUT policy is not DROP" >&2
    return 1
  fi
  if ! printf '%s\n' "$fw_rules" | grep -q -x -- '-A OUTPUT -o lo -j ACCEPT'; then
    echo "ERROR: Loopback egress rule (-o lo -j ACCEPT) missing" >&2
    return 1
  fi
  if ! printf '%s\n' "$fw_rules" | grep -q -x -- '-A OUTPUT -o wg0 -j ACCEPT'; then
    echo "ERROR: WireGuard egress rule (-o wg0 -j ACCEPT) missing" >&2
    return 1
  fi
  if [ -f "$CONFIG_FILE" ] && [ ! -L "$CONFIG_FILE" ]; then
    raw_ep_full="$(extract_cfg_value "Endpoint" "$CONFIG_FILE")"
  fi
  if [ -z "$raw_ep_full" ]; then
    raw_ep_full="$(query_wg endpoints 2>/dev/null | awk '{print $2}' || true)"
  fi
  if printf '%s\n' "$raw_ep_full" | grep -q -x -E '[0-9]{1,3}(\.[0-9]{1,3}){3}:[0-9]{1,5}'; then
    vpn_server_ip="${raw_ep_full%:*}"
    vpn_server_port="${raw_ep_full#*:}"
  fi
  check_output_rules "$fw_rules" "$vpn_server_ip" "$vpn_server_port" "$non_vpn_if" "$container_ip" "$bridge_subnet" || return 1
  if [ "$(docker exec gluetun cat /proc/sys/net/ipv6/conf/all/disable_ipv6 2>/dev/null || true)" != "1" ]; then
    echo "ERROR: IPv6 is not disabled inside Gluetun container" >&2
    return 1
  fi
  route_out="$(docker exec gluetun ip route get 192.0.2.1 2>/dev/null || true)"
  if ! echo "$route_out" | grep -q 'dev wg0'; then
    echo "ERROR: Default route does not go through wg0: $route_out" >&2
    return 1
  fi
  echo "DNS_FIREWALL_OK"
}

require_kernel_wireguard() {
  if ! command -v wg >/dev/null 2>&1 || ! command -v nsenter >/dev/null 2>&1; then
    echo "ERROR: Required WireGuard tools (wg, nsenter) unavailable on host" >&2
    return 1
  fi
  # Kernel WireGuard; a module listing is not evidence.
  if [ ! -d /sys/module/wireguard ] && ! modprobe wireguard; then
    echo "ERROR: Kernel WireGuard unavailable: modprobe wireguard failed" >&2
    return 1
  fi
  if [ ! -d /sys/module/wireguard ]; then
    echo "ERROR: Kernel WireGuard unavailable: /sys/module/wireguard missing after modprobe" >&2
    return 1
  fi
}

write_config() {
  local parent_dir="" tmp_cfg=""
  parent_dir="$(dirname "$CONFIG_DIR")"
  if [ -L "$parent_dir" ] || [ -L "$CONFIG_DIR" ]; then
    echo "ERROR: $CONFIG_DIR or parent is a symlink" >&2
    return 1
  fi
  # Called as `write_config || exit 1`, so set -e is off in here: every
  # step checks its own result, and the temp file goes on any exit.
  if ! install -d -m 0755 -o root -g root "$parent_dir" || ! install -d -m 0700 -o root -g root "$CONFIG_DIR"; then
    echo "ERROR: cannot create $CONFIG_DIR" >&2
    return 1
  fi
  if ! tmp_cfg="$(mktemp "$CONFIG_DIR/wg0.tmp.XXXXXX")"; then
    echo "ERROR: cannot create a temporary file in $CONFIG_DIR" >&2
    return 1
  fi
  WRITE_TMP="$tmp_cfg"
  trap 'rm -f -- "$WRITE_TMP"' EXIT
  # One byte over the limit is enough to refuse; never read stdin unbounded.
  if ! head -c "$((MAX_CONFIG_BYTES + 1))" > "$tmp_cfg" || ! chmod 0600 "$tmp_cfg"; then
    echo "ERROR: cannot write the configuration" >&2
    return 1
  fi
  if ! validate_wg_config "$tmp_cfg" || ! verify_pia_endpoint_binding "$tmp_cfg"; then
    return 1
  fi
  if [ -e "$CONFIG_FILE" ] && [ ! -f "$CONFIG_FILE" ] && [ ! -L "$CONFIG_FILE" ]; then
    echo "ERROR: $CONFIG_FILE exists and is not a file; run cleanup first" >&2
    return 1
  fi
  if ! mv -f -T "$tmp_cfg" "$CONFIG_FILE"; then
    echo "ERROR: cannot install $CONFIG_FILE" >&2
    return 1
  fi
  echo "CONFIG_OK"
}

compose_down_verified() {
  if ! docker info >/dev/null 2>&1; then
    echo "ERROR: Docker daemon is unavailable; cannot stop VPN containers safely" >&2
    return 1
  fi
  if ! compose --profile crawler down --remove-orphans; then
    echo "ERROR: Failed to stop VPN and crawler containers via Docker Compose" >&2
    return 1
  fi
  local remaining_q=""
  if ! remaining_q="$(compose --profile crawler ps -q 2>&1)"; then
    echo "ERROR: Failed to query Compose container state after shutdown: $remaining_q" >&2
    return 1
  fi
  if [ -n "$(printf '%s' "$remaining_q" | tr -d '[:space:]')" ]; then
    echo "ERROR: Compose service containers remain active after shutdown ($remaining_q)" >&2
    return 1
  fi
}

# One remote action per VM at a time: the lock the fleet steps and
# deliver-pia hold (infra/fleet/steps/remote.py, VM_LOCK).
VM_LOCK="/run/lodestar-pia.lock"
take_vm_lock() {
  if [ -L "$VM_LOCK" ]; then
    echo "ERROR: $VM_LOCK is a symlink" >&2
    return 1
  fi
  exec 9>"$VM_LOCK"
  if ! flock -w 30 9; then
    echo "ERROR: another action holds $VM_LOCK (vm_busy)" >&2
    return 1
  fi
}

main() {
  local cmd="${1:-}"
  case "$cmd" in
    write-config|start-vpn|start-crawler|stop-vpn|cleanup) take_vm_lock || exit 1 ;;
  esac
  case "$cmd" in
    wg-handshake|wg-transfer|wg-endpoints|query-egress|query-ipinfo|query-dns|inspect-firewall|inspect-ipv6|verify-dns-firewall)
      require_stack_gluetun || exit 1 ;;
  esac
  case "$cmd" in
    preflight)
      docker info >/dev/null 2>&1
      test -c /dev/net/tun
      require_kernel_wireguard || exit 1
      echo "PREFLIGHT_OK"
      ;;
    write-config)
      write_config || exit 1
      ;;
    verify-permissions)
      if [ -L "$CONFIG_DIR" ] || [ ! -d "$CONFIG_DIR" ]; then
        echo "ERROR: $CONFIG_DIR missing or symlink" >&2
        exit 1
      fi
      require_config_file || exit 1
      stat -c '%a %U:%G' "$CONFIG_DIR"
      stat -c '%a %U:%G' "$CONFIG_FILE"
      ;;
    validate-compose)
      require_compose_file || exit 1
      compose config --quiet
      echo "COMPOSE_VALID"
      ;;
    start-vpn)
      require_compose_file || exit 1
      require_config_file || exit 1
      require_kernel_wireguard || exit 1
      verify_pia_endpoint_binding "$CONFIG_FILE" || exit 1
      # Recreate, so a running gluetun picks up a new wg0.conf.
      compose up -d --force-recreate gluetun
      echo "VPN_STARTED"
      ;;
    start-crawler)
      verify_tunnel_isolation_gate >/dev/null || exit 1
      if ! compose --profile crawler config --services 2>/dev/null | grep -q -x crawler; then
        echo "ERROR: $COMPOSE_FILE has no crawler service (see pia-example/crawler-service.example.yaml)" >&2
        exit 1
      fi
      compose --profile crawler up -d crawler
      echo "CRAWLER_STARTED"
      ;;
    verify-tunnel)
      verify_tunnel_isolation_gate || exit 1
      ;;
    stop-vpn)
      require_compose_file || exit 1
      compose_down_verified || exit 1
      echo "STOP_OK"
      ;;
    cleanup)
      if [ -L "$COMPOSE_FILE" ]; then
        echo "ERROR: $COMPOSE_FILE is a symlink; refusing cleanup" >&2
        exit 1
      fi
      if [ -f "$COMPOSE_FILE" ]; then
        # Keep wg0.conf unless this stack's containers are verifiably down.
        compose_down_verified || { echo "ERROR: preserving runtime configuration" >&2; exit 1; }
      fi
      if [ -L "$CONFIG_DIR" ]; then
        echo "ERROR: $CONFIG_DIR is a symlink; refusing removal" >&2
        exit 1
      fi
      if [ -d "$CONFIG_DIR" ]; then
        rm -rf -- "$CONFIG_DIR"
      fi
      echo "CLEANUP_OK"
      ;;
    wg-handshake)
      # The kernel's handshake times; never derived from carrier or byte counters.
      query_wg latest-handshakes || exit 1
      ;;
    wg-transfer)
      query_wg transfer || exit 1
      ;;
    wg-endpoints)
      # The live endpoints; never the configured Endpoint.
      query_wg endpoints || exit 1
      ;;
    query-egress)
      query_egress || exit 1
      ;;
    query-ipinfo)
      docker exec gluetun wget -qO- -T 5 https://ipinfo.io/json
      ;;
    query-dns)
      docker exec gluetun nslookup -timeout=5 example.com 127.0.0.1
      ;;
    verify-dns-firewall)
      verify_dns_firewall || exit 1
      ;;
    inspect-firewall)
      docker exec gluetun iptables -S OUTPUT
      ;;
    inspect-ipv6)
      docker exec gluetun cat /proc/sys/net/ipv6/conf/all/disable_ipv6
      ;;
    *)
      echo "ERROR: Unauthorized helper command: $cmd" >&2
      exit 1
      ;;
  esac
}

# Sourced only by the offline test, to call the validators; run, it takes one verb.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  if [ "$#" -ne 1 ]; then
    echo "ERROR: Unauthorized helper command: expected exactly one verb" >&2
    exit 1
  fi
  main "$1"
fi
