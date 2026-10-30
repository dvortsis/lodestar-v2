-- The importer's role: it can log in to `bitmagnet` over the socket and
-- read the three tables the importer reads, and nothing else. Its password
-- comes from /run/secrets/reader_pw, never from a command line.
\set ON_ERROR_STOP on
\set pw `cat /run/secrets/reader_pw`
SELECT length(:'pw') >= 32 AS pw_ok \gset
\if :pw_ok
\else
  \echo 'STOP: /run/secrets/reader_pw is missing, unreadable or too short. Nothing was changed.'
  \quit
\endif
SELECT NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'bitmagnet_reader') AS role_new \gset
\if :role_new
  CREATE ROLE bitmagnet_reader LOGIN;
\endif
ALTER ROLE bitmagnet_reader NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS
  LOGIN CONNECTION LIMIT 4 PASSWORD :'pw';
\unset pw
ALTER ROLE bitmagnet_reader SET default_transaction_read_only = on;
ALTER ROLE bitmagnet_reader SET statement_timeout = '30min';
REVOKE ALL ON DATABASE bitmagnet FROM PUBLIC;
GRANT CONNECT ON DATABASE bitmagnet TO bitmagnet_reader;
REVOKE ALL ON SCHEMA public FROM PUBLIC;
GRANT USAGE ON SCHEMA public TO bitmagnet_reader;
GRANT SELECT ON public.torrents, public.torrent_files, public.torrent_pieces TO bitmagnet_reader;
\echo 'bitmagnet_reader is ready'
