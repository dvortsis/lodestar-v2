#!/usr/bin/env python3
"""Tests for test_shards.py and the `test` job's aggregation in ci-ok.

Run: python3 .github/scripts/test_test_shards.py
"""

import copy
import re
import subprocess
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import test_shards  # noqa: E402

CI = HERE.parent / "workflows" / "ci.yml"


def target(name, kind, **extra):
    return {"name": name, "kind": [kind], **extra}


def meta(*packages):
    pkgs = [{"id": f"{n}#0", "name": n, "targets": ts} for n, ts in packages]
    return {"packages": pkgs, "workspace_members": [p["id"] for p in pkgs]}


def workspace():
    return meta(
        ("lodestar", [
            target("lodestar", "bin"),
            target("build-script-build", "custom-build"),
            *[target(n, "test") for n in test_shards.ROOT_STEP_TESTS],
            target("watch", "test"),
            target("alpha", "test"),
        ]),
        ("lodestar-log", [target("lodestar_log", "lib"), target("watch", "test"), target("beta", "test")]),
        ("lodestar-bench", [
            target("lodestar_bench", "lib"),
            target("dataset", "test"),
            target("mem-cheap", "example", test=False),
            target("search", "bench", test=False),
        ]),
    )


def names(args):
    return {a.removeprefix("--test=") for a in args if a.startswith("--test=")}


class Plan(unittest.TestCase):
    def test_every_test_target_runs_on_exactly_one_shard(self):
        p = test_shards.plan(workspace())
        seen = [n for s in test_shards.SHARDS for line in p[s] for n in names(line)]
        self.assertEqual(sorted(seen), sorted({"cli", "privdrop", "supervise", "worker_join", "watch", "alpha", "beta", "dataset"}))
        self.assertEqual(len(seen), len(set(seen)))

    def test_shard_lines(self):
        p = test_shards.plan(workspace())
        self.assertEqual(p["lib"], [["--lib"]])
        self.assertEqual(p["bins"], [["--bins", *[f"--test={n}" for n in test_shards.ROOT_STEP_TESTS]]])
        # Doc tests can't share an invocation with other target filters.
        self.assertIn(["--doc"], p["tests-a"])
        self.assertNotIn(["--doc"], p["tests-b"])
        for s in test_shards.SPLIT:
            self.assertTrue(any(names(line) for line in p[s]), s)

    def test_split_balances_by_weight_and_is_stable(self):
        p = test_shards.plan(workspace())
        a = names(p["tests-a"][0])
        b = names(p["tests-b"][0])
        # watch counts once per package (2 x 90), so it outweighs dataset (142).
        self.assertEqual(a, {"watch"})
        self.assertEqual(b, {"dataset", "alpha", "beta"})
        shuffled = workspace()
        for pkg in shuffled["packages"]:
            pkg["targets"].reverse()
        shuffled["packages"].reverse()
        self.assertEqual(test_shards.plan(shuffled), p)

    def test_a_new_test_file_lands_on_a_shard(self):
        m = workspace()
        m["packages"][1]["targets"].append(target("brand_new", "test"))
        p = test_shards.plan(m)
        self.assertIn("brand_new", names(p["tests-a"][0]) | names(p["tests-b"][0]))

    def test_non_members_are_ignored(self):
        m = workspace()
        m["packages"].append({"id": "dep#1", "name": "dep", "targets": [target("odd", "bench", test=True)]})
        test_shards.plan(m)

    def test_uncovered_kinds_fail(self):
        cases = {
            "bench test": target("b", "bench", test=True),
            "example test": target("e", "example", test=True),
            "test off": target("t", "test", test=False),
            "bin test off": target("x", "bin", test=False),
            "unknown kind": target("u", "weird"),
            "odd name": target("a b", "test"),
        }
        for label, t in cases.items():
            with self.subTest(label):
                m = copy.deepcopy(workspace())
                m["packages"][0]["targets"].append(t)
                with self.assertRaises(test_shards.ShardError):
                    test_shards.plan(m)

    def test_a_lost_root_step_test_fails(self):
        m = workspace()
        m["packages"][0]["targets"] = [t for t in m["packages"][0]["targets"] if t["name"] != "privdrop"]
        with self.assertRaisesRegex(test_shards.ShardError, "privdrop"):
            test_shards.plan(m)

    def test_cli_rejects_unknown_shards(self):
        self.assertEqual(test_shards.main(["test_shards.py", "nope"]), 2)
        self.assertEqual(test_shards.main(["test_shards.py"]), 2)


class Workflow(unittest.TestCase):
    """ci.yml's `test` job and ci-ok agree with the script (stdlib only: no YAML parser)."""

    def setUp(self):
        self.text = CI.read_text()

    def job(self, name):
        m = re.search(rf"^  {re.escape(name)}:\n(.*?)(?=^  [a-z][a-z0-9_-]*:\n|\Z)", self.text, re.M | re.S)
        self.assertIsNotNone(m, name)
        return m.group(1)

    def test_matrix_is_literal_and_lists_every_shard(self):
        body = self.job("test")
        for line in ("os: [ubuntu-latest, windows-latest, macos-latest]",
                     f"shard: [{', '.join(test_shards.SHARDS)}]",
                     "runs-on: ${{ matrix.os }}", "fail-fast: false"):
            self.assertTrue(line in body, line)

    def test_ci_ok_needs_test_and_fails_on_a_failed_or_cancelled_shard(self):
        body = self.job("ci-ok")
        needs = re.search(r"needs: \[(.*?)\]", body).group(1).split(", ")
        self.assertIn("test", needs)
        script = re.search(r"run: \|\n(.*)", body, re.S).group(1)
        script = "\n".join(line[10:] for line in script.splitlines())

        def run(results):
            return subprocess.run(["bash", "-e", "-c", script], env={"RESULTS": results, "PATH": "/usr/bin:/bin"},
                                  capture_output=True, text=True).returncode

        # needs.test.result is the matrix's aggregate: one failed or cancelled
        # shard makes it "failure" or "cancelled".
        self.assertNotEqual(run('["success", "failure", "success"]'), 0)
        self.assertNotEqual(run('["success", "cancelled"]'), 0)
        self.assertEqual(run('["success", "skipped", "success"]'), 0)


if __name__ == "__main__":
    unittest.main()
