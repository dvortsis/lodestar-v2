#!/usr/bin/env python3
"""Inventory a stopped run and validate a disposable SQLite copy, never its source.

The manifest is PRIVATE: it contains evidence filenames. Run only after checking
that the collector and crawler have stopped. Output must be a new directory.
"""
import argparse
from contextlib import closing
import hashlib
import json
import os
from pathlib import Path
import shutil
import sqlite3


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def preserve(source, output):
    source, output = source.resolve(strict=True), output.resolve()
    if output.is_relative_to(source) or source.is_relative_to(output):
        raise ValueError("Evidence and output must be disjoint")
    output.mkdir(parents=True, exist_ok=False)
    manifest = {"schema_version": 1, "files": [], "errors": [], "missing": []}
    def walk_error(error):
        manifest["errors"].append({"operation": "enumerate", "path": str(error.filename),
                                   "error": type(error).__name__})
    for directory, dirs, files in os.walk(source, onerror=walk_error, followlinks=False):
        # Do not follow directory junctions/symlinks outside the evidence tree.
        for name in list(dirs):
            path = Path(directory) / name
            if path.is_symlink() or path.is_junction():
                dirs.remove(name)
                manifest["errors"].append({"operation": "skip_link", "path": str(path.relative_to(source))})
        for name in sorted(files):
            path = Path(directory) / name
            record = {"path": path.relative_to(source).as_posix()}
            try:
                if path.is_symlink():
                    raise ValueError("Symbolic link")
                before = path.stat()
                record.update(bytes=before.st_size, sha256=digest(path))
                after = path.stat()
                record["stable"] = (before.st_size, before.st_mtime_ns) == (after.st_size, after.st_mtime_ns)
            except (OSError, ValueError) as error:
                record["error"] = type(error).__name__
            manifest["files"].append(record)
    for name in ("run.json", "status.json", "result.json", "samples.jsonl", "final.prom"):
        if not (source / name).exists():
            manifest["missing"].append(name)
    (output / "manifest.private.json").write_text(json.dumps(manifest, indent=2), encoding="utf-8")
    fixture = output / "recovery"
    fixture.mkdir()
    recovery = {"copied": [], "integrity": None, "foreign_key_violations": None}
    try:
        data = source / "data"
        if data.is_symlink() or data.is_junction() or not data.resolve().is_relative_to(source):
            raise ValueError("Linked data directory")
        for suffix in ("", "-wal", "-shm"):
            path = data / ("lodestar.db" + suffix)
            if path.is_symlink() or path.is_junction():
                raise ValueError("Linked database file")
            if path.exists():
                target = fixture / path.name
                shutil.copyfile(path, target)
                if digest(path) != digest(target):
                    raise ValueError("Copy hash mismatch")
                recovery["copied"].append(path.name)
        if "lodestar.db" not in recovery["copied"]:
            raise ValueError("Missing source database")
        with closing(sqlite3.connect(fixture / "lodestar.db")) as db:
            # Only counts are exported: row-level diagnostics remain private.
            rows = db.execute("PRAGMA integrity_check").fetchall()
            recovery["integrity"] = "ok" if rows == [("ok",)] else "failed"
            recovery["foreign_key_violations"] = sum(1 for _ in db.execute("PRAGMA foreign_key_check"))
    except (OSError, ValueError, sqlite3.Error) as error:
        recovery["error"] = type(error).__name__
    (output / "recovery.json").write_text(json.dumps(recovery, indent=2), encoding="utf-8")
    return {"files": len(manifest["files"]), "enumeration_errors": len(manifest["errors"]),
            "file_errors": sum("error" in f or not f.get("stable", False) for f in manifest["files"]),
            "missing": manifest["missing"], "recovery": recovery}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    print(json.dumps(preserve(args.source, args.output), indent=2))
