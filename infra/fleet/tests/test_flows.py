import io
import json
import unittest
from unittest import mock
from contextlib import redirect_stderr, redirect_stdout
from datetime import timedelta

import fleet
import limits
import ancestry
from ancestry import head_of, is_ancestor, remote_main_head
from lock import LOCK_KEY, FleetLock, MemoryStore
from reaper import DOWN, EMERGENCY, NONE, decide, only_down
from state import DryRunBackend, new_fleet, validate
from status import MARKER, render
from tests.helpers import T0, Clock, FakeGitHub, event_env, make_repo
from timefmt import fmt

L = limits.load()


class Limits(unittest.TestCase):
    def test_values(self):
        self.assertEqual((L["max_lifetime_hours"], L["monthly_compute_cap_usd"]), (44, 40))
        self.assertNotIn("max_fleets", L)  # one fleet is enforced by check_up, not a count

    def test_load_refuses_values_outside_the_plan(self):
        import json
        import tempfile
        from pathlib import Path
        base = json.loads(json.dumps(L))
        bad = [("monthly_compute_cap_usd", 41), ("monthly_compute_cap_usd", 0), ("monthly_compute_cap_usd", True),
               ("monthly_compute_cap_usd", "40"), ("max_lifetime_hours", 45), ("lock_ttl_minutes", 91),
               ("reap_grace_minutes", 16), ("max_fleets", 2)]
        for key, value in bad:
            with self.subTest(key=key, value=value), tempfile.TemporaryDirectory() as d:
                p = Path(d) / "limits.yaml"
                p.write_text(json.dumps(dict(base, **{key: value})), encoding="utf-8")
                with self.assertRaises(ValueError):
                    limits.load(p)
        self.assertEqual(set(L["presets"]), {"screen"})
        self.assertEqual(set(L["presets"]["screen"]["roles"]), {"main", "worker"})
        self.assertAlmostEqual(limits.projected_usd(L, "screen"), 0.0795 * 44)

    def test_valid_yaml_too(self):
        import yaml
        self.assertEqual(yaml.safe_load(limits.LIMITS_PATH.read_text()), L)

    def test_check_up(self):
        self.assertIsNone(limits.check_up(L, "screen", DryRunBackend(), T0))
        self.assertEqual(limits.check_up(L, "screen", DryRunBackend(linodes=[{"id": "1", "created": fmt(T0)}]), T0),
                         "a Lodestar Linode exists")
        self.assertEqual(limits.check_up(L, "screen", DryRunBackend(fleet={}), T0), "a fleet is recorded")
        self.assertIsNone(limits.check_up(L, "screen", DryRunBackend(spend_usd=36.5), T0))
        self.assertEqual(limits.check_up(L, "screen", DryRunBackend(spend_usd=36.6), T0),
                         "the monthly fleet-compute cap would be exceeded")


class FleetJson(unittest.TestCase):
    def test_new_and_validate(self):
        f = new_fleet(L, "screen", "b" * 40, T0)
        self.assertEqual(f["expires_at"], fmt(T0 + timedelta(hours=44)))
        self.assertEqual(f["roles"]["main"], dict.fromkeys(("address", "host_key", "fingerprint", "firewall_id", "admin_ipv4")))
        bad = [("commit", "B" * 40), ("commit", "b" * 39), ("expires_at", fmt(T0 + timedelta(hours=44, seconds=1))),
               ("expires_at", fmt(T0)), ("preset", "windows"), ("version", 2), ("created_at", "2026-11-08 20:00")]
        for k, v in bad:
            with self.subTest(k=k, v=v), self.assertRaises(ValueError):
                validate(dict(f, **{k: v}), L)
        with self.assertRaises(ValueError):
            validate(dict(f, roles={"main": f["roles"]["main"]}), L)
        with self.assertRaises(ValueError):
            validate(dict(f, roles={**f["roles"], "main": dict(f["roles"]["main"], address=1)}), L)
        with self.assertRaises(ValueError):
            validate(dict(f, roles={**f["roles"], "main": dict(f["roles"]["main"], admin_ipv4="203.0.113.1/32")}), L)
        with self.assertRaises(ValueError):
            validate(dict(f, roles={**f["roles"], "main": {"address": None}}), L)


