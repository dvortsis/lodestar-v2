"""Offline tests for the pin tool (CMP-13): an in-memory network and fixture
OpenPGP keys made here with gpg in a temporary home. No private key is
committed, and nothing touches the network.

    python3 -m unittest discover -s tools/companion-pins -p 'test_*.py' -v
"""

import contextlib
import dataclasses
import datetime
import hashlib
import io
import json
import os
import re
import shutil
import subprocess
import tarfile
import tempfile
import unittest
import warnings
import zipfile
from pathlib import Path
from unittest import mock

import pin
import upstream

ROOT = Path(__file__).resolve().parents[2]
CATALOGUE = ROOT / "crates/companions/data/catalogue.json"
GPG = shutil.which("gpg")
REL = "https://github.com/test/testco/releases/download/"
BODY = b"\x7fELF a companion build" * 100


class Fake:
    """URL -> (status, headers, body); anything else is a 404."""

    def __init__(self, routes=None):
        self.routes = dict(routes or {})
        self.calls = []

    def get(self, url, headers):
        self.calls.append((url, dict(headers)))
        status, hdrs, body = self.routes.get(url, (404, {}, b""))
        return pin.Response(status, hdrs, io.BytesIO(body))


def ok(body, **headers):
    return (200, headers, body)


def redirect(to):
    return (302, {"Location": to}, b"")


def test_entry():
    return {
        "id": "testco",
        "name": "Testco",
        "licence": {"spdx": "MIT", "url": "https://example.org/LICENSE"},
        "homepage": "https://example.org/",
        "needed_for": ["download_client"],
        "opt_in": False,
        "hosts": list(pin.DOWNLOAD_HOSTS),
        "native": {
            "release": REL,
            "artifacts": [
                {"platform": "linux", "arch": "x86_64", "archive": "binary", "executable": None, "pin": None},
                {"platform": "windows", "arch": "x86_64", "archive": "installer", "executable": None, "pin": None},
            ],
        },
        "container": None,
        "ports": [],
        "health": None,
        "templates": [],
    }


class Gpg:
    """Fixture keys, generated per test run in a throwaway home."""

    def __init__(self, root: Path):
        self.home = root / "gnupg"
        self.home.mkdir(mode=0o700)
        self.keys = root / "keys"
        self.keys.mkdir()

    def run(self, *args, data=None):
        return subprocess.run(
            [
                GPG,
                "--homedir",
                str(self.home),
                "--batch",
                "--no-tty",
                "--pinentry-mode",
                "loopback",
                "--passphrase",
                "",
                *args,
            ],
            input=data,
            capture_output=True,
            check=True,
        ).stdout

    def new_key(self, name, *opts):
        self.run(*opts, "--quick-gen-key", f"{name} <{name}@test.invalid>", "ed25519", "sign", "never")
        out = self.run("--with-colons", "--list-keys", f"{name}@test.invalid").decode()
        return pin.primaries(out)[0]

    def export(self, *fprs, as_name=None):
        path = self.keys / f"{as_name or fprs[0]}.asc"
        path.write_bytes(self.run("--armor", "--export", *fprs))
        return path

    def sign(self, fpr, data, detached=True):
        mode = "--detach-sign" if detached else "--sign"
        return self.run("--armor", "--local-user", fpr, mode, data=data)


class Case(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="pins-test-"))
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        doc = json.loads(CATALOGUE.read_text())
        doc["entries"].append(test_entry())
        self.cat = self.tmp / "catalogue.json"
        self.cat.write_text(pin.format_catalogue(doc))
        self.out = self.tmp / "out.json"
        self.report = self.tmp / "report.md"
        self.keys = self.tmp / "keys"
        self.keys.mkdir(exist_ok=True)

    def run_pin(self, fake, *extra, companion="testco", version="1.2.3", recipe=None):
        recipes = dict(upstream.RECIPES)
        if recipe is not None:
            recipes[companion] = recipe
        argv = [
            "--catalogue",
            str(self.cat),
            "--out",
            str(self.out),
            "--report",
            str(self.report),
            "--companion",
            companion,
            "--version",
            version,
            *extra,
        ]
        err = io.StringIO()
        with (
            mock.patch.object(upstream, "RECIPES", recipes),
            contextlib.redirect_stderr(err),
            contextlib.redirect_stdout(io.StringIO()),
        ):
            code = pin.main(argv, transport=fake, keys_dir=self.keys)
        return code, err.getvalue()

    def refused(self, fake, code, *extra, **kw):
        status, err = self.run_pin(fake, *extra, **kw)
        self.assertEqual(status, 1, err)
        self.assertIn(f"{code}:", err)
        self.assertFalse(self.out.exists(), "a refused pin wrote a catalogue")
        self.assertFalse(self.report.exists())
        return err

    def entry(self, cid="testco"):
        return next(e for e in json.loads(self.out.read_text())["entries"] if e["id"] == cid)


def unsigned_recipe(files=None):
    return upstream.Recipe(
        tag="v{version}",
        files=files or {"linux-x86_64": "testco"},
        signature={"linux-x86_64": upstream.Unsigned("fixture")},
        image_tag=None,
    )


URL = f"{REL}v1.2.3/testco"


