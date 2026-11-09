"""Live mode (OP-2) with fakes for S3, the Linode API, Terraform, ssh-keygen,
the Actions API and /meta. No network, no cloud, no terraform binary."""

import base64
import io
import json
import re
import unittest
from contextlib import redirect_stderr, redirect_stdout
from datetime import timedelta
from pathlib import Path
from unittest import mock

import ancestry
import apply
import audit
import fleet
import limits
from linode import Linode, LinodeError
from lock import LOCK_KEY, FleetLock, LockLost, MemoryStore
from state import FLEET_KEY, LEDGER_KEY, MISMATCH_KEY, FleetUnknown, LedgerUnreadable, new_fleet
from tests.helpers import T0, Clock, FakeGitHub, event_env, make_repo
from timefmt import fmt

L = limits.load()
RANGES = {"actions": ["4.148.0.0/16", "2a01:111::/32"]}
ADMIN = "198.51.100.4/32"
HERE = Path(__file__).resolve().parent.parent


class S3Error(Exception):
    def __init__(self, code):
        super().__init__(code)
        self.response = {"Error": {"Code": code}}


class FakeS3:
    def __init__(self, deny=()):
        self.objects, self.deny = {}, set(deny)

    def get_object(self, Bucket, Key):
        if (Bucket, Key) in self.deny:
            raise S3Error("AccessDenied")
        if (Bucket, Key) not in self.objects:
            raise S3Error("NoSuchKey")
        return {"Body": io.BytesIO(self.objects[(Bucket, Key)])}

    def put_object(self, Bucket, Key, Body, IfNoneMatch=None):
        if IfNoneMatch == "*" and (Bucket, Key) in self.objects:
            raise S3Error("PreconditionFailed")
        self.objects[(Bucket, Key)] = Body

    def delete_object(self, Bucket, Key):
        if (Bucket, Key) not in self.objects:
            raise S3Error("NoSuchKey")
        del self.objects[(Bucket, Key)]

    def doc(self, bucket, key):
        return json.loads(self.objects[(bucket, key)])


def fw_rules(admin=(ADMIN,), extra=()):
    return {"inbound_policy": "DROP", "outbound_policy": "ACCEPT", "outbound": [],
            "inbound": [{"label": "ssh-admin-only", "action": "ACCEPT", "protocol": "TCP", "ports": "22",
                         "addresses": {"ipv4": list(admin)}}, *extra]}


class FakeLinodeAPI:
    """The Linode API as a transport. Two tagged Linodes, one untagged, three firewalls."""

    def __init__(self):
        self.linodes = {"11": {"id": 11, "tags": ["lodestar", "main"], "created": "2026-11-08T19:00:00"},
                        "12": {"id": 12, "tags": ["lodestar", "worker"], "created": "2026-11-08T19:00:00"},
                        "99": {"id": 99, "tags": ["soak"], "created": "2026-11-01T00:00:00"}}
        self.firewalls = {"21": {"id": 21, "tags": ["lodestar"], "status": "enabled"},
                          "22": {"id": 22, "tags": ["lodestar"], "status": "enabled"},
                          "98": {"id": 98, "tags": [], "status": "enabled"}}
        self.rules = {"21": fw_rules(), "22": fw_rules(), "98": fw_rules()}
        self.devices = {"21": [{"entity": {"type": "linode", "id": 11}}], "22": [{"entity": {"type": "linode", "id": 12}}],
                        "98": [{"entity": {"type": "linode", "id": 99}}]}
        self.calls, self.fail_deletes = [], False

    def __call__(self, method, path, body):
        self.calls.append((method, path.split("?")[0]))
        p = path.split("?")[0]
        if method == "DELETE" and self.fail_deletes:
            raise LinodeError("Linode API DELETE failed: HTTP 500")
        if p == "/account":
            return {"euuid": "EUUID-FLEET"}
        if p == "/linode/instances":
            return {"data": list(self.linodes.values()), "page": 1, "pages": 1}
        if p == "/networking/firewalls":
            return {"data": list(self.firewalls.values()), "page": 1, "pages": 1}
        parts = p.strip("/").split("/")
        if parts[:2] == ["linode", "instances"] and method == "DELETE":
            del self.linodes[parts[2]]
            return {}
        if parts[:2] == ["networking", "firewalls"]:
            fid = parts[2]
            if len(parts) == 3 and method == "DELETE":
                del self.firewalls[fid]
                return {}
            if parts[3] == "rules":
                if method == "PUT":
                    self.rules[fid] = body
                    return {}
                return self.rules[fid]
            if parts[3] == "devices":
                return {"data": self.devices[fid], "page": 1, "pages": 1}
        raise AssertionError(f"unexpected {method} {p}")


