#!/usr/bin/env bash
# Smoke check for PUID/PGID (crates/lodestar/src/privdrop.rs), run on the CI
# host: the image has no shell, so everything is read from the host side.
#
#   check-ids.sh <container> <uid> <gid> [<host path of /data>]
#
# Every thread of the container's PID 1 must run as uid:gid (real, effective,
# saved and filesystem ids), with no supplementary groups and no permitted,
# effective or ambient capabilities, so setuid(0) can't succeed. If a path is
# given, /data's owner must be uid:gid too.
set -euo pipefail

name=$1 uid=$2 gid=$3 data=${4:-}
pid=$(docker inspect --format '{{.State.Pid}}' "$name")
if [ -z "$pid" ] || [ "$pid" = 0 ]; then
  echo "::error::$name is not running"
  exit 1
fi

fail=0
for status in /proc/"$pid"/task/*/status; do
  field() { sudo awk -v k="$1:" '$1 == k { $1 = ""; sub(/^[ \t]+/, ""); print; exit }' "$status"; }
  got_uid=$(field Uid) got_gid=$(field Gid) groups=$(field Groups)
  echo "$name ${status%/status}: Uid=[$got_uid] Gid=[$got_gid] Groups=[$groups]"
  [ "$got_uid" = "$uid $uid $uid $uid" ] || { echo "::error::$name runs as uid $got_uid, not $uid"; fail=1; }
  [ "$got_gid" = "$gid $gid $gid $gid" ] || { echo "::error::$name runs as gid $got_gid, not $gid"; fail=1; }
  [ -z "$groups" ] || { echo "::error::$name keeps supplementary groups: $groups"; fail=1; }
  for cap in CapPrm CapEff CapAmb; do
    v=$(field "$cap")
    case "$v" in
      *[!0]*) echo "::error::$name has $cap=$v"; fail=1 ;;
    esac
  done
done

if [ -n "$data" ]; then
  owner=$(sudo stat --format '%u:%g' "$data")
  echo "$name /data ($data) owner: $owner"
  [ "$owner" = "$uid:$gid" ] || { echo "::error::$name /data is owned by $owner, not $uid:$gid"; fail=1; }
fi
exit "$fail"