class Download(Case):
    def test_unsigned_pin_records_size_sha256_and_none(self):
        status, err = self.run_pin(Fake({URL: ok(BODY)}), "--artifacts", "linux-x86_64", recipe=unsigned_recipe())
        self.assertEqual(status, 0, err)
        p = self.entry()["native"]["artifacts"][0]["pin"]
        self.assertEqual(
            p,
            {
                "version": "1.2.3",
                "url": URL,
                "size": len(BODY),
                "sha256": hashlib.sha256(BODY).hexdigest(),
                "upstream_sig": {"kind": "none"},
            },
        )
        self.assertIsNone(self.entry()["native"]["artifacts"][1]["pin"], "the installer stays awaiting its pin")

    def test_redirect_within_the_allowlist_is_followed(self):
        asset = "https://release-assets.githubusercontent.com/x/y?sig=1"
        status, err = self.run_pin(Fake({URL: redirect(asset), asset: ok(BODY)}), recipe=unsigned_recipe())
        self.assertEqual(status, 0, err)

    def test_host_outside_the_allowlist(self):
        doc = json.loads(self.cat.read_text())
        doc["entries"][-1]["native"]["release"] = "https://evil.example/test/testco/releases/download/"
        doc["entries"][-1]["hosts"].append("evil.example")
        self.cat.write_text(pin.format_catalogue(doc))
        self.refused(
            Fake({"https://evil.example/test/testco/releases/download/v1.2.3/testco": ok(BODY)}),
            "host_not_allowed",
            recipe=unsigned_recipe(),
        )

    def test_redirect_to_a_disallowed_host(self):
        for to in (
            "https://evil.example/x",
            "http://github.com/x",
            "https://github.com:8443/x",
            "https://user@github.com/x",
            "https://downloads.sourceforge.net/x",
            "https://www.fosshub.com/x",
        ):
            with self.subTest(to=to):
                self.refused(Fake({URL: redirect(to), to: ok(BODY)}), "host_not_allowed", recipe=unsigned_recipe())

    def test_too_many_redirects(self):
        hops = {URL: redirect("https://github.com/h0")}
        hops |= {f"https://github.com/h{i}": redirect(f"https://github.com/h{i + 1}") for i in range(pin.MAX_HOPS)}
        self.refused(Fake(hops), "redirect", recipe=unsigned_recipe())

    def test_oversized_body(self):
        with mock.patch.object(pin, "MAX_ARTIFACT_SIZE", 100):
            self.refused(Fake({URL: ok(b"x" * 101)}), "too_large", recipe=unsigned_recipe())
            self.refused(Fake({URL: ok(b"x", **{"Content-Length": "101"})}), "too_large", recipe=unsigned_recipe())

    def test_http_error_and_missing_artifact(self):
        self.refused(Fake({URL: (500, {}, b"")}), "http_status", recipe=unsigned_recipe())
        self.refused(Fake({}), "http_status", recipe=unsigned_recipe())

    def test_unsigned_artifact_that_now_has_a_signature(self):
        self.refused(Fake({URL: ok(BODY), URL + ".asc": ok(b"sig")}), "signature_unexpected", recipe=unsigned_recipe())

    def test_installer_is_refused_when_asked_for(self):
        status, err = self.run_pin(Fake({URL: ok(BODY)}), "--artifacts", "windows-x86_64", recipe=unsigned_recipe())
        self.assertEqual(status, 1)
        self.assertIn("installer:", err)
        self.assertFalse(self.out.exists())

    def test_bad_requests(self):
        for version in ("01.2", "1.2.3.4.5", "1.x", "../1"):
            with self.subTest(version=version):
                self.refused(Fake(), "request", version=version, recipe=unsigned_recipe())
        self.refused(Fake(), "request", companion="nobody")
        for tag in ("v1/../x", "..", ".", ".v1.2.3", "v1.2.4"):
            with self.subTest(tag=tag):
                self.refused(Fake({URL: ok(BODY)}), "request", "--tag", tag, recipe=unsigned_recipe())

    def test_qbittorrent_tag_must_be_the_versions_release(self):
        files = {
            f"https://github.com/userdocs/qbittorrent-nox-static/releases/download/{tag}/{arch}-qbittorrent-nox": ok(
                BODY
            )
            for tag in ("release-5.0.3_v2.0.11", "release-5.1.0_v2.0.11")
            for arch in ("x86_64", "aarch64")
        }
        for tag in ("release-5.1.0_v2.0.11", "release-5.0.3", "release-5.0.3_v2.0.11/x", "5.0.3"):
            with self.subTest(tag=tag):
                self.refused(
                    Fake(files),
                    "request",
                    "--tag",
                    tag,
                    "--skip-image",
                    "--artifacts",
                    "linux-x86_64",
                    companion="qbittorrent",
                    version="5.0.3",
                )
        status, err = self.run_pin(
            Fake(files),
            "--tag",
            "release-5.0.3_v2.0.11",
            "--skip-image",
            "--artifacts",
            "linux-x86_64,linux-aarch64",
            companion="qbittorrent",
            version="5.0.3",
        )
        self.assertEqual(status, 0, err)
        self.assertIn("image digest was not re-pinned", self.report.read_text())

    def test_relative_redirect_is_resolved_and_checked(self):
        fake = Fake({URL: (302, {"Location": "/test/testco/x"}, b""), "https://github.com/test/testco/x": ok(BODY)})
        self.assertEqual(self.run_pin(fake, recipe=unsigned_recipe())[0], 0)

    def test_same_version_with_other_bytes_is_refused(self):
        self.assertEqual(self.run_pin(Fake({URL: ok(BODY)}), recipe=unsigned_recipe())[0], 0)
        self.cat.write_text(self.out.read_text())
        self.out.unlink()
        self.report.unlink()
        self.refused(Fake({URL: ok(BODY + b"replaced")}), "sha_changed", recipe=unsigned_recipe())

    def test_a_new_minisig_beside_an_unsigned_artifact(self):
        self.refused(
            Fake({URL: ok(BODY), URL + ".minisig": ok(b"untrusted comment")}),
            "signature_unexpected",
            recipe=unsigned_recipe(),
        )

    def test_an_unforeseen_error_fails_closed(self):
        with mock.patch.object(pin, "fetch_file", side_effect=OSError("disk")):
            err = self.refused(Fake(), "error", recipe=unsigned_recipe())
        self.assertIn("OSError", err)

    def test_a_failed_report_write_leaves_no_catalogue(self):
        real = Path.write_text

        def write_text(path, body, *a, **k):
            if path.name == "report.md.tmp":
                raise OSError("disk full")
            return real(path, body, *a, **k)

        with mock.patch.object(Path, "write_text", write_text):
            self.refused(Fake({URL: ok(BODY)}), "error", recipe=unsigned_recipe())
        self.assertEqual(sorted(p.name for p in self.tmp.iterdir()), ["catalogue.json", "keys"])

    def test_the_report_says_what_made_the_pin(self):
        with mock.patch.dict(os.environ, {"GITHUB_SHA": "abc123", "GITHUB_REF": "refs/heads/main"}):
            self.assertEqual(self.run_pin(Fake({URL: ok(BODY)}), recipe=unsigned_recipe())[0], 0)
        report = self.report.read_text()
        self.assertIn("GITHUB_SHA: `abc123`", report)
        self.assertIn("`upstream.py` SHA-256", report)

    def test_mixed_versions_in_one_entry(self):
        recipe = upstream.Recipe(
            tag="v{version}",
            files={"linux-x86_64": "testco", "linux-aarch64": "testco-arm"},
            signature={k: upstream.Unsigned("fixture") for k in ("linux-x86_64", "linux-aarch64")},
            image_tag=None,
        )
        doc = json.loads(self.cat.read_text())
        doc["entries"][-1]["native"]["artifacts"].insert(
            1, {"platform": "linux", "arch": "aarch64", "archive": "binary", "executable": None, "pin": None}
        )
        self.cat.write_text(pin.format_catalogue(doc))
        self.assertEqual(self.run_pin(Fake({URL: ok(BODY), f"{REL}v1.2.3/testco-arm": ok(BODY)}), recipe=recipe)[0], 0)
        self.cat.write_text(self.out.read_text())
        self.out.unlink()
        self.report.unlink()
        status, err = self.run_pin(
            Fake({f"{REL}v1.2.4/testco": ok(BODY)}), "--artifacts", "linux-x86_64", version="1.2.4", recipe=recipe
        )
        self.assertEqual(status, 1)
        self.assertIn("several versions", err)


