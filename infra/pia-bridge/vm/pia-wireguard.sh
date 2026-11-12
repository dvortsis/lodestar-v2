#!/usr/bin/env bash
# The VM step (OP-2b; docs/plan/2026-11-08-cloud-operator.md §4): the PIA login
# in, a root-only WireGuard configuration for gluetun out, and the login gone
# whatever happens.
#
#   pia-wireguard.sh <workdir> <wg0.conf path>
#
# <workdir> holds login.json ({"username", "password"}), region (a PIA region
# ID) and pia-ca.crt (PIA's CA, committed beside this file). deliver-pia sends
# them over SSH stdin and runs this as root; it discards every byte of output,
# because PIA's API answers with the exit address. Exit codes are the only
# signal: 0 done; 10-19 one failed stage each (see `fail` calls). Stages 13 and
# 15 also print one bounded, sanitised line to stderr for someone running the
# step by hand (never the token, the key or PIA's whole reply).
#
# PIA's manual-connections flow (github.com/pia-foss/manual-connections), with
# the password and token passed to curl only from files, never argv, and the
# endpoint pinned to the address the server list gave.
set -Eeuo pipefail
umask 077
export LC_ALL=C  # byte-wise regex ranges and tr: [a-z] means ASCII only

work=${1:?workdir}
out=${2:?output path}
tmp=""

# shellcheck disable=SC2317,SC2329  # run by the EXIT trap
cleanup() {
  [[ -n $tmp ]] && rm -f -- "$tmp"
  rm -rf -- "$work"
}
trap cleanup EXIT
# HUP, INT and TERM (a cancelled job, or the bootstrap's timeout) end the run, so
# EXIT cleans. A dropped SSH session sends none of them (no PTY): the step then
# runs to the bootstrap's timeout, and EXIT cleans just the same.
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

fail() { exit "$1"; }

# Printable ASCII only: untrusted text made safe for a terminal. Callers bound it.
clean() { printf '%s' "$1" | tr -cd '\40-\176'; }

