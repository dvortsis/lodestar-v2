"""operator-wake.yml, operator-apply.yml and operator-tripwire.yml: structure
and hygiene (plan §2 "Workflow hygiene", the OP-4 row's tripwire requirements)."""

import re
import unittest
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[3]
WF = ROOT / ".github" / "workflows"
NAMES = ("operator-wake.yml", "operator-apply.yml", "operator-tripwire.yml")
SHA = re.compile(r"^[\w.-]+/[\w.-]+@[0-9a-f]{40}$")
# The only event fields any expression may read: numbers, IDs and enums, never
# a title, body, label, branch or login (the comment body reaches env only, in
# the two jobs that parse it).
SAFE_EVENT = {"github.event.action", "github.event.issue.pull_request", "github.event.issue.number",
              "github.event.comment.user.id", "github.event.sender.id", "github.event.pull_request.number"}
BODY = "github.event.comment.body"
INSTALL = ('python3 -m pip install --quiet --no-deps --require-hashes --target "$RUNNER_TEMP/fleet-deps" '
           '-r infra/fleet/requirements-live.txt')


def load(name):
    wf = yaml.safe_load((WF / name).read_text(encoding="utf-8"))
    wf["on"] = wf.pop(True, wf.get("on"))
    return wf


def exprs(obj):
    if isinstance(obj, dict):
        for v in obj.values():
            yield from exprs(v)
    elif isinstance(obj, list):
        for v in obj:
            yield from exprs(v)
    elif isinstance(obj, str):
        yield from re.findall(r"\$\{\{(.*?)\}\}", obj, re.S)