class Drift(unittest.TestCase):
    """The tool's copies of the Rust constants it mirrors."""

    def test_constants_match_the_rust_sources(self):
        cat = (ROOT / "crates/companions/src/catalogue.rs").read_text()
        hosts = re.search(r"pub const DOWNLOAD_HOSTS: &\[&str\] = &\[(.*?)\];", cat, re.S).group(1)
        self.assertEqual(tuple(re.findall(r'"([^"]+)"', hosts)), pin.DOWNLOAD_HOSTS)
        self.assertIn("pub const MAX_ARTIFACT_SIZE: u64 = 512 * 1024 * 1024;", cat)
        self.assertEqual(pin.MAX_ARTIFACT_SIZE, 512 * 1024 * 1024)
        registries = re.search(r"pub const REGISTRIES: &\[&str\] = &\[(.*?)\];", cat, re.S).group(1)
        self.assertEqual(sorted(re.findall(r'"([^"]+)"', registries)), sorted(pin.REGISTRIES))
        sources = re.search(r"pub const INSTALLER_SOURCES: &\[&str\] = &\[(.*?)\];", cat, re.S).group(1)
        self.assertEqual(tuple(re.findall(r'"([^"]+)"', sources)), pin.INSTALLER_SOURCES)
        recipe = upstream.RECIPES["qbittorrent"]
        self.assertIn(pin.host_of(recipe.installer_release), pin.INSTALLER_SOURCES)
        check = (ROOT / "crates/lodestar/src/update/check.rs").read_text()
        self.assertIn(f"pub const MAX_HOPS: usize = {pin.MAX_HOPS};", check)


class Determinism(Case):
    def test_the_committed_catalogue_is_in_the_tools_layout(self):
        text = CATALOGUE.read_text()
        self.assertEqual(pin.format_catalogue(pin.load_catalogue(text)), text)

    def test_a_re_run_gives_the_same_bytes(self):
        fake = Fake({URL: ok(BODY)})
        self.assertEqual(self.run_pin(fake, recipe=unsigned_recipe())[0], 0)
        first = self.out.read_bytes()
        self.assertEqual(self.run_pin(fake, recipe=unsigned_recipe())[0], 0)
        self.assertEqual(self.out.read_bytes(), first)
        # Pinning again from the pinned file changes nothing either.
        self.cat.write_bytes(first)
        self.assertEqual(self.run_pin(fake, recipe=unsigned_recipe())[0], 0)
        self.assertEqual(self.out.read_bytes(), first)

    def test_duplicate_keys_are_refused(self):
        with self.assertRaises(pin.Refused):
            pin.load_catalogue('{"format": 1, "format": 1, "entries": []}')


@unittest.skipUnless(GPG, "gpg is not installed")
class Signatures(Case):
    @classmethod
    def setUpClass(cls):
        cls.root = Path(tempfile.mkdtemp(prefix="pins-gpg-"))
        cls.gpg = Gpg(cls.root)
        cls.a = cls.gpg.new_key("pinned")
        cls.b = cls.gpg.new_key("other")

    @classmethod
    def tearDownClass(cls):
        subprocess.run(["gpgconf", "--homedir", str(cls.gpg.home), "--kill", "all"], capture_output=True)
        shutil.rmtree(cls.root, ignore_errors=True)

    def setUp(self):
        super().setUp()
        shutil.copy(self.gpg.export(self.a), self.keys / f"{self.a}.asc")

    def recipe(self, keys=None):
        return upstream.Recipe(
            tag="v{version}",
            files={"linux-x86_64": "testco"},
            signature={"linux-x86_64": upstream.OpenPgp(".asc", keys or (self.a,))},
            image_tag=None,
        )

    def test_good_signature_by_the_pinned_key(self):
        fake = Fake({URL: ok(BODY), URL + ".asc": ok(self.gpg.sign(self.a, BODY))})
        status, err = self.run_pin(fake, recipe=self.recipe())
        self.assertEqual(status, 0, err)
        self.assertEqual(
            self.entry()["native"]["artifacts"][0]["pin"]["upstream_sig"],
            {"kind": "verified_at_pin", "fingerprint": self.a},
        )

    def test_wrong_sha256_tampered_artifact(self):
        fake = Fake({URL: ok(BODY + b"!"), URL + ".asc": ok(self.gpg.sign(self.a, BODY))})
        self.refused(fake, "bad_signature", recipe=self.recipe())

    def test_valid_signature_by_an_unpinned_key(self):
        fake = Fake({URL: ok(BODY), URL + ".asc": ok(self.gpg.sign(self.b, BODY))})
        self.refused(fake, "bad_signature", recipe=self.recipe())

    def test_a_key_file_carrying_an_extra_key(self):
        shutil.copy(self.gpg.export(self.a, self.b, as_name="both"), self.keys / f"{self.a}.asc")
        fake = Fake({URL: ok(BODY), URL + ".asc": ok(self.gpg.sign(self.b, BODY))})
        self.refused(fake, "unknown_key", recipe=self.recipe())

    def test_a_key_file_holding_another_key(self):
        shutil.copy(self.gpg.export(self.b), self.keys / f"{self.a}.asc")
        fake = Fake({URL: ok(BODY), URL + ".asc": ok(self.gpg.sign(self.b, BODY))})
        self.refused(fake, "unknown_key", recipe=self.recipe())

    def test_bad_signature(self):
        sig = bytearray(self.gpg.sign(self.a, BODY))
        lines = sig.split(b"\n")
        body = next(i for i, ln in enumerate(lines) if ln and not ln.startswith(b"-") and b":" not in ln)
        lines[body] = lines[body][:-4] + (b"AAAA" if not lines[body].endswith(b"AAAA") else b"BBBB")
        fake = Fake({URL: ok(BODY), URL + ".asc": ok(b"\n".join(lines))})
        self.refused(fake, "bad_signature", recipe=self.recipe())
        self.refused(Fake({URL: ok(BODY), URL + ".asc": ok(b"not a signature")}), "bad_signature", recipe=self.recipe())

    def test_a_signed_message_is_not_a_detached_signature(self):
        fake = Fake({URL: ok(BODY), URL + ".asc": ok(self.gpg.sign(self.a, BODY, detached=False))})
        self.refused(fake, "bad_signature", recipe=self.recipe())

    def test_missing_signature_where_one_is_expected(self):
        self.refused(Fake({URL: ok(BODY)}), "signature_missing", recipe=self.recipe())

    def test_signature_redirect_to_a_disallowed_host(self):
        fake = Fake({URL: ok(BODY), URL + ".asc": redirect("https://evil.example/sig")})
        self.refused(fake, "host_not_allowed", recipe=self.recipe())

    def test_a_text_mode_signature_is_refused(self):
        sig = self.gpg.run("--armor", "--local-user", self.a, "--textmode", "--detach-sign", data=BODY)
        self.refused(Fake({URL: ok(BODY), URL + ".asc": ok(sig)}), "bad_signature", recipe=self.recipe())

    def test_a_key_with_an_expired_unused_subkey_still_verifies(self):
        fpr = self.gpg.new_key("old", "--faked-system-time", "20200101T000000")
        self.gpg.run("--faked-system-time", "20200101T000000", "--quick-add-key", fpr, "ed25519", "sign", "2021-01-01")
        self.gpg.export(fpr)
        shutil.copy(self.gpg.keys / f"{fpr}.asc", self.keys / f"{fpr}.asc")
        sig = self.gpg.run("--armor", "--local-user", fpr + "!", "--detach-sign", data=BODY)
        status, err = self.run_pin(Fake({URL: ok(BODY), URL + ".asc": ok(sig)}), recipe=self.recipe((fpr,)))
        self.assertEqual(status, 0, err)

    def test_pinned_key_without_its_file_or_fingerprint(self):
        self.refused(Fake({URL: ok(BODY), URL + ".asc": ok(b"x")}), "key_not_pinned", recipe=self.recipe(("ABCD",)))
        (self.keys / f"{self.a}.asc").unlink()
        self.refused(
            Fake({URL: ok(BODY), URL + ".asc": ok(self.gpg.sign(self.a, BODY))}), "key_not_pinned", recipe=self.recipe()
        )


