#!/usr/bin/env python3
"""Tests for licenses.py. Standard library only:

    python3 .github/scripts/test_licenses.py

The fixtures under fixtures/licenses/ are `cargo metadata` output recorded
from a small workspace (a `lodestar` package with a path dependency, a
normal, a proc-macro, a Windows-only, a dev and a build dependency), with
paths scrubbed and unused package fields trimmed.
"""

from __future__ import annotations

import atexit
import copy
import io
import json
import shutil
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import licenses  # noqa: E402

FIXTURES = HERE / "fixtures" / "licenses"
LINUX = "x86_64-unknown-linux-musl"
WINDOWS = "x86_64-pc-windows-msvc"

# owner-decisions §2.12, word for word. allow.json must match it exactly: the
# list is widened only through a decisions entry the runner flags (§2.19).
OWNER_ALLOWED = {
    "MIT",
    "MIT-0",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Zlib",
    "0BSD",
    "BSL-1.0",
    "BlueOak-1.0.0",
    "Unicode-3.0",
    "MPL-2.0",
    "CC0-1.0",
    "OFL-1.1",
    "GPL-3.0-only",
    "GPL-3.0-or-later",
    "AGPL-3.0-only",
    "AGPL-3.0-or-later",
}

POLICY = licenses.Policy.load()


# The recorded paths are scrubbed; give each registry package a folder with a LICENSE.
CRATES = Path(tempfile.mkdtemp(prefix="licenses-test-"))
atexit.register(shutil.rmtree, CRATES, ignore_errors=True)


def fixture(target: str) -> dict:
    meta = json.loads((FIXTURES / f"metadata-{target}.json").read_text(encoding="utf-8"))
    for p in meta["packages"]:
        if p["source"] is None:
            continue
        folder = CRATES / f"{p['name']}-{p['version']}"
        folder.mkdir(exist_ok=True)
        (folder / "LICENSE").write_text(f"licence of {p['name']}\n")
        p["manifest_path"] = str(folder / "Cargo.toml")
    return meta


def recorded(tmp: str, *targets: str) -> list[str]:
    """`--metadata` arguments for the fixtures, written with their folders."""
    out = []
    for t in targets:
        path = Path(tmp) / f"metadata-{t}.json"
        path.write_text(json.dumps(fixture(t)))
        out.append(f"{t}={path}")
    return out


def metas() -> dict[str, dict]:
    return {LINUX: fixture(LINUX), WINDOWS: fixture(WINDOWS)}


def package(meta: dict, name: str) -> dict:
    return next(p for p in meta["packages"] if p["name"] == name)


class AllowList(unittest.TestCase):
    def test_matches_the_owner_decision(self):
        data = json.loads((licenses.LICENSES / "allow.json").read_text(encoding="utf-8"))
        self.assertEqual(set(data["allowed"]), OWNER_ALLOWED)
        for refused in ("GPL-2.0-only", "SSPL-1.0"):
            self.assertIn(refused, data["refused"])
        self.assertFalse(set(data["allowed"]) & set(data["refused"]))


