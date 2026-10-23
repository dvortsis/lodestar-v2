#!/usr/bin/env bash
# Smoke check for PUID/PGID (crates/lodestar/src/privdrop.rs), run on the CI
# host: the image has no shell, so everything is read from the host side.
#
#   check-ids.sh <container> <drop|user> <uid> <gid> [<host path of /data>]
#
# Every thread of the container's PID 1 must run as uid:gid (real, effective,
# saved and filesystem ids) with no permitted, effective or ambient
# capabilities, so setuid(0) can't succeed. `drop` (the binary switched
# users itself) also needs no supplementary groups and no_new_privs set;
# `user` (docker --user u:g) allows g itself as the only group, which Docker
# adds and which grants nothing beyond the primary group. If a path is
# given, /data's owner must be uid:gid too.
set -euo pipefail

name=$1 mode=$2 uid=$3 gid=$4 data=${5:-}
case "$mode" in
  drop) want_groups="" ;;
  user) want_groups=$gid ;;
  *) echo "usage: $0 <container> <drop|user> <uid> <gid> [<data>]" >&2; exit 2 ;;
esac
pid=$(docker inspect --format '{{.State.Pid}}' "$name")
if [ -z "$pid" ] || [ "$pid" = 0 ]; then
  echo "::error::$name is not running"
  exit 1
fi

fail=0 checked=0 main_checked=0
for status in /proc/"$pid"/task/*/status; do
  # Read each thread's status once. A thread can exit between the glob and
  # the read; one that's gone runs nothing, so it's skipped, but the main
  # thread (task id = pid) must always be read.
  snap=$(sudo cat "$status" 2>/dev/null) || {
    echo "$name ${status%/status}: exited before it was read, skipped"
    continue
  }
  field() { awk -v k="$1:" '$1 == k { $1 = ""; sub(/^[ \t]+/, ""); print; exit }' <<<"$snap"; }
  got_uid=$(field Uid) got_gid=$(field Gid) groups=$(field Groups)
  checked=$((checked + 1))
  if [ "$status" = /proc/"$pid"/task/"$pid"/status ]; then main_checked=1; fi
  echo "$name ${status%/status}: Uid=[$got_uid] Gid=[$got_gid] Groups=[$groups]"
  [ "$got_uid" = "$uid $uid $uid $uid" ] || { echo "::error::$name runs as uid $got_uid, not $uid"; fail=1; }
  [ "$got_gid" = "$gid $gid $gid $gid" ] || { echo "::error::$name runs as gid $got_gid, not $gid"; fail=1; }
  [ "$groups" = "$want_groups" ] || { echo "::error::$name has groups [$groups], not [$want_groups]"; fail=1; }
  if [ "$mode" = drop ]; then
    nnp=$(field NoNewPrivs)
    [ "$nnp" = 1 ] || { echo "::error::$name has NoNewPrivs=$nnp"; fail=1; }
  fi
  for cap in CapPrm CapEff CapAmb; do
    v=$(field "$cap")
    case "$v" in
      *[!0]*) echo "::error::$name has $cap=$v"; fail=1 ;;
    esac
  done
done
if [ "$main_checked" != 1 ]; then
  echo "::error::$name: the main thread's status couldn't be read ($checked threads checked)"
  fail=1
fi

if [ -n "$data" ]; then
  owner=$(sudo stat --format '%u:%g' "$data")
  echo "$name /data ($data) owner: $owner"
  [ "$owner" = "$uid:$gid" ] || { echo "::error::$name /data is owned by $owner, not $uid:$gid"; fail=1; }
fi
exit "$fail"