def keygen_fake(n=[0]):
    n[0] += 1
    blob = base64.b64encode(b"\x00\x00\x00\x0bssh-ed25519" + bytes([n[0]]) * 32).decode()
    return f"-----BEGIN OPENSSH PRIVATE KEY-----\nPRIVATEHOSTKEY{n[0]}\n-----END OPENSSH PRIVATE KEY-----\n", f"ssh-ed25519 {blob}"


class FakeTerraform:
    """Stands in for subprocess.run(["terraform", ...])."""

    def __init__(self, lock_held=0, fail=()):
        self.calls, self.lock_held, self.fail, self.varfiles = [], lock_held, set(fail), []

    def __call__(self, argv, cwd, env, capture_output, text):
        verb = argv[1]
        self.calls.append(verb)
        for a in argv:
            if a.startswith("-var-file="):
                self.varfiles.append(json.loads(Path(a.split("=", 1)[1]).read_text()))
        out, rc = f"{verb} ok", 0
        if verb in ("apply", "destroy") and self.lock_held:
            self.lock_held -= 1
            out, rc = "Error: Error acquiring the state lock", 1
        if verb in self.fail:
            out, rc = f"{verb} failed", 1
        if verb == "output":
            out = json.dumps({"fleet_hosts": {"sensitive": True, "value": {
                "main": {"address": "203.0.113.11", "firewall_id": "21", "admin_ipv4": [ADMIN]},
                "worker-01": {"address": "203.0.113.12", "firewall_id": "22", "admin_ipv4": [ADMIN]}}}})
        return mock.Mock(returncode=rc, stdout=out, stderr="")


ENV = {"STATE_BUCKET": "state-b", "EVIDENCE_BUCKET": "evidence-b", "CONTROL_BUCKET": "control-b",
       "OWNER_ADMIN_CIDR": ADMIN, "FLEET_SSH_PUBLIC_KEY": "ssh-ed25519 AAAAfleet", "OWNER_SSH_PUBLIC_KEY": "ssh-ed25519 AAAAowner",
       "FLEET_LINODE_ACCOUNT_EUUID": "EUUID-FLEET", "GITHUB_TOKEN": "gh", "GITHUB_REPOSITORY": "o/r"}


def backend(s3=None, api=None, tf=None, runs=None, clock=None):
    s3, api, tf = s3 or FakeS3(), api or FakeLinodeAPI(), tf or FakeTerraform()
    clock = clock or Clock()
    runs = runs if runs is not None else {}
    pins = []
    tf_runner = apply.Terraform(Path("/nonexistent"), {}, apply.Evidence(s3, "evidence-b", "7-1"), run=tf,
                                pin=lambda c: pins.append(c) or Path("/pinned"))
    tf.pins = pins
    b = apply.LiveBackend(s3, Linode(api), tf_runner,
                          ENV, L, clock, lookup_run=lambda rid: runs.get(rid), meta=lambda: RANGES, keygen=keygen_fake)
    return b, s3, api, tf


def noop():
    pass


