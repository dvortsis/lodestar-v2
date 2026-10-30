"""Offline provenance and package safety gates; never builds or starts a child."""
import copy
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import prepare_windows as prepare


class PreparationTests(unittest.TestCase):
    def test_dirty_source_is_rejected(self):
        with patch.object(prepare, "git", return_value="?? extra.rs"):
            with self.assertRaisesRegex(ValueError, "source changes"):
                prepare.source_identity(Path("repo"))

    def test_clean_identity_records_commit_tree_and_epoch(self):
        with patch.object(prepare, "git", side_effect=["", "a" * 40, "b" * 40, "123"]):
            self.assertEqual(prepare.source_identity(Path("repo")), {
                "source_commit": "a" * 40, "source_tree": "b" * 40, "source_date_epoch": "123",
            })

    def test_mid_build_source_change_is_rejected(self):
        with patch.object(prepare, "source_identity", return_value={"source_commit": "changed"}):
            with self.assertRaisesRegex(ValueError, "changed during"):
                prepare.check_identity(Path("repo"), {"source_commit": "original"})

    def test_output_must_be_new_direct_target_child(self):
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory).resolve()
            (repo / "target" / "old").mkdir(parents=True)
            self.assertEqual(prepare.output_path(repo, Path("target/new")), repo / "target" / "new")
            for bad in ("target", "target/old", "target/old/nested", "target/../evidence", "target/cargo"):
                with self.subTest(path=bad), self.assertRaises(ValueError):
                    prepare.output_path(repo, Path(bad))

    def test_storage_low_space_rejected(self):
        with patch.object(prepare.shutil, "disk_usage") as usage:
            usage.return_value.free = 49 * prepare.GIB
            with self.assertRaisesRegex(ValueError, "50 GiB"):
                prepare.check_storage(Path("repo"))

    def test_storage_cache_ceiling_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory).resolve()
            (repo / "target" / "cargo").mkdir(parents=True)
            with patch.object(prepare.shutil, "disk_usage") as usage, patch.object(prepare, "files_below") as files:
                usage.return_value.free = 60 * prepare.GIB
                item = unittest.mock.Mock()
                item.stat.return_value.st_size = 36 * prepare.GIB
                files.return_value = [item]
                with self.assertRaisesRegex(ValueError, "35 GiB"):
                    prepare.check_storage(repo)

    def test_complete_smoke_must_match_artifacts_and_clean_shutdown(self):
        good = {"checks_passed": True, "failure": None, "exe_sha256": "exe", "index_sha256": "ui",
                "shutdown": {"signal_delivered": True, "exit_confirmed": True, "exit_code": 0,
                             "stop_mode": "exited_0", "errors": []}}
        self.assertTrue(prepare.smoke_passed(good, "exe", "ui"))
        for field, value in (("checks_passed", False), ("failure", "failed"), ("exe_sha256", "other"),
                             ("index_sha256", "other"), ("shutdown", None)):
            bad = copy.deepcopy(good)
            bad[field] = value
            with self.subTest(field=field):
                self.assertFalse(prepare.smoke_passed(bad, "exe", "ui"))
        for field, value in (("signal_delivered", False), ("exit_confirmed", False), ("exit_code", 1),
                             ("stop_mode", "forced_kill"), ("errors", ["failure"])):
            bad = copy.deepcopy(good)
            bad["shutdown"][field] = value
            with self.subTest(field=field):
                self.assertFalse(prepare.smoke_passed(bad, "exe", "ui"))

    def test_records_never_overwrite_existing_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "record.json"
            prepare.write_json(path, {"old": True})
            before = prepare.digest(path)
            with self.assertRaises(FileExistsError):
                prepare.write_json(path, {"new": True})
            self.assertEqual(prepare.digest(path), before)

    def test_package_tamper_missing_file_and_path_escape_fail(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            (root / "ui").mkdir()
            for name in ("lodestar.exe", "ui/index.html", *prepare.TOOLS):
                (root / name).write_text("fixture", encoding="utf-8")
            identity = {"source_commit": "a" * 40, "source_tree": "b" * 40}
            prepare.write_json(root / "build-record.json", {
                **identity, "exe_sha256": prepare.digest(root / "lodestar.exe"),
                "index_sha256": prepare.digest(root / "ui/index.html"),
            })
            manifest = {"schema_version": 1, **identity,
                        "state": "packaging_checks_passed_awaiting_launch_environment_review",
                        "files": {p.relative_to(root).as_posix(): prepare.digest(p) for p in prepare.files_below(root)}}
            with patch.object(prepare.json, "loads", side_effect=[manifest, {
                **identity, "exe_sha256": manifest["files"]["lodestar.exe"],
                "index_sha256": manifest["files"]["ui/index.html"],
            }]):
                (root / "manifest.json").write_text("mock", encoding="utf-8")
                prepare.verify_package(root)
            for change in ("missing", "hash", "escape", "identity"):
                bad = copy.deepcopy(manifest)
                if change == "missing":
                    del bad["files"]["cr0-common.ps1"]
                elif change == "hash":
                    bad["files"]["lodestar.exe"] = "0" * 64
                elif change == "escape":
                    bad["files"]["../elsewhere"] = "0" * 64
                else:
                    bad["source_commit"] = "c" * 40
                (root / "manifest.json").write_text(prepare.json.dumps(bad), encoding="utf-8")
                with self.subTest(change=change), self.assertRaises(ValueError):
                    prepare.verify_package(root)


if __name__ == "__main__":
    unittest.main()