def runtimeconfig(tfm, frameworks, key="includedFrameworks"):
    return json.dumps({"runtimeOptions": {"tfm": tfm, key: [{"name": n, "version": v} for n, v in frameworks]}})


class DotNet(unittest.TestCase):
    def test_runtimeconfig_rules(self):
        good = pin.dotnet_reads_socks5(
            runtimeconfig("net8.0", [("Microsoft.NETCore.App", "8.0.11"), ("Microsoft.AspNetCore.App", "8.0.11")])
        )
        self.assertIn("self-contained", good)
        for text in (
            runtimeconfig("net8.0", [("Microsoft.AspNetCore.App", "8.0.11")], key="frameworks"),
            runtimeconfig("net6.0", [("Microsoft.NETCore.App", "6.0.36")]),
            runtimeconfig("net8.0", [("Microsoft.NETCore.App", "7.0.20")]),
            runtimeconfig("netcoreapp3.1", [("Microsoft.NETCore.App", "8.0.0")]),
            "{}",
            "not json",
        ):
            with self.subTest(text=text), self.assertRaises(pin.Refused):
                pin.dotnet_reads_socks5(text)

    def test_runtimeconfig_beside_the_program_in_each_archive_kind(self):
        tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, tmp)
        cfg = runtimeconfig("net8.0", [("Microsoft.NETCore.App", "8.0.11")]).encode()
        z = tmp / "a.zip"
        with zipfile.ZipFile(z, "w") as f:
            f.writestr("Prowlarr/Prowlarr.exe", b"MZ")
            f.writestr("Prowlarr/Prowlarr.runtimeconfig.json", cfg)
        self.assertIn("net8.0", pin.check_dotnet_socks5(z, {"archive": "zip", "executable": "Prowlarr/Prowlarr.exe"}))
        t = tmp / "a.tar.gz"
        with tarfile.open(t, "w:gz") as f:
            for name, data in (("Prowlarr/Prowlarr", b"\x7fELF"),):
                info = tarfile.TarInfo(name)
                info.size = len(data)
                f.addfile(info, io.BytesIO(data))
        with self.assertRaises(pin.Refused):  # no runtimeconfig.json beside it
            pin.check_dotnet_socks5(t, {"archive": "tar_gz", "executable": "Prowlarr/Prowlarr"})
        d = tmp / "dup.zip"
        with zipfile.ZipFile(d, "w") as f, warnings.catch_warnings():
            warnings.simplefilter("ignore")
            f.writestr("Prowlarr/Prowlarr.exe", b"MZ")
            f.writestr("Prowlarr/Prowlarr.runtimeconfig.json", cfg)
            f.writestr("Prowlarr/Prowlarr.runtimeconfig.json", b"{}")
        with self.assertRaises(pin.Refused):  # CMP-2 refuses two members of one name
            pin.check_dotnet_socks5(d, {"archive": "zip", "executable": "Prowlarr/Prowlarr.exe"})


SRC_ROOT = "Prowlarr-1.2.3/src"


def prowlarr_source(extra=""):
    files = {
        f"{SRC_ROOT}/NzbDrone.Core/Configuration/ConfigService.cs": (
            'public bool ProxyEnabled => GetValueBoolean("ProxyEnabled", false);'
        ),
        f"{SRC_ROOT}/NzbDrone.Common/Http/Dispatchers/ManagedHttpDispatcher.cs": (
            "var handler = new SocketsHttpHandler(); Dns.GetHostAddressesAsync(h); new Socket(a, b, c);"
        )
        + extra,
        "Prowlarr-1.2.3/src/NzbDrone.Core.Test/Fake.cs": "new TcpClient(); UseProxy = false;",
    }
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz") as tf:
        for name, text in files.items():
            info = tarfile.TarInfo(name)
            info.size = len(text.encode())
            tf.addfile(info, io.BytesIO(text.encode()))
    return buf.getvalue()


class Prowlarr(unittest.TestCase):
    SRC = "https://codeload.github.com/Prowlarr/Prowlarr/tar.gz/refs/tags/v1.2.3"

    def check(self, body):
        with tempfile.TemporaryDirectory() as work:
            return pin.check_prowlarr_source(Fake({self.SRC: ok(body)}), "Prowlarr", "Prowlarr", "v1.2.3", Path(work))

    def test_source_checks_pass_and_skip_tests(self):
        lines = self.check(prowlarr_source(" if (p != null) { handler.Proxy = p; }"))
        self.assertIn("ProxyEnabled defaults to false (ConfigService.cs)", lines)
        self.assertIn(
            "  - `var handler = new SocketsHttpHandler(); Dns.GetHostAddressesAsync(h); new Socket(a, b, c);"
            " if (p != null) { handler.Proxy = p; }`".replace("{", "?").replace("}", "?"),
            lines,
        )

    def test_upstream_text_in_the_report_is_defanged(self):
        self.assertEqual(pin.safe("a`b[c](d)<e>\n*"), "a?b?c?(d)?e???")
        self.assertEqual(pin.safe("ping @someone"), "ping ?someone")

    def test_source_checks_fail_closed(self):
        for extra in (
            " UseProxy = false;",
            " new TcpClient();",
            " new SocketsHttpHandler();",
            " handler.UseProxy = x;",
            " handler.Proxy = a; handler.Proxy = b;",
            " new HttpClientHandler();",
            " HttpClient.DefaultProxy = p;",
            " WebRequest.DefaultWebProxy = p;",
            " new HttpClient(a); new HttpClient(b);",
        ):
            with self.subTest(extra=extra), self.assertRaises(pin.Refused):
                self.check(prowlarr_source(extra))
        buf = io.BytesIO()
        with tarfile.open(fileobj=buf, mode="w:gz") as tf:
            text = b'public bool ProxyEnabled => GetValueBoolean("ProxyEnabled", true);'
            info = tarfile.TarInfo("P/src/NzbDrone.Core/Configuration/ConfigService.cs")
            info.size = len(text)
            tf.addfile(info, io.BytesIO(text))
        with self.assertRaises(pin.Refused):
            self.check(buf.getvalue())


