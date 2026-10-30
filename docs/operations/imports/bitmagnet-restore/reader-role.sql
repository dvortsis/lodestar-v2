-- The importer's role: it can log in to the current database over the
-- socket and read the three tables the importer reads, and nothing else.
-- Its password comes from a file, /run/secrets/reader_pw unless
-- READER_PW_FILE names another, never from a command line.
--
-- Privileges are what keep it from writing. Its read-only default is only
-- a default: any session can turn it off. So the role owns nothing, can
-- create nothing, and can't run the functions that write without a table
-- (the large-object ones), which PostgreSQL otherwise grants to everyone.
\set ON_ERROR_STOP on
-- The password is sent in the ALTER ROLE below; if that statement failed,
-- PostgreSQL would log it, password and all. Not in this session.
SET log_min_error_statement = panic;
SET log_statement = 'none';
\getenv pwfile READER_PW_FILE
\if :{?pwfile}
\else
  \set pwfile /run/secrets/reader_pw
\endif
\set pw `cat :'pwfile' 2>/dev/null`
SELECT length(:'pw') >= 32 AS pw_ok \gset
\if :pw_ok
\else
  \echo 'STOP: the password file is missing, unreadable or too short. Nothing was changed.'
  \quit
\endif
SELECT NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'bitmagnet_reader') AS role_new \gset
\if :role_new
  CREATE ROLE bitmagnet_reader LOGIN;
\endif
ALTER ROLE bitmagnet_reader NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS
  NOINHERIT LOGIN CONNECTION LIMIT 4 PASSWORD :'pw';
\unset pw
ALTER ROLE bitmagnet_reader SET default_transaction_read_only = on;
ALTER ROLE bitmagnet_reader SET statement_timeout = '30min';
-- Connect only: no temporary tables (TEMP), no schemas (CREATE).
REVOKE ALL ON DATABASE :"DBNAME" FROM PUBLIC;
GRANT CONNECT ON DATABASE :"DBNAME" TO bitmagnet_reader;
REVOKE ALL ON SCHEMA public FROM PUBLIC;
GRANT USAGE ON SCHEMA public TO bitmagnet_reader;
-- Large objects are rows in pg_largeobject that these functions create,
-- write and delete with no table privilege at all. Revoked in this database
-- only; nothing here uses them.
REVOKE EXECUTE ON FUNCTION
  pg_catalog.lo_create(oid), pg_catalog.lo_creat(integer),
  pg_catalog.lo_from_bytea(oid, bytea), pg_catalog.lo_open(oid, integer),
  pg_catalog.lowrite(integer, bytea), pg_catalog.lo_put(oid, bigint, bytea),
  pg_catalog.lo_truncate(integer, integer), pg_catalog.lo_truncate64(integer, bigint),
  pg_catalog.lo_unlink(oid)
  FROM PUBLIC;
-- Bitmagnet's and its extensions' own functions, none of which it needs.
REVOKE EXECUTE ON ALL FUNCTIONS IN SCHEMA public FROM PUBLIC;
GRANT SELECT ON public.torrents, public.torrent_files, public.torrent_pieces TO bitmagnet_reader;
\echo 'bitmagnet_reader is ready'
