-- Fast, crash-unsafe settings for the one-off restore. If the host or the
-- container crashes while these are on, delete the restored cluster and
-- start the restore again. normal-settings.sql undoes them.
ALTER SYSTEM SET fsync = off;
ALTER SYSTEM SET synchronous_commit = off;
ALTER SYSTEM SET full_page_writes = off;
ALTER SYSTEM SET autovacuum = off;
ALTER SYSTEM SET max_wal_size = '8GB';
ALTER SYSTEM SET checkpoint_timeout = '30min';
SELECT pg_reload_conf();