# A DNS hostname, lowercased by the caller: labels of [a-z0-9-], 1-63 bytes,
# no leading or trailing hyphen, at most 253 bytes. So no ':', '/', '@',
# whitespace or control character can reach --connect-to or the URL.
LABEL='[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?'
HOSTNAME_RE="^$LABEL(\.$LABEL)*\$"
valid_hostname() { (( ${#1} <= 253 )) && [[ $1 =~ $HOSTNAME_RE ]]; }

# Stage 13's failures each say which check failed, in one bounded line.
fail13() { printf 'pia-wireguard: stage 13: %s\n' "$(clean "${1:0:200}")" >&2; fail 13; }

IPV4='^([0-9]{1,3}\.){3}[0-9]{1,3}$'
WGKEY='^[A-Za-z0-9+/]{43}=$'

region=$(<"$work/region")
[[ $region =~ ^[a-z0-9_-]{1,64}$ ]] || fail 10
jq -e '(.username | type == "string" and length > 0) and (.password | type == "string" and length > 0)' \
  "$work/login.json" >/dev/null 2>&1 || fail 11
jq -j '.username' "$work/login.json" >"$work/user"
jq -j '.password' "$work/login.json" >"$work/pass"
rm -f -- "$work/login.json"

# 1. A token for the login. curl's "name=<file" sends the file's contents.
curl --fail --silent --max-time 30 --request POST \
  --form "username=<$work/user" --form "password=<$work/pass" \
  https://www.privateinternetaccess.com/api/client/v2/token >"$work/token.json" 2>/dev/null || fail 12
rm -f -- "$work/user" "$work/pass"
jq -ej '.token | select(type == "string" and length > 0)' "$work/token.json" >"$work/token" 2>/dev/null || fail 12
rm -f -- "$work/token.json"

# 2. The region's WireGuard server: its address and certificate name.
curl --fail --silent --max-time 30 https://serverlist.piaservers.net/vpninfo/servers/v6 \
  >"$work/servers" 2>/dev/null || fail13 "could not fetch the server list"
head -n 1 "$work/servers" >"$work/servers.json"
jq -e 'type == "object"' "$work/servers.json" >/dev/null 2>&1 || fail13 "the server list is not JSON"
jq -e --arg r "$region" 'any(.regions[]?; .id == $r)' "$work/servers.json" >/dev/null 2>&1 \
  || fail13 "region $region is not in the server list"
jq -e --arg r "$region" '[.regions[] | select(.id == $r)][0].servers.wg[0] | type == "object"' \
  "$work/servers.json" >/dev/null 2>&1 || fail13 "region $region has no WireGuard server"
# jq checks the characters first, as bash's $(...) drops NUL bytes ("a\u0000b"
# would come out as "ab"). Anything else becomes "invalid:" and a JSON-escaped
# copy, which the hostname check below rejects and reports.
wg_ip=$(jq -er --arg r "$region" '[.regions[] | select(.id == $r)][0].servers.wg[0].ip
  | select(type == "string" and test("^[0-9.]{7,15}$"))' "$work/servers.json" 2>/dev/null) \
  || fail13 "region $region: the WireGuard server's address is not IPv4"
wg_cn=$(jq -er --arg r "$region" '[.regions[] | select(.id == $r)][0].servers.wg[0].cn
  | select(type == "string") | if test("^[A-Za-z0-9.-]{1,253}$") then . else "invalid:" + (.[:80] | @json) end' \
  "$work/servers.json" 2>/dev/null) || fail13 "region $region: the certificate name is not a string"
[[ $wg_ip =~ $IPV4 ]] || fail13 "region $region: the WireGuard server's address is not IPv4"
# PIA's certificate names are DNS names; some carry capitals. Names match without
# case, in DNS and in curl's certificate check, so lowercase first.
wg_cn=${wg_cn,,}
valid_hostname "$wg_cn" \
  || fail13 "region $region: the certificate name is not a hostname: \"${wg_cn:0:100}\""

# 3. A fresh key pair; PIA registers the public half. The server must present
#    a certificate for its name signed by PIA's CA, at the listed address.
priv=$(wg genkey)
pub=$(printf '%s' "$priv" | wg pubkey)
[[ $priv =~ $WGKEY && $pub =~ $WGKEY ]] || fail 14
#    No --fail: a refusal's status and PIA's reason are what stage 15 reports.
http=$(curl --silent --max-time 30 --max-filesize 65536 --get \
  --connect-to "$wg_cn::$wg_ip:" --cacert "$work/pia-ca.crt" \
  --data-urlencode "pt@$work/token" --data-urlencode "pubkey=$pub" \
  --output "$work/addkey.json" --write-out '%{http_code}' \
  "https://$wg_cn:1337/addKey" 2>/dev/null) || true
[[ $http =~ ^[0-9]{3}$ ]] || http=000  # 000: no HTTP reply (connection, TLS or timeout)
if ! [[ $http =~ ^2 ]] || ! jq -e '.status == "OK"' "$work/addkey.json" >/dev/null 2>&1; then
  # One line: the HTTP status, and PIA's .status and .message if the reply is a
  # JSON object. Never the reply itself; the token and key are masked, to be sure.
  st="" msg=""
  if jq -e 'type == "object"' "$work/addkey.json" >/dev/null 2>&1; then
    # Printable ASCII only, in jq too: a NUL would make bash warn on stderr.
    st=$(jq -r '.status // "" | tostring | gsub("[^ -~]"; "")' "$work/addkey.json" 2>/dev/null) || st=""
    msg=$(jq -r '.message // "" | tostring | gsub("[^ -~]"; "")' "$work/addkey.json" 2>/dev/null) || msg=""
  fi
  token=$(<"$work/token")
  st=$(clean "${st:0:4096}") msg=$(clean "${msg:0:4096}")
  st=${st//"$token"/[token]} msg=${msg//"$token"/[token]}
  st=${st//"$priv"/[key]} msg=${msg//"$priv"/[key]}
  printf 'pia-wireguard: stage 15: addKey refused: http %s, status "%s", message "%s"\n' \
    "$http" "${st:0:32}" "${msg:0:200}" >&2
  fail 15
fi
rm -f -- "$work/token"
server_key=$(jq -er '.server_key' "$work/addkey.json") || fail 16
server_port=$(jq -er '.server_port' "$work/addkey.json") || fail 16
peer_ip=$(jq -er '.peer_ip' "$work/addkey.json") || fail 16
peer_ip=${peer_ip%/32}
[[ $server_key =~ $WGKEY && $server_port =~ ^[0-9]{1,5}$ && $server_port -ge 1 && $server_port -le 65535 \
   && $peer_ip =~ $IPV4 ]] || fail 16

# 4. wg0.conf, root-only, replaced atomically.
dir=$(dirname -- "$out")
[[ -d $dir ]] || install -d -m 0700 -- "$dir"
tmp=$(mktemp -- "$dir/.wg0.conf.XXXXXX")  # 0600 under umask 077
printf '[Interface]\nAddress = %s/32\nPrivateKey = %s\n\n[Peer]\nPublicKey = %s\nAllowedIPs = 0.0.0.0/0\nEndpoint = %s:%s\nPersistentKeepalive = 25\n' \
  "$peer_ip" "$priv" "$server_key" "$wg_ip" "$server_port" >"$tmp"
if [[ $(id -u) == 0 ]]; then chown -h 0:0 -- "$tmp"; fi
mv -f -- "$tmp" "$out"
tmp=""
exit 0