class Workflows(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.wfs = {n: load(n) for n in NAMES}

    def jobs(self):
        for n, wf in self.wfs.items():
            for j, job in wf["jobs"].items():
                yield n, j, job

    def test_triggers(self):
        self.assertEqual(self.wfs["operator-wake.yml"]["on"], {"issue_comment": {"types": ["created"]}})
        self.assertEqual(self.wfs["operator-apply.yml"]["on"], {"workflow_dispatch": None})
        on = self.wfs["operator-tripwire.yml"]["on"]
        self.assertEqual(set(on), {"pull_request_target", "issue_comment", "schedule"})
        self.assertEqual(on["pull_request_target"]["types"], ["opened", "edited", "closed", "reopened", "labeled",
                                                              "unlabeled", "ready_for_review", "converted_to_draft"])
        self.assertEqual(on["issue_comment"]["types"], ["created", "edited", "deleted"])

    def test_no_workflow_call_or_reusable_workflow(self):
        for n in NAMES:
            code = "\n".join(ln for ln in (WF / n).read_text().splitlines() if not ln.lstrip().startswith("#"))
            self.assertNotIn("workflow_call", code, n)
        for n, j, job in self.jobs():
            self.assertNotIn("uses", job, f"{n}:{j}")
            for s in job.get("steps", []):
                self.assertNotIn(".github/workflows/", str(s.get("uses", "")))

    def test_permissions_are_minimal(self):
        want = {
            ("operator-wake.yml", "gate"): {"contents": "read"},
            ("operator-wake.yml", "refuse"): {"contents": "read", "pull-requests": "write"},
            ("operator-wake.yml", "wake"): {"contents": "read", "pull-requests": "write", "id-token": "write", "actions": "read"},
            ("operator-wake.yml", "sweep"): {"contents": "read", "issues": "read", "pull-requests": "write", "actions": "write"},
            ("operator-apply.yml", "apply"): {"contents": "read", "id-token": "write"},
            ("operator-tripwire.yml", "trip"): {"contents": "read", "pull-requests": "write", "actions": "write"},
            ("operator-tripwire.yml", "sweep"): {"contents": "read", "issues": "read", "pull-requests": "write", "actions": "write"},
        }
        for n in NAMES:
            self.assertEqual(self.wfs[n]["permissions"], {}, n)
        self.assertEqual({(n, j): job.get("permissions") for n, j, job in self.jobs()}, want)

    def test_environment_jobs_gate_first_and_only_two_hold_secrets(self):
        envs = {(n, j): job.get("environment") for n, j, job in self.jobs() if "environment" in job}
        self.assertEqual(envs, {("operator-wake.yml", "wake"): "cloud-operator", ("operator-apply.yml", "apply"): "cloud-operator"})
        for n, j, job in self.jobs():
            text = yaml.safe_dump(job)
            with self.subTest(job=f"{n}:{j}"):
                if "environment" in job:
                    keys = list(job)
                    self.assertLess(keys.index("if"), keys.index("environment"))
                    for check in ("github.ref == 'refs/heads/main'", "github.run_attempt == 1", "vars.OPERATOR_STOP == 'false'"):
                        self.assertIn(check, job["if"])
                else:
                    self.assertNotIn("secrets.", text)
                self.assertNotIn("secrets.", str(job.get("if", "")))
                self.assertIsInstance(job.get("timeout-minutes"), int)

    def test_wake_gates(self):
        jobs = self.wfs["operator-wake.yml"]["jobs"]
        for j in ("gate", "wake"):
            for check in ("github.event.action == 'created'",
                          "format('{0}', github.event.issue.number) == vars.CHANNEL_PR",
                          "format('{0}', github.event.comment.user.id) == vars.RUNNER_USER_ID", "vars.RUNNER_USER_ID != ''"):
                self.assertIn(check, jobs[j]["if"], j)
        self.assertIn("needs.gate.outputs.allowed == 'true'", jobs["wake"]["if"])
        self.assertLess(jobs["wake"]["timeout-minutes"], 60)  # the operator lock's TTL and the role's hour

    def test_the_wake_sweep_runs_only_after_a_started_session_or_a_failed_job(self):
        jobs = self.wfs["operator-wake.yml"]["jobs"]
        self.assertEqual(jobs["wake"]["outputs"], {"started": "${{ steps.wake.outputs.started }}"})
        self.assertEqual([st.get("id") for st in jobs["wake"]["steps"] if "run" in st][-1], "wake")
        self.assertIn("&& (needs.wake.outputs.started == 'true' || needs.wake.result == 'failure' "
                      "|| needs.wake.result == 'cancelled')", jobs["sweep"]["if"])

    def test_apply_is_the_owners_alone(self):
        cond = self.wfs["operator-apply.yml"]["jobs"]["apply"]["if"]
        self.assertTrue(cond.startswith("github.event_name == 'workflow_dispatch'"))
        self.assertIn("vars.OWNER_USER_ID != '' && github.actor_id == vars.OWNER_USER_ID", cond)

    def test_tripwire_gates_on_sender_id_and_reads_no_pr_text(self):
        jobs = self.wfs["operator-tripwire.yml"]["jobs"]
        self.assertIn("format('{0}', github.event.sender.id) == vars.OPERATOR_BOT_USER_ID", jobs["trip"]["if"])
        self.assertIn("vars.OPERATOR_BOT_USER_ID != ''", jobs["trip"]["if"])
        checkout = jobs["trip"]["steps"][0]
        self.assertEqual(checkout["with"], {"ref": "main", "persist-credentials": False})
        self.assertEqual(jobs["sweep"]["if"], "github.event_name == 'schedule' && github.ref == 'refs/heads/main'")

    def test_expressions_read_only_safe_event_fields(self):
        for n, j, job in self.jobs():
            for e in list(exprs(job)) + [str(job.get("if", ""))]:
                for field in re.findall(r"github\.event\.[\w.]+", e):
                    with self.subTest(job=f"{n}:{j}", field=field):
                        if field == BODY:
                            self.assertIn((n, j), {("operator-wake.yml", "gate"), ("operator-wake.yml", "wake")})
                        else:
                            self.assertIn(field, SAFE_EVENT)

    def test_body_and_secrets_reach_env_only(self):
        for n, j, job in self.jobs():
            for s in job.get("steps", []):
                with self.subTest(job=f"{n}:{j}", step=s.get("run", s.get("uses", ""))[:40]):
                    self.assertNotIn("${{", s.get("run", ""))
                    self.assertNotIn("secrets.", yaml.safe_dump(s.get("with") or {}))
                    env = s.get("env") or {}
                    if BODY in yaml.safe_dump(s):
                        self.assertEqual(env.get("COMMENT_BODY"), "${{ github.event.comment.body }}")
                    if "uses" in s:
                        self.assertRegex(s["uses"], SHA)
                        self.assertFalse(s.get("with", {}).get("persist-credentials", True))
                    else:
                        self.assertTrue(s["run"] == INSTALL or re.fullmatch(r"python3 infra/operator/\w+\.py [a-z]+", s["run"]),
                                        s["run"])

    def test_no_artifacts_or_concurrency(self):
        for n in NAMES:
            text = (WF / n).read_text().lower()
            self.assertNotIn("artifact", text.replace("no actions artifacts", ""), n)
            self.assertNotIn("concurrency:", text, n)