def prowlarr_build(archive):
    cfg = runtimeconfig(
        "net8.0", [("Microsoft.NETCore.App", "8.0.11"), ("Microsoft.AspNetCore.App", "8.0.11")]
    ).encode()
    buf = io.BytesIO()
    if archive == "zip":
        with zipfile.ZipFile(buf, "w") as z:
            z.writestr("Prowlarr/Prowlarr.exe", b"MZ")
            z.writestr("Prowlarr/Prowlarr.runtimeconfig.json", cfg)
    else:
        with tarfile.open(fileobj=buf, mode="w:gz") as tf:
            for name, data in (("Prowlarr/Prowlarr", b"\x7fELF"), ("Prowlarr/Prowlarr.runtimeconfig.json", cfg)):
                info = tarfile.TarInfo(name)
                info.size = len(data)
                tf.addfile(info, io.BytesIO(data))
    return buf.getvalue()


class ProwlarrEndToEnd(Case):
    def test_every_artifact_the_source_and_the_image(self):
        rel = "https://github.com/Prowlarr/Prowlarr/releases/download/v1.2.3.4/Prowlarr.master.1.2.3.4."
        builds = {
            f"{rel}linux-core-x64.tar.gz": prowlarr_build("tar"),
            f"{rel}linux-core-arm64.tar.gz": prowlarr_build("tar"),
            f"{rel}windows-core-x64.zip": prowlarr_build("zip"),
        }
        routes = {u: ok(b) for u, b in builds.items()}
        routes[Prowlarr.SRC.replace("v1.2.3", "v1.2.3.4")] = ok(prowlarr_source())
        index = Container.INDEX
        routes["https://lscr.io/v2/linuxserver/prowlarr/manifests/1.2.3.4"] = ok(index)
        status, err = self.run_pin(Fake(routes), companion="prowlarr", version="1.2.3.4")
        self.assertEqual(status, 0, err)
        e = self.entry("prowlarr")
        self.assertEqual(
            [a["pin"]["sha256"] for a in e["native"]["artifacts"]],
            [hashlib.sha256(b).hexdigest() for b in builds.values()],
        )
        self.assertEqual(e["container"]["digest"], "sha256:" + hashlib.sha256(index).hexdigest())
        report = self.report.read_text()
        self.assertIn("net8.0, self-contained", report)
        self.assertIn("ProxyEnabled defaults to false", report)
        # Everything else in the file is as it was.
        before = json.loads(self.cat.read_text())
        after = json.loads(self.out.read_text())
        before["entries"][1] = after["entries"][1]
        self.assertEqual(before, after)

    def test_a_build_that_drops_its_runtimeconfig_is_refused(self):
        rel = "https://github.com/Prowlarr/Prowlarr/releases/download/v1.2.3.4/Prowlarr.master.1.2.3.4."
        self.refused(
            Fake({f"{rel}linux-core-x64.tar.gz": ok(prowlarr_source())}),
            "check_failed",
            companion="prowlarr",
            version="1.2.3.4",
        )


class Container(Case):
    IMAGE_URL = "https://lscr.io/v2/linuxserver/prowlarr/manifests/1.2.3.4"
    GHCR = "https://ghcr.io/v2/linuxserver/prowlarr/manifests/1.2.3.4"
    TOKEN = "https://ghcr.io/token?service=ghcr.io&scope=repository%3Alinuxserver%2Fprowlarr%3Apull"
    INDEX = json.dumps(
        {
            "mediaType": "application/vnd.oci.image.index.v1+json",
            "manifests": [
                {"platform": {"os": "linux", "architecture": "amd64"}},
                {"platform": {"os": "linux", "architecture": "arm64"}},
            ],
        }
    ).encode()

    def routes(self, digest=None):
        d = digest or "sha256:" + hashlib.sha256(self.INDEX).hexdigest()
        return {
            self.IMAGE_URL: (307, {"Location": self.GHCR}, b""),
            self.GHCR: (401, {"WWW-Authenticate": 'Bearer realm="https://ghcr.io/token",service="ghcr.io"'}, b""),
            self.TOKEN: ok(b'{"token": "t0k"}'),
        }, d

    def test_digest_is_computed_from_the_bytes_and_the_token_goes_to_its_host_only(self):
        routes, d = self.routes()
        fake = Fake(routes)
        real_get = fake.get

        def get(url, headers):
            if url == self.GHCR and headers.get("Authorization") == "Bearer t0k":
                return pin.Response(200, {"Docker-Content-Digest": d}, io.BytesIO(self.INDEX))
            return real_get(url, headers)

        fake.get = get
        digest, platforms = pin.resolve_digest(fake, "lscr.io/linuxserver/prowlarr", "1.2.3.4")
        self.assertEqual(digest, d)
        self.assertEqual(platforms, ["linux/amd64", "linux/arm64"])
        self.assertTrue(all("Authorization" not in h for u, h in fake.calls if not u.startswith("https://ghcr.io/v2/")))

    def test_wrong_digest_from_the_registry(self):
        routes, _ = self.routes()
        routes[self.GHCR] = ok(self.INDEX, **{"Docker-Content-Digest": "sha256:" + "ab" * 32})
        with self.assertRaises(pin.Refused) as e:
            pin.resolve_digest(Fake(routes), "lscr.io/linuxserver/prowlarr", "1.2.3.4")
        self.assertEqual(e.exception.code, "digest_mismatch")

    def test_registry_refusals(self):
        routes, _ = self.routes()
        for name, mutate, tag in (
            ("latest", lambda r: None, "latest"),
            (
                "token realm off the allowlist",
                lambda r: r.__setitem__(
                    self.GHCR, (401, {"WWW-Authenticate": 'Bearer realm="https://evil.example/token",service="x"'}, b"")
                ),
                "1.2.3.4",
            ),
            (
                "redirect off the allowlist",
                lambda r: r.__setitem__(self.IMAGE_URL, redirect("https://evil.example/m")),
                "1.2.3.4",
            ),
            (
                "no amd64",
                lambda r: r.__setitem__(
                    self.GHCR,
                    ok(
                        json.dumps(
                            {
                                "mediaType": "application/vnd.oci.image.index.v1+json",
                                "manifests": [{"platform": {"os": "linux", "architecture": "arm64"}}],
                            }
                        ).encode()
                    ),
                ),
                "1.2.3.4",
            ),
        ):
            with self.subTest(name):
                r = dict(routes)
                mutate(r)
                with self.assertRaises(pin.Refused):
                    pin.resolve_digest(Fake(r), "lscr.io/linuxserver/prowlarr", tag)