class Spdx(unittest.TestCase):
    def ok(self, expr: str) -> None:
        verdict = POLICY.evaluate(expr)
        self.assertTrue(verdict.ok, f"{expr!r}: {verdict.reason}")

    def fails(self, expr: str, mentions: str | None = None) -> None:
        verdict = POLICY.evaluate(expr)
        self.assertFalse(verdict.ok, expr)
        if mentions:
            self.assertIn(mentions, verdict.reason)

    def test_single_ids(self):
        self.ok("MIT")
        self.ok("mit")  # SPDX ids match without regard to case
        self.ok("AGPL-3.0-or-later")
        self.fails("WTFPL", "WTFPL")
        self.fails("LicenseRef-Custom")

    def test_or_passes_if_any_branch_passes(self):
        self.ok("MIT OR Apache-2.0")
        self.ok("GPL-2.0-only OR MIT")
        self.ok("zlib-acknowledgement OR MIT")
        self.ok("MIT or Apache-2.0")  # crates.io has lower-case operators
        self.fails("WTFPL OR GPL-2.0-only", "GPL-2.0-only (refused)")

    def test_and_needs_every_branch(self):
        self.ok("MIT AND BSD-3-Clause")
        self.fails("MIT AND WTFPL", "WTFPL")
        self.fails("MIT AND GPL-2.0-only", "GPL-2.0-only (refused)")

    def test_precedence_and_parentheses(self):
        # AND binds tighter than OR: MIT OR (WTFPL AND Apache-2.0)
        self.ok("MIT OR WTFPL AND Apache-2.0")
        self.fails("(MIT OR WTFPL) AND SSPL-1.0", "SSPL-1.0 (refused)")
        self.ok("(Apache-2.0 OR MIT) AND BSD-3-Clause")
        self.ok("((MIT))")
        self.fails("(MIT OR Apache-2.0", "parentheses")
        self.fails("MIT OR Apache-2.0)", "unexpected")
        self.fails("MIT OR", "end")
        self.fails("AND MIT")
        self.fails("")

    def test_with_passes_when_its_base_does(self):
        self.ok("Apache-2.0 WITH LLVM-exception")
        self.ok("Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT")
        self.fails("GPL-2.0-only WITH Classpath-exception-2.0", "refused")
        self.fails("MIT WITH", "exception")

    def test_slash_form_reads_as_or(self):
        self.ok("MIT/Apache-2.0")
        self.ok("Apache-2.0 / MIT / MPL-2.0")
        self.ok("Unlicense/MIT")
        self.fails("WTFPL/SSPL-1.0", "SSPL-1.0 (refused)")

    def test_deprecated_ids(self):
        self.ok("GPL-3.0")
        self.ok("GPL-3.0+")
        self.ok("AGPL-3.0")
        self.fails("GPL-2.0", "GPL-2.0 (read as GPL-2.0-only)")
        self.fails("GPL-2.0", "refused")
        self.fails("LGPL-2.1", "LGPL-2.1-only")

    def test_gpl_2_only_refused(self):
        self.fails("GPL-2.0-only", "GPL-2.0-only (refused)")

    def test_parse_tree(self):
        self.assertEqual(
            licenses.parse("A OR B AND (C WITH X)"),
            ("or", [("id", "A"), ("and", [("id", "B"), ("with", "C", "X")])]),
        )


