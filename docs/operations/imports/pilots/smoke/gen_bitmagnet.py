#!/usr/bin/env python3
"""Turns a synthetic Magnetico source (gen_magnetico.py) into a SQL script
that fills Bitmagnet 0.10.0's three importer tables, for the optional
Bitmagnet leg of G-import-smoke. Same hashes, names, files and sizes, so
the two adapters can be compared on the same data.

Load the schema first (the checker fixture's
crates/sources-importers/tests/fixtures/bitmagnet_check/schema.sql, which
is pg_dump --schema-only of a migrated 0.10.0 database, without its
`not_bitmagnet` table), into a database created with ENCODING 'SQL_ASCII'.

`torrent_pieces.pieces` holds zero bytes of the right length: the importer
reads only `octet_length(pieces)`, and piece_length is the smallest power of
two from 16 KiB that keeps the count at or under 2,000.

Usage: gen_bitmagnet.py SOURCE.sqlite3 > fill.sql
"""
import datetime
import sqlite3
import sys


def esc(s):
    return s.replace("\\", "\\\\").replace("\t", "\\t").replace("\n", "\\n").replace("\r", "\\r")


def ts(unix):
    return datetime.datetime.fromtimestamp(unix, datetime.timezone.utc).isoformat()


def main():
    c = sqlite3.connect("file:%s?mode=ro" % sys.argv[1], uri=True)
    w = sys.stdout.write
    w("BEGIN;\nCREATE TEMP TABLE pieces_tmp (info_hash bytea, piece_length bigint, n int);\n")
    files = {}
    for tid, size, path in c.execute("SELECT torrent_id, size, path FROM files ORDER BY torrent_id, id"):
        files.setdefault(tid, []).append((size, path))
    rows = list(c.execute("SELECT id, info_hash, name, total_size, discovered_on, "
                          "coalesce(updated_on, discovered_on) FROM torrents ORDER BY id"))
    w("COPY public.torrents (info_hash, name, size, private, created_at, updated_at, files_status,"
      " files_count) FROM stdin;\n")
    for tid, ih, name, size, created, updated in rows:
        fs = files.get(tid, [])
        status, count = ("single", "\\N") if len(fs) <= 1 else ("multi", str(len(fs)))
        w("\\\\x%s\t%s\t%d\tf\t%s\t%s\t%s\t%s\n" % (bytes(ih).hex(), esc(name), size, ts(created),
                                                   ts(updated), status, count))
    w("\\.\n")
    w("COPY public.torrent_files (info_hash, index, path, size, created_at, updated_at) FROM stdin;\n")
    for tid, ih, name, size, created, updated in rows:
        fs = files.get(tid, [])
        if len(fs) <= 1:
            continue
        for i, (fsize, path) in enumerate(fs):
            w("\\\\x%s\t%d\t%s\t%d\t%s\t%s\n" % (bytes(ih).hex(), i, esc(path), fsize, ts(created),
                                              ts(updated)))
    w("\\.\n")
    w("COPY pieces_tmp (info_hash, piece_length, n) FROM stdin;\n")
    for tid, ih, name, size, created, updated in rows:
        pl = 16384
        while -(-size // pl) > 2000:
            pl *= 2
        w("\\\\x%s\t%d\t%d\n" % (bytes(ih).hex(), pl, -(-size // pl)))
    w("\\.\n")
    w("INSERT INTO public.torrent_pieces (info_hash, piece_length, pieces, created_at)\n"
      "  SELECT p.info_hash, p.piece_length, decode(repeat('00', p.n * 20), 'hex'), t.created_at\n"
      "  FROM pieces_tmp p JOIN public.torrents t USING (info_hash);\n")
    w("COMMIT;\nVACUUM ANALYZE;\n")


if __name__ == "__main__":
    main()
