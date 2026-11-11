"""Offline tests for the pin tool (CMP-13): an in-memory network and fixture
OpenPGP keys made here with gpg in a temporary home. No private key is
committed, and nothing touches the network.

    python3 -m unittest discover -s tools/companion-pins -p 'test_*.py' -v
"""

import contextlib
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
                    Fake(files), "request", "--tag", tag, "--skip-image", companion="qbittorrent", version="5.0.3"
                )
        status, err = self.run_pin(
            Fake(files), "--tag", "release-5.0.3_v2.0.11", "--skip-image", companion="qbittorrent", version="5.0.3"
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


if __name__ == "__main__":
    unittest.main()