class Reaper(unittest.TestCase):
    def fleet(self, age_h):
        return new_fleet(L, "screen", "c" * 40, T0 - timedelta(hours=age_h))

    def test_decisions(self):
        self.assertEqual(decide(None, [], False, T0, L).action, NONE)
        self.assertEqual(decide(self.fleet(43), [], False, T0, L).action, NONE)
        self.assertEqual(decide(self.fleet(44), [], False, T0, L).action, DOWN)
        self.assertEqual(decide(self.fleet(44.25), [], False, T0, L).action, DOWN)
        self.assertEqual(decide(self.fleet(44.26), [], False, T0, L).action, EMERGENCY)
        self.assertEqual(decide(self.fleet(1), [], True, T0, L).action, EMERGENCY)
        # A recorded mismatch goes down whether or not a fleet is recorded, if anything exists.
        node = [{"id": "n", "created": fmt(T0)}]
        self.assertEqual(decide(None, node, True, T0, L).action, EMERGENCY)
        self.assertEqual(decide(None, [], True, T0, L).action, NONE)

    def test_age_reap_by_creation_time(self):
        nodes = [{"id": "a", "created": fmt(T0 - timedelta(hours=44))}, {"id": "b", "created": fmt(T0 - timedelta(hours=43))}]
        self.assertEqual(decide(None, nodes, False, T0, L).age_reap, ("a",))

    def test_only_down(self):
        self.assertFalse(only_down(None, False, T0))
        self.assertTrue(only_down(None, True, T0))
        self.assertFalse(only_down(self.fleet(43.9), False, T0))
        self.assertTrue(only_down(self.fleet(44), False, T0))


class Status(unittest.TestCase):
    def test_render(self):
        f = new_fleet(L, "screen", "d" * 40, T0 - timedelta(hours=2, minutes=5))
        f["roles"]["main"] = {"address": "203.0.113.9", "host_key": "ssh-ed25519 AAAAzz", "fingerprint": "SHA256:zz",
                              "firewall_id": "12345", "admin_ipv4": ["198.51.100.4/32"]}
        out = render(f, L, T0, dry_run=False)
        self.assertTrue(out.startswith(MARKER))
        self.assertIn("| main | 2 h 05 min |", out)
        self.assertIn("| worker | 2 h 05 min |", out)
        self.assertIn("Expires: 2026-11-10 13:55Z (in 41 h 55 min)", out)
        self.assertIn("Compute cost so far: $0.17", out)
        for secret in ("203.0.113.9", "AAAAzz", "SHA256:zz", "12345", "198.51.100.4", "d" * 40, "lodestar-"):
            self.assertNotIn(secret, out)
        self.assertIn("No fleet.", render(None, L, T0, dry_run=True))


