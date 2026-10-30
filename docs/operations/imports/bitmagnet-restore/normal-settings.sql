-- Back to PostgreSQL's safe defaults after the restore, then write
-- everything to disk. Restart the container afterwards.
ALTER SYSTEM RESET ALL;
SELECT pg_reload_conf();
CHECKPOINT;
