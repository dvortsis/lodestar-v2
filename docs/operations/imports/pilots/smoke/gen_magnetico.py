#!/usr/bin/env python3
"""Synthetic Magnetico SQLite source for the G-import-smoke stages.

Writes a magneticod v3-schema database (the layout the IMP-2 adapter's
`Layout::V3` fixture builds) with N torrents. Everything is generated from
a seed: no real torrent names or hashes are used. Infohashes are
SHA-1(seed || index), names come from a fixed synthetic vocabulary.

Distributions (chosen to resemble a DHT crawl, not taken from one):
- files per torrent: 45 % single file, the rest log-normal (median ~8,
  long tail), capped at 2,000;
- file size: log-normal, median ~40 MiB for single files, ~4 MiB inside
  multi-file torrents, clamped to [1 B, 64 GiB];
- names: release-style (`Title.Words.2019.1080p.WEB-DL.x264-GRP`), plain
  phrases, and a few non-ASCII ones (Cyrillic, CJK, accents);
- modified_on is written explicitly, as magneticod does: left to its
  default, the default trigger's UPDATE runs before the FTS insert
  trigger and SQLite reports the FTS index malformed;
- discovered_on: uniform over 2019-01-01 .. 2026-10-01; 30 % carry
  swarm counts (`updated_on`, `n_seeders`, `n_leechers`).

Usage: gen_magnetico.py OUT.sqlite3 COUNT [SEED] [FIRST]

FIRST (default 0) is the index of the first infohash, so a second source
can overlap the first one's hashes (the removal check). With FIRST > 0 the
names and files differ from the first source's for the shared hashes.
"""
import hashlib
import math
import os
import random
import sqlite3
import sys

SCHEMA = r'''
PRAGMA journal_mode = WAL;
CREATE TABLE torrents (
    id             INTEGER PRIMARY KEY,
    info_hash      BLOB NOT NULL UNIQUE,
    name           TEXT NOT NULL,
    total_size     INTEGER NOT NULL CHECK(total_size > 0),
    discovered_on  INTEGER NOT NULL CHECK(discovered_on > 0)
);
CREATE TABLE files (
    id          INTEGER PRIMARY KEY,
    torrent_id  INTEGER REFERENCES torrents ON DELETE CASCADE ON UPDATE RESTRICT,
    size        INTEGER NOT NULL,
    path        TEXT NOT NULL
);
DROP INDEX IF EXISTS info_hash_index;
CREATE UNIQUE INDEX info_hash_index ON torrents (info_hash);
ALTER TABLE torrents ADD COLUMN updated_on INTEGER CHECK (updated_on > 0) DEFAULT NULL;
ALTER TABLE torrents ADD COLUMN n_seeders  INTEGER CHECK ((updated_on IS NOT NULL AND n_seeders >= 0) OR (updated_on IS NULL AND n_seeders IS NULL)) DEFAULT NULL;
ALTER TABLE torrents ADD COLUMN n_leechers INTEGER CHECK ((updated_on IS NOT NULL AND n_leechers >= 0) OR (updated_on IS NULL AND n_leechers IS NULL)) DEFAULT NULL;
ALTER TABLE files ADD COLUMN is_readme INTEGER CHECK (is_readme IS NULL OR is_readme=1) DEFAULT NULL;
ALTER TABLE files ADD COLUMN content   TEXT    CHECK ((content IS NULL AND is_readme IS NULL) OR (content IS NOT NULL AND is_readme=1)) DEFAULT NULL;
CREATE UNIQUE INDEX readme_index ON files (torrent_id, is_readme);
CREATE VIRTUAL TABLE torrents_idx USING fts5(name, content='torrents', content_rowid='id', tokenize="porter unicode61 separators ' !""#$%&''()*+,-./:;<=>?@[\]^_`{|}~'");
CREATE TRIGGER torrents_idx_ai_t AFTER INSERT ON torrents BEGIN
  INSERT INTO torrents_idx(rowid, name) VALUES (new.id, new.name);
END;
CREATE TRIGGER torrents_idx_ad_t AFTER DELETE ON torrents BEGIN
  INSERT INTO torrents_idx(torrents_idx, rowid, name) VALUES('delete', old.id, old.name);
END;
CREATE TRIGGER torrents_idx_au_t AFTER UPDATE ON torrents BEGIN
  INSERT INTO torrents_idx(torrents_idx, rowid, name) VALUES('delete', old.id, old.name);
  INSERT INTO torrents_idx(rowid, name) VALUES (new.id, new.name);
END;
ALTER TABLE torrents ADD COLUMN modified_on INTEGER NOT NULL
    CHECK (modified_on >= discovered_on AND (updated_on IS NOT NULL OR modified_on >= updated_on))
    DEFAULT 32503680000;
CREATE TRIGGER "torrents_modified_on_default_t" AFTER INSERT ON "torrents" BEGIN
  UPDATE "torrents" SET "modified_on" = NEW."discovered_on" WHERE "id" = NEW."id" AND NEW."modified_on" = 32503680000;
END;
CREATE INDEX modified_on_index ON torrents (modified_on);
PRAGMA user_version = 3;
'''

# A synthetic vocabulary: syllables glued into made-up words, plus a few
# generic English words, so token frequencies follow a skewed (Zipf-like)
# distribution without naming any real work.
SYL = ["ka", "lo", "mi", "ren", "tas", "vor", "el", "dun", "shi", "ba",
       "qua", "zel", "nor", "pi", "ex", "tor", "lan", "mur", "eth", "sol"]