class Ancestry(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.repo, cls.c = make_repo()

    def test_checks(self):
        self.assertTrue(is_ancestor(self.c["first"], "origin/main", self.repo))
        self.assertTrue(is_ancestor(self.c["head"], "origin/main", self.repo))
        self.assertFalse(is_ancestor(self.c["side"], "origin/main", self.repo))
        self.assertFalse(is_ancestor("0" * 40, "origin/main", self.repo))
        self.assertFalse(is_ancestor("--help", "origin/main", self.repo))
        self.assertFalse(is_ancestor(self.c["head"][:12], "origin/main", self.repo))
        self.assertEqual(head_of("origin/main", self.repo), self.c["head"])


class RemoteHead(unittest.TestCase):
    def run_ls_remote(self, stdout="", fail=False):
        def fake(*a, **kw):
            if fail:
                raise ancestry.subprocess.CalledProcessError(128, a[0])
            return ancestry.subprocess.CompletedProcess(a[0], 0, stdout=stdout)
        with mock.patch.object(ancestry.subprocess, "run", side_effect=fake) as m:
            got = remote_main_head("/r")
        return got, m

    def test_parses_one_main_ref(self):
        got, m = self.run_ls_remote("a" * 40 + "\trefs/heads/main\n")
        self.assertEqual(got, "a" * 40)
        self.assertEqual(m.call_args[0][0], ["git", "-C", "/r", "ls-remote", "origin", "refs/heads/main"])

    def test_fails_closed(self):
        for out in ("", "a" * 40 + "\trefs/heads/mainx\n", "zz\trefs/heads/main\n",
                    ("a" * 40 + "\trefs/heads/main\n") * 2):
            with self.subTest(out=out):
                self.assertIsNone(self.run_ls_remote(out)[0])
        self.assertIsNone(self.run_ls_remote(fail=True)[0])

    def test_no_remote_in_a_real_repo(self):
        repo, _ = make_repo()  # no origin remote: fails closed, no network
        self.assertIsNone(remote_main_head(repo))


class Flows(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.repo, cls.c = make_repo()

    def run_cli(self, sub, env, backend=None, store=None, gh=None, clock=None, remote="head"):
        out = io.StringIO()
        self.gh = gh if gh is not None else FakeGitHub()
        clock = clock or Clock()
        remote_sha = self.c.get(remote, remote)  # a key of self.c, or a literal (None: ls-remote failed)
        with redirect_stdout(out), redirect_stderr(out), \
                mock.patch.object(ancestry, "remote_main_head", return_value=remote_sha):
            rc = fleet.main([sub], env=env, backend=backend, store=store, transport=self.gh,
                            clock=clock, sleep=clock.sleep, repo=self.repo)
        return rc, out.getvalue()

    def test_up_then_status(self):
        b = DryRunBackend()
        rc, out = self.run_cli("act", event_env("/fleet up screen"), backend=b)
        self.assertEqual(rc, 0)
        self.assertIn("up screen: would create main, worker", out)
        self.assertEqual(b.fleet["commit"], self.c["head"])
        posted = self.gh.bodies()
        self.assertTrue(any(x.startswith(MARKER) for x in posted))
        rc, out = self.run_cli("act", event_env("/fleet up screen"), backend=b)
        self.assertIn("refused: up screen: a fleet is recorded", out)

    def test_stranger_never_touches_the_lock(self):
        store = MemoryStore()
        for over in ({"AUTHOR_ID": "1"}, {"EVENT_ACTION": "edited"}, {"ISSUE_NUMBER": "9"}):
            rc, out = self.run_cli("act", event_env("/fleet down", **over), store=store)
            self.assertEqual((rc, out, self.gh.calls), (0, "", []))
        self.assertEqual(store.calls, [])

    def test_busy_and_reaping(self):
        store = MemoryStore()
        FleetLock(store, 90, Clock()).acquire("7-1")
        _, out = self.run_cli("act", event_env("/fleet status"), store=store)
        self.assertIn("busy, retry", out)
        FleetLock(store, 90, Clock()).preempt("8-1")
        _, out = self.run_cli("act", event_env("/fleet down"), store=store)
        self.assertIn("being reaped", out)

    def test_overdue_fleet_only_down(self):
        b = DryRunBackend(fleet=new_fleet(L, "screen", self.c["head"], T0 - timedelta(hours=44)))
        _, out = self.run_cli("act", event_env("/fleet run leak-test main"), backend=b)
        self.assertIn("only down runs", out)
        store = MemoryStore()
        _, out = self.run_cli("act", event_env("/fleet down"), backend=b, store=store)
        self.assertIn("would destroy", out)
        self.assertIsNone(b.fleet)
        self.assertNotIn(LOCK_KEY, store.objects)  # released

    def test_run_commit_must_descend_from_main(self):
        b = DryRunBackend(fleet=new_fleet(L, "screen", self.c["head"], T0))
        _, out = self.run_cli("act", event_env(f"/fleet run start-block main {self.c['side']}"), backend=b)
        self.assertIn("the commit is not an ancestor of main", out)
        _, out = self.run_cli("act", event_env(f"/fleet run start-block main {self.c['first']}"), backend=b)
        self.assertIn("would run the step", out)
        b.fleet["commit"] = self.c["side"]
        _, out = self.run_cli("act", event_env("/fleet run leak-test worker"), backend=b)
        self.assertIn("pinned commit is not an ancestor", out)

    def test_result_comment_carries_an_unedited_status(self):
        b = DryRunBackend(fleet=new_fleet(L, "screen", self.c["head"], T0 - timedelta(hours=1)))
        self.run_cli("act", event_env("/fleet status"), backend=b)
        result = [x for x in self.gh.bodies() if not x.startswith(MARKER)]
        self.assertEqual(len(result), 1)
        self.assertIn("| main | 1 h 00 min |", result[0])
        self.assertNotIn(MARKER, result[0])

    def test_rerun_or_stale_checkout_never_acts(self):
        store = MemoryStore()
        _, out = self.run_cli("act", event_env("/fleet up screen", GITHUB_RUN_ATTEMPT="2"), store=store)
        self.assertEqual((out, store.calls), ("", []))
        # main moved on GitHub since the event: the checkout is an old commit.
        _, out = self.run_cli("act", event_env("/fleet up screen"), store=store, remote="side")
        self.assertIn("not main's head", out)
        # GitHub can't be asked: fail closed.
        _, out = self.run_cli("act", event_env("/fleet up screen"), store=store, remote=None)
        self.assertIn("not main's head", out)
        self.assertEqual(store.calls, [])

    def test_lock_lost_mid_command(self):
        store = MemoryStore()
        b = DryRunBackend()
        real = b.lodestar_linodes

        def preempted():  # the reaper pre-empts while up checks limits
            FleetLock(store, 90, Clock()).preempt("9-1")
            return real()
        b.lodestar_linodes = preempted
        rc, out = self.run_cli("act", event_env("/fleet up screen"), backend=b, store=store)
        self.assertEqual(rc, 1)
        self.assertIn("lock was lost", out)
        self.assertIsNone(b.fleet)
        self.assertIn(LOCK_KEY, store.objects)  # the reaper's marker stays

    def test_invalid_fleet_json(self):
        b = DryRunBackend(fleet={"version": 9})
        rc, out = self.run_cli("act", event_env("/fleet status"), backend=b)
        self.assertEqual(rc, 1)
        self.assertIn("fleet.json is invalid", out)
        rc, out = self.run_cli("reap", event_env(), backend=b)
        self.assertEqual(rc, 1)

    def test_internal_errors_print_no_detail(self):
        class Boom(DryRunBackend):
            def read_fleet(self):
                raise KeyError("LEAKMARK-secret-value")
        rc, out = self.run_cli("dispatch-down", event_env(), backend=Boom())
        self.assertEqual(rc, 1)
        self.assertIn("internal error (KeyError)", out)
        self.assertNotIn("LEAKMARK", out)

    def test_reaper_decides_again_under_the_lock(self):
        clock, store = Clock(), MemoryStore()
        holder = FleetLock(store, 90, clock)
        holder.acquire("7-1")
        b = DryRunBackend(fleet=new_fleet(L, "screen", self.c["head"], T0 - timedelta(hours=44)))

        def sleep(seconds):  # a /fleet down finishes while the reaper waits
            clock.sleep(seconds)
            b.fleet = None
            holder.release("7-1")
        out = io.StringIO()
        with redirect_stdout(out):
            fleet.main(["reap"], env=event_env(), backend=b, store=store, transport=FakeGitHub(),
                       clock=clock, sleep=sleep, repo=self.repo)
        self.assertIn("reaper: none (no fleet)", out.getvalue())
        self.assertEqual(b.log, [])

    def test_live_mode_refuses_without_its_settings(self):
        secret = "LEAKMARK-not-printed"
        for sub in ("act", "reap", "dispatch-down"):
            with self.subTest(sub=sub), self.assertRaises(SystemExit) as cm:
                self.run_cli(sub, event_env("/fleet status", FLEET_LIVE="true", STATE_BUCKET=secret))
            msg = str(cm.exception)
            self.assertIn("live mode needs", msg)
            self.assertIn("FLEET_LINODE_ACCOUNT_EUUID", msg)
            self.assertNotIn(secret, msg)  # names only, never values

    def test_gate_outputs(self):
        import os
        import tempfile
        path = os.path.join(tempfile.mkdtemp(), "out")
        self.run_cli("gate", event_env("/fleet extend", GITHUB_OUTPUT=path))
        self.run_cli("gate", event_env("/fleet up screen", GITHUB_OUTPUT=path, OPERATOR_STOP="true"))
        self.run_cli("gate", event_env("/fleet down", GITHUB_OUTPUT=path, OPERATOR_STOP="true"))
        self.run_cli("gate", event_env("/fleet down", GITHUB_OUTPUT=path, AUTHOR_ID="3"))
        with open(path, encoding="utf-8") as f:
            lines = f.read().splitlines()
        self.assertEqual(lines, ["allowed=false", "reason=unknown verb",
                                 "allowed=false", "reason=OPERATOR_STOP is set: only down runs",
                                 "allowed=true", "reason=", "allowed=false", "reason="])

    def test_refuse_posts_only_known_reasons(self):
        self.run_cli("refuse", {"REFUSAL_REASON": "unknown verb", "CHANNEL_PR": "700", "GITHUB_REPOSITORY": "o/r"})
        self.assertEqual(self.gh.bodies(), ["`fleet.yml` (dry-run): refused: unknown verb"])
        self.run_cli("refuse", {"REFUSAL_REASON": "@everyone pwned", "CHANNEL_PR": "700", "GITHUB_REPOSITORY": "o/r"})
        self.assertEqual(self.gh.calls, [])

    def test_status_comment_edited_in_place(self):
        gh = FakeGitHub([{"id": 5, "user": {"id": 999}, "body": MARKER}, {"id": 6, "user": {"id": 41898282}, "body": MARKER}])
        self.run_cli("act", event_env("/fleet status"), gh=gh)
        self.assertIn(("PATCH", "/repos/o/r/issues/comments/6"), [(m, p) for m, p, _ in gh.calls])

    def test_reaper(self):
        clock = Clock()
        b = DryRunBackend(fleet=new_fleet(L, "screen", self.c["head"], T0 - timedelta(hours=44)),
                          linodes=[{"id": "x", "created": fmt(T0 - timedelta(hours=45))}])
        _, out = self.run_cli("reap", event_env(OPERATOR_STOP="true"), backend=b, clock=clock)
        self.assertIn("reaper: down (past expiry); Linodes over 44 h: 1", out)
        self.assertEqual(b.log, ["destroy", "finish_down"])  # destroy also takes the over-44 h Linode

    def test_reaper_waits_then_gives_up_on_a_live_holder(self):
        clock, store = Clock(), MemoryStore()
        FleetLock(store, 90, clock).acquire("7-1")
        b = DryRunBackend(fleet=new_fleet(L, "screen", self.c["head"], T0 - timedelta(hours=44)))
        _, out = self.run_cli("reap", event_env(), backend=b, store=store, clock=clock)
        self.assertIn("lock busy", out)
        self.assertEqual(b.log, [])
        self.assertEqual(clock() - T0, timedelta(seconds=fleet.REAPER_WAIT_SECONDS))

    def test_reaper_emergency_preempts(self):
        store = MemoryStore()
        FleetLock(store, 90, Clock()).acquire("7-1")
        b = DryRunBackend(fleet=new_fleet(L, "screen", self.c["head"], T0 - timedelta(hours=44, minutes=16)))
        _, out = self.run_cli("reap", event_env(), backend=b, store=store)
        self.assertIn("reaper: emergency", out)
        self.assertEqual(b.log, ["destroy", "finish_down"])

    def test_dispatch_down_does_only_down(self):
        # Whatever the environment holds, a dispatch never parses a command.
        store = MemoryStore()
        FleetLock(store, 90, Clock()).acquire("7-1")
        b = DryRunBackend(fleet=new_fleet(L, "screen", self.c["head"], T0))
        env = event_env("/fleet up screen", OPERATOR_STOP="true", EVENT_NAME="workflow_dispatch")
        _, out = self.run_cli("dispatch-down", env, backend=b, store=store)
        self.assertIn("owner's down: would destroy", out)
        self.assertEqual(b.log, ["destroy", "finish_down"])
        self.assertNotIn(LOCK_KEY, store.objects)
        # With no fleet recorded it still sweeps, in case anything unrecorded is left.
        _, out = self.run_cli("dispatch-down", env, backend=b)
        self.assertIn("no fleet recorded; nothing tagged is left", out)
        self.assertEqual(b.log, ["destroy", "finish_down", "destroy", "finish_down"])


if __name__ == "__main__":
    unittest.main()
