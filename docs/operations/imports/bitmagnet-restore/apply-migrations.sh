#!/bin/sh
# Creates bitmagnet 0.10.0's schema in the `bitmagnet` database from its
# own migrations (the "-- +goose Up" half of each file, in order), for a
# data-only dump. Run inside the restore container as postgres.
set -eu
dir=/restore/migrations
[ -f "$dir/00019_queue_fix_duplicate_key.sql" ] || {
  echo "STOP: $dir doesn't hold bitmagnet v0.10.0's migrations" >&2
  exit 1
}
for f in "$dir"/0*.sql; do
  awk '/^-- \+goose Up/ { up = 1; next } /^-- \+goose Down/ { up = 0 } up' "$f"
done > /tmp/bitmagnet-schema.sql
psql -X -q -v ON_ERROR_STOP=1 -d bitmagnet -f /tmp/bitmagnet-schema.sql
echo "schema created"
