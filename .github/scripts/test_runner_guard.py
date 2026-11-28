#!/usr/bin/env python3
"""Tests for runner_guard.py (M8-PERF-3).

Run: python3 .github/scripts/test_runner_guard.py
"""

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import runner_guard as g  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
REF_IF = g.REF_IF

BENCH_OK = f"""\
name: Bench 10M
on:
  schedule:
    - cron: "17 3 * * 1"
  push:
    branches: ["release/**"]
  workflow_dispatch:
    inputs:
      smoke_only:
        type: boolean
        default: false
permissions: {{}}
jobs:
  bench:
    if: {REF_IF}
    runs-on: lodestar-bench-10m
    permissions:
      contents: read
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - run: echo "$SMOKE"
        env:
          SMOKE: ${{{{ inputs.smoke_only }}}}
"""


def job(runs_on, extra=""):
    return f"name: x\non: push\njobs:\n  a:\n{extra}    runs-on: {runs_on}\n    steps: []\n"


class OtherWorkflows(unittest.TestCase):
    def bad(self, text, name="x.yml"):
        problems = g.check_workflow(name, text)
        self.assertTrue(problems, f"expected a problem in:\n{text}")
        return problems

    def ok(self, text, name="x.yml"):
        self.assertEqual(g.check_workflow(name, text), [])

    def test_hosted_images_pass(self):
        for ro in ["ubuntu-latest", "windows-latest", "macos-latest", "ubuntu-24.04", "macos-14", "[ubuntu-latest]"]:
            with self.subTest(ro=ro):
                self.ok(job(ro))

    def test_label_as_a_string(self):
        for ro in ["lodestar-bench-10m", "LODESTAR-Bench-10M", '"lodestar-bench-10m"']:
            with self.subTest(ro=ro):
                self.bad(job(ro))

    def test_label_in_a_list(self):
        self.bad(job("[self-hosted, lodestar-bench-10m]"))
        self.bad(job("[lodestar-bench-10m]"))
        self.bad(job("\n      - linux\n      - lodestar-bench-10m"))

    def test_label_in_a_group_or_labels_mapping(self):
        self.bad(job("\n      group: default\n      labels: [lodestar-bench-10m]"))
        self.bad(job("\n      labels: lodestar-bench-10m"))
        # A mapping is refused even without the label: groups are self-hosted or larger runners.
        self.bad(job("\n      group: bench"))

    def test_label_in_an_expression(self):
        self.bad(job("${{ 'lodestar-bench-10m' }}"))
        self.bad(job("${{ github.ref == 'refs/heads/main' && 'lodestar-bench-10m' || 'ubuntu-latest' }}"))

    def test_label_built_by_an_expression_without_its_text(self):
        for ro in ["${{ format('lodestar-bench-{0}', '10m') }}", "${{ inputs.runner }}", "${{ vars.RUNNER }}",
                   "${{ github.event.inputs.runner }}", "${{ fromJSON(needs.a.outputs.os) }}",
                   "${{ matrix.os == 'x' && 'y' || 'z' }}"]:
            with self.subTest(ro=ro):
                self.bad(job(ro))

    def test_label_as_a_matrix_value(self):
        strat = "    strategy:\n      matrix:\n        os: [ubuntu-latest, lodestar-bench-10m]\n"
        self.bad(job("${{ matrix.os }}", strat))
        strat = "    strategy:\n      matrix:\n        os: [ubuntu-latest]\n        include:\n          - os: lodestar-bench-10m\n"
        self.bad(job("${{ matrix.os }}", strat))

    def test_matrix_of_hosted_images_passes(self):
        strat = "    strategy:\n      matrix:\n        os: [ubuntu-latest, windows-latest]\n"
        self.ok(job("${{ matrix.os }}", strat))
        strat = "    strategy:\n      matrix:\n        os: [ubuntu-latest]\n        include:\n          - os: macos-latest\n"
        self.ok(job("${{ matrix.os }}", strat))

    def test_dynamic_or_missing_matrix_fails(self):
        self.bad(job("${{ matrix.os }}", "    strategy:\n      matrix: ${{ fromJSON(needs.a.outputs.m) }}\n"))
        self.bad(job("${{ matrix.os }}", "    strategy:\n      matrix:\n        os: ${{ fromJSON(needs.a.outputs.o) }}\n"))
        self.bad(job("${{ matrix.os }}"))
        self.bad(job("${{ matrix.os }}", "    strategy:\n      matrix:\n        os: ['${{ inputs.os }}']\n"))

    def test_only_the_named_dynamic_matrix_is_allowed(self):
        text = ("name: x\non: push\njobs:\n  test:\n    strategy:\n      matrix:\n"
                "        os: ${{ fromJSON(needs.changes.outputs.test_os) }}\n    runs-on: ${{ matrix.os }}\n    steps: []\n")
        self.ok(text, "ci.yml")
        self.bad(text, "other.yml")

    def test_the_allowed_dynamic_matrix_still_checks_include(self):
        base = ("name: x\non: push\njobs:\n  test:\n    strategy:\n      matrix:\n"
                "        os: ${{ fromJSON(needs.changes.outputs.test_os) }}\n{inc}    runs-on: ${{ matrix.os }}\n    steps: []\n")
        for inc in ["        include:\n          - os: ${{ format('lodestar-bench-{0}', '10m') }}\n",
                    '        include:\n          - os: "lodestar\\x2dbench-10m"\n',
                    "        include:\n          - os: self-hosted\n",
                    "        include: ${{ fromJSON(needs.changes.outputs.inc) }}\n"]:
            with self.subTest(inc=inc):
                self.bad(base.replace("{inc}", inc), "ci.yml")
        self.ok(base.replace("{inc}", "        include:\n          - os: macos-latest\n"), "ci.yml")

    def test_only_the_exact_dynamic_expression_is_allowed(self):
        for expr in ["${{ fromJSON(needs.other.outputs.test_os) }}", "${{ fromJSON(needs.changes.outputs.os) }}",
                     "${{ fromJSON(inputs.os) }}"]:
            with self.subTest(expr=expr):
                text = (f"name: x\non: push\njobs:\n  test:\n    strategy:\n      matrix:\n        os: {expr}\n"
                        "    runs-on: ${{ matrix.os }}\n    steps: []\n")
                self.bad(text, "ci.yml")

    def test_an_escaped_label_is_caught(self):
        self.bad(job('"lodestar\\x2dbench-10m"'))
        self.bad("name: x\non: push\nenv:\n  R: \"LODESTAR\\u002dBENCH-10M\"\njobs:\n  a:\n    runs-on: ubuntu-latest\n"
                 "    steps: []\n")

    def test_self_hosted_without_the_label(self):
        self.bad(job("self-hosted"))
        self.bad(job("[self-hosted, linux, x64]"))
        self.bad(job("${{ matrix.os }}", "    strategy:\n      matrix:\n        os: [self-hosted]\n"))

    def test_unknown_runner_names_fail(self):
        for ro in ["linux", "x64", "bench", "ubuntu-latest-bench"]:
            with self.subTest(ro=ro):
                self.bad(job(ro))

    def test_label_anywhere_else_in_the_file(self):
        self.bad("name: x\non: push\n# lodestar-bench-10m\njobs:\n  a:\n    runs-on: ubuntu-latest\n    steps: []\n")
        self.bad("name: x\non: push\njobs:\n  a:\n    runs-on: ubuntu-latest\n    env:\n      R: lodestar-bench-10m\n"
                 "    steps: []\n")

    def test_label_on_a_pull_request_job(self):
        for event in ["pull_request", "pull_request_target", "[push, pull_request]",
                      "\n  pull_request_target:\n    types: [opened]"]:
            with self.subTest(event=event):
                text = f"name: x\non: {event}\njobs:\n  a:\n    runs-on: lodestar-bench-10m\n    steps: []\n"
                self.bad(text)

    def test_reusable_workflows(self):
        self.ok("name: x\non: push\njobs:\n  a:\n    uses: ./.github/workflows/y.yml\n")
        self.bad("name: x\non: push\njobs:\n  a:\n    uses: someone/repo/.github/workflows/y.yml@main\n")

    def test_a_file_that_does_not_parse_fails(self):
        self.bad("name: x\non: [push\n")
        self.bad("- just a list\n")