class LinodeClient(unittest.TestCase):
    def test_tag_filter_and_refusals(self):
        api = FakeLinodeAPI()
        ln = Linode(api)
        self.assertEqual({n["id"] for n in ln.linodes()}, {11, 12})
        self.assertEqual({f["id"] for f in ln.firewalls()}, {21, 22})
        for call in (lambda: ln.delete_linode(99), lambda: ln.delete_firewall(98),
                     lambda: ln.put_firewall_rules(98, {}), lambda: ln.firewall_rules(98)):
            with self.assertRaises(LinodeError):
                call()
        self.assertIn("99", api.linodes)
        ln.check_account("EUUID-FLEET")
        for wrong in ("EUUID-OTHER", ""):
            with self.assertRaises(LinodeError):
                ln.check_account(wrong)


class Audit(unittest.TestCase):
    R = audit.actions_ranges(RANGES)

    def job(self, run="5", ip="4.148.1.2/32", **over):
        return dict(audit.job_rule(run, "1", ip), **over)

    def problems(self, rules, devices=None, admin=(ADMIN,), status="enabled"):
        devices = devices if devices is not None else [{"entity": {"type": "linode", "id": 11}}]
        return audit.audit_firewall({"status": status}, rules, devices, list(admin) if admin is not None else None,
                                    self.R, {"11", "12"})

    def test_ranges(self):
        self.assertEqual([str(n) for n in self.R], ["4.148.0.0/16"])
        with self.assertRaises(ValueError):
            audit.actions_ranges({"actions": ["2a01:111::/32"]})

    def test_clean_firewall_passes(self):
        self.assertEqual(self.problems(fw_rules(extra=[self.job()])), [])

    def test_each_mismatch(self):
        cases = {
            "disabled": dict(rules=fw_rules(), status="disabled"),
            "accept policy": dict(rules=dict(fw_rules(), inbound_policy="ACCEPT")),
            "unattached": dict(rules=fw_rules(), devices=[]),
            "attached elsewhere": dict(rules=fw_rules(), devices=[{"entity": {"type": "linode", "id": 99}}]),
            "nodebalancer": dict(rules=fw_rules(), devices=[{"entity": {"type": "nodebalancer", "id": 11}}]),
            "admin changed": dict(rules=fw_rules(admin=("0.0.0.0/0",))),
            "admin missing": dict(rules=dict(fw_rules(), inbound=[])),
            "unrecorded": dict(rules=fw_rules(), admin=None),
            "out of range": dict(rules=fw_rules(extra=[self.job(ip="8.8.8.8/32")])),
            "wide": dict(rules=fw_rules(extra=[self.job(ip="4.148.0.0/24")])),
            "two addresses": dict(rules=fw_rules(extra=[dict(self.job(), addresses={"ipv4": ["4.148.1.2/32", "4.148.1.3/32"]})])),
            "ipv6": dict(rules=fw_rules(extra=[dict(self.job(), addresses={"ipv4": ["4.148.1.2/32"], "ipv6": ["::/0"]})])),
            "all ports": dict(rules=fw_rules(extra=[self.job(ports="1-65535")])),
            "udp": dict(rules=fw_rules(extra=[self.job(protocol="UDP")])),
            "bad label": dict(rules=fw_rules(extra=[self.job(label="debug")])),
            "two for one run": dict(rules=fw_rules(extra=[self.job(), self.job(ip="4.148.1.3/32", label="job-5-2")])),
        }
        for name, kw in cases.items():
            with self.subTest(name):
                got = self.problems(**kw)
                self.assertTrue(got, name)
                self.assertFalse(any(c.isdigit() for c in "".join(got)))  # no addresses or IDs in problems

    def test_sweep(self):
        now = T0
        live = {"status": "in_progress", "run_started_at": fmt(now - timedelta(minutes=10))}
        old = {"status": "in_progress", "run_started_at": fmt(now - timedelta(minutes=91))}
        done = {"status": "completed", "run_started_at": fmt(now - timedelta(minutes=5))}
        rules = fw_rules(extra=[self.job("1"), self.job("2"), self.job("3"), self.job("4"), {"label": "odd"}])
        runs = {"1": live, "2": old, "3": done}
        kept, n = audit.sweep(rules, runs.get, now)
        self.assertEqual(n, 3)  # old, done, and unknown run 4
        self.assertEqual([r["label"] for r in kept["inbound"]], ["ssh-admin-only", "job-1-1", "odd"])

    def test_open_and_close(self):
        opened = audit.open_rule(fw_rules(), "5", "1", "4.148.1.2/32", self.R)
        self.assertEqual(self.problems(opened), [])
        self.assertEqual(audit.close_rule(opened, "5", "1"), fw_rules())
        with self.assertRaises(ValueError):
            audit.open_rule(fw_rules(), "5", "1", "8.8.8.8/32", self.R)


