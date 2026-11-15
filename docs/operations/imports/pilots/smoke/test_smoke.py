"""smoke.py's live-segment index sizes (X-index-bytes-1m), on a fake data
folder; no server, binary or network. Run: python3 -m unittest test_smoke"""
import json
import os
from pathlib import Path
import tempfile
import unittest

import smoke

LIVE = "0123456789abcdef0123456789abcdef"
GONE = "fedcba9876543210fedcba9876543210"
FILE_SEG = "00000000000000000000000000000001"


def hyphenated(simple):
    s = simple
    return f"{s[:8]}-{s[8:12]}-{s[12:16]}-{s[16:20]}-{s[20:]}"


def write(path, size):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(b"x" * size)


class IndexLive(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.index = Path(self.tmp.name) / "index"
        current = self.index / "v3-t2"
        current.mkdir(parents=True)
        (current / "meta.json").write_text(json.dumps({"segments": [
            {"segment_id": hyphenated(LIVE), "max_doc": 10,
             "deletes": {"num_deleted_docs": 2, "opstamp": 7}},
        ]}))
        write(current / f"{LIVE}.idx", 100)
        write(current / f"{LIVE}.store", 50)
        write(current / f"{LIVE}.7.del", 3)        # its current deletes
        write(current / f"{LIVE}.5.del", 4)        # superseded: stale
        write(current / f"{LIVE}.store.temp", 5)   # leftover: stale
        write(current / f"{GONE}.idx", 60)         # merged away: stale
        write(current / ".managed.json", 9)        # no segment's
        files = current / "files"
        files.mkdir()
        (files / "meta.json").write_text(json.dumps({"segments": [
            {"segment_id": hyphenated(FILE_SEG), "max_doc": 30, "deletes": None},
        ]}))
        write(files / f"{FILE_SEG}.idx", 200)
        write(files / f"{FILE_SEG}.3.del", 6)      # no deletes now: stale
        older = self.index / "v2-t9"               # an older schema's folder
        older.mkdir()
        (older / "meta.json").write_text('{"segments": []}')
        write(older / "junk", 500)
        self.current = current

    def tearDown(self):
        self.tmp.cleanup()

    def test_counts_only_what_the_live_segments_use(self):
        got = smoke.index_live(str(self.index))
        self.assertEqual(got["torrent"], 100 + 50 + 3)
        self.assertEqual(got["files"], 200)
        self.assertEqual(got["stale"], 4 + 5 + 60 + 6)
        self.assertEqual((got["deleted"], got["segments"]), (2, 1))
        self.assertEqual(got["other_versions"], smoke.du(str(self.index / "v2-t9")))

    def test_newest_version_folder_is_current_by_number(self):
        # v3-t2 beats v2-t9 although 9 > 2: schema first, then tokenizer.
        os.rename(self.index / "v2-t9", self.index / "v3-t1")
        got = smoke.index_live(str(self.index))
        self.assertEqual(got["torrent"], 153)

    def test_empty_or_missing_index_is_zero(self):
        empty = smoke.index_live(str(Path(self.tmp.name) / "nothing"))
        self.assertEqual(empty, {"torrent": 0, "files": 0, "stale": 0, "deleted": 0,
                                 "segments": 0, "other_versions": 0})


if __name__ == "__main__":
    unittest.main()