class Generate(unittest.TestCase):
    def run_generate(self, ms=None, overrides=None):
        ov = {"missing": [], "texts": [], "notices": []} if overrides is None else overrides
        return licenses.generate(ms or metas(), POLICY, ov)

    def crates(self, doc) -> dict[str, dict]:
        return {c["name"]: c for c in doc["crates"]}

    def test_walks_normal_edges_only(self):
        doc, problems = self.run_generate()
        self.assertEqual(problems, [])
        names = set(self.crates(doc))
        self.assertIn("cfg-if", names)
        self.assertIn("memchr", names)  # through the path dependency
        self.assertNotIn("itoa", names)  # dev-dependency
        self.assertNotIn("autocfg", names)  # build-dependency

    def test_filter_platform_drops_a_platform_only_crate(self):
        doc, _ = self.run_generate()
        crates = self.crates(doc)
        self.assertEqual(crates["winapi"]["targets"], [WINDOWS])
        self.assertEqual(crates["cfg-if"]["targets"], [LINUX, WINDOWS])
        linux_only, _ = self.run_generate({LINUX: fixture(LINUX)})
        self.assertNotIn("winapi", self.crates(linux_only))
        # The fixture records it: the package is known, but no Linux edge reaches it.
        self.assertNotIn("winapi", {p["name"] for p in fixture(LINUX)["packages"]})

    def test_proc_macros_count_as_shipped(self):
        doc, _ = self.run_generate()
        crates = self.crates(doc)
        self.assertTrue(crates["rustversion"]["proc_macro"])
        self.assertNotIn("proc_macro", crates["cfg-if"])

    def test_workspace_crates_are_lodestars_own(self):
        doc, _ = self.run_generate()
        self.assertEqual(doc["lodestar"]["crates"], ["lodestar", "lodestar-helper"])
        self.assertEqual(doc["lodestar"]["license"], "AGPL-3.0-or-later")
        self.assertNotIn("lodestar", self.crates(doc))

    def test_no_licence_fails_unless_overridden(self):
        ms = metas()
        for m in ms.values():
            package(m, "cfg-if")["license"] = None
        _, problems = self.run_generate(ms)
        self.assertEqual(len(problems), 1)
        self.assertIn("cfg-if 1.0.5: no licence field", problems[0])

        override = {"ecosystem": "cargo", "name": "cfg-if", "version": "1.0.5", "license": "MIT"}
        doc, problems = self.run_generate(ms, {"missing": [override]})
        self.assertEqual(problems, [])
        self.assertEqual(self.crates(doc)["cfg-if"]["license"], "MIT")
        self.assertEqual(self.crates(doc)["cfg-if"]["license_source"], "override")

        stale = {**override, "version": "1.0.4"}
        _, problems = self.run_generate(ms, {"missing": [stale]})
        self.assertTrue(any("reviewed for 1.0.4" in p for p in problems), problems)

    def test_no_licence_text_fails_unless_a_reviewed_file_supplies_it(self):
        ms = metas()
        for m in ms.values():
            package(m, "cfg-if")["manifest_path"] = "/nonexistent/cfg-if/Cargo.toml"
        _, problems = self.run_generate(ms)
        self.assertEqual(len(problems), 1)
        self.assertIn("cfg-if 1.0.5: ships no licence text", problems[0])

        with tempfile.TemporaryDirectory() as tmp:
            text = Path(tmp) / "web" / "licenses" / "texts" / "cfg-if-MIT.txt"
            text.parent.mkdir(parents=True)
            text.write_text("MIT License\r\n\r\nCopyright (c) someone\r\n")
            entry = {
                "ecosystem": "cargo",
                "name": "cfg-if",
                "version": "1.0.5",
                "file": "licenses/texts/cfg-if-MIT.txt",
            }
            ov = {"texts": [entry]}
            doc, problems = licenses.generate(ms, POLICY, ov, root=Path(tmp))
            self.assertEqual(problems, [])
            crate = self.crates(doc)["cfg-if"]
            h = licenses.sha256("MIT License\n\nCopyright (c) someone\n")
            self.assertEqual(crate["texts"], {"cfg-if-MIT.txt": h})
            self.assertEqual(crate["texts_from"], "web/licenses/texts/cfg-if-MIT.txt")
            self.assertIn(h, doc["texts"])

            missing = {"texts": [{**entry, "file": "licenses/texts/nope.txt"}]}
            _, problems = licenses.generate(ms, POLICY, missing, root=Path(tmp))
            self.assertIn("file web/licenses/texts/nope.txt not found", problems[0])

            stale = {"texts": [{**entry, "version": "1.0.4"}]}
            _, problems = licenses.generate(ms, POLICY, stale, root=Path(tmp))
            self.assertTrue(any("reviewed for 1.0.4" in p for p in problems), problems)

        # A crate that ships its own text must not keep an override.
        _, problems = self.run_generate(overrides={"texts": [entry]})
        self.assertTrue(any("now ships its own licence text" in p for p in problems), problems)

    def test_override_never_replaces_a_licence_field(self):
        override = {"ecosystem": "cargo", "name": "cfg-if", "license": "MIT"}
        _, problems = self.run_generate(overrides={"missing": [override]})
        self.assertTrue(any("no longer needed" in p for p in problems), problems)

    def test_unused_override_warns(self):
        override = {"ecosystem": "cargo", "name": "gone", "license": "MIT"}
        _, problems = self.run_generate(overrides={"missing": [override]})
        self.assertEqual(problems, [])
        self.assertEqual(
            problems.warnings, ["overrides.json `missing` names gone, which no target builds"]
        )

    def test_disallowed_licence_names_crate_and_licence(self):
        ms = metas()
        for m in ms.values():
            package(m, "memchr")["license"] = "GPL-2.0-only"
        _, problems = self.run_generate(ms)
        self.assertEqual(
            problems, ["memchr 2.8.3: licence 'GPL-2.0-only' not allowed: GPL-2.0-only (refused)"]
        )

    def test_texts_are_read_and_deduplicated(self):
        with tempfile.TemporaryDirectory() as tmp:
            ms = metas()
            mit = "MIT License\n\nCopyright (c) someone\n"
            for i, name in enumerate(["cfg-if", "memchr"]):
                folder = Path(tmp) / name
                folder.mkdir()
                (folder / "LICENSE-MIT").write_text(mit)
                (folder / "license.rs").write_text("// not a licence")
                if i:
                    (folder / "COPYING").write_text("other\r\n")
                for m in ms.values():
                    package(m, name)["manifest_path"] = str(folder / "Cargo.toml")
            doc, _ = self.run_generate(ms)
        crates = self.crates(doc)
        h = licenses.sha256(mit)
        self.assertEqual(crates["cfg-if"]["texts"], {"LICENSE-MIT": h})
        self.assertEqual(
            crates["memchr"]["texts"], {"COPYING": licenses.sha256("other\n"), "LICENSE-MIT": h}
        )
        self.assertEqual(doc["texts"][h], mit)
        refs = {h for n in ("cfg-if", "memchr") for h in crates[n]["texts"].values()}
        self.assertEqual(len(refs), 2)  # the shared MIT text is stored once
        self.assertEqual(set(doc["lodestar"]["texts"]), {"LICENSE"})

    def test_deterministic(self):
        a, _ = self.run_generate()
        b, _ = self.run_generate(dict(reversed(list(metas().items()))))
        b["targets"] = a["targets"]
        self.assertEqual(licenses.dumps(a), licenses.dumps(b))