COMMON = ["the", "of", "and", "night", "river", "stone", "light", "world",
          "house", "blue", "garden", "winter", "city", "story", "season",
          "collection", "complete", "live", "edition", "remastered"]
NONASCII = ["Звезда", "река", "夜の街", "光", "Café", "Ærø", "über", "niño"]
RES = ["480p", "720p", "1080p", "2160p"]
SRC = ["WEB-DL", "WEBRip", "BluRay", "HDTV", "DVDRip"]
CODEC = ["x264", "x265", "HEVC", "AV1", "XviD"]
AUDIO = ["AAC", "AC3", "FLAC", "MP3", "DTS"]
EXT_VIDEO = ["mkv", "mp4", "avi"]
EXT_OTHER = ["mp3", "flac", "pdf", "epub", "zip", "iso", "jpg", "txt", "nfo", "srt"]


def make_lexicon(rng, n=6000):
    words = set()
    while len(words) < n:
        words.add("".join(rng.choice(SYL) for _ in range(rng.randint(2, 4))))
    return sorted(words)


def zipf_pick(rng, items):
    # index ~ floor(len * u^3): heavy head, long tail
    return items[min(len(items) - 1, int(len(items) * rng.random() ** 3))]


def title(rng, lex):
    k = rng.choice([1, 2, 2, 3, 3, 4])
    ws = []
    for _ in range(k):
        r = rng.random()
        if r < 0.25:
            ws.append(rng.choice(COMMON))
        elif r < 0.27:
            ws.append(rng.choice(NONASCII))
        else:
            ws.append(zipf_pick(rng, lex))
    return [w.capitalize() for w in ws]


def name_for(rng, lex, multi):
    t = title(rng, lex)
    r = rng.random()
    if r < 0.45:  # video release
        parts = t + [str(rng.randint(1975, 2026))]
        if rng.random() < 0.3:
            parts.append("S%02dE%02d" % (rng.randint(1, 12), rng.randint(1, 24)))
        parts += [rng.choice(RES), rng.choice(SRC), rng.choice(CODEC)]
        name = ".".join(parts) + "-" + zipf_pick(rng, lex).upper()[:6]
        ext = rng.choice(EXT_VIDEO)
    elif r < 0.65:  # music
        name = "%s - %s (%d) [%s]" % (" ".join(title(rng, lex)), " ".join(t),
                                      rng.randint(1960, 2026), rng.choice(AUDIO))
        ext = rng.choice(["mp3", "flac"])
    else:  # plain phrase
        name = " ".join(t + ([str(rng.randint(1, 99))] if rng.random() < 0.3 else []))
        ext = rng.choice(EXT_OTHER)
    return name, ext


def file_count(rng):
    if rng.random() < 0.45:
        return 1
    return max(2, min(2000, int(math.exp(rng.gauss(math.log(8), 1.3)))))


def size(rng, median):
    return max(1, min(64 << 30, int(math.exp(rng.gauss(math.log(median), 1.6)))))


def main():
    out, count = sys.argv[1], int(sys.argv[2])
    seed = int(sys.argv[3]) if len(sys.argv) > 3 else 20261102
    first = int(sys.argv[4]) if len(sys.argv) > 4 else 0
    if os.path.exists(out):
        sys.exit("refusing to overwrite " + out)
    rng = random.Random(seed if first == 0 else "%d:%d" % (seed, first))
    lex = make_lexicon(rng)
    conn = sqlite3.connect(out)
    conn.executescript(SCHEMA)
    t0, t1 = 1546300800, 1790812800  # 2019-01-01 .. 2026-10-01
    files_total = 0
    with conn:
        for i in range(first, first + count):
            ih = hashlib.sha1(b"lodestar-smoke:%d:%d" % (seed, i)).digest()
            n = file_count(rng)
            name, ext = name_for(rng, lex, n > 1)
            if n == 1:
                files = [(name + "." + ext, size(rng, 40 << 20))]
            else:
                dirs = [""] + ["%s/" % zipf_pick(rng, lex).capitalize() for _ in range(rng.randint(0, 3))]
                files = []
                for j in range(n):
                    e = ext if rng.random() < 0.8 else rng.choice(EXT_OTHER)
                    files.append(("%s%02d %s.%s" % (rng.choice(dirs), j + 1,
                                                    " ".join(title(rng, lex)), e),
                                  size(rng, 4 << 20)))
            total = sum(s for _, s in files)
            disc = rng.randint(t0, t1)
            if rng.random() < 0.3:
                upd = disc + rng.randint(0, 86400 * 30)
                se, le = int(rng.expovariate(1 / 20)), int(rng.expovariate(1 / 5))
            else:
                upd = se = le = None
            cur = conn.execute(
                "INSERT INTO torrents (info_hash, name, total_size, discovered_on,"
                " updated_on, n_seeders, n_leechers, modified_on) VALUES (?,?,?,?,?,?,?,?)",
                (ih, name, total, disc, upd, se, le, max(disc, upd or 0)))
            tid = cur.lastrowid
            conn.executemany("INSERT INTO files (torrent_id, size, path) VALUES (?,?,?)",
                             [(tid, s, p) for p, s in files])
            files_total += n
    conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    conn.execute("PRAGMA journal_mode = DELETE")
    conn.close()
    print("torrents=%d files=%d seed=%d first=%d bytes=%d" % (count, files_total, seed, first, os.path.getsize(out)))


if __name__ == "__main__":
    main()
