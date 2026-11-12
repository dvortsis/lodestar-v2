#!/usr/bin/env bash
# The owner runs this once on the test VM (X-test-vm): it asks for the PIA login
# (nothing is echoed), picks a US WireGuard region, and has the fleet's reviewed
# step (infra/pia-bridge/vm/pia-wireguard.sh) turn it into a root-only
# WireGuard configuration for gluetun. The login lives only in a tmpfs
# directory that the step deletes whatever happens; it never reaches a file
# outside it, argv, the shell history, git or a log.
#
#   sudo infra/dev-vm/pia-login.sh [region-id]
#
# Writes /home/lodestar/lodestar/secrets/wg0.conf (root:root 0600), where the
# fleet's compose.yaml reads it from when run from /home/lodestar/lodestar.
set -Eeuo pipefail
umask 077
[[ $EUID -eq 0 ]] || { echo "run with sudo" >&2; exit 2; }
repo=$(cd "$(dirname "$0")/../.." && pwd)
step="$repo/infra/pia-bridge/vm/pia-wireguard.sh"
ca="$repo/infra/pia-bridge/vm/pia-ca.rsa.4096.crt"
dest_dir=/home/lodestar/lodestar/secrets
dest="$dest_dir/wg0.conf"

# A region with WireGuard servers, in the US (PIA's US servers have no port
# forwarding, as the test plan's ground rule 1 requires).
servers=$(curl --fail --silent --show-error --max-time 30 https://serverlist.piaservers.net/vpninfo/servers/v6 | head -1)
mapfile -t regions < <(jq -r '.regions[] | select(.country == "US" and (.servers.wg | length) > 0 and (.offline | not) and (.port_forward | not)) | .id' <<<"$servers" | sort)
[[ ${#regions[@]} -gt 0 ]] || { echo "no US WireGuard region found" >&2; exit 3; }
region=${1:-}
if [[ -z $region ]]; then
  for r in "${regions[@]}"; do [[ $r =~ new_?jersey|newjersey|new_york|newyork ]] && { region=$r; break; }; done
  region=${region:-${regions[0]}}
fi
printf '%s\n' "${regions[@]}" | grep -qx -- "$region" || { echo "unknown or unsuitable region: $region" >&2; exit 4; }
echo "PIA region: $region (pass another id as the first argument to change it)"

read -r -p "PIA username: " pia_user
read -r -s -p "PIA password: " pia_pass; echo
[[ -n $pia_user && -n $pia_pass ]] || { echo "empty username or password" >&2; exit 5; }

work=$(mktemp -d /dev/shm/pia.XXXXXXXX)
trap 'rm -rf -- "$work"; unset pia_pass' EXIT
# jq builds the JSON from the environment, so the password never sits in argv.
PIA_USER=$pia_user PIA_PASS=$pia_pass jq -n '{username: env.PIA_USER, password: env.PIA_PASS}' >"$work/login.json"
unset pia_pass
printf '%s' "$region" >"$work/region"
cp -- "$ca" "$work/pia-ca.crt"

install -d -m 0700 -o lodestar -g lodestar /home/lodestar/lodestar
install -d -m 0700 -o root -g root "$dest_dir"
set +e
bash "$step" "$work" "$dest" >/dev/null 2>&1
rc=$?
set -e
case $rc in
  0) chmod 600 "$dest"; echo "OK: WireGuard configuration written (root only). The login is deleted." ;;
  11) echo "FAILED ($rc): the login file could not be read" >&2 ;;
  12) echo "FAILED ($rc): PIA refused the login; check the username and password" >&2 ;;
  *) echo "FAILED ($rc): see the fail calls in $step for the stage" >&2 ;;
esac
exit "$rc"
