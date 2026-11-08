#!/usr/bin/env python3
"""Tests for update_manifest.py: field values and order, hashes and sizes
from real files, the caps, missing artifacts, re-runs, the envelope, and
that no secret reaches the output.
Run: python3 .github/scripts/test_update_manifest.py"""

import base64
import contextlib
import hashlib
import io
import json
import os
import shutil
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import update_manifest as um  # noqa: E402

REPO = "dvortsis/lodestar-v2"
BASE = f"https://github.com/{REPO}/releases/download/v0.6.0"
NOW = 1_800_000_000
SIG = "untrusted comment: x\nRUQ=\ntrusted comment: y\nAAAA\n"


class Builder(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp)
        self.files = {
            "x86": self.write("lodestar-0.6.0-x86_64-linux-musl", b"x86 binary"),
            "arm": self.write("lodestar-0.6.0-aarch64-linux-musl", b"arm64 binary!"),
            "msi": self.write("lodestar-0.6.0-x86_64.msi", b"msi" * 1000),
            "zip": self.write("lodestar-0.6.0-x86_64-windows-portable.zip", b"z"),
        }
        self.statements = self.write("statements.json", b"[]")
        self.meta = self.write("meta.json", json.dumps(um.CONSERVATIVE).encode())
        self.out = self.tmp / "body.json"

    def write(self, name, data):
        p = self.tmp / name
        p.write_bytes(data)
        return p

    def run_cli(self, *argv):
        err, out = io.StringIO(), io.StringIO()
        with contextlib.redirect_stderr(err), contextlib.redirect_stdout(out):
            code = um.main([str(a) for a in argv])
        return code, out.getvalue() + err.getvalue()

    def build(self, *extra, channel="stable", version="0.6.0", repo=REPO, tag=None, meta=True):
        if tag is None:
            tag = "nightly" if channel == "nightly" else f"v{version}"
        if meta and "--metadata" not in extra:
            extra = ("--metadata", self.meta, *extra)
        return self.run_cli(
            "build", "--channel", channel, "--version", version, "--repo", repo, "--tag", tag,
            "--x86-64-binary", self.files["x86"], "--aarch64-binary", self.files["arm"],
            "--msi", self.files["msi"], "--portable", self.files["zip"],
            "--statements", self.statements, "--out", self.out, "--now", NOW, *extra)

    def body(self):
        return json.loads(self.out.read_bytes())

    def test_fields_values_and_order(self):
        notes = self.write("notes.md", "First line.\r\nSecond line.\n".encode())
        meta = self.write("meta2.json", json.dumps({
            "level": "feature", "security": True, "manual_steps": False, "min_from": "0.5.1",
            "migrates": True, "reindex": False, "reindex_bytes": 1024}).encode())
        code, _ = self.build("--metadata", meta, "--notes-file", notes,
                             "--notes-url", "https://github.com/dvortsis/lodestar-v2/releases/tag/v0.6.0")
        self.assertEqual(code, 0)
        b = self.body()
        self.assertEqual(list(b), ["type", "format", "channel", "timestamp", "expires", "statements", "releases"])
        self.assertEqual((b["type"], b["format"], b["channel"]), ("lodestar-manifest", 1, "stable"))
        self.assertEqual((b["timestamp"], b["expires"]), (NOW, NOW + 30 * 86400))
        self.assertEqual(b["statements"], [])
        (r,) = b["releases"]
        self.assertEqual(list(r), list(um.RELEASE_FIELDS))
        self.assertEqual(r["version"], "0.6.0")
        self.assertEqual(r["date"], NOW)
        self.assertEqual((r["level"], r["security"], r["manual_steps"], r["min_from"]), ("feature", True, False, "0.5.1"))
        self.assertEqual((r["withdrawn"], r["migrates"], r["reindex"], r["reindex_bytes"]), (False, True, False, 1024))
        self.assertEqual(r["notes"], "First line.\nSecond line.", "line endings normalised, ends trimmed")
        self.assertEqual(r["notes_url"], "https://github.com/dvortsis/lodestar-v2/releases/tag/v0.6.0")
        self.assertEqual([(a["target"], a["kind"]) for a in r["artifacts"]], [
            ("x86_64-unknown-linux-musl", "binary"), ("aarch64-unknown-linux-musl", "binary"),
            ("x86_64-pc-windows-msvc", "msi"), ("x86_64-pc-windows-msvc", "portable")])
        for a in r["artifacts"]:
            self.assertEqual(list(a), list(um.ARTIFACT_FIELDS))
        # Compact JSON, no trailing newline: these exact bytes get signed.
        self.assertFalse(self.out.read_bytes().endswith(b"\n"))
        self.assertNotIn(b": ", self.out.read_bytes())

    def test_hashes_and_sizes_come_from_the_files(self):
        self.assertEqual(self.build()[0], 0)
        arts = self.body()["releases"][0]["artifacts"]
        for a, key in zip(arts, ["x86", "arm", "msi", "zip"]):
            data = self.files[key].read_bytes()
            self.assertEqual(a["sha256"], hashlib.sha256(data).hexdigest())
            self.assertEqual(a["size"], len(data))
            self.assertEqual(a["url"], f"{BASE}/{self.files[key].name}")

    def test_a_release_without_metadata_is_refused(self):
        code, out = self.build(meta=False)
        self.assertEqual(code, 1)
        self.assertIn("metadata file", out)
        self.assertFalse(self.out.exists())

    def test_a_prerelease_without_metadata_is_cautious_and_warns(self):
        code, out = self.build(channel="beta", version="0.6.0-rc.1", meta=False)
        self.assertEqual(code, 0)
        r = self.body()["releases"][0]
        self.assertEqual((r["level"], r["migrates"], r["reindex"], r["security"]), ("major", True, True, False))
        self.assertEqual(r["notes"], "Lodestar 0.6.0-rc.1.")
        self.assertIn("::warning::", out)

    def test_the_drafts_placeholder_is_refused_as_notes(self):
        notes = self.write("notes.md", b"Draft. Windows files are not code-signed (SmartScreen will warn).")
        code, out = self.build("--notes-file", notes)
        self.assertEqual(code, 1)
        self.assertIn("placeholder", out)

    def test_a_missing_artifact_fails_loudly(self):
        self.files["msi"].unlink()
        code, out = self.build()
        self.assertEqual(code, 1)
        self.assertIn("msi for x86_64-pc-windows-msvc is missing", out)
        self.assertFalse(self.out.exists())

    def test_an_empty_artifact_is_refused(self):
        self.files["zip"].write_bytes(b"")
        self.assertEqual(self.build()[0], 1)

    def test_urls_are_under_this_tag_only(self):
        for repo in ["dvortsis", "../..", "a/b/c", "a/b?x=1", "a@b/c", "someone/lodestar-v2", "a/" + "b" * 600]:
            with self.subTest(repo=repo):
                self.assertEqual(self.build(repo=repo)[0], 1)
        for tag in ["v0.5.2", "0.6.0", "nightly", "v0.6.0/../v0.5.2"]:
            with self.subTest(tag=tag):
                self.assertEqual(self.build(tag=tag)[0], 1)
        self.assertEqual(self.build(channel="nightly", version="0.6.1-nightly.1.1", tag="v0.6.1")[0], 1)

    def test_url_checks(self):
        for url in ["http://github.com/x", "https://evil.example/x", "https://GitHub.com/x",
                    "https://github.com:8443/x", "https://u@github.com/x", "https://github.com/x?y=1",
                    "https://github.com/x#y", "https://github.com/" + "a" * 600, "https://github.com",
                    "https://a_b.example/x", "https://-a.example/x", "https://a..b/x", "https://github.com/a b",
                    "https://github.com/x\n", "https://github.com/\u00e9"]:
            with self.subTest(url=url), self.assertRaises(um.Refused):
                um.check_url(url, um.DOWNLOAD_HOSTS, "url")
        um.check_url(BASE + "/x", um.DOWNLOAD_HOSTS, "url")
        um.check_url("https://example.org/notes/v0.6.0", None, "notes_url")

    def test_a_previous_release_hosted_elsewhere_is_refused(self):
        prev = self.previous(["0.5.2"])
        body = json.loads(prev.read_bytes())
        body["releases"][0]["artifacts"][0]["url"] = "https://github.com/someone/else/releases/download/v0.5.2/x"
        prev.write_text(json.dumps(body))
        code, out = self.build("--previous", prev)
        self.assertEqual(code, 1)
        self.assertIn("must be under", out)

    def test_versions(self):
        self.assertEqual(self.build(version="0.6.0-rc.1")[0], 1, "a pre-release never goes on stable")
        self.assertEqual(self.build(channel="beta", version="0.6.0-rc.1")[0], 0)
        self.assertEqual(self.build(channel="nightly", version="0.6.0")[0], 1)
        self.assertEqual(self.build(channel="nightly", version="0.6.1-nightly.20261108.57")[0], 0)
        for bad in ["v0.6.0", "0.6.0+build.1", "0.6", "0.06.0", "0.6.0-01", "0.6.0-" + "a" * 64,
                    "0.6.0\n", "1\u0663.0.0", "0.6.0-rc.\u0661"]:
            with self.subTest(version=bad):
                self.assertEqual(self.build(channel="beta", version=bad)[0], 1)

    def test_semver_precedence(self):
        order = ["0.5.1", "0.6.0-nightly.20261108.9", "0.6.0-nightly.20261108.10",
                 "0.6.0-nightly.20261109.1", "0.6.0-rc.1", "0.6.0-rc.2", "0.6.0-rc.10", "0.6.0", "0.6.1"]
        self.assertEqual(sorted(reversed(order), key=um.version_key), order)

    def previous(self, versions, channel="stable", timestamp=NOW + 5):
        self.assertEqual(self.build(channel=channel, version=versions[0])[0], 0)
        r = self.body()["releases"][0]
        body = {"type": "lodestar-manifest", "format": 1, "channel": channel, "timestamp": timestamp,
                "expires": timestamp + 86400, "statements": [],
                "releases": [{**r, "version": v} for v in versions]}
        return self.write("previous.json", json.dumps(body).encode())

    def test_previous_releases_are_carried_forward_newest_first(self):
        prev = self.previous(["0.5.1", "0.5.2"])
        self.assertEqual(self.build("--previous", prev)[0], 0)
        b = self.body()
        self.assertEqual([r["version"] for r in b["releases"]], ["0.6.0", "0.5.2", "0.5.1"])
        self.assertEqual(b["timestamp"], NOW + 6, "newer than the published manifest")

    def test_a_rerun_never_replaces_a_published_entry(self):
        prev = self.previous(["0.6.0"])
        self.out.unlink()
        code, out = self.build("--previous", prev)
        self.assertEqual(code, um.EXIT_ALREADY)
        self.assertIn("already in the published stable manifest", out)
        self.assertFalse(self.out.exists())

    def test_keep_drops_the_oldest(self):
        prev = self.previous(["0.6.0-nightly.1.1", "0.6.0-nightly.1.2"], channel="nightly")
        self.assertEqual(self.build("--previous", prev, "--keep", 2, channel="nightly",
                                    version="0.6.0-nightly.2.1")[0], 0)
        self.assertEqual([r["version"] for r in self.body()["releases"]],
                         ["0.6.0-nightly.2.1", "0.6.0-nightly.1.2"])
        self.assertEqual(self.build("--previous", prev, "--keep", 1, channel="nightly",
                                    version="0.6.0-nightly.0.1")[0], 1, "older than everything kept")

    def test_a_previous_manifest_listing_a_version_twice_is_refused(self):
        prev = self.previous(["0.5.2", "0.5.2"])
        code, out = self.build("--previous", prev)
        self.assertEqual(code, 1)
        self.assertIn("twice", out)

    def test_statement_fields_must_be_strings_and_json_finite(self):
        for st in [{"body": 1, "minisig": SIG}, {"body": "e30=", "minisig": None},
                   {"body": "e30=", "minisig": SIG, "cosig": 7}]:
            with self.subTest(st=st):
                self.statements.write_text(json.dumps([st]))
                self.assertEqual(self.build()[0], 1)
        for raw in ['[{"body": "e30=", "minisig": NaN}]', '[{"body": "e30=", "minisig": Infinity}]']:
            with self.subTest(raw=raw):
                self.statements.write_text(raw)
                self.assertEqual(self.build()[0], 1)

    def test_check_version_is_the_tag_check(self):
        for good in ["0.6.0", "0.6.0-rc.1", "0.6.0-rc.10", "1.0.0-beta.2", "0.6.1-nightly.20261108.57"]:
            with self.subTest(version=good):
                self.assertEqual(self.run_cli("check-version", good)[0], 0)
        for bad in ["0.6.0-rc.01", "0.6.0-01", "0.6.0-", "0.6.0-rc..1", "0.6.0+b", "v0.6.0", "00.6.0",
                    "0.6.0-rc.1\n", "0.6.0-" + "a" * 64]:
            with self.subTest(version=bad):
                self.assertEqual(self.run_cli("check-version", bad)[0], 1)

    def test_a_previous_manifest_of_another_channel_is_refused(self):
        prev = self.previous(["0.5.2"], channel="beta")
        self.assertEqual(self.build("--previous", prev)[0], 1)

    def test_a_previous_manifest_with_unknown_fields_is_refused(self):
        prev = self.previous(["0.5.2"])
        body = json.loads(prev.read_bytes())
        body["releases"][0]["extra"] = 1
        prev.write_text(json.dumps(body))
        self.assertEqual(self.build("--previous", prev)[0], 1)

    def test_caps(self):
        notes = self.write("notes.md", b"n" * (16 * 1024 + 1))
        self.assertEqual(self.build("--notes-file", notes)[0], 1, "notes over 16 KiB")
        notes.write_bytes("\u00e9".encode() * (8 * 1024 + 1))
        self.assertEqual(self.build("--notes-file", notes)[0], 1, "16 KiB counts bytes, not characters")
        notes.write_bytes(b"n" * (16 * 1024))
        self.assertEqual(self.build("--notes-file", notes)[0], 0, "notes at 16 KiB")
        prev = self.previous([f"0.5.{i}" for i in range(64)])
        self.assertEqual(self.build("--previous", prev)[0], 0)
        self.assertEqual(len(self.body()["releases"]), 64, "at most 64 releases")
        self.assertEqual(self.body()["releases"][-1]["version"], "0.5.1")
        stmts = [{"body": base64.b64encode(json.dumps({"timestamp": 1}).encode()).decode(), "minisig": SIG}] * 65
        self.statements.write_text(json.dumps(stmts))
        self.assertEqual(self.build()[0], 1, "at most 64 statements")
        self.statements.write_text("[]")
        meta = self.write("meta3.json", json.dumps({**um.CONSERVATIVE, "level": "huge"}).encode())
        self.assertEqual(self.build("--metadata", meta)[0], 1)
        meta.write_text(json.dumps({**um.CONSERVATIVE, "surprise": True}))
        self.assertEqual(self.build("--metadata", meta)[0], 1, "unknown metadata field")
        meta.write_text(json.dumps(um.CONSERVATIVE)[:-1] + ', "level": "fix"}')
        self.assertEqual(self.build("--metadata", meta)[0], 1, "a duplicate key")
        meta.write_text(json.dumps({**um.CONSERVATIVE, "min_from": "0.5.1\n"}))
        self.assertEqual(self.build("--metadata", meta)[0], 1, "min_from with a newline")
        meta.write_text(json.dumps({**um.CONSERVATIVE, "min_from": "1\u0663.0.0"}))
        self.assertEqual(self.build("--metadata", meta)[0], 1, "min_from with a non-ASCII digit")

    def test_a_body_over_384_kib_is_refused(self):
        notes = self.write("notes.md", b"n" * (16 * 1024))
        self.assertEqual(self.build("--notes-file", notes)[0], 0)
        r = self.body()["releases"][0]
        body = {"type": "lodestar-manifest", "format": 1, "channel": "stable", "timestamp": NOW,
                "expires": NOW + 1, "statements": [], "releases": [{**r, "version": f"0.5.{i}"} for i in range(30)]}
        prev = self.write("previous.json", json.dumps(body).encode())
        code, out = self.build("--previous", prev, "--notes-file", notes)
        self.assertEqual(code, 1)
        self.assertIn("over the 393216-byte cap", out)

    def test_statements_are_carried_and_date_the_manifest(self):
        st = {"body": base64.b64encode(json.dumps({"timestamp": NOW + 100}).encode()).decode(), "minisig": SIG}
        self.statements.write_text(json.dumps([st]))
        self.assertEqual(self.build()[0], 1, "dated before a statement it carries")
        st["body"] = base64.b64encode(json.dumps({"timestamp": NOW - 100}).encode()).decode()
        self.statements.write_text(json.dumps([st]))
        self.assertEqual(self.build()[0], 0)
        self.assertEqual(self.body()["statements"], [st])

    def test_envelope_and_extract_round_trip(self):
        self.assertEqual(self.build()[0], 0)
        sig = self.write("body.minisig", SIG.encode())
        env = self.tmp / "stable.json"
        self.assertEqual(self.run_cli("envelope", self.out, sig, env)[0], 0)
        e = json.loads(env.read_bytes())
        self.assertEqual(list(e), ["manifest", "freshness"])
        self.assertIsNone(e["freshness"])
        self.assertEqual(base64.b64decode(e["manifest"]["body"]), self.out.read_bytes())
        self.assertEqual(e["manifest"]["minisig"], SIG)
        body2, sig2 = self.tmp / "b2", self.tmp / "s2"
        self.assertEqual(self.run_cli("extract", env, body2, sig2)[0], 0)
        self.assertEqual(body2.read_bytes(), self.out.read_bytes())
        self.assertEqual(sig2.read_text(), SIG)
        sig.write_text("one line\n")
        self.assertEqual(self.run_cli("envelope", self.out, sig, env)[0], 1)

    def test_no_secret_reaches_the_output(self):
        secret = "untrusted comment: minisign encrypted secret key\nRWRTY0IySECRETSENTINEL"
        old = {k: os.environ.get(k) for k in ("RELEASE_SIGNING_KEY", "NIGHTLY_SIGNING_KEY")}
        self.addCleanup(lambda: [os.environ.pop(k, None) if v is None else os.environ.__setitem__(k, v)
                                 for k, v in old.items()])
        os.environ["RELEASE_SIGNING_KEY"] = os.environ["NIGHTLY_SIGNING_KEY"] = secret
        outputs = [self.build()[1], self.build(repo="bad")[1]]
        sig = self.write("body.minisig", SIG.encode())
        outputs.append(self.run_cli("envelope", self.out, sig, self.tmp / "e.json")[1])
        written = b"".join(p.read_bytes() for p in self.tmp.iterdir())
        for text in outputs:
            self.assertNotIn("SECRETSENTINEL", text)
        self.assertNotIn(b"SECRETSENTINEL", written)
        source = Path(um.__file__).read_text()
        self.assertNotIn("environ", source, "the builder reads no environment, so no key")


if __name__ == "__main__":
    unittest.main()
