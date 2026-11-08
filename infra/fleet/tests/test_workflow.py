"""fleet.yml's structure, and the public-log leak test (plan §2, §3, OP-1 row).

Covers fleet.yml now, and operator-wake.yml and operator-injection.yml as soon
as they exist: every script those workflows run must have a leak scenario
below (OP-4 and OP-5 add theirs), or this test fails.
"""

import io
import os
import re
import secrets
import subprocess
import sys
import tempfile
from unittest import mock
import unittest
from contextlib import redirect_stderr, redirect_stdout
from datetime import timedelta
from pathlib import Path

import yaml

import ancestry
import fleet
import limits
from state import DryRunBackend, new_fleet
from tests.helpers import T0, Clock, FakeGitHub, event_env, make_repo
from timefmt import fmt

ROOT = Path(__file__).resolve().parents[3]
WORKFLOWS = ROOT / ".github" / "workflows"
COVERED = ["fleet.yml", "operator-wake.yml", "operator-injection.yml"]
FLEET = WORKFLOWS / "fleet.yml"
L = limits.load()


def load(path):
    wf = yaml.safe_load(path.read_text(encoding="utf-8"))
    wf["on"] = wf.pop(True, wf.get("on"))  # PyYAML reads the key `on` as True
    return wf


def code_text(path):
    """The workflow without full-line comments, for text checks."""
    lines = path.read_text(encoding="utf-8").splitlines()
    return "\n".join(ln for ln in lines if not ln.lstrip().startswith("#"))


def steps(job):
    return job.get("steps") or []


def covered():
    return [WORKFLOWS / n for n in COVERED if (WORKFLOWS / n).exists()]


class NoReusableWorkflows(unittest.TestCase):
    def test_no_workflow_call_or_reusable_workflow(self):
        self.assertTrue(covered(), "fleet.yml is missing")
        for path in covered():
            text, wf = code_text(path), load(path)
            with self.subTest(workflow=path.name):
                self.assertFalse("workflow_call" in text, "workflow_call appears")
                self.assertNotIn("workflow_call", wf["on"] if isinstance(wf["on"], (dict, list)) else [wf["on"]])
                for name, job in wf["jobs"].items():
                    self.assertNotIn("uses", job, f"{name} calls a reusable workflow")
                    for s in steps(job):
                        self.assertNotIn(".github/workflows/", str(s.get("uses", "")))


