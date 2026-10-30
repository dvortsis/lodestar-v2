-- The tables bitmagnet_check reads, exactly as bitmagnet 0.10.0's
-- migrations leave them (pg_dump --schema-only of a migrated database),
-- plus one table bitmagnet doesn't have, which must be listed as an extra.
-- Load into an empty database created with ENCODING 'SQL_ASCII'.
CREATE TYPE public."FilesStatus" AS ENUM ('no_info', 'single', 'multi', 'over_threshold');

CREATE TABLE public.torrents (
    info_hash bytea NOT NULL,
    name text NOT NULL,
    size bigint NOT NULL,
    private boolean NOT NULL,
    created_at timestamp with time zone NOT NULL,
    updated_at timestamp with time zone NOT NULL,
    files_status public."FilesStatus" DEFAULT 'no_info'::public."FilesStatus" NOT NULL,
    extension text GENERATED ALWAYS AS (
CASE
    WHEN (files_status = 'single'::public."FilesStatus") THEN "substring"(lower(name), '[^/.]\.([a-z0-9]+)$'::text)
    ELSE NULL::text
END) STORED,
    files_count integer
);

CREATE TABLE public.torrent_files (
    info_hash bytea NOT NULL,
    index integer NOT NULL,
    path text NOT NULL,
    extension text GENERATED ALWAYS AS ("substring"(lower(path), '[^/.]\.([a-z0-9]+)$'::text)) STORED,
    size bigint NOT NULL,
    created_at timestamp with time zone NOT NULL,
    updated_at timestamp with time zone NOT NULL
);

CREATE TABLE public.torrent_pieces (
    info_hash bytea NOT NULL,
    piece_length bigint NOT NULL,
    pieces bytea NOT NULL,
    created_at timestamp with time zone NOT NULL
);

ALTER TABLE ONLY public.torrent_files
    ADD CONSTRAINT torrent_files_info_hash_index_key UNIQUE (info_hash, index);
ALTER TABLE ONLY public.torrent_files
    ADD CONSTRAINT torrent_files_pkey PRIMARY KEY (info_hash, path);
ALTER TABLE ONLY public.torrent_pieces
    ADD CONSTRAINT torrent_pieces_pkey PRIMARY KEY (info_hash);
ALTER TABLE ONLY public.torrents
    ADD CONSTRAINT torrents_pkey PRIMARY KEY (info_hash);
ALTER TABLE ONLY public.torrent_files
    ADD CONSTRAINT torrent_files_info_hash_fkey FOREIGN KEY (info_hash) REFERENCES public.torrents(info_hash) ON DELETE CASCADE;
ALTER TABLE ONLY public.torrent_pieces
    ADD CONSTRAINT torrent_pieces_info_hash_fkey FOREIGN KEY (info_hash) REFERENCES public.torrents(info_hash) ON DELETE CASCADE;

CREATE TABLE public.not_bitmagnet (id integer);