class Backend(unittest.TestCase):
    def test_first_tombstone_only_with_nothing_in_the_account(self):
        b, s3, api, _ = backend()
        with self.assertRaises(FleetUnknown):  # 404, but tagged Linodes exist
            b.read_fleet()
        api.linodes = {"99": api.linodes["99"]}
        self.assertIsNone(b.read_fleet())
        self.assertEqual(s3.doc("state-b", FLEET_KEY)["fleet"], None)
        self.assertIsNone(b.read_fleet())  # now from the tombstone

    def test_any_read_error_means_a_fleet_may_exist(self):
        b, *_ = backend(s3=FakeS3(deny={("state-b", FLEET_KEY)}))
        with self.assertRaises(FleetUnknown):
            b.read_fleet()
        b, s3, *_ = backend()
        s3.objects[("state-b", FLEET_KEY)] = b"{not json"
        with self.assertRaises(FleetUnknown):
            b.read_fleet()
        s3.objects[("state-b", FLEET_KEY)] = b'{"version": 1}'
        with self.assertRaises(FleetUnknown):
            b.read_fleet()

    def test_mismatch_flag_fails_closed(self):
        b, s3, api, _ = backend()
        self.assertTrue(b.mismatch())  # absent: only a readable false means no mismatch
        api.linodes, api.firewalls = {}, {}
        b.read_fleet()  # the first tombstone also writes the flag as false
        self.assertFalse(b.mismatch())
        b.set_mismatch(["x"])
        self.assertTrue(b.mismatch())
        s3.objects[("control-b", MISMATCH_KEY)] = b"junk"
        self.assertTrue(b.mismatch())
        b2, *_ = backend(s3=FakeS3(deny={("control-b", MISMATCH_KEY)}))
        self.assertTrue(b2.mismatch())

    def test_create_records_first_then_fills_slots(self):
        b, s3, _, tf = backend()
        fences = []
        f = new_fleet(L, "screen", "a" * 40, T0)
        filled = b.create(f, lambda: fences.append(1))
        self.assertGreaterEqual(len(fences), 3)
        self.assertEqual(tf.calls, ["init", "apply", "output"])
        self.assertEqual(tf.pins, ["a" * 40])  # Terraform runs at the fleet's pinned commit
        main = filled["roles"]["main"]
        self.assertEqual((main["address"], main["firewall_id"], main["admin_ipv4"]), ("203.0.113.11", "21", [ADMIN]))
        self.assertTrue(main["fingerprint"].startswith("SHA256:"))
        self.assertEqual(main["fingerprint"], apply.fingerprint(main["host_key"]))
        self.assertEqual(s3.doc("state-b", FLEET_KEY), filled)
        doc = json.dumps(filled)
        self.assertNotIn("PRIVATE", doc)  # only public halves in fleet.json
        varfile = tf.varfiles[0]
        self.assertEqual(set(varfile["host_keys"]), {"main", "worker-01"})
        self.assertEqual(varfile["extra_ssh_public_keys"], ["ssh-ed25519 AAAAfleet"])
        self.assertEqual(varfile["ssh_admin_ipv4_cidr"], ADMIN)
        evidence = b"".join(v for (bk, _), v in s3.objects.items() if bk == "evidence-b")
        self.assertNotIn(b"PRIVATEHOSTKEY", evidence)
        self.assertNotIn(b"203.0.113", evidence)  # terraform output never reaches the evidence bucket

    def test_create_stops_when_the_lock_is_lost(self):
        b, s3, _, tf = backend()
        calls = []

        def fence():
            calls.append(1)
            if len(calls) == 2:
                raise LockLost("lost")
        with self.assertRaises(LockLost):
            b.create(new_fleet(L, "screen", "a" * 40, T0), fence)
        self.assertNotIn("apply", tf.calls)
        self.assertIn(("state-b", FLEET_KEY), s3.objects)  # the expiry is recorded

    def test_destroy_retries_a_held_lock_then_sweeps_through_the_api(self):
        b, s3, api, tf = backend(tf=FakeTerraform(lock_held=5))
        self.assertTrue(b.destroy(noop))
        self.assertEqual(tf.pins, [None])  # nothing recorded: the current configuration
        self.assertEqual(tf.calls.count("destroy"), 3)  # retried, never force-unlock
        self.assertNotIn("force-unlock", tf.calls)
        self.assertEqual(set(api.linodes), {"99"})  # tagged ones deleted, the untagged one untouched
        self.assertEqual(set(api.firewalls), {"98"})

    def test_finish_down_books_spend_tombstones_and_clears_the_flag(self):
        clock = Clock(T0 + timedelta(hours=10))
        b, s3, *_ = backend(clock=clock)
        f = new_fleet(L, "screen", "a" * 40, T0)
        b.set_mismatch(["x"])
        b.finish_down(f, noop)
        self.assertAlmostEqual(s3.doc("state-b", LEDGER_KEY)["2026-11"], round(0.0795 * 10, 4))
        # Past the expiry, accrual keeps counting: a fleet that outlives it is still billed.
        b.finish_down(new_fleet(L, "screen", "a" * 40, T0 - timedelta(hours=50)), noop)
        self.assertAlmostEqual(s3.doc("state-b", LEDGER_KEY)["2026-11"], round(0.0795 * 10, 4) + round(0.0795 * 60, 4))
        self.assertIsNone(s3.doc("state-b", FLEET_KEY)["fleet"])
        self.assertIs(s3.doc("control-b", MISMATCH_KEY)["mismatch"], False)

    def test_audit_sweeps_under_the_fence_then_checks(self):
        b, s3, api, _ = backend(runs={"5": {"status": "completed", "run_started_at": fmt(T0)}})
        f = new_fleet(L, "screen", "a" * 40, T0)
        b.create(f, noop)
        api.rules["21"]["inbound"].append(audit.job_rule("5", "1", "4.148.1.2/32"))
        fences = []
        self.assertEqual(b.audit(lambda: fences.append(1)), [])
        self.assertEqual(fences, [1])
        self.assertEqual(len(api.rules["21"]["inbound"]), 1)
        api.rules["22"]["inbound_policy"] = "ACCEPT"
        self.assertEqual(b.audit(noop), ["a fleet firewall's inbound policy isn't DROP"])

    def test_job_rules_open_and_close_fenced(self):
        b, s3, api, _ = backend()
        f = b.create(new_fleet(L, "screen", "a" * 40, T0), noop)
        fences = []
        b.set_job_rules(f, "5", "1", "4.148.1.2/32", lambda: fences.append(1))
        self.assertEqual(len(fences), 2)
        self.assertEqual(len(api.rules["21"]["inbound"]), 2)
        b.set_job_rules(f, "5", "1", None, noop)
        self.assertEqual(api.rules["21"], fw_rules())
        self.assertEqual(apply.known_hosts(f).count("\n"), 2)

    def test_token_secret_shapes(self):
        self.assertEqual(apply.linode_token("tok\n"), "tok")
        self.assertEqual(apply.linode_token('{"token": "tok"}'), "tok")
        with self.assertRaises(ValueError):
            apply.linode_token("  ")
        oidc = {"AWS_FLEET_ROLE_ARN": "r", "ACTIONS_ID_TOKEN_REQUEST_URL": "u", "ACTIONS_ID_TOKEN_REQUEST_TOKEN": "t"}
        self.assertEqual(apply.missing_live_env(dict(ENV, **oidc)), [])
        self.assertEqual(apply.missing_live_env(ENV), sorted(oidc))  # id-token: write and the role ARN are required
        self.assertIn("OWNER_SSH_PUBLIC_KEY", apply.missing_live_env(dict(ENV, OWNER_SSH_PUBLIC_KEY=" ")))