class FleetStructure(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.wf = load(FLEET)
        cls.jobs = cls.wf["jobs"]

    def test_triggers(self):
        on = self.wf["on"]
        self.assertEqual(set(on), {"issue_comment", "schedule", "workflow_dispatch"})
        self.assertEqual(on["issue_comment"], {"types": ["created"]})
        self.assertFalse(on["workflow_dispatch"], "workflow_dispatch must take no inputs")

    def test_permissions_minimal(self):
        self.assertEqual(self.wf["permissions"], {})
        allowed = {("contents", "read"), ("pull-requests", "write")}
        for name, job in self.jobs.items():
            with self.subTest(job=name):
                self.assertIsInstance(job.get("permissions"), dict)
                self.assertLessEqual(set(job["permissions"].items()), allowed)
        self.assertEqual(self.jobs["gate"]["permissions"], {"contents": "read"})

    def test_if_before_environment(self):
        for name, job in self.jobs.items():
            if "environment" not in job:
                continue
            with self.subTest(job=name):
                keys = list(job)
                self.assertIn("if", job)
                self.assertLess(keys.index("if"), keys.index("environment"))

    def test_filter_job_has_no_secrets_or_environment(self):
        gate = self.jobs["gate"]
        self.assertNotIn("environment", gate)
        self.assertNotIn("secrets.", yaml.safe_dump(gate))
        self.assertNotIn("github.token", yaml.safe_dump(gate))
        for check in ("github.event.action == 'created'", "vars.CHANNEL_PR",
                      "format('{0}', github.event.comment.user.id) == vars.OPERATOR_BOT_USER_ID"):
            self.assertIn(check, gate["if"])

    def test_untrusted_text_and_secrets_never_in_run_or_with(self):
        for name, job in self.jobs.items():
            for s in steps(job):
                with self.subTest(job=name, step=s.get("name") or s.get("run") or s.get("uses")):
                    self.assertNotIn("${{", s.get("run", ""))
                    self.assertNotIn("secrets.", yaml.safe_dump(s.get("with") or {}))
            self.assertNotIn("secrets.", str(job.get("if", "")))

    def test_timeouts(self):
        for name, job in self.jobs.items():
            with self.subTest(job=name):
                self.assertIsInstance(job.get("timeout-minutes"), int)
                if "environment" in job:  # every job that can hold the fleet lock
                    self.assertLess(job["timeout-minutes"], 90)

    def test_actions_pinned_by_sha(self):
        for name, job in self.jobs.items():
            for s in steps(job):
                if "uses" in s:
                    with self.subTest(job=name, uses=s["uses"]):
                        self.assertRegex(s["uses"], r"^[\w.-]+/[\w.-]+@[0-9a-f]{40}$")

    def test_no_artifacts(self):
        self.assertFalse("artifact" in code_text(FLEET).lower(), "an Actions artifact appears")

    def test_dispatch_can_do_nothing_but_down(self):
        for name, job in self.jobs.items():
            cond = str(job.get("if", ""))
            with self.subTest(job=name):
                if name == "dispatch-down":
                    self.assertTrue(cond.startswith("github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/main'"))
                    self.assertIn("github.actor_id == vars.OWNER_USER_ID", cond)
                    self.assertIn("vars.OWNER_USER_ID != ''", cond)
                    runs = [s["run"] for s in steps(job) if "run" in s]
                    self.assertEqual(runs, ["python3 infra/fleet/fleet.py dispatch-down"])
                    envs = {k for s in steps(job) for k in (s.get("env") or {})}
                    self.assertNotIn("COMMENT_BODY", envs)
                else:
                    self.assertRegex(cond, r"^github\.event_name == '(issue_comment|schedule)' && ")

    def test_stop_never_gates_reaper_or_dispatch(self):
        for name in ("reaper", "dispatch-down"):
            self.assertNotIn("OPERATOR_STOP", yaml.safe_dump(self.jobs[name]))
        # Only disabling the workflow stops the reaper: no variable gates it.
        self.assertEqual(self.jobs["reaper"]["if"], "github.event_name == 'schedule' && github.ref == 'refs/heads/main'")

    def test_act_repeats_the_filter(self):
        cond = self.jobs["act"]["if"]
        for check in ("github.event.action == 'created'", "needs.gate.outputs.allowed == 'true'",
                      "format('{0}', github.event.issue.number) == vars.CHANNEL_PR",
                      "format('{0}', github.event.comment.user.id) == vars.OPERATOR_BOT_USER_ID",
                      "github.ref == 'refs/heads/main'"):
            self.assertIn(check, cond)


# ---- the public-log leak test ----

SECRET_NAMES = [
    "GITHUB_TOKEN", "AWS_FLEET_ROLE_ARN", "AWS_PIA_ROLE_ARN", "AWS_BRIDGE_ROLE_ARN", "FLEET_SSH_KEY",
    "OWNER_ADMIN_CIDR", "STATE_BUCKET", "EVIDENCE_BUCKET", "CONTROL_BUCKET", "PIA_SERVERS",
    "OPERATOR_APP_ID", "OPERATOR_APP_PRIVATE_KEY", "ANTHROPIC_API_KEY", "FLEET_SSH_PUBLIC_KEY",
]


def marks(n):
    return [f"LEAKMARK{i}{secrets.token_hex(6)}" for i in range(n)]


def scrub_env(extra):
    # No GITHUB_REPOSITORY: nothing posts from a subprocess (the in-process
    # test below covers the comment path with a fake transport).
    extra = {k: v for k, v in extra.items() if k != "GITHUB_REPOSITORY"}
    env = {"PATH": os.environ.get("PATH", ""), "HOME": os.environ.get("HOME", ""), "PYTHONDONTWRITEBYTECODE": "1"}
    env.update(extra)
    return env


# (script, subcommand) -> the event environments it runs under in its workflow.
SCENARIOS = {
    ("infra/fleet/fleet.py", "gate"): [event_env("/fleet status"), event_env("/fleet nope"),
                                       event_env("/fleet up screen", OPERATOR_STOP="true"), event_env(AUTHOR_ID="1")],
    ("infra/fleet/fleet.py", "refuse"): [{"REFUSAL_REASON": "unknown verb", "CHANNEL_PR": "700"}],
    ("infra/fleet/fleet.py", "act"): [event_env("/fleet status"), event_env("/fleet down"),
                                      event_env("/fleet run leak-test main"), event_env("/fleet extend")],
    ("infra/fleet/fleet.py", "reap"): [event_env()],
    ("infra/fleet/fleet.py", "dispatch-down"): [event_env()],
}
# Every run: step in a covered workflow is exactly one such invocation, so a
# script can't slip past the scenarios by being started some other way.
INVOKE = re.compile(r"python3 (infra/[\w/.-]+\.py) ([a-z][\w-]*)")


class PublicLogLeak(unittest.TestCase):
    def test_every_workflow_script_has_a_scenario(self):
        for path in covered():
            for name, job in load(path)["jobs"].items():
                for s in steps(job):
                    if "run" not in s:
                        continue
                    m = INVOKE.fullmatch(s["run"].strip())
                    with self.subTest(workflow=path.name, job=name, run=s["run"][:60]):
                        self.assertIsNotNone(m, "a run: step must be one `python3 infra/<script>.py <sub>`")
                        self.assertIn(m.groups(), SCENARIOS, "add a leak scenario for this script")

    def setUp(self):
        self.outside = tempfile.mkdtemp(prefix="fleet-leak-")
        self.addCleanup(__import__("shutil").rmtree, self.outside, True)

    def test_marked_secrets_never_reach_stdout_or_stderr(self):
        values = marks(len(SECRET_NAMES))
        secret_env = dict(zip(SECRET_NAMES, values))
        for (script, sub), envs in SCENARIOS.items():
            for env in envs:
                with self.subTest(script=script, sub=sub, body=env.get("COMMENT_BODY")):
                    # Outside any git repo: the main-head check fails closed
                    # without a network call.
                    r = subprocess.run([sys.executable, str(ROOT / script), sub], cwd=self.outside, capture_output=True,
                                       text=True, env=scrub_env({**env, **secret_env}), timeout=60)
                    self.assertEqual(r.returncode, 0, r.stderr[-400:])
                    for v in values:
                        self.assertNotIn(v, r.stdout + r.stderr)

    def test_marked_fleet_values_never_reach_logs_or_comments(self):
        repo, c = make_repo()
        m = marks(8)
        f = new_fleet(L, "screen", c["head"], T0 - timedelta(hours=3))
        f["roles"]["main"] = {"address": m[0], "fingerprint": m[1], "firewall_id": m[2]}
        f["roles"]["worker"] = {"address": m[3], "fingerprint": m[4], "firewall_id": m[5]}
        secret_env = dict(zip(SECRET_NAMES, marks(len(SECRET_NAMES))))
        old = [{"id": m[6], "created": fmt(T0 - timedelta(hours=50))}, {"id": m[7], "created": fmt(T0)}]
        runs = [("act", "/fleet status", 1), ("act", "/fleet run leak-test main", 1), ("act", "/fleet down", 1),
                ("act", "/fleet up screen", 0), ("reap", "", 1), ("dispatch-down", "", 1), ("gate", "/fleet down", 1)]
        for sub, body, overdue in runs:
            with self.subTest(sub=sub, body=body):
                fl = dict(f, roles={k: dict(v) for k, v in f["roles"].items()})
                if overdue and sub == "reap":
                    fl = dict(fl, created_at=fmt(T0 - timedelta(hours=44)), expires_at=fmt(T0))
                b = DryRunBackend(fleet=fl if overdue else None, linodes=list(old) if overdue else [])
                gh, out, clock = FakeGitHub(), io.StringIO(), Clock()
                with redirect_stdout(out), redirect_stderr(out), \
                        mock.patch.object(ancestry, "remote_main_head", return_value=c["head"]):
                    fleet.main([sub], env=event_env(body, **secret_env), backend=b, transport=gh,
                               clock=clock, sleep=clock.sleep, repo=repo)
                seen = out.getvalue() + "\n".join(gh.bodies())
                self.assertTrue(seen.strip())
                for v in m + list(secret_env.values()):
                    self.assertNotIn(v, seen)


if __name__ == "__main__":
    unittest.main()