SF_DIR = "https://downloads.sourceforge.net/project/qbittorrent/qbittorrent-win32/qbittorrent-5.0.3/"
SF = SF_DIR + "qbittorrent_5.0.3_x64_setup.exe"
MIRROR = SF.replace("downloads.sourceforge.net", "netix.dl.sourceforge.net")
INSTALLER = b"MZ an installer" * 200
FEED = "https://sourceforge.net/projects/qbittorrent/rss?path=/qbittorrent-win32/qbittorrent-5.0.3"
TAG_REF = "https://api.github.com/repos/qbittorrent/qBittorrent/git/ref/tags/release-5.0.3"
TAG_SHA = "a" * 40
TAG_OBJ = f"https://api.github.com/repos/qbittorrent/qBittorrent/git/tags/{TAG_SHA}"
FEED_PATH = "/qbittorrent-win32/qbittorrent-5.0.3/qbittorrent_5.0.3_x64_setup.exe"


def feed(items):
    """SourceForge's folder feed, in its documented RSS and Media RSS shape."""
    body = "".join(
        f"<item><title><![CDATA[{path}]]></title>"
        f"<link>https://sourceforge.net/projects/qbittorrent/files{path}/download</link>"
        f'<media:content url="https://sourceforge.net/projects/qbittorrent/files{path}/download" '
        f'type="application/x-msdownload; charset=binary" filesize="{size}">'
        f'<media:hash algo="md5">{md5}</media:hash></media:content></item>'
        for path, size, md5 in items
    )
    return (
        '<?xml version="1.0" encoding="utf-8"?><rss xmlns:content="http://purl.org/rss/1.0/modules/content/" '
        'xmlns:media="http://video.search.yahoo.com/mrss/" version="2.0"><channel><title>qBittorrent</title>'
        f"{body}</channel></rss>"
    ).encode()


def md5(b):
    return hashlib.md5(b, usedforsecurity=False).hexdigest()


