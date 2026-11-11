#!/usr/bin/env bash
# start-block (OP-3b; docs/plan/2026-11-08-cloud-test-plan.md Lane L and the
# cloud screen): one ABBA block at a rules commit, from a fresh data directory.
# It returns once segment 1's collector is running; the collectors and the
# outage watcher run detached.
#
# In order:
# 1. Refuse unless no block is recorded or running (fail closed).
# 2. The leak test, in this same run: it must exit 0 and end `ok leak_test`.
#    Anything else is a stop condition, and nothing starts.
# 3. The durable block marker, before Lodestar or a collector starts.
# 4. Stage the tools from the rules commit, start Lodestar on the screen's image
#    (by digest), accept the Legal Notice version the rules name and set a
#    random admin password (it stays in a root-only file here), then start
#    segment 1's collector and the outage watcher.
# Output is fixed names only. Run as root by the step bootstrap:
# step.sh <workdir> [stack] [pia-run] [block] (tests pass the last three).
set -uo pipefail
work=${1:?workdir}
STACK=${2:-/home/lodestar/lodestar}
PIA_RUN=${3:-/run/lodestar-pia}
BLOCK=${4:-/var/lib/lodestar-block}
# shellcheck source=block-lib.sh
. "$work/block-lib.sh"

started=""
# Once Lodestar has started, a failure stops it, so copy-off can end the block.
fail() {
  [[ -n $started ]] && { stop_lodestar || echo "fail lodestar_stopped"; stop_proxy; }
  echo "fail $1"; echo "fail start_block"; exit 1
}

# The arguments, already checked by the control side; checked again here.
while IFS='=' read -r k v; do
  case $k in
    IMAGE_DIGEST) [[ $v =~ ^[0-9a-f]{64}$ ]] ;;
    LODESTAR_COMMIT | SPEC_COMMIT | RULES_COMMIT) [[ $v =~ ^[0-9a-f]{40}$ ]] ;;
    TREATMENT) [[ $v =~ ^(adm|utp|route)$ ]] ;;
    SEGMENT_HOURS) [[ $v =~ ^[23]$ ]] ;;
    ROLE) [[ $v =~ ^(main|worker)$ ]] ;;
    NOTICE_VERSION) [[ $v =~ ^[1-9][0-9]{0,5}$ ]] ;;
    BLOCK_ID) [[ $v =~ ^[0-9]{1,20}-[0-9]{1,6}$ ]] ;;
    *) false ;;
  esac || fail args_valid
  printf -v "$k" '%s' "$v"
done <"$work/args"
for k in IMAGE_DIGEST LODESTAR_COMMIT SPEC_COMMIT RULES_COMMIT TREATMENT SEGMENT_HOURS ROLE NOTICE_VERSION BLOCK_ID; do
  [[ -n ${!k:-} ]] || fail args_valid
done
echo "ok args_valid"
for tool in docker systemd-run systemctl; do
  command -v "$tool" >/dev/null 2>&1 || fail "tool_missing_${tool//-/_}"
done

# 1. No block, recorded or running.
case $(block_state) in
  none) ;;
  block) fail block_running ;;
  *) fail block_check ;;
esac
block_procs; rc=$?
((rc == 2)) && fail block_check
((rc == 0)) && fail block_running
# A past block's evidence is never set aside before copy-off has taken it.
if [[ -n $(ls -A -- "$BLOCK/evidence" 2>/dev/null) && ! -e $BLOCK/copied ]]; then fail evidence_copied; fi
echo "ok no_block"

# 2. The leak test, in this run. Its lines are reported as they are.
out=$(timeout 600 bash "$work/leak-test.sh" "$work" "$STACK" "$PIA_RUN" "$BLOCK"); rc=$?
printf '%s\n' "$out"
[[ $rc -eq 0 && $(printf '%s\n' "$out" | tail -n 1) == "ok leak_test" ]] || fail leak_test_passed
echo "ok leak_test_passed"

