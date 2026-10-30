import json
from contextlib import closing
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest.mock import patch

from preserve_interrupted import preserve


class PreserveTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.source = self.root / "original"
        (self.source / "data").mkdir(parents=True)
        self.output = self.root / "copy"
        self.database = self.source / "data/lodestar.db"

    def database_fixture(self):
        with closing(sqlite3.connect(self.database)) as db, db:
            db.executescript("CREATE TABLE parent(id INTEGER PRIMARY KEY);"
                             "CREATE TABLE child(id INTEGER REFERENCES parent(id));"
                             "INSERT INTO child VALUES(7);")

    def test_copy_checks_foreign_keys_without_changing_original(self):
        self.database_fixture()
        before = self.database.read_bytes()
        result = preserve(self.source, self.output)
        self.assertEqual(result["recovery"]["integrity"], "ok")
        self.assertEqual(result["recovery"]["foreign_key_violations"], 1)
        self.assertEqual(self.database.read_bytes(), before)
        self.assertIn("result.json", result["missing"])
        manifest = json.loads((self.output / "manifest.private.json").read_text())
        self.assertTrue(manifest["files"][0]["stable"])

    def test_existing_output_and_overlapping_paths_are_refused(self):
        self.output.mkdir()
        with self.assertRaises(FileExistsError):
            preserve(self.source, self.output)
        with self.assertRaises(ValueError):
            preserve(self.source, self.source / "copy")
        with self.assertRaises(ValueError):
            preserve(self.source, self.root)

    def test_read_failures_and_corrupt_database_are_explicit(self):
        self.database.write_bytes(b"incomplete database")
        with patch("preserve_interrupted.digest", side_effect=PermissionError):
            result = preserve(self.source, self.output)
        self.assertEqual(result["file_errors"], 1)
        self.assertEqual(result["recovery"]["error"], "PermissionError")
        self.assertIsNone(result["recovery"]["integrity"])

    def test_recovery_does_not_follow_a_link_skipped_by_inventory(self):
        self.database_fixture()
        original_is_link = Path.is_symlink
        with patch.object(Path, "is_symlink", lambda path: path == self.source / "data" or original_is_link(path)):
            result = preserve(self.source, self.output)
        self.assertEqual(result["enumeration_errors"], 1)
        self.assertEqual(result["recovery"]["copied"], [])
        self.assertEqual(result["recovery"]["error"], "ValueError")


if __name__ == "__main__":
    unittest.main()