@unittest.skipUnless(GPG, "gpg is not installed")
class InstallerPins(Case):
    """qBittorrent's Windows installer: read from SourceForge at pin time only,
    pinned only with the release key's signature (X-cmp13-installer-pins)."""

    @classmethod
    def setUpClass(cls):
        cls.root = Path(tempfile.mkdtemp(prefix="pins-gpg-"))
        cls.gpg = Gpg(cls.root)
        cls.a = cls.gpg.new_key("release")
        cls.b = cls.gpg.new_key("intruder")

    @classmethod
    def tearDownClass(cls):
        subprocess.run(["gpgconf", "--homedir", str(cls.gpg.home), "--kill", "all"], capture_output=True)
        shutil.rmtree(cls.root, ignore_errors=True)

    def recipe(self, policy=None):
        policy = policy or upstream.OpenPgp(".asc", (self.a,))
        base = upstream.RECIPES["qbittorrent"]
        return dataclasses.replace(base, signature={**base.signature, "windows-x86_64": policy})

    def routes(self, sig=None, body=INSTALLER, listed=None, tag_date="2026-01-01T00:00:00Z", tag_type="tag"):
        sig = self.gpg.sign(self.a, INSTALLER) if sig is None else sig
        listed = [(FEED_PATH, len(INSTALLER), md5(INSTALLER))] if listed is None else listed
        return {
            SF: redirect(MIRROR),
            MIRROR: ok(body),
            SF + ".asc": redirect(MIRROR + ".asc"),
            MIRROR + ".asc": ok(sig),
            FEED: ok(feed(listed)),
            TAG_REF: ok(json.dumps({"object": {"type": tag_type, "sha": TAG_SHA}}).encode()),
            TAG_OBJ: ok(json.dumps({"tagger": {"date": tag_date}}).encode()),
        }

    def pin(self, fake, *extra, recipe=None):
        return self.run_pin(
            fake,
            "--tag",
            "release-5.0.3_v2.0.11",
            "--skip-image",
            "--artifacts",
            "windows-x86_64",
            *extra,
            companion="qbittorrent",
            version="5.0.3",
            recipe=recipe or self.recipe(),
        )

    def refuse(self, fake, code, recipe=None):
        status, err = self.pin(fake, recipe=recipe)
        self.assertEqual(status, 1, err)
        self.assertIn(f"{code}:", err)
        self.assertFalse(self.out.exists())
        self.assertFalse(self.report.exists())
        return err

    def test_pinned_from_its_own_folder_with_the_release_key(self):
        shutil.copy(self.gpg.export(self.a), self.keys / f"{self.a}.asc")
        status, err = self.pin(Fake(self.routes()))
        self.assertEqual(status, 0, err)
        a = self.entry("qbittorrent")["native"]["artifacts"][2]
        self.assertEqual(list(a), ["platform", "arch", "archive", "executable", "release", "pin"])
        self.assertEqual(a["release"], "https://downloads.sourceforge.net/project/qbittorrent/qbittorrent-win32/")
        self.assertEqual(
            a["pin"],
            {
                "version": "5.0.3",
                "url": SF,
                "size": len(INSTALLER),
                "sha256": hashlib.sha256(INSTALLER).hexdigest(),
                "upstream_sig": {"kind": "verified_at_pin", "fingerprint": self.a},
            },
        )
        # The fetch hosts are untouched: SourceForge is never one.
        self.assertNotIn("downloads.sourceforge.net", self.entry("qbittorrent")["hosts"])

    def test_the_bytes_are_tied_to_the_version(self):
        shutil.copy(self.gpg.export(self.a), self.keys / f"{self.a}.asc")
        status, err = self.pin(Fake(self.routes()))
        self.assertEqual(status, 0, err)
        report = self.report.read_text()
        self.assertIn(f"MD5 `{md5(INSTALLER)}`", report)
        self.assertIn("dated 2026-01-01T00:00:00Z", report)
        self.assertIn("answered at `https://netix.dl.sourceforge.net/project/qbittorrent/", report)
        self.out.unlink()
        self.report.unlink()
        other = b"MZ another genuinely signed file" * 100
        same = bytes(reversed(INSTALLER))  # same length, other bytes
        cases = {
            # A mirror serving another signed file: the feed's MD5 and size don't match.
            "other bytes": self.routes(sig=self.gpg.sign(self.a, other), body=other),
            "wrong size": self.routes(listed=[(FEED_PATH, len(INSTALLER) + 1, md5(INSTALLER))]),
            # Same size, other MD5: the MD5 is the binding, not the size.
            "wrong md5": self.routes(listed=[(FEED_PATH, len(INSTALLER), md5(b"x"))]),
            "same-size signed file": self.routes(sig=self.gpg.sign(self.a, same), body=same),
            "a time without a zone": self.routes(tag_date="2026-01-01T00:00:00"),
            "no item": self.routes(listed=[]),
            "two items": self.routes(listed=[(FEED_PATH, len(INSTALLER), md5(INSTALLER))] * 2),
            "another path only": self.routes(
                listed=[(FEED_PATH.replace("_x64_", "_lt12_x64_"), len(INSTALLER), md5(INSTALLER))]
            ),
            # Signed before the release was tagged: an older file.
            "older signature": self.routes(tag_date="2099-01-01T00:00:00Z"),
            "lightweight tag": self.routes(tag_type="commit"),
            "no tag date": self.routes(tag_date="soon"),
        }
        for name, routes in cases.items():
            with self.subTest(name):
                self.refuse(Fake(routes), "installer_mismatch")
        # The download ending at another project's copy of the path.
        routes = self.routes()
        elsewhere = MIRROR.replace("/project/qbittorrent/", "/project/other/")
        routes[SF] = redirect(elsewhere)
        routes[elsewhere] = ok(INSTALLER)
        self.refuse(Fake(routes), "installer_mismatch")
        # The download ending at another file's path.
        routes = self.routes()
        lt12 = MIRROR.replace("_x64_", "_lt12_x64_")
        routes[SF] = redirect(lt12)
        routes[lt12] = ok(INSTALLER)
        self.refuse(Fake(routes), "installer_mismatch")
        # The feed only from SourceForge's origin, never a mirror or elsewhere.
        for to in ("https://netix.dl.sourceforge.net/rss", "https://evil.example/rss"):
            with self.subTest(to):
                routes = self.routes()
                routes[FEED] = redirect(to)
                routes[to] = ok(feed([(FEED_PATH, len(INSTALLER), md5(INSTALLER))]))
                self.refuse(Fake(routes), "host_not_allowed")
        # A document type in the feed (entity expansion) is refused, even with
        # a matching item and in another encoding.
        good = feed([(FEED_PATH, len(INSTALLER), md5(INSTALLER))]).decode()
        doctype = good.replace("<rss ", '<!DOCTYPE rss [<!ENTITY a "aaaa">]><rss ', 1)
        for name, body in (
            ("utf-8", doctype.encode()),
            ("utf-16", doctype.replace('encoding="utf-8"', 'encoding="utf-16"').encode("utf-16")),
        ):
            with self.subTest(name):
                routes = self.routes()
                routes[FEED] = ok(body)
                err = self.refuse(Fake(routes), "installer_mismatch")
                self.assertTrue("document type" in err or "isn't UTF-8" in err, err)

    def test_the_two_days_of_slack(self):
        shutil.copy(self.gpg.export(self.a), self.keys / f"{self.a}.asc")
        now = datetime.datetime.now(datetime.UTC)
        at = lambda d: (now + datetime.timedelta(days=d)).strftime("%Y-%m-%dT%H:%M:%SZ")  # noqa: E731
        status, err = self.pin(Fake(self.routes(tag_date=at(1))))
        self.assertEqual(status, 0, err)  # signed a day before the tag: within the slack
        self.out.unlink()
        self.report.unlink()
        self.refuse(Fake(self.routes(tag_date=at(3))), "installer_mismatch")

    def test_refusals(self):
        shutil.copy(self.gpg.export(self.a), self.keys / f"{self.a}.asc")
        no_sig = self.routes()
        del no_sig[MIRROR + ".asc"]
        del no_sig[SF + ".asc"]
        self.refuse(Fake(no_sig), "signature_missing")
        self.refuse(Fake(self.routes(body=INSTALLER + b"!")), "bad_signature")
        self.refuse(Fake(self.routes(sig=self.gpg.sign(self.b, INSTALLER))), "bad_signature")
        self.refuse(Fake(self.routes(sig=b"not a signature")), "bad_signature")
        self.refuse(Fake(self.routes()), "installer", recipe=self.recipe(upstream.Unsigned("no")))
        for to in (
            "https://evil.example/x.exe",
            "https://a.b.dl.sourceforge.net/x.exe",
            "https://dl.sourceforge.net.evil.example/x.exe",
            "http://netix.dl.sourceforge.net/x.exe",
            "https://github.com/x.exe",
            "https://www.fosshub.com/x.exe",
        ):
            with self.subTest(to=to):
                routes = self.routes()
                routes[SF] = redirect(to)
                routes[to] = ok(INSTALLER)
                self.refuse(Fake(routes), "host_not_allowed")
        routes = self.routes()
        routes[SF + ".asc"] = redirect("https://evil.example/sig.asc")
        self.refuse(Fake(routes), "host_not_allowed")

    def test_the_key_is_fetched_by_fingerprint_when_not_committed(self):
        sources = (("https://keys.example/k", ("keys.example",)), ("https://github.com/u.gpg", ("github.com",)))
        policy = upstream.OpenPgp(".asc", (self.a,), key_sources=sources)
        routes = self.routes()
        routes["https://keys.example/k"] = ok(self.gpg.run("--armor", "--export", self.a))
        # A user's key list may hold other keys too; only the pinned one is used.
        routes["https://github.com/u.gpg"] = ok(self.gpg.run("--armor", "--export", self.b, self.a))
        status, err = self.pin(Fake(routes), recipe=self.recipe(policy))
        self.assertEqual(status, 0, err)
        report = self.report.read_text()
        self.assertIn(f"Commit it as `keys/{self.a}.asc`", report)
        self.assertIn("BEGIN PGP PUBLIC KEY BLOCK", report)
        # A source that doesn't serve the pinned key refuses the pin.
        self.out.unlink()
        self.report.unlink()
        routes["https://github.com/u.gpg"] = ok(self.gpg.run("--armor", "--export", self.b))
        self.refuse(Fake(routes), "unknown_key", recipe=self.recipe(policy))
        # Nowhere to fetch it from, and no committed key: refused.
        none = upstream.OpenPgp(".asc", (self.a,))
        self.refuse(Fake(routes), "key_not_pinned", recipe=self.recipe(none))

    def test_mirror_hosts(self):
        hosts = pin.INSTALLER_HOSTS
        for h in ("downloads.sourceforge.net", "sourceforge.net", "netix.dl.sourceforge.net", "a-1.dl.sourceforge.net"):
            self.assertTrue(hosts.allows(h), h)
        for h in (
            "dl.sourceforge.net",
            "a.b.dl.sourceforge.net",
            "-a.dl.sourceforge.net",
            "evil.example",
            "sourceforge.net.evil.example",
            "xdl.sourceforge.net",
        ):
            self.assertFalse(hosts.allows(h), h)
        # SourceForge stays refused on every other fetch.
        with self.assertRaises(pin.Refused):
            pin.check_url("https://downloads.sourceforge.net/x", pin.DOWNLOAD_HOSTS + ("downloads.sourceforge.net",))


class LocalServer:
    """A real HTTP server on loopback, for the real transport's tests."""

    def __init__(self, handler):
        import http.server
        import threading

        self.release = threading.Event()
        release = self.release

        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def do_GET(self):
                handler(self, release)

            def log_message(self, *args):
                pass

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.url = f"http://127.0.0.1:{self.server.server_port}/"

    def close(self):
        self.release.set()
        self.server.shutdown()
        self.server.server_close()


