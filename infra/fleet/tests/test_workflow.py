"""fleet.yml's structure, and the public-log leak test (plan §2, §3, OP-1 row).

Covers fleet.yml and the four operator workflows: every script they run must
have a leak scenario below, or this test fails. The injection harness's live
paths are checked in process in infra/operator/tests/test_injection.py.
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
COVERED = ["fleet.yml", "operator-wake.yml", "operator-apply.yml", "operator-tripwire.yml", "operator-injection.yml"]
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
        base = {"contents": "read", "pull-requests": "write"}
        oidc = dict(base, **{"id-token": "write"})
        want = {"gate": {"contents": "read"}, "refuse": base, "act": oidc, "dispatch-down": oidc,
                "reaper": dict(oidc, actions="read"),  # the sweeping jobs alone read run status
                "pia-fence": oidc, "deliver-pia": dict(oidc, actions="read"), "pia-release": oidc}
        self.assertEqual({n: j.get("permissions") for n, j in self.jobs.items()}, want)

    def test_live_only_steps_and_secrets(self):
        for name, job in self.jobs.items():
            for s in steps(job):
                text = yaml.safe_dump(s)
                with self.subTest(job=name, step=s.get("uses") or s.get("run", "")[:40]):
                    if s.get("uses", "").startswith("hashicorp/setup-terraform") or s.get("run") == LIVE_INSTALL:
                        # The delivery chain takes its mode from act, so a flip mid-chain can't split it.
                        want = "needs.act.outputs.live == 'true'" if name in CHAIN else "vars.FLEET_LIVE == 'true'"
                        self.assertEqual(s.get("if"), want)
                    live = (s.get("env") or {}).get("FLEET_LIVE")
                    if live is not None:
                        want = "${{ needs.act.outputs.live }}" if name in CHAIN else "${{ vars.FLEET_LIVE }}"
                        self.assertEqual(live, want)
                    if "secrets." in text:
                        # Secrets reach only the one script step, through env: fleet.py in
                        # cloud-fleet jobs, deliver.py in pia-delivery.
                        self.assertNotIn("secrets.", str(s.get("run", "")))
                        names = {k for k, v in (s.get("env") or {}).items() if "secrets." in str(v)}
                        if job.get("environment") == "pia-delivery":
                            self.assertEqual(s.get("run"), "python3 infra/pia-bridge/deliver.py deliver")
                            self.assertEqual(names, PIA_SECRETS)
                        else:
                            self.assertEqual(job.get("environment"), "cloud-fleet")
                            self.assertTrue(str(s.get("run", "")).startswith("python3 infra/fleet/fleet.py "))
                            self.assertFalse(names & {"AWS_PIA_ROLE_ARN", "PIA_SERVERS"})
                            # The SSH key reaches only act's step (live `run <step>`, OP-3a).
                            self.assertEqual("FLEET_SSH_KEY" in names, name == "act")
                            # Every cloud-fleet step names the buckets' owner (P8).
                            self.assertIn("FLEET_BUCKET_OWNER", names)

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
                    self.assertIn("github.run_attempt == 1", cond)  # a re-run keeps the owner's actor_id
                    self.assertIn("vars.OWNER_USER_ID != ''", cond)
                    runs = [s["run"] for s in steps(job) if "run" in s and s["run"] != LIVE_INSTALL]
                    self.assertEqual(runs, ["python3 infra/fleet/fleet.py dispatch-down"])
                    envs = {k for s in steps(job) for k in (s.get("env") or {})}
                    self.assertNotIn("COMMENT_BODY", envs)
                else:
                    self.assertRegex(cond, r"^github\.event_name == '(issue_comment|schedule)' && ")

    def test_stop_never_gates_reaper_or_dispatch(self):
        for name in ("reaper", "dispatch-down"):
            self.assertNotIn("OPERATOR_STOP", yaml.safe_dump(self.jobs[name]))
        # Only disabling the workflow stops the reaper: no variable gates it. A manual
        # re-run is refused, since it would run the old run's code (see fleet.py).
        self.assertEqual(self.jobs["reaper"]["if"],
                         "github.event_name == 'schedule' && github.ref == 'refs/heads/main' && github.run_attempt == 1")

    def test_lock_takers_run_on_disjoint_events(self):
        # The lock's run ID is <run>-<attempt>, shared by every job of a run (P2 of
        # #646's review: GITHUB_JOB isn't added, because the delivery chain must
        # share one ID). So the jobs that take or pre-empt the lock must never run
        # in the same workflow run, and the chain jobs only follow act.
        takers = {"act": "issue_comment", "reaper": "schedule", "dispatch-down": "workflow_dispatch"}
        chain = ("pia-fence", "deliver-pia", "pia-release")
        lock_jobs = {n for n, j in self.jobs.items() if j.get("environment") in ("cloud-fleet", "pia-delivery")}
        self.assertEqual(lock_jobs, set(takers) | set(chain))
        for name, event in takers.items():
            self.assertTrue(self.jobs[name]["if"].startswith(f"github.event_name == '{event}' "), name)
        self.assertEqual(len(set(takers.values())), len(takers))
        for name in chain:
            self.assertTrue(self.jobs[name]["if"].startswith("github.event_name == 'issue_comment' "), name)
            self.assertIn("act", self.jobs[name]["needs"])
            self.assertIn("github.run_attempt == 1", self.jobs[name]["if"])

    def test_delivery_chain(self):
        fence, deliver, release = (self.jobs[n] for n in ("pia-fence", "deliver-pia", "pia-release"))
        self.assertIn("needs.act.result == 'success' && needs.act.outputs.deliver == 'true'", fence["if"])
        self.assertEqual(deliver["environment"], "pia-delivery")
        self.assertIn("pia-fence", deliver["needs"])
        for check in ("needs.pia-fence.result == 'success'", "vars.OPERATOR_STOP != 'true'",
                      "github.event.action == 'created'", "github.ref == 'refs/heads/main'",
                      "format('{0}', github.event.issue.number) == vars.CHANNEL_PR",
                      "format('{0}', github.event.comment.user.id) == vars.OPERATOR_BOT_USER_ID"):
            self.assertIn(check, deliver["if"])  # its own gates, not inherited through needs
        self.assertIn("github.event.issue.pull_request", fence["if"])
        self.assertIn("github.event.issue.pull_request", deliver["if"])
        self.assertEqual(fence["outputs"], {"lock_expires": "${{ steps.fence.outputs.lock_expires }}"})
        envs = {k: v for st in steps(deliver) for k, v in (st.get("env") or {}).items()}
        self.assertEqual(envs["LOCK_EXPIRES_AT"], "${{ needs.pia-fence.outputs.lock_expires }}")
        self.assertNotIn("vars.", release["if"])  # a variable changed mid-chain can't strand the lock
        self.assertIn("always()", release["if"])  # released whatever deliver-pia did
        self.assertIn("needs.act.outputs.deliver == 'true'", release["if"])
        self.assertEqual(set(release["needs"]), {"act", "pia-fence", "deliver-pia"})
        self.assertEqual(self.jobs["act"]["outputs"], {"deliver": "${{ steps.act.outputs.deliver }}",
                                                       "live": "${{ steps.act.outputs.live }}"})
        total = sum(self.jobs[n]["timeout-minutes"] for n in ("act", "pia-fence", "deliver-pia", "pia-release"))
        self.assertLess(total, 90)  # the lock's chain, queue time aside

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
    "OWNER_SSH_PUBLIC_KEY", "FLEET_LINODE_ACCOUNT_EUUID", "ACTIONS_ID_TOKEN_REQUEST_TOKEN", "STATE_BUCKET_OWNER",
    "FLEET_BUCKET_OWNER",
]
CHAIN = ("pia-fence", "deliver-pia", "pia-release")
PIA_SECRETS = {"AWS_PIA_ROLE_ARN", "STATE_BUCKET", "STATE_BUCKET_OWNER", "FLEET_SSH_KEY", "PIA_SERVERS"}


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
                                      event_env("/fleet run leak-test main"), event_env("/fleet run vm-status worker"),
                                      # OP-3b's block steps
                                      event_env("/fleet run start-block main " + "c" * 40),
                                      event_env("/fleet run arm-switch worker"),
                                      event_env("/fleet run outage-watcher main"), event_env("/fleet run copy-off main"),
                                      event_env("/fleet extend")],
    ("infra/fleet/fleet.py", "reap"): [event_env()],
    ("infra/fleet/fleet.py", "dispatch-down"): [event_env()],
    ("infra/fleet/fleet.py", "pia-fence"): [event_env("/fleet up screen"), event_env("/fleet run renew-vpn main")],
    ("infra/fleet/fleet.py", "pia-release"): [event_env("/fleet up screen")],
    ("infra/pia-bridge/deliver.py", "deliver"): [event_env("/fleet up screen"), event_env("/fleet run renew-vpn worker"),
                                                 event_env("/fleet run renew-vpn main", OPERATOR_STOP="true"),
                                                 event_env("/fleet status"), event_env(AUTHOR_ID="1")],
}

def op_env(**over):
    """An operator-wake event (OP-4): a runner instruction on the channel PR."""
    env = event_env("operator-instruction v1\nid: ping-1\nkind: query\n\nping", AUTHOR_ID="111", RUNNER_USER_ID="111")
    env.update(over)
    return env


SCENARIOS.update({
    ("infra/operator/wake.py", "gate"): [op_env(), op_env(AUTHOR_ID="9"), op_env(OPERATOR_STOP="true"),
                                         op_env(COMMENT_BODY="operator-instruction v1\nid: x")],
    ("infra/operator/wake.py", "refuse"): [{"REFUSAL_REASON": "missing field", "CHANNEL_PR": "700"}],
    # Gated before any setting is read: a stranger, the stop, a re-run, the bot.
    ("infra/operator/wake.py", "wake"): [op_env(AUTHOR_ID="9"), op_env(OPERATOR_STOP="true"),
                                         op_env(GITHUB_RUN_ATTEMPT="2"), op_env(AUTHOR_ID="555")],
    ("infra/operator/tripwire.py", "trip"): [op_env(SENDER_ID="9"), op_env(SENDER_ID="555", EVENT_ACTION="edited")],
    ("infra/operator/tripwire.py", "sweep"): [op_env()],
    ("infra/operator/agent_apply.py", "apply"): [op_env(GITHUB_EVENT_NAME="workflow_dispatch", GITHUB_ACTOR_ID="9",
                                                        OWNER_USER_ID="111")],
})


def inj_env(**over):
    """An operator-injection dispatch (OP-5) by the owner, on main."""
    env = op_env(GITHUB_EVENT_NAME="workflow_dispatch", GITHUB_ACTOR_ID="111", OWNER_USER_ID="111",
                 GITHUB_REF="refs/heads/main", INJECTION_LEVEL="1")
    env.update(over)
    return env


# Each refuses before any setting is read: a stranger, the stop, a re-run, a
# branch, and (with every secret marked, the App's key included) the key itself.
SCENARIOS[("infra/operator/injection/harness.py", "run")] = [
    inj_env(GITHUB_ACTOR_ID="9"), inj_env(OPERATOR_STOP="true"), inj_env(GITHUB_RUN_ATTEMPT="2"),
    inj_env(GITHUB_REF="refs/heads/x"), inj_env()]
# These refuse with a failing exit in the leak test's offline, repository-less
# environment, before any network call (the operator tests cover their live paths
# with fakes): the sweep and a tripped trip need GITHUB_REPOSITORY, apply the owner.
FAILS_CLOSED = {("infra/operator/tripwire.py", "sweep"), ("infra/operator/agent_apply.py", "apply"),
                ("infra/operator/tripwire.py", "trip"), ("infra/operator/injection/harness.py", "run")}
OPERATOR_WORKFLOWS = ("operator-wake.yml", "operator-apply.yml", "operator-tripwire.yml", "operator-injection.yml")
# Every run: step in a covered workflow is exactly one such invocation, so a
# script can't slip past the scenarios by being started some other way.
LIVE_INSTALL = ('python3 -m pip install --quiet --no-deps --require-hashes --target "$RUNNER_TEMP/fleet-deps" '
                '-r infra/fleet/requirements-live.txt')
INVOKE = re.compile(r"python3 (infra/[\w/.-]+\.py) ([a-z][\w-]*)")


class PublicLogLeak(unittest.TestCase):
    def test_every_workflow_script_has_a_scenario(self):
        for path in covered():
            for name, job in load(path)["jobs"].items():
                for s in steps(job):
                    if "run" not in s:
                        continue
                    if s["run"] == LIVE_INSTALL and s.get("if") in ("vars.FLEET_LIVE == 'true'",
                                                                     "needs.act.outputs.live == 'true'"):
                        continue  # the one other allowed step: pip from hash-pinned wheels, live only
                    if s["run"] == LIVE_INSTALL and path.name in OPERATOR_WORKFLOWS and "environment" in job:
                        continue  # the operator's jobs are always live (no dry-run): the same pinned wheels
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
                    # Dry-run, outside any git repo: up and run fail the main-head
                    # check closed without a network call; the rest run dry.
                    r = subprocess.run([sys.executable, str(ROOT / script), sub], cwd=self.outside, capture_output=True,
                                       text=True, env=scrub_env({**env, **secret_env}), timeout=60)
                    if (script, sub) in FAILS_CLOSED and env.get("SENDER_ID") != "9":
                        self.assertNotEqual(r.returncode, 0)
                    else:
                        self.assertEqual(r.returncode, 0, r.stderr[-400:])
                    for v in values:
                        self.assertNotIn(v, r.stdout + r.stderr)

    def test_live_mode_without_settings_names_them_only(self):
        values = marks(len(SECRET_NAMES))
        secret_env = dict(zip(SECRET_NAMES, values))
        secret_env.pop("FLEET_LINODE_ACCOUNT_EUUID")  # one missing setting
        for sub in ("act", "reap", "dispatch-down"):
            with self.subTest(sub=sub):
                r = subprocess.run([sys.executable, str(ROOT / "infra/fleet/fleet.py"), sub], cwd=self.outside,
                                   capture_output=True, text=True, timeout=60,
                                   env=scrub_env({**event_env("/fleet status", FLEET_LIVE="true"), **secret_env}))
                # status, the reaper and the owner's down skip the head check, so they reach
                # the settings check, which names the missing setting and never a value.
                self.assertNotEqual(r.returncode, 0)
                self.assertIn("FLEET_LINODE_ACCOUNT_EUUID", r.stdout + r.stderr)
                for v in values:
                    self.assertNotIn(v, r.stdout + r.stderr)

    def test_deliver_live_without_settings_names_them_only(self):
        values = marks(len(SECRET_NAMES))
        secret_env = dict(zip(SECRET_NAMES, values))
        secret_env.pop("STATE_BUCKET_OWNER")
        r = subprocess.run([sys.executable, str(ROOT / "infra/pia-bridge/deliver.py"), "deliver"], cwd=self.outside,
                           capture_output=True, text=True, timeout=60,
                           env=scrub_env({**event_env("/fleet up screen", FLEET_LIVE="true"), **secret_env}))
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("STATE_BUCKET_OWNER", r.stdout + r.stderr)
        for v in values:
            self.assertNotIn(v, r.stdout + r.stderr)

    def test_marked_fleet_values_never_reach_logs_or_comments(self):
        repo, c = make_repo()
        m = marks(12)
        f = new_fleet(L, "screen", c["head"], T0 - timedelta(hours=3))
        f["roles"]["main"] = {"address": m[0], "host_key": m[8], "fingerprint": m[1], "firewall_id": m[2], "admin_ipv4": [m[9]]}
        f["roles"]["worker"] = {"address": m[3], "host_key": m[10], "fingerprint": m[4], "firewall_id": m[5], "admin_ipv4": [m[11]]}
        secret_env = dict(zip(SECRET_NAMES, marks(len(SECRET_NAMES))))
        old = [{"id": m[6], "created": fmt(T0 - timedelta(hours=50))}, {"id": m[7], "created": fmt(T0)}]
        runs = [("act", "/fleet status", 1), ("act", "/fleet run leak-test main", 1), ("act", "/fleet down", 1),
                ("act", f"/fleet run start-block main {c['head']}", 1), ("act", "/fleet run arm-switch main", 1),
                ("act", "/fleet run outage-watcher worker", 1), ("act", "/fleet run copy-off main", 1),
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
