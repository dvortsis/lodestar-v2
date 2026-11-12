#!/usr/bin/env bash
# The owner runs this on the test VM (X-test-vm): it asks for the PIA login
# (nothing is echoed, not even the username), picks a US WireGuard region, and has the fleet's reviewed
# step (infra/pia-bridge/vm/pia-wireguard.sh) turn it into a root-only
# WireGuard configuration for gluetun. While it runs, the login lives only in
# memory and in a tmpfs directory that the step deletes whatever happens; it
# never reaches argv, the shell history, git or a log.
#
#   sudo infra/dev-vm/pia-login.sh [--keep] [region-id]   asks for the login
#   sudo infra/dev-vm/pia-login.sh --rekey [region-id]    uses the kept login
#
# --keep (owner, 2026-11-12T16:55Z, docs/decisions/2026-11-12-X-test-vm.md):
# after a success, the login and region are also kept in /root/.lodestar-pia
# (root:root, 0700/0600) on the VM's encrypted disk, so --rekey can register a
# fresh key later without the owner. The kept login goes with the VM at deletion.
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
store=/root/.lodestar-pia

mode=ask
region=""
for arg in "$@"; do
  case $arg in
    --keep) mode=keep ;;
    --rekey) mode=rekey ;;
    -*) echo "unknown option: $arg" >&2; exit 2 ;;
    *) region=$arg ;;
  esac
done
if [[ $mode == rekey && -z $region && -f $store/region ]]; then region=$(<"$store/region"); fi

# A region with WireGuard servers, in the US (PIA's US servers have no port
# forwarding, as the test plan's ground rule 1 requires), whose first server's
# name passes the step's own check (some PIA names don't, e.g. us-brooklyn's).
servers=$(curl --fail --silent --show-error --max-time 30 https://serverlist.piaservers.net/vpninfo/servers/v6 | head -1)
mapfile -t regions < <(jq -r '.regions[] | select(.country == "US" and (.servers.wg | length) > 0 and (.offline | not) and (.port_forward | not)) | select(.servers.wg[0].cn | test("^[a-z0-9.-]{1,253}$")) | .id' <<<"$servers" | sort)
[[ ${#regions[@]} -gt 0 ]] || { echo "no US WireGuard region found" >&2; exit 3; }
if [[ -z $region ]]; then
  for r in "${regions[@]}"; do [[ $r =~ new_?york|new_?jersey ]] && { region=$r; break; }; done
  region=${region:-${regions[0]}}
fi
printf '%s\n' "${regions[@]}" | grep -qx -- "$region" || { echo "unknown or unsuitable region: $region" >&2; exit 4; }
echo "PIA region: $region (pass another id to change it)"

if [[ $mode == rekey ]]; then
  [[ -f $store/login.json ]] || { echo "no kept login: run once with --keep" >&2; exit 6; }
  pia_user=$(jq -j '.username' "$store/login.json")
  pia_pass=$(jq -j '.password' "$store/login.json")
else
  read -r -s -p "PIA username (hidden): " pia_user; echo
  read -r -s -p "PIA password: " pia_pass; echo
fi
[[ -n $pia_user && -n $pia_pass ]] || { echo "empty username or password" >&2; exit 5; }

work=""
trap '[[ -n $work ]] && rm -rf -- "$work"; unset pia_pass' EXIT
install -d -m 0700 -o lodestar -g lodestar /home/lodestar/lodestar
install -d -m 0700 -o root -g root "$dest_dir"

stage() {
  case $1 in
    10) echo "the region id was rejected" ;;
    11) echo "the login file could not be read" ;;
    12) echo "PIA refused the login (check the username and password)" ;;
    13) echo "PIA's server list could not be fetched, or had no usable server for $region" ;;
    14) echo "no WireGuard key pair could be made" ;;
    15) echo "PIA's VPN server did not accept the new key" ;;
    16) echo "PIA's VPN server sent back an unusable answer" ;;
    *) echo "unexpected exit $1" ;;
  esac
}

# PIA's servers fail now and then (stages 13, 15 and 16), so those retry with the
# same login, typed once. A refused login or a local error doesn't retry.
rc=1
for attempt in 1 2 3 4; do
  # The step deletes its work directory whatever happens, so each attempt gets a new one.
  work=$(mktemp -d /dev/shm/pia.XXXXXXXX)
  # jq builds the JSON from the environment, so the password never sits in argv.
  PIA_USER=$pia_user PIA_PASS=$pia_pass jq -n '{username: env.PIA_USER, password: env.PIA_PASS}' >"$work/login.json"
  printf '%s' "$region" >"$work/region"
  cp -- "$ca" "$work/pia-ca.crt"
  set +e
  bash "$step" "$work" "$dest" >/dev/null 2>&1
  rc=$?
  set -e
  rm -rf -- "$work"; work=""
  [[ $rc -eq 0 ]] && break
  echo "attempt $attempt: $(stage "$rc") (exit $rc)" >&2
  case $rc in 13|15|16) ;; *) break ;; esac
  [[ $attempt -lt 4 ]] && sleep $((attempt * 15))
done
if [[ $rc -eq 0 ]]; then
  chmod 600 "$dest"
  if [[ $mode == keep ]]; then
    install -d -m 0700 -o root -g root "$store"
    PIA_USER=$pia_user PIA_PASS=$pia_pass jq -n '{username: env.PIA_USER, password: env.PIA_PASS}' >"$store/login.json"
    printf '%s' "$region" >"$store/region"
    chmod 600 "$store/login.json" "$store/region"
  fi
  case $mode in
    keep) echo "OK: WireGuard configuration written (root only). The login is kept root-only in $store for --rekey." ;;
    rekey) echo "OK: WireGuard configuration written (root only), from the kept login." ;;
    *) echo "OK: WireGuard configuration written (root only). The login is deleted." ;;
  esac
else
  echo "FAILED ($rc): $(stage "$rc")." >&2
fi
unset pia_pass
exit "$rc"