class FailClosed(unittest.TestCase):
    """B1-B3 of the self-review: the audit's failure never stops lifetime
    enforcement, down always sweeps, and the marker holds while resources remain."""

    @classmethod
    def setUpClass(cls):
        cls.repo, cls.c = make_repo()

    def overdue(self, api=None, minutes_past=60, **kw):
        b, s3, api, tf = backend(api=api, **kw)
        clock = Clock(T0)
        b.clock = clock
        f = new_fleet(L, "screen", self.c["head"], T0 - timedelta(hours=44, minutes=minutes_past))
        b.create(f, noop)
        return b, s3, api, tf, clock

    def reap(self, b, clock, store=None):
        out, store = io.StringIO(), store if store is not None else MemoryStore()
        with redirect_stdout(out), redirect_stderr(out):
            rc = fleet.main(["reap"], env=event_env(FLEET_LIVE="true", **ENV), backend=b, store=store,
                            transport=FakeGitHub(), clock=clock, sleep=clock.sleep, repo=self.repo)
        return rc, out.getvalue(), store

    def test_audit_failure_never_stops_lifetime_enforcement(self):
        def boom():
            raise RuntimeError("GitHub API failed: HTTP 503")
        for label, meta in (("meta raises", boom), ("meta empty", lambda: {})):
            with self.subTest(label):
                b, s3, api, _, clock = self.overdue()
                b.meta = meta
                rc, out, _ = self.reap(b, clock)
                self.assertEqual(rc, 0)
                self.assertIn("the firewall audit couldn't run; escalate", out)
                self.assertNotIn("fleet.json is invalid", out)
                self.assertIn("reaper: emergency", out)
                self.assertFalse(api.linodes.keys() & {"11", "12"})  # the fleet still went down
                self.assertNotIn("PUT", [m for m, _ in api.calls])

    def test_actions_api_failure_sweeps_nothing(self):
        b, s3, api, _, clock = self.overdue(minutes_past=-120)  # within lifetime: only the audit runs
        for fid in ("21", "22"):
            api.rules[fid]["inbound"].append(audit.job_rule("5", "1", "4.148.1.2/32"))
        calls = []

        def lookup(rid):
            calls.append(rid)
            if len(calls) == 2:
                raise RuntimeError("GitHub API failed: HTTP 403")
            return None  # unknown run: stale
        b.lookup_run = lookup
        _, out, _ = self.reap(b, clock)
        self.assertIn("the firewall audit couldn't run; escalate", out)
        self.assertNotIn("PUT", [m for m, _ in api.calls])  # read first: nothing was half-swept
        self.assertEqual(len(api.rules["21"]["inbound"]), 2)

    def test_destroy_sweeps_whatever_failed_before(self):
        b, s3, api, tf = backend()
        b.create(new_fleet(L, "screen", self.c["head"], T0), noop)

        def bad_pin(c):
            raise __import__("subprocess").CalledProcessError(128, ["git"])
        b.tf.pin = bad_pin
        b.tf.evidence.s3 = None  # the evidence bucket is unreachable too
        self.assertTrue(b.destroy(noop))
        self.assertEqual(set(api.linodes), {"99"})

    def test_marker_holds_while_resources_remain(self):
        b, s3, api, _, clock = self.overdue(minutes_past=30)  # >15 min past expiry: pre-empt
        api.fail_deletes = True
        rc, out, store = self.reap(b, clock)
        self.assertIn("tagged resources remain; escalate", out)
        holder = json.loads(store.objects[LOCK_KEY][0])
        self.assertEqual(holder["kind"], "reaping")  # kept until its own expiry
        self.assertIsNotNone(s3.doc("state-b", FLEET_KEY)["roles"])  # no tombstone
        # The marker's own expiry: after 90 minutes a normal run can take the lock again.
        clock.sleep(91 * 60)
        self.assertEqual(FleetLock(store, 90, clock).acquire("8-1"), "acquired")

    def test_mismatch_flag_kept_when_down_leaves_resources(self):
        b, s3, api, _, clock = self.overdue(minutes_past=-120)
        api.rules["21"]["inbound_policy"] = "ACCEPT"
        api.fail_deletes = True
        _, out, store = self.reap(b, clock)
        self.assertIn("reaper: emergency (firewall audit mismatch)", out)
        self.assertIs(s3.doc("control-b", MISMATCH_KEY)["mismatch"], True)
        self.assertIn(LOCK_KEY, store.objects)

    def test_age_reap_takes_orphan_firewalls_and_up_refuses_while_one_exists(self):
        b, s3, api, _ = backend()
        api.devices["22"] = []
        b.delete_linodes(["11"])
        self.assertNotIn("22", api.firewalls)  # no device left: deleted with the age reap
        api.linodes = {}
        self.assertEqual(limits.check_up(L, "screen", b, T0), "a Lodestar firewall exists")

    def test_unreadable_ledger_never_strands_the_tombstone(self):
        b, s3, *_ = backend()
        s3.objects[("state-b", LEDGER_KEY)] = b'["not", "a", "ledger"]'
        with self.assertRaises(LedgerUnreadable):
            b.finish_down(new_fleet(L, "screen", "a" * 40, T0), noop)
        self.assertIsNone(s3.doc("state-b", FLEET_KEY)["fleet"])  # written first

    def test_credentials_are_minted_only_after_the_gates(self):
        with mock.patch.object(apply, "live_context", side_effect=AssertionError("minted too early")):
            out = io.StringIO()
            with redirect_stdout(out):
                rc = fleet.main(["act"], env=event_env("/fleet up screen", FLEET_LIVE="true", AUTHOR_ID="1", **ENV))
            self.assertEqual(rc, 0)

    def test_rule_writes_send_only_writable_fields(self):
        doc = dict(fw_rules(), version=7, fingerprint="abc")
        self.assertEqual(set(apply.writable(doc)), set(apply.RULE_FIELDS))