class BenchWorkflow(unittest.TestCase):
    def problems(self, text):
        return g.check_workflow(g.BENCH, text)

    def test_the_reference_bench_workflow_passes(self):
        self.assertEqual(self.problems(BENCH_OK), [])

    def test_untrusted_triggers_fail(self):
        for event in ["pull_request", "pull_request_target", "workflow_run", "issue_comment", "pull_request_review",
                      "pull_request_review_comment", "discussion_comment", "repository_dispatch", "workflow_call",
                      "merge_group"]:
            with self.subTest(event=event):
                text = BENCH_OK.replace("on:\n", f"on:\n  {event}:\n", 1)
                self.assertTrue(any(repr(event) in p for p in self.problems(text)), self.problems(text))

    def test_on_as_a_string_or_list(self):
        self.assertTrue(self.problems(BENCH_OK.replace("on:\n", "on: pull_request\nx:\n", 1)))
        text = BENCH_OK.replace("on:\n  schedule:", "on: [push, pull_request]\nold:\n  schedule:", 1)
        self.assertTrue(self.problems(text))

    def test_push_only_to_release(self):
        for branches in ['["main"]', '["**"]', '["release/**", "feature/**"]', '["release/*"]']:
            with self.subTest(branches=branches):
                self.assertTrue(self.problems(BENCH_OK.replace('["release/**"]', branches)))
        text = BENCH_OK.replace('    branches: ["release/**"]', '    branches: ["release/**"]\n    tags: ["*"]')
        self.assertTrue(self.problems(text))

    def test_dispatch_inputs(self):
        text = BENCH_OK.replace("      smoke_only:", "      ref:\n        type: string\n      smoke_only:")
        self.assertTrue(self.problems(text))
        self.assertTrue(self.problems(BENCH_OK.replace("type: boolean", "type: string")))

    def test_permissions(self):
        self.assertTrue(self.problems(BENCH_OK.replace("permissions: {}", "permissions: write-all")))
        self.assertTrue(self.problems(BENCH_OK.replace("permissions: {}\n", "")))
        self.assertTrue(self.problems(BENCH_OK.replace("contents: read", "contents: write")))
        self.assertTrue(self.problems(BENCH_OK.replace("contents: read", "contents: read\n      actions: write")))

    def test_secrets_and_vars(self):
        for ref in ["${{ secrets.TOKEN }}", "${{ secrets['TOKEN'] }}", "${{ vars.X }}", "${{ toJSON(secrets) }}",
                    "${{ toJSON(vars) }}", "${{ github.token }}", "${{ SECRETS.X }}"]:
            with self.subTest(ref=ref):
                self.assertTrue(self.problems(BENCH_OK.replace("${{ inputs.smoke_only }}", ref)))

    def test_no_expression_inside_run(self):
        text = BENCH_OK.replace('run: echo "$SMOKE"', 'run: echo "${{ github.event.head_commit.message }}"')
        self.assertTrue(self.problems(text))

    def test_ref_check_on_the_labelled_job(self):
        self.assertTrue(self.problems(BENCH_OK.replace(f"    if: {REF_IF}\n", "")))
        self.assertTrue(self.problems(BENCH_OK.replace(f"if: {REF_IF}", "if: true")))

    def test_runs_on_exactly_the_label(self):
        for ro in ["[self-hosted, lodestar-bench-10m]", "${{ 'lodestar-bench-10m' }}", "self-hosted"]:
            with self.subTest(ro=ro):
                self.assertTrue(self.problems(BENCH_OK.replace("runs-on: lodestar-bench-10m", f"runs-on: {ro}")))
        # A hosted helper job (a summary) is fine.
        self.assertEqual(self.problems(BENCH_OK.replace(
            "runs-on: lodestar-bench-10m", "runs-on: ubuntu-latest").replace(f"    if: {REF_IF}\n", "")), [])

    def test_pinned_actions_and_no_credentials(self):
        self.assertTrue(self.problems(BENCH_OK.replace(
            "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1", "actions/checkout@v4")))
        self.assertTrue(self.problems(BENCH_OK.replace("persist-credentials: false", "persist-credentials: true")))
        self.assertTrue(self.problems(BENCH_OK.replace(
            "        with:\n          persist-credentials: false\n", "")))

    def test_no_container_environment_or_reusable_call(self):
        for extra in ["    environment: bench\n", "    container: ubuntu:24.04\n", "    services:\n      db:\n        image: x\n"]:
            with self.subTest(extra=extra):
                self.assertTrue(self.problems(BENCH_OK.replace("    runs-on:", extra + "    runs-on:")))
        text = BENCH_OK + "  other:\n    uses: ./.github/workflows/ci.yml\n"
        self.assertTrue(self.problems(text))


