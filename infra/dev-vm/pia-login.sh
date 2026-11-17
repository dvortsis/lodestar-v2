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
#   sudo infra/dev-vm/pia-login.sh --aws-secret NAME [region-id]
#                                         reads the login from AWS Secrets Manager
#
# --keep (owner, 2026-11-12T16:55Z, docs/decisions/2026-11-12-X-test-vm.md):
# after a success, the login and region are also kept in /root/.lodestar-pia
# (root:root, 0700/0600) on the VM's encrypted disk, so --rekey can register a
# fresh key later without the owner. The kept login goes with the VM at deletion.
#
# --aws-secret (owner, 2026-11-15, the same decisions file): on an AWS test
# instance, the login is read with the instance role from the Secrets Manager
# secret the owner filled in (infra/dev-vm/aws-claude-testvm.json), straight
# into memory. Nothing is kept on disk; run it again for a fresh key.
#
# Writes <run folder>/secrets/wg0.conf (root:root 0600), where the fleet's
# compose.yaml reads it from when run from that folder. The run folder is
# /home/lodestar/lodestar on a Linode (lodestar is an admin there anyway) and
# /opt/lodestar-run with --aws-secret, where lodestar must not be able to touch
# what root runs: there, the folder and every parent must be root-owned and
# writable by root only, or the script refuses. --run-dir DIR overrides.
#   sudo infra/dev-vm/pia-login.sh [--run-dir DIR] ...
set -Eeuo pipefail
umask 077
[[ $EUID -eq 0 ]] || { echo "run with sudo" >&2; exit 2; }
repo=$(cd "$(dirname "$0")/../.." && pwd)
step="$repo/infra/pia-bridge/vm/pia-wireguard.sh"
ca="$repo/infra/pia-bridge/vm/pia-ca.rsa.4096.crt"
run_dir=""
store=/root/.lodestar-pia

mode=ask
region=""
secret_id=""
while [[ $# -gt 0 ]]; do
  case $1 in
    --keep) mode=keep ;;
    --rekey) mode=rekey ;;
    --aws-secret)
      mode=aws
      [[ $# -ge 2 && $2 =~ ^[A-Za-z0-9/_+=.@-]{1,512}$ ]] || { echo "--aws-secret needs a secret name" >&2; exit 2; }
      secret_id=$2; shift ;;
    --run-dir)
      [[ $# -ge 2 && $2 == /* ]] || { echo "--run-dir needs an absolute path" >&2; exit 2; }
      run_dir=$2; shift ;;
    -*) echo "unknown option: $1" >&2; exit 2 ;;
    *) region=$1 ;;
  esac
  shift
done
if [[ $mode == rekey && -z $region && -f $store/region ]]; then region=$(<"$store/region"); fi

# A region with WireGuard servers, in the US (PIA's US servers have no port
# forwarding, as the test plan's ground rule 1 requires), whose first server's
# name passes the step's own check (some PIA names don't, e.g. us-brooklyn's).
# Read the first line from a file: with pipefail, `curl | head -1` can fail
# with curl's exit 23 when head closes the pipe early.
list=$(mktemp)
curl --fail --silent --show-error --max-time 30 --max-filesize 20000000 -o "$list" \
  https://serverlist.piaservers.net/vpninfo/servers/v6 || { rm -f -- "$list"; echo "can't fetch PIA's server list" >&2; exit 3; }
servers=$(head -n 1 -- "$list"); rm -f -- "$list"
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
  pia_user=$(jq -j '.username // empty' "$store/login.json")
  pia_pass=$(jq -j '.password // empty' "$store/login.json")
elif [[ $mode == aws ]]; then
  command -v aws >/dev/null || { echo "the AWS CLI is missing (snap install aws-cli --classic)" >&2; exit 7; }
  # The value goes from the CLI's stdout into memory; errors are summarised, never echoed.
  pia_secret=$(aws secretsmanager get-secret-value --region us-east-1 --secret-id "$secret_id" \
    --query SecretString --output text 2>/dev/null) || { echo "the secret $secret_id could not be read" >&2; exit 7; }
  pia_user=$(printf '%s' "$pia_secret" | jq -j '.username // empty' 2>/dev/null) || pia_user=""
  pia_pass=$(printf '%s' "$pia_secret" | jq -j '.password // empty' 2>/dev/null) || pia_pass=""
  unset pia_secret
else
  read -r -s -p "PIA username (hidden): " pia_user; echo
  read -r -s -p "PIA password: " pia_pass; echo
fi
[[ -n $pia_user && -n $pia_pass ]] || { echo "empty username or password" >&2; exit 5; }

work=""
trap '[[ -n $work ]] && rm -rf -- "$work"; unset pia_pass' EXIT
if [[ -z $run_dir ]]; then
  if [[ $mode == aws ]]; then run_dir=/opt/lodestar-run; else run_dir=/home/lodestar/lodestar; fi
fi
if [[ $run_dir == /home/lodestar/lodestar && $mode != aws ]]; then
  install -d -m 0700 -o lodestar -g lodestar "$run_dir"
else
  # Root runs the stack from here: no other user may own or write any part of
  # the path, or it could swap the scripts or the secrets folder.
  install -d -m 0755 -o root -g root "$run_dir"
  p=$run_dir
  while :; do
    [[ ! -L $p && $(stat -c '%u' -- "$p") == 0 && $(( 0$(stat -c '%a' -- "$p") & 022 )) -eq 0 ]] \
      || { echo "refusing: $p must be a root-owned folder writable by root only" >&2; exit 2; }
    [[ $p == / ]] && break
    p=$(dirname -- "$p")
  done
fi
dest_dir=$run_dir/secrets
dest="$dest_dir/wg0.conf"
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
    aws) echo "OK: WireGuard configuration written (root only), from the AWS secret. Nothing is kept." ;;
    *) echo "OK: WireGuard configuration written (root only). The login is deleted." ;;
  esac
else
  echo "FAILED ($rc): $(stage "$rc")." >&2
fi
unset pia_pass
exit "$rc"