class Pin(unittest.TestCase):
    def test_pinned_tf_dir(self):
        self.assertEqual(apply.pinned_tf_dir(None), apply.TF_DIR)
        head = ancestry.head_of("HEAD", str(apply.TF_DIR.parents[2]))
        self.assertEqual(apply.pinned_tf_dir(head), apply.TF_DIR)
        calls = []
        with self.assertRaises(apply.ApplyFailed):  # not an ancestor of main: no worktree
            apply.pinned_tf_dir("f" * 40, run=lambda *a, **k: calls.append(a))
        self.assertEqual(calls, [])

    def test_fingerprint_format(self):
        # OpenSSH's SHA256 fingerprint: "SHA256:" and 43 unpadded base64 characters.
        pub = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl"
        fp = apply.fingerprint(pub)
        self.assertRegex(fp, r"^SHA256:[A-Za-z0-9+/]{43}$")
        self.assertNotEqual(fp, apply.fingerprint(pub[:-2] + "Kk"))


class LiveFlows(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.repo, cls.c = make_repo()

    def run_cli(self, sub, body, b, store=None, clock=None):
        out, gh, clock = io.StringIO(), FakeGitHub(), clock or Clock()
        env = event_env(body, FLEET_LIVE="true", **ENV)
        with redirect_stdout(out), redirect_stderr(out), \
                mock.patch.object(ancestry, "remote_main_head", return_value=self.c["head"]):
            rc = fleet.main([sub], env=env, backend=b, store=store or MemoryStore(), transport=gh,
                            clock=clock, sleep=clock.sleep, repo=self.repo)
        return rc, out.getvalue() + "\n".join(gh.bodies())

    def empty_account(self):
        api = FakeLinodeAPI()
        api.linodes, api.firewalls = {"99": api.linodes["99"]}, {"98": api.firewalls["98"]}
        return api

    def test_up_run_down(self):
        api = self.empty_account()
        b, s3, _, tf = backend(api=api)
        rc, out = self.run_cli("act", "/fleet up screen", b)
        self.assertEqual(rc, 0)
        self.assertIn("up screen: create main, worker", out)
        self.assertIn("`fleet.yml` (live)", out)
        _, out = self.run_cli("act", "/fleet run leak-test main", b)
        self.assertIn("step not built (OP-3)", out)
        _, out = self.run_cli("act", "/fleet down", b)
        self.assertIn("down: destroy the fleet", out)
        self.assertIsNone(s3.doc("state-b", FLEET_KEY)["fleet"])

    def test_unreadable_state_allows_only_down(self):
        b, *_ = backend(s3=FakeS3(deny={("state-b", FLEET_KEY)}))
        _, out = self.run_cli("act", "/fleet up screen", b)
        self.assertIn("fleet state unreadable: only down runs", out)
        self.assertIn("a fleet may exist", out)

    def test_reaper_audit_mismatch_preempts_and_goes_down(self):
        api = self.empty_account()
        b, s3, _, _ = backend(api=api)
        self.run_cli("act", "/fleet up screen", b)
        api.linodes.update({"11": {"id": 11, "tags": ["lodestar"], "created": "2026-11-08T19:00:00"}})
        api.firewalls["21"] = {"id": 21, "tags": ["lodestar"], "status": "enabled"}
        api.rules["21"] = dict(fw_rules(), inbound_policy="ACCEPT")
        api.devices["21"] = [{"entity": {"type": "linode", "id": 11}}]
        store = MemoryStore()
        _, out = self.run_cli("reap", "", b, store=store)
        self.assertIn("reaper: emergency (firewall audit mismatch)", out)
        self.assertIs(s3.doc("control-b", MISMATCH_KEY)["mismatch"], False)  # cleared once the inventory is empty
        self.assertIsNone(s3.doc("state-b", FLEET_KEY)["fleet"])
        self.assertNotIn(LOCK_KEY, store.objects)

    def test_no_private_value_reaches_logs_or_comments(self):
        api = self.empty_account()
        b, s3, _, _ = backend(api=api)
        outs = [self.run_cli("act", "/fleet up screen", b)[1], self.run_cli("act", "/fleet status", b)[1],
                self.run_cli("reap", "", b)[1], self.run_cli("act", "/fleet down", b)[1]]
        text = "\n".join(outs)
        for value in ("203.0.113.", "PRIVATEHOSTKEY", "ssh-ed25519", "SHA256:", ADMIN, "EUUID", "state-b",
                      "evidence-b", "control-b", "AAAAowner", "AAAAfleet"):
            self.assertNotIn(value, text)


class NoEcho(unittest.TestCase):
    def test_live_modules_never_print(self):
        for name in ("apply.py", "linode.py", "audit.py"):
            src = (HERE / name).read_text(encoding="utf-8")
            with self.subTest(module=name):
                for pattern in (r"\bprint\(", r"sys\.std(out|err)", r"add-mask", r"logging\."):
                    self.assertIsNone(re.search(pattern, src), pattern)

    def test_host_keys_never_pass_through_a_workflow_step(self):
        wf = (HERE.parents[1] / ".github" / "workflows" / "fleet.yml").read_text(encoding="utf-8")
        for word in ("ssh-keygen", "HOST_KEY", "host_key", "PRIVATE KEY", "tfvars"):
            self.assertNotIn(word, wf)


if __name__ == "__main__":
    unittest.main()