class Repo(unittest.TestCase):
    def test_the_repository_is_clean(self):
        self.assertEqual(g.check(ROOT), [])

    def test_every_other_workflow_passes_one_by_one(self):
        # main's runs-on expressions (ci.yml's dynamic matrix.os, companions-smoke's
        # literal one) must not trip the expression rule.
        names = sorted(p.name for p in (ROOT / ".github/workflows").iterdir() if p.suffix in {".yml", ".yaml"})
        self.assertIn("ci.yml", names)
        self.assertIn("companions-smoke.yml", names)
        for name in names:
            with self.subTest(workflow=name):
                text = (ROOT / ".github/workflows" / name).read_text(encoding="utf-8")
                self.assertEqual(g.check_workflow(name, text), [])

    def test_the_real_bench_workflow_runs_on_the_label(self):
        text = (ROOT / ".github/workflows" / g.BENCH).read_text(encoding="utf-8")
        self.assertIn(f"runs-on: {g.LABEL}", text)

    def tree(self, files):
        d = tempfile.TemporaryDirectory()
        self.addCleanup(d.cleanup)
        root = Path(d.name)
        for rel, body in files.items():
            (root / rel).parent.mkdir(parents=True, exist_ok=True)
            (root / rel).write_text(body, encoding="utf-8")
        return root

    def test_a_missing_bench_workflow_or_a_yaml_file_is_caught(self):
        root = self.tree({".github/workflows/a.yaml": job("lodestar-bench-10m")})
        problems = g.check(root)
        self.assertTrue(any("missing" in p for p in problems))
        self.assertTrue(any(p.startswith("a.yaml") for p in problems))

    def test_upper_case_suffixes_and_other_files_in_workflows_are_checked(self):
        root = self.tree({".github/workflows/bench-10m.yml": BENCH_OK,
                          ".github/workflows/X.YML": job("self-hosted"),
                          ".github/workflows/notes.txt": "lodestar-bench-10m\n"})
        problems = g.check(root)
        self.assertTrue(any(p.startswith("X.YML") for p in problems), problems)
        self.assertTrue(any(p.startswith(".github/workflows/notes.txt") for p in problems), problems)

    def test_a_script_naming_the_label_is_caught(self):
        root = self.tree({".github/workflows/bench-10m.yml": BENCH_OK,
                          ".github/scripts/os.py": 'print(\'["lodestar-bench-10m"]\')\n',
                          ".github/actions/x/action.yml": "runs:\n  using: composite\n"})
        self.assertEqual(g.check(root), [".github/scripts/os.py: names the bench runner's label 'lodestar-bench-10m'"])

    def test_main_exit_status(self):
        root = self.tree({".github/workflows/bench-10m.yml": BENCH_OK})
        self.assertEqual(g.main(["x", str(root)]), 0)
        (root / ".github/workflows/b.yml").write_text(job("self-hosted"), encoding="utf-8")
        self.assertEqual(g.main(["x", str(root)]), 1)


if __name__ == "__main__":
    unittest.main()
