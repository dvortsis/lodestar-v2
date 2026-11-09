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
# signal: 0 done; 10-19 one failed stage each (see `fail` calls).
#
# PIA's manual-connections flow (github.com/pia-foss/manual-connections), with
# the password and token passed to curl only from files, never argv, and the
# endpoint pinned to the address the server list gave.
set -Eeuo pipefail
umask 077

work=${1:?workdir}
out=${2:?output path}
tmp=""

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
  >"$work/servers" 2>/dev/null || fail 13
head -n 1 "$work/servers" >"$work/servers.json"
wg_ip=$(jq -er --arg r "$region" '[.regions[] | select(.id == $r)][0].servers.wg[0].ip' "$work/servers.json" 2>/dev/null) || fail 13
wg_cn=$(jq -er --arg r "$region" '[.regions[] | select(.id == $r)][0].servers.wg[0].cn' "$work/servers.json" 2>/dev/null) || fail 13
[[ $wg_ip =~ $IPV4 && $wg_cn =~ ^[a-z0-9.-]{1,253}$ ]] || fail 13

# 3. A fresh key pair; PIA registers the public half. The server must present
#    a certificate for its name signed by PIA's CA, at the listed address.
priv=$(wg genkey)
pub=$(printf '%s' "$priv" | wg pubkey)
[[ $priv =~ $WGKEY && $pub =~ $WGKEY ]] || fail 14
curl --fail --silent --max-time 30 --get \
  --connect-to "$wg_cn::$wg_ip:" --cacert "$work/pia-ca.crt" \
  --data-urlencode "pt@$work/token" --data-urlencode "pubkey=$pub" \
  "https://$wg_cn:1337/addKey" >"$work/addkey.json" 2>/dev/null || fail 15
rm -f -- "$work/token"
jq -e '.status == "OK"' "$work/addkey.json" >/dev/null 2>&1 || fail 15
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