class RealTransport(unittest.TestCase):
    """fetch() over a real socket: complete bodies arrive, stalls and drips
    can't outlast the deadline. Loopback is plain HTTP, so the https rule is
    lifted for these tests only."""

    def serve(self, handler):
        srv = LocalServer(handler)
        self.addCleanup(srv.close)
        env = mock.patch.dict(os.environ, {"NO_PROXY": "127.0.0.1", "no_proxy": "127.0.0.1"})
        env.start()
        self.addCleanup(env.stop)
        lifted = mock.patch.object(pin, "check_url", lambda url, hosts: None)
        lifted.start()
        self.addCleanup(lifted.stop)
        return srv.url

    def test_complete_bodies_arrive_length_delimited_and_chunked(self):
        body = b"companion build " * 10000

        def length(h, _):
            h.send_response(200)
            h.send_header("Content-Length", str(len(body)))
            h.end_headers()
            h.wfile.write(body)

        def chunked(h, _):
            h.send_response(200)
            h.send_header("Transfer-Encoding", "chunked")
            h.end_headers()
            for i in range(0, len(body), 4096):
                part = body[i : i + 4096]
                h.wfile.write(b"%x\r\n%s\r\n" % (len(part), part))
            h.wfile.write(b"0\r\n\r\n")

        for handler in (length, chunked):
            with self.subTest(handler.__name__):
                url = self.serve(handler)
                self.assertEqual(pin.fetch_bytes(pin.Transport(), url, (), len(body)), body)
                with tempfile.TemporaryDirectory() as d:
                    size, sha = pin.fetch_file(pin.Transport(), url, (), len(body), Path(d) / "f")
                self.assertEqual((size, sha), (len(body), hashlib.sha256(body).hexdigest()))

    def test_a_stall_or_a_drip_is_refused_at_the_deadline(self):
        import time

        def stall(h, release):
            h.send_response(200)
            h.send_header("Content-Length", "100000")
            h.end_headers()
            h.wfile.write(b"x" * 10)
            h.wfile.flush()
            release.wait(30)

        def drip(h, release):
            h.send_response(200)
            h.send_header("Content-Length", "100000")
            h.end_headers()
            while not release.wait(0.05):  # one byte per tick, until the test ends
                h.wfile.write(b"x")
                h.wfile.flush()

        for handler in (stall, drip):
            with self.subTest(handler.__name__), mock.patch.object(pin, "OVERALL", 1):
                url = self.serve(handler)
                start = time.monotonic()
                with self.assertRaises(pin.Refused) as e:
                    pin.fetch_bytes(pin.Transport(), url, (), 100000)
                self.assertEqual(e.exception.code, "timeout")
                self.assertLess(time.monotonic() - start, 10)


class SinglePlatform(Case):
    GHCR = "https://ghcr.io/v2/qdm12/thing/manifests/v1"

    def manifest(self, cfg):
        digest = "sha256:" + hashlib.sha256(cfg).hexdigest()
        body = json.dumps(
            {"mediaType": "application/vnd.oci.image.manifest.v1+json", "config": {"digest": digest}}
        ).encode()
        blob = f"https://ghcr.io/v2/qdm12/thing/blobs/{digest}"
        store = "https://pkg-containers.githubusercontent.com/b"
        return {self.GHCR: ok(body), blob: redirect(store), store: ok(cfg)}

    def test_the_config_names_the_platform(self):
        routes = self.manifest(json.dumps({"os": "linux", "architecture": "amd64"}).encode())
        _, platforms = pin.resolve_digest(Fake(routes), "ghcr.io/qdm12/thing", "v1")
        self.assertEqual(platforms, ["linux/amd64"])

    def test_the_token_reaches_the_registry_only(self):
        cfg = json.dumps({"os": "linux", "architecture": "amd64"}).encode()
        routes = self.manifest(cfg)
        body = routes.pop(self.GHCR)[2]
        routes[self.GHCR] = (401, {"WWW-Authenticate": 'Bearer realm="https://ghcr.io/token",service="ghcr.io"'}, b"")
        routes["https://ghcr.io/token?service=ghcr.io&scope=repository%3Aqdm12%2Fthing%3Apull"] = ok(
            b'{"token": "t0k"}'
        )
        fake = Fake(routes)
        real_get = fake.get

        def get(url, headers):
            if url == self.GHCR and headers.get("Authorization") == "Bearer t0k":
                fake.calls.append((url, dict(headers)))
                return pin.Response(200, {}, io.BytesIO(body))
            return real_get(url, headers)

        fake.get = get
        _, platforms = pin.resolve_digest(fake, "ghcr.io/qdm12/thing", "v1")
        self.assertEqual(platforms, ["linux/amd64"])
        blob = [h for u, h in fake.calls if "/blobs/" in u]
        self.assertEqual([h.get("Authorization") for h in blob], ["Bearer t0k"])
        store = [h for u, h in fake.calls if u.startswith("https://pkg-containers.githubusercontent.com/")]
        self.assertEqual(store, [{}], "the blob host got credentials")

    def test_a_manifest_without_a_config_or_a_config_without_a_platform(self):
        no_cfg = {self.GHCR: ok(json.dumps({"mediaType": "application/vnd.oci.image.manifest.v1+json"}).encode())}
        with self.assertRaises(pin.Refused) as e:
            pin.resolve_digest(Fake(no_cfg), "ghcr.io/qdm12/thing", "v1")
        self.assertEqual(e.exception.code, "registry")
        no_os = self.manifest(json.dumps({"architecture": "amd64"}).encode())
        with self.assertRaises(pin.Refused) as e:
            pin.resolve_digest(Fake(no_os), "ghcr.io/qdm12/thing", "v1")
        self.assertEqual(e.exception.code, "registry")

    def test_another_platform_or_a_wrong_config_is_refused(self):
        arm = self.manifest(json.dumps({"os": "linux", "architecture": "arm64"}).encode())
        with self.assertRaises(pin.Refused) as e:
            pin.resolve_digest(Fake(arm), "ghcr.io/qdm12/thing", "v1")
        self.assertEqual(e.exception.code, "registry")
        bad = self.manifest(json.dumps({"os": "linux", "architecture": "amd64"}).encode())
        store = "https://pkg-containers.githubusercontent.com/b"
        bad[store] = ok(b'{"os": "linux", "architecture": "amd64", "x": 1}')
        with self.assertRaises(pin.Refused) as e:
            pin.resolve_digest(Fake(bad), "ghcr.io/qdm12/thing", "v1")
        self.assertEqual(e.exception.code, "digest_mismatch")


class RealOpener(unittest.TestCase):
    """The real transport hands a redirect back instead of following it."""

    def test_a_302_is_returned_not_followed(self):
        import http.server
        import threading

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                if self.path == "/start":
                    self.send_response(302)
                    self.send_header("Location", "/followed")
                    self.end_headers()
                else:
                    self.send_response(200)
                    self.end_headers()
                    self.wfile.write(b"followed")

            def log_message(self, *args):
                pass

        server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        with mock.patch.dict(os.environ, {"NO_PROXY": "127.0.0.1", "no_proxy": "127.0.0.1"}):
            r = pin.Transport().get(f"http://127.0.0.1:{server.server_port}/start", {})
        try:
            self.assertEqual(r.status, 302)
            self.assertEqual(r.headers["location"], "/followed")
        finally:
            r.close()


if __name__ == "__main__":
    unittest.main()
