#!/bin/sh
# Loads the plain-SQL dump into the `bitmagnet` database without converting
# a single byte: the database and the client are both SQL_ASCII, and
# PostgreSQL never converts to or from SQL_ASCII. Run inside the restore
# container as postgres; it logs to /restore/logs/restore.log.
set -eu
dump=/restore/dump/backup.sql
log=/restore/logs/restore.log
enc=$(psql -X -At -d bitmagnet -c "SELECT pg_encoding_to_char(encoding) FROM pg_database WHERE datname = 'bitmagnet'")
if [ "$enc" != "SQL_ASCII" ]; then
  echo "STOP: database bitmagnet is $enc, not SQL_ASCII. Drop it and create it again." >&2
  exit 1
fi
echo "restore started $(date -u +%FT%TZ)" > "$log"
status=0
PGCLIENTENCODING=SQL_ASCII psql -X -v ON_ERROR_STOP=1 -d bitmagnet -f "$dump" >> "$log" 2>&1 || status=$?
echo "restore finished $(date -u +%FT%TZ), psql exit $status" >> "$log"
exit "$status"