# 3. The marker, first. From here on every mid-block check sees a block.
# A past block's directory (its evidence) is kept beside the new one, never wiped.
if [[ -d $BLOCK ]]; then mv -- "$BLOCK" "$BLOCK.$(date -u +%Y%m%dT%H%M%SZ)" || fail block_dir; fi
mkdir -m 0700 -- "$BLOCK" || fail block_dir
printf 'block=%s\nsegment=1\nstate=starting\n' "$BLOCK_ID" | marker_write || fail marker_written
echo "ok marker_written"
{
  for k in IMAGE_DIGEST LODESTAR_COMMIT SPEC_COMMIT RULES_COMMIT TREATMENT SEGMENT_HOURS ROLE; do
    printf '%s=%s\n' "$k" "${!k}"
  done
} >"$BLOCK/block.env" && load_block_env || fail block_env

# 4. Stage, then start. analyze_cr.py imports collect.py from ./linux.
mkdir -m 0700 -p -- "$BLOCK/tools/linux" "$BLOCK/evidence" "$BLOCK/witness" \
  && install -m 0600 -- "$work/collect.py" "$BLOCK/tools/linux/collect.py" \
  && install -m 0600 -- "$work/analyze_cr.py" "$BLOCK/tools/analyze_cr.py" \
  && install -m 0600 -- "$work/scrub.py" "$BLOCK/tools/scrub.py" \
  && install -m 0644 -- "$work/compose.yaml" "$STACK/compose.yaml" || fail tools_staged
for n in 1 2 3 4; do timeout 60 docker rm "lodestar-collector-$n" >/dev/null 2>&1; done  # a past block's, stopped
rm -rf -- "$STACK/data" "$STACK/witness" && mkdir -m 0700 -- "$STACK/data" "$STACK/witness" || fail fresh_data
timeout 300 docker pull "$LODESTAR_REPO@sha256:$IMAGE_DIGEST" >/dev/null 2>&1 || fail lodestar_image
timeout 120 docker pull "$COLLECTOR_IMAGE" >/dev/null 2>&1 || fail collector_image
timeout 120 docker pull "$PROXY_IMAGE" >/dev/null 2>&1 || fail proxy_image
started=1
start_lodestar 1 fresh || fail lodestar_started
network_parent_ok || fail network_parent
start_proxy || fail docker_proxy
wait_api || fail api_up
echo "ok lodestar_started"

# First-run setup: the token from Lodestar's own log, the notice version the
# rules name (a changed notice is never accepted silently), and a random
# password. Nothing here is printed; the password stays in $BLOCK.
token=""
until=$((SECONDS + ${TOKEN_WAIT:-90}))
while :; do
  token=$(timeout 30 docker logs lodestar 2>&1 | grep -oE 'enter setup token [0-9A-F]{4}(-[0-9A-F]{4}){3}' \
    | tail -n 1 | cut -d' ' -f4)
  [[ -n $token ]] && break
  ((SECONDS >= until)) && break
  sleep 3
done
[[ $token =~ ^[0-9A-F]{4}(-[0-9A-F]{4}){3}$ ]] || fail setup_token
served=$(api "$API/api/v1/legal/notice" 2>/dev/null | grep -oE '"version":[0-9]{1,6}' | head -n 1 | cut -d: -f2)
served=${served//[!0-9]/}
if [[ $served != "$NOTICE_VERSION" ]]; then  # a stop: never retried, never accepted anyway
  echo "count notice_version_served ${served:-0}"
  echo "count notice_version_pinned $NOTICE_VERSION"
  fail notice_version
fi
pw=$(od -An -tx1 -N24 /dev/urandom | tr -dc '0-9a-f')
[[ ${#pw} -eq 48 ]] || fail password_made
(umask 077 && printf '%s\n' "$pw" >"$BLOCK/password") || fail password_made
code=$(printf '{"token":"%s","password":"%s","notice_version":%s}' "$token" "$pw" "$NOTICE_VERSION" \
  | api -o /dev/null -w '%{http_code}' -H 'Content-Type: application/json' --data-binary @- "$API/api/v1/setup" \
    2>/dev/null)
unset pw token
[[ $code == 200 ]] || fail setup_done
echo "ok setup_done"

start_collector 1 || fail collector_started
collector_ok 1 || fail collector_started
printf 'block=%s\nsegment=1\nstate=running\n' "$BLOCK_ID" | marker_write || fail marker_written
echo "ok collector_started"
if start_watcher; then echo "ok outage_watcher"; else echo "fail outage_watcher"; fi
echo "count block_segment 1"
echo "ok start_block"
