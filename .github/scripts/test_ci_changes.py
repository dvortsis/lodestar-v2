#!/usr/bin/env python3
"""Tests for the web / e2e / infra applicability rules in ci_changes.py.

Run: python3 .github/scripts/test_ci_changes.py
"""

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import ci_changes  # noqa: E402


def applies(path: str) -> tuple[bool, bool]:
    return bool(ci_changes.WEB.match(path)), bool(ci_changes.E2E.match(path))


class Applicability(unittest.TestCase):
    def test_docs_only_skips_both(self):
        for f in ["README.md", "docs/architecture.md", "docs/decisions/x.md", "runner/README.md", ".github/workflows/ci.yml"]:
            self.assertEqual(applies(f), (False, False), f)

    def test_rust_only_skips_web_but_api_runs_e2e(self):
        self.assertEqual(applies("crates/dht/src/lib.rs"), (False, False))
        self.assertEqual(applies("crates/api/src/lib.rs"), (False, True))

    def test_web_inputs_apply_to_both(self):
        for f in ["web/src/app.css", "web/src/lib/api/openapi.json", "brand/source/logo.svg",
                  "crates/lodestar/src/settings/golden/settings_index.txt", "crates/search/src/query.rs",
                  "docs/design-tokens.md", "docs/themes.md", ".github/scripts/ci_changes.py"]:
            self.assertEqual(applies(f), (True, True), f)

    def test_each_workflow_applies_to_itself_only(self):
        self.assertEqual(applies(".github/workflows/web.yml"), (True, False))
        self.assertEqual(applies(".github/workflows/e2e.yml"), (False, True))


class Infra(unittest.TestCase):
    def test_only_infra_paths_match(self):
        for f in ["infra/aws/expiration-guard/deployed/index.py", "infra/akamai/terraform/main.tf",
                  "infra/akamai/terraform/scripts/Deploy.ps1"]:
            self.assertTrue(ci_changes.INFRA.match(f), f)
        for f in ["docs/operations/lodestar-cloud-assets.md", ".agents/skills/lodestar-fleet/SKILL.md",
                  "crates/infra/src/lib.rs", "web/src/infra.ts"]:
            self.assertFalse(ci_changes.INFRA.match(f), f)


class UpdateManifest(unittest.TestCase):
    def test_the_builder_release_yml_and_the_verifier_apply(self):
        for f in [".github/scripts/update_manifest.py", ".github/scripts/test_update_manifest.py",
                  ".github/workflows/release.yml", "crates/update/src/doc.rs", "crates/update/examples/verify_envelope.rs"]:
            self.assertTrue(ci_changes.UPDATE_MANIFEST.match(f), f)

    def test_other_paths_dont(self):
        for f in ["crates/updater/x.rs", "crates/lodestar/src/update/engine.rs", ".github/workflows/freshness.yml",
                  "docs/decisions/2026-11-08-PK-manifest.md", ".github/scripts/update_manifest.pyc"]:
            self.assertFalse(ci_changes.UPDATE_MANIFEST.match(f), f)


class EndToEnd(unittest.TestCase):
    def test_docs_only_pr_reports_web_and_e2e_false(self):
        with tempfile.TemporaryDirectory() as tmp:
            env = {**os.environ, "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_NOSYSTEM": "1", "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@t",
                   "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@t"}

            def git(*a):
                return subprocess.run(["git", "-C", tmp, *a], check=True, capture_output=True, text=True, env=env).stdout.strip()

            git("init", "-q")
            git("commit", "-q", "--allow-empty", "-m", "base")
            base = git("rev-parse", "HEAD")
            (Path(tmp) / "docs").mkdir()
            (Path(tmp) / "docs/x.md").write_text("x")
            git("add", ".")
            git("commit", "-q", "-m", "docs")
            head = git("rev-parse", "HEAD")
            code = (
                f"import sys, pathlib; sys.path.insert(0, {str(HERE)!r}); import ci_changes; "
                f"ci_changes.ROOT = pathlib.Path({tmp!r}); ci_changes.packages = lambda: {{}}; "
                "ci_changes.FUZZ = []; ci_changes.main()"
            )
            out = subprocess.run([sys.executable, "-c", code, "pull_request", base, head, "[]"],
                                 check=True, capture_output=True, text=True, cwd=tmp).stdout
            kv = dict(line.split("=", 1) for line in out.splitlines())
            self.assertEqual((kv["web"], kv["e2e"]), ("false", "false"))
            self.assertEqual(kv["rust"], "false")
            self.assertEqual(kv["infra"], "false")
            self.assertEqual(kv["update_manifest"], "false")

    def test_crash_exits_nonzero(self):
        # A bad base must fail the script (the workflow step then fails, not an empty output).
        r = subprocess.run([sys.executable, str(HERE / "ci_changes.py"), "pull_request", "0" * 40, "0" * 40, "[]"],
                           capture_output=True, text=True)
        self.assertNotEqual(r.returncode, 0)
        self.assertNotIn("web=", r.stdout)

    def test_non_pr_events_run_everything(self):
        out = subprocess.run([sys.executable, str(HERE / "ci_changes.py"), "push", "a", "b"],
                             check=True, capture_output=True, text=True).stdout
        self.assertIn("web=true", out)
        self.assertIn("e2e=true", out)
        self.assertIn("mmap_retention=true", out)
        self.assertIn("infra=true", out)
        self.assertIn("update_manifest=true", out)


if __name__ == "__main__":
    unittest.main()
