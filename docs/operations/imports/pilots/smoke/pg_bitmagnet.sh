#!/bin/sh
# The Bitmagnet leg's PostgreSQL, for G-import-smoke (README "Bitmagnet
# leg"): a throwaway PostgreSQL 16 cluster on a Unix socket only, an
# SQL_ASCII `bitmagnet` database with Bitmagnet 0.10.0's importer tables,
# filled from a synthetic Magnetico source, the runbook's read-only role
# with scram-sha-256, then the IMP-0 checker.
#
# Usage, as root, from the repository root:
#   pg_bitmagnet.sh up SOURCE.sqlite3 DIR   # DIR must not exist
#   pg_bitmagnet.sh down DIR
# Needs /usr/lib/postgresql/16/bin, a `postgres` account, and
# target/release/bitmagnet_check
#   (cargo build --release -p lodestar-sources-importers --bin bitmagnet_check).
set -eu
PG=/usr/lib/postgresql/16/bin
HERE=$(dirname "$0")
case "${1:-}" in
up)
  src=$2 dir=$3
  [ ! -e "$dir" ] || { echo "STOP: $dir exists" >&2; exit 1; }
  mkdir -p "$dir" && chown postgres:postgres "$dir"
  su postgres -c "$PG/initdb -D $dir/data -E SQL_ASCII --locale=C -A trust -U postgres" >"$dir/initdb.log"
  su postgres -c "$PG/pg_ctl -D $dir/data -o \"-k $dir -c listen_addresses=''\" -l $dir/pg.log -w start"
  grep -v not_bitmagnet crates/sources-importers/tests/fixtures/bitmagnet_check/schema.sql >"$dir/schema.sql"
  python3 "$HERE/gen_bitmagnet.py" "$src" >"$dir/fill.sql"
  head -c 100 /dev/urandom | sha256sum | cut -c1-48 >"$dir/reader_pw"
  chmod 600 "$dir/reader_pw"
  chown postgres "$dir/schema.sql" "$dir/fill.sql" "$dir/reader_pw"
  su postgres -c "createdb -h $dir -E SQL_ASCII -T template0 bitmagnet"
  su postgres -c "psql -h $dir -X -q -v ON_ERROR_STOP=1 -d bitmagnet -f $dir/schema.sql"
  su postgres -c "psql -h $dir -X -q -v ON_ERROR_STOP=1 -d bitmagnet -f $dir/fill.sql"
  su postgres -c "READER_PW_FILE=$dir/reader_pw psql -h $dir -X -q -v DBNAME=bitmagnet -d bitmagnet \
    -f docs/operations/imports/bitmagnet-restore/reader-role.sql"
  printf 'local all postgres trust\nlocal bitmagnet bitmagnet_reader scram-sha-256\n' >"$dir/data/pg_hba.conf"
  chown postgres "$dir/data/pg_hba.conf"
  su postgres -c "psql -h $dir -XAtq -d bitmagnet -c 'SELECT pg_reload_conf()'" >/dev/null
  su postgres -c "psql -h $dir -XAt -d bitmagnet -c 'SELECT pg_database_size(current_database())'" \
    >"$dir/database_bytes"
  # The reader's password file must be readable by the importer (root here).
  target/release/bitmagnet_check --host "$dir" --password-file "$dir/reader_pw" \
    --data-dir "$dir/data" --reserve 1GiB >"$dir/check.json"
  echo "database bytes: $(cat "$dir/database_bytes"); checker: $dir/check.json"
  ;;
down)
  su postgres -c "$PG/pg_ctl -D $2/data -w stop"
  rm -rf "$2"
  ;;
*)
  echo "usage: $0 up SOURCE.sqlite3 DIR | down DIR" >&2
  exit 2
  ;;
esac