class Output(unittest.TestCase):
    def test_prettier_layout(self):
        short = {"a": ["x", "y"], "b": {}, "c": []}
        self.assertEqual(
            licenses.dumps(short), '{\n\t"a": ["x", "y"],\n\t"b": {},\n\t"c": []\n}\n'
        )
        long = {"k": ["x" * 50, "y" * 50]}
        broken = '{\n\t"k": [\n\t\t"' + "x" * 50 + '",\n\t\t"' + "y" * 50 + '"\n\t]\n}\n'
        self.assertEqual(licenses.dumps(long), broken)
        self.assertEqual(licenses.dumps({"t": "é\n"}), '{\n\t"t": "é\\n"\n}\n')


class Cli(unittest.TestCase):
    def main(self, *args: str) -> tuple[int, str]:
        err = io.StringIO()
        with redirect_stderr(err), redirect_stdout(io.StringIO()):
            code = licenses.main(list(args))
        return code, err.getvalue()

    def test_check_fails_on_drift(self):
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "rust.json"
            ov = Path(tmp) / "overrides.json"
            ov.write_text('{"missing": [], "texts": [], "notices": []}')
            metadata = recorded(tmp, LINUX, WINDOWS)
            common = ["--out", str(out), "--overrides", str(ov), "--metadata", *metadata]
            self.assertEqual(self.main(*common)[0], 0)
            self.assertEqual(self.main("--check", *common)[0], 0)
            out.write_text(out.read_text().replace("1.0.5", "1.0.6"))
            code, err = self.main("--check", *common)
            self.assertEqual(code, 1)
            self.assertIn("is out of date", err)

    def test_check_fails_on_a_disallowed_licence(self):
        with tempfile.TemporaryDirectory() as tmp:
            meta = copy.deepcopy(fixture(LINUX))
            package(meta, "cfg-if")["license"] = "SSPL-1.0"
            path = Path(tmp) / "m.json"
            path.write_text(json.dumps(meta))
            ov = Path(tmp) / "overrides.json"
            ov.write_text("{}")
            out = Path(tmp) / "rust.json"
            code, err = self.main(
                "--out", str(out), "--overrides", str(ov), "--metadata", f"{LINUX}={path}"
            )
            self.assertEqual(code, 1)
            self.assertIn("cfg-if 1.0.5: licence 'SSPL-1.0' not allowed: SSPL-1.0 (refused)", err)


if __name__ == "__main__":
    unittest.main()
