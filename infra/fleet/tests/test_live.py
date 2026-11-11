"""Live mode (OP-2) with fakes for S3, the Linode API, Terraform, ssh-keygen,
the Actions API and /meta. No network, no cloud, no terraform binary."""

import base64
import io
import json
import re
import tempfile
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
RANGES = {"actions": ["198.51.100.0/24", "2001:db8::/32"]}  # RFC 5737 / 3849 documentation ranges
ADMIN = "203.0.113.4/32"
HERE = Path(__file__).resolve().parent.parent


class S3Error(Exception):
    def __init__(self, code):
        super().__init__(code)
        self.response = {"Error": {"Code": code}}


class FakeS3:
    def __init__(self, deny=()):
        self.objects, self.deny, self.puts, self.fail_after = {}, set(deny), [], {}

    def get_object(self, Bucket, Key):
        if (Bucket, Key) in self.fail_after:  # {(bucket, key): reads left before it starts failing}
            left = self.fail_after[(Bucket, Key)]
            self.fail_after[(Bucket, Key)] = left - 1
            if left <= 0:
                raise S3Error("ServiceUnavailable")
        if (Bucket, Key) in self.deny:
            raise S3Error("AccessDenied")
        if (Bucket, Key) not in self.objects:
            raise S3Error("NoSuchKey")
        return {"Body": io.BytesIO(self.objects[(Bucket, Key)])}

    def put_object(self, Bucket, Key, Body, IfNoneMatch=None):
        self.puts.append((Bucket, Key, IfNoneMatch))
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
        self.linodes = {"11": {"id": 11, "tags": ["lodestar", "test", "managed-terraform", "main"], "label": "lodestar-main-fleet", "type": "g6-standard-4", "region": "us-east", "ipv4": ["203.0.113.11"], "created": "2026-11-08T20:00:00"},
                        "12": {"id": 12, "tags": ["lodestar", "test", "managed-terraform", "worker"], "label": "lodestar-worker-01-fleet", "type": "g6-nanode-1", "region": "us-east", "ipv4": ["203.0.113.12"], "created": "2026-11-08T20:00:00"},
                        "99": {"id": 99, "tags": ["soak"], "created": "2026-11-01T00:00:00"}}
        self.firewalls = {"21": {"id": 21, "tags": ["lodestar", "test", "managed-terraform"], "label": "lodestar-main-fw-fleet", "created": "2026-11-08T20:00:00Z", "status": "enabled"},
                          "22": {"id": 22, "tags": ["lodestar", "test", "managed-terraform"], "label": "lodestar-worker-fw-fleet", "created": "2026-11-08T20:00:00Z", "status": "enabled"},
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
        if parts[:2] == ["linode", "instances"] and len(parts) == 4 and parts[3] == "firewalls":
            fws = [fid for fid, devs in self.devices.items() if any(str(d["entity"]["id"]) == parts[2] for d in devs)]
            return {"data": [{"id": int(f)} for f in fws], "page": 1, "pages": 1}
        if parts[:2] == ["linode", "instances"] and method == "DELETE":
            del self.linodes[parts[2]]
            for fid in self.devices:  # deleting a Linode detaches it from its firewall
                self.devices[fid] = [d for d in self.devices[fid] if str(d["entity"]["id"]) != parts[2]]
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
    blob = base64.b64encode(b"\x00\x00\x00\x0bssh-ed25519\x00\x00\x00\x20" + bytes([n[0]]) * 32).decode()
    return f"-----BEGIN OPENSSH PRIVATE KEY-----\nPRIVATEHOSTKEY{n[0]}\n-----END OPENSSH PRIVATE KEY-----\n", f"ssh-ed25519 {blob}"


class FakeTerraform:
    """Stands in for subprocess.run(["terraform", ...])."""

    def __init__(self, lock_held=0, fail=()):
        self.calls, self.lock_held, self.fail, self.varfiles, self.timeouts = [], lock_held, set(fail), [], []

    def __call__(self, argv, cwd, env, capture_output, text, timeout=None):
        self.timeouts.append((argv[1], timeout))
        verb = argv[1]
        self.calls.append(verb)
        for a in argv:
            if a.startswith("-var-file="):
                self.varfiles.append(json.loads(Path(a.split("=", 1)[1]).read_text()))
        out, rc = f"{verb} ok", 0
        if verb == "apply":
            defaults = FakeLinodeAPI()
            for ident in ("11", "12"):
                self.api.linodes.setdefault(ident, defaults.linodes[ident])
            for ident in ("21", "22"):
                self.api.firewalls.setdefault(ident, defaults.firewalls[ident])
                self.api.devices.setdefault(ident, defaults.devices[ident])

        if verb in ("apply", "destroy") and self.lock_held:
            self.lock_held -= 1
            out, rc = "Error: Error acquiring the state lock", 1
        if verb in self.fail:
            out, rc = f"{verb} failed", 1
        if verb == "output":
            out = json.dumps({"fleet_hosts": {"sensitive": True, "value": {
                "main": {"linode_id": "11", "address": "203.0.113.11", "firewall_id": "21", "admin_ipv4": [ADMIN]},
                "worker-01": {"linode_id": "12", "address": "203.0.113.12", "firewall_id": "22", "admin_ipv4": [ADMIN]}}}})
        return mock.Mock(returncode=rc, stdout=out, stderr="")


ENV = {"STATE_BUCKET": "state-b", "EVIDENCE_BUCKET": "evidence-b", "CONTROL_BUCKET": "control-b",
       "FLEET_BUCKET_OWNER": "111122223333",
       "OWNER_ADMIN_CIDR": ADMIN, "FLEET_SSH_PUBLIC_KEY": "ssh-ed25519 AAAAfleet", "OWNER_SSH_PUBLIC_KEY": "ssh-ed25519 AAAAowner",
       "FLEET_LINODE_ACCOUNT_EUUID": "EUUID-FLEET", "GITHUB_TOKEN": "gh", "GITHUB_REPOSITORY": "o/r"}


def backend(s3=None, api=None, tf=None, runs=None, clock=None):
    s3, api, tf = s3 or FakeS3(), api or FakeLinodeAPI(), tf or FakeTerraform()
    clock = clock or Clock()
    runs = runs if runs is not None else {}
    pins = []
    unpins = []
    tf_runner = apply.Terraform(Path("/nonexistent"), {}, apply.Evidence(s3, "evidence-b", "7-1"), run=tf,
                                pin=lambda c: pins.append(c) or Path("/pinned"), unpin=unpins.append)
    tf.api = api
    tf.pins, tf.unpins = pins, unpins
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

    def job(self, run="5", ip="198.51.100.7/32", **over):
        return dict(audit.job_rule(run, "1", ip), **over)

    def problems(self, rules, devices=None, admin=(ADMIN,), status="enabled"):
        devices = devices if devices is not None else [{"entity": {"type": "linode", "id": 11}}]
        return audit.audit_firewall({"status": status}, rules, devices, list(admin) if admin is not None else None,
                                    self.R, {"11", "12"})

    def test_ranges(self):
        self.assertEqual([str(n) for n in self.R], ["198.51.100.0/24"])
        with self.assertRaises(ValueError):
            audit.actions_ranges({"actions": ["2001:db8::/32"]})

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
            "out of range": dict(rules=fw_rules(extra=[self.job(ip="192.0.2.9/32")])),
            "wide": dict(rules=fw_rules(extra=[self.job(ip="198.51.100.0/25")])),
            "two addresses": dict(rules=fw_rules(extra=[dict(self.job(), addresses={"ipv4": ["198.51.100.7/32", "198.51.100.8/32"]})])),
            "ipv6": dict(rules=fw_rules(extra=[dict(self.job(), addresses={"ipv4": ["198.51.100.7/32"], "ipv6": ["::/0"]})])),
            "all ports": dict(rules=fw_rules(extra=[self.job(ports="1-65535")])),
            "udp": dict(rules=fw_rules(extra=[self.job(protocol="UDP")])),
            "bad label": dict(rules=fw_rules(extra=[self.job(label="debug")])),
            "two for one run": dict(rules=fw_rules(extra=[self.job(), self.job(ip="198.51.100.8/32", label="job-5-2")])),
        }
        for name, kw in cases.items():
            with self.subTest(name):
                got = self.problems(**kw)
                self.assertTrue(got, name)
                self.assertFalse(any(c.isdigit() for c in "".join(got)))  # no addresses or IDs in problems

    def test_sweep(self):
        now = T0
        run = {"path": ".github/workflows/fleet.yml", "run_attempt": 1}
        live = dict(run, status="in_progress", run_started_at=fmt(now - timedelta(minutes=10)))
        old = dict(run, status="in_progress", run_started_at=fmt(now - timedelta(minutes=91)))
        done = dict(run, status="completed", run_started_at=fmt(now - timedelta(minutes=5)))
        rules = fw_rules(extra=[self.job("1"), self.job("2"), self.job("3"), self.job("4"), {"label": "odd"}])
        runs = {"1": live, "2": old, "3": done}
        kept, n, failed = audit.sweep(rules, runs.get, now)
        self.assertEqual((n, failed), (3, 0))  # old, done, and unknown run 4
        self.assertEqual([r["label"] for r in kept["inbound"]], ["ssh-admin-only", "job-1-1", "odd"])

        def flaky(rid):
            if rid == "2":
                raise RuntimeError("GitHub API failed")
            return runs.get(rid)
        kept, n, failed = audit.sweep(rules, flaky, now)
        self.assertEqual((n, failed), (3, 1))  # a failed lookup is swept and counted

    def test_a_run_is_live_only_as_fleet_yml_at_its_attempt(self):
        now = T0
        base = {"path": ".github/workflows/fleet.yml", "run_attempt": 2, "status": "in_progress",
                "run_started_at": fmt(now - timedelta(minutes=5))}
        self.assertTrue(audit.run_is_live(base, "2", now))
        self.assertFalse(audit.run_is_live(base, "1", now))  # a different attempt
        self.assertFalse(audit.run_is_live(dict(base, path=".github/workflows/ci.yml"), "2", now))
        self.assertTrue(audit.run_is_live(dict(base, path=".github/workflows/fleet.yml@refs/heads/main"), "2", now))
        self.assertFalse(audit.run_is_live(dict(base, path=".github/workflows/fleet.yml.bak"), "2", now))
        self.assertFalse(audit.run_is_live(dict(base, path="x/.github/workflows/fleet.yml"), "2", now))
        self.assertFalse(audit.run_is_live(None, "2", now))

    def test_more_shapes(self):
        self.assertTrue(self.problems(fw_rules(extra=[self.job(action="DROP")])))  # a job rule must be ACCEPT
        two_admins = dict(fw_rules(), inbound=fw_rules()["inbound"] * 2)
        self.assertIn("a fleet firewall's admin rule isn't exactly the recorded one", self.problems(two_admins))

    def test_open_and_close(self):
        opened = audit.open_rule(fw_rules(), "5", "1", "198.51.100.7/32", self.R)
        self.assertEqual(self.problems(opened), [])
        self.assertEqual(audit.close_rule(opened, "5", "1"), fw_rules())
        with self.assertRaises(ValueError):
            audit.open_rule(fw_rules(), "5", "1", "192.0.2.9/32", self.R)


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

    def test_legacy_deletion_is_disabled_before_any_api_or_terraform_call(self):
        b, s3, api, tf = backend()
        for call in (lambda: b.destroy(noop), lambda: b.delete_linodes([11])):
            with self.assertRaisesRegex(apply.ApplyFailed, "legacy live deletion disabled"):
                call()
        self.assertEqual(api.calls, [])
        self.assertEqual(tf.calls, [])


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
        api.rules["21"]["inbound"].append(audit.job_rule("5", "1", "198.51.100.7/32"))
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
        b.set_job_rules(f, "5", "1", "198.51.100.7/32", lambda: fences.append(1))
        self.assertEqual(len(fences), 2)
        self.assertEqual(len(api.rules["21"]["inbound"]), 2)
        b.set_job_rules(f, "5", "1", None, noop)
        self.assertEqual(api.rules["21"], fw_rules())
        self.assertEqual(apply.known_hosts(f).count("\n"), 2)

    def test_first_tombstone_survives_a_lost_response(self):
        class LostResponse(FakeS3):
            def put_object(self, Bucket, Key, Body, IfNoneMatch=None):
                super().put_object(Bucket, Key, Body, IfNoneMatch)
                if Key == FLEET_KEY:
                    raise S3Error("RequestTimeout")  # it landed; the answer didn't
        b, s3, api, _ = backend(s3=LostResponse())
        api.linodes, api.firewalls = {}, {}
        self.assertIsNone(b.read_fleet())  # re-read once: a tombstone
        self.assertIs(s3.doc("control-b", MISMATCH_KEY)["mismatch"], False)

        class Raced(FakeS3):  # someone else wrote a fleet between our read and our put
            def put_object(self, Bucket, Key, Body, IfNoneMatch=None):
                if Key == FLEET_KEY and IfNoneMatch == "*":
                    self.objects[(Bucket, Key)] = json.dumps(new_fleet(L, "screen", "a" * 40, T0)).encode()
                super().put_object(Bucket, Key, Body, IfNoneMatch)
        b, s3, api, _ = backend(s3=Raced())
        api.linodes, api.firewalls = {}, {}
        with self.assertRaises(FleetUnknown):
            b.read_fleet()
        self.assertNotIn(("control-b", MISMATCH_KEY), s3.objects)

        class Unreadable(FakeS3):  # the put fails and so does the re-read
            def put_object(self, Bucket, Key, Body, IfNoneMatch=None):
                if Key == FLEET_KEY:
                    self.deny.add((Bucket, Key))
                    raise S3Error("RequestTimeout")
                super().put_object(Bucket, Key, Body, IfNoneMatch)
        b, s3, api, _ = backend(s3=Unreadable())
        api.linodes, api.firewalls = {}, {}
        with self.assertRaises(FleetUnknown):
            b.read_fleet()
        self.assertNotIn(("control-b", MISMATCH_KEY), s3.objects)

    def test_first_tombstone_is_conditional(self):
        b, s3, api, _ = backend()
        api.linodes = {}
        b.read_fleet()
        self.assertIn(("state-b", FLEET_KEY, "*"), s3.puts)

    def test_403_with_an_empty_account_still_means_a_fleet_may_exist(self):
        b, s3, api, _ = backend(s3=FakeS3(deny={("state-b", FLEET_KEY)}))
        api.linodes, api.firewalls = {}, {}
        with self.assertRaises(FleetUnknown):
            b.read_fleet()
        self.assertNotIn(("state-b", FLEET_KEY), s3.objects)  # no tombstone over a fleet.json it couldn't read

    def test_terraform_timeouts_count_as_failure(self):
        calls = []

        def slow(argv, **kw):
            calls.append((argv[1], kw["timeout"]))
            raise __import__("subprocess").TimeoutExpired(argv, kw["timeout"])
        tf = apply.Terraform(Path("/x"), {}, apply.Evidence(FakeS3(), "e", "1"), run=slow)
        with self.assertRaises(apply.ApplyFailed):
            tf.destroy("v.json")
        self.assertEqual(calls, [("destroy", apply.TF_TIMEOUTS["destroy"])])  # only a held lock is retried
        b, s3, api, ftf = backend()
        b.create(new_fleet(L, "screen", "a" * 40, T0), noop)
        self.assertTrue(all(t == apply.TF_TIMEOUTS[v] for v, t in ftf.timeouts))
        self.assertEqual(ftf.unpins, [Path("/pinned")])  # the pinned worktree is removed after use
        with self.assertRaises(apply.ApplyFailed):
            b.destroy(noop)
        self.assertEqual(len(ftf.unpins), 1)

    def test_outputs_parse_stdout_only(self):
        def run(argv, **kw):
            return mock.Mock(returncode=0, stdout='{"fleet_hosts": {"value": {}}}', stderr="Warning: something")
        tf = apply.Terraform(Path("/x"), {}, apply.Evidence(FakeS3(), "e", "1"), run=run)
        self.assertEqual(tf.outputs(), {"fleet_hosts": {"value": {}}})

    def test_terraform_env_holds_no_github_or_oidc_token(self):
        for name in apply.TF_ENV_PASS:
            self.assertNotRegex(name, r"GITHUB|ACTIONS_|TOKEN|AWS_")

    def test_pages_past_fifty_raise(self):
        ln = Linode(lambda m, p, b: {"data": [], "page": 1, "pages": 99})
        with self.assertRaises(LinodeError):
            ln.linodes()

    def test_token_secret_shapes(self):
        self.assertEqual(apply.linode_token("tok\n"), "tok")
        self.assertEqual(apply.linode_token('{"token": "tok"}'), "tok")
        with self.assertRaises(ValueError):
            apply.linode_token("  ")
        oidc = {"AWS_FLEET_ROLE_ARN": "r", "ACTIONS_ID_TOKEN_REQUEST_URL": "u", "ACTIONS_ID_TOKEN_REQUEST_TOKEN": "t"}
        self.assertEqual(apply.missing_live_env(dict(ENV, **oidc)), [])
        self.assertEqual(apply.missing_live_env(ENV), sorted(oidc))  # id-token: write and the role ARN are required
        self.assertIn("OWNER_SSH_PUBLIC_KEY", apply.missing_live_env(dict(ENV, OWNER_SSH_PUBLIC_KEY=" ")))


class OwnedBuckets(unittest.TestCase):
    """P8: every cloud-fleet S3 call passes ExpectedBucketOwner."""

    def test_every_call_names_the_owner(self):
        calls = []

        class Client:
            def __getattr__(self, name):
                return lambda **kw: calls.append((name, kw)) or {"Body": io.BytesIO(b"{}"), "ETag": '"1"'}
        s3 = apply.OwnedS3(Client(), "111122223333")
        s3.get_object(Bucket="b", Key="k")
        s3.put_object(Bucket="b", Key="k", Body=b"x", IfNoneMatch="*")
        s3.delete_object(Bucket="b", Key="k", IfMatch='"1"')
        self.assertEqual([n for n, _ in calls], ["get_object", "put_object", "delete_object"])
        self.assertTrue(all(kw["ExpectedBucketOwner"] == "111122223333" for _, kw in calls))
        with self.assertRaises(AttributeError):  # nothing else is reachable through it
            s3.list_objects_v2(Bucket="b")

    def test_the_owner_must_be_an_account_id(self):
        for bad in ("", "11112222333", "1111222233334", "11112222333x", " 111122223333", "١١١١٢٢٢٢٣٣٣٣"):
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                apply.OwnedS3(object(), bad)

    def test_the_backend_and_evidence_go_through_it(self):
        calls = []

        class Client:
            def get_object(self, **kw):
                calls.append(kw)
                raise S3Error("NoSuchKey")

            def put_object(self, **kw):
                calls.append(kw)
        b, *_ = backend(s3=apply.OwnedS3(Client(), "111122223333"))
        b.set_mismatch(["x"])
        b.mismatch_state()
        apply.Evidence(b.s3, "evidence-b", "1-1").put("init", "text")
        self.assertTrue(calls)
        self.assertTrue(all(kw.get("ExpectedBucketOwner") == "111122223333" for kw in calls))

    def test_live_settings_include_the_owner(self):
        self.assertIn("FLEET_BUCKET_OWNER", apply.missing_live_env({}))


class FailClosed(unittest.TestCase):
    """B1-B3 of the self-review: the audit's failure never stops lifetime
    enforcement, down always sweeps, and the marker holds while resources remain."""

    @classmethod
    def setUpClass(cls):
        cls.repo, cls.c = make_repo()











    def test_recorded_firewall_deleted_untagged_or_detached(self):
        def case(mutate):
            b, s3, api, _ = backend()
            b.create(new_fleet(L, "screen", self.c["head"], T0), noop)
            b._clear_mismatch()
            mutate(api)
            return b.audit(noop)
        self.assertEqual(case(lambda api: None), [])
        deleted = case(lambda api: (api.firewalls.pop("21"), api.devices.pop("21")))
        self.assertIn("a recorded fleet firewall is missing or untagged", deleted)
        self.assertIn("a fleet Linode isn't behind exactly one firewall", deleted)
        untagged = case(lambda api: api.firewalls["21"].__setitem__("tags", []))
        self.assertIn("a recorded fleet firewall is missing or untagged", untagged)
        self.assertIn("a fleet Linode's firewall isn't a recorded, tagged fleet firewall", untagged)

        def detach_and_untag(api):
            api.devices["21"] = []
            api.firewalls["21"]["tags"] = []
        detached = case(detach_and_untag)
        self.assertIn("a fleet Linode isn't behind exactly one firewall", detached)
        self.assertIn("a recorded fleet firewall is missing or untagged", detached)

        def second_firewall(api):
            api.devices["98"].append({"entity": {"type": "linode", "id": 11}})
        self.assertIn("a fleet Linode isn't behind exactly one firewall", case(second_firewall))









    def test_unreadable_ledger_never_strands_the_tombstone(self):
        b, s3, *_ = backend()
        s3.objects[("state-b", LEDGER_KEY)] = b'["not", "a", "ledger"]'
        with self.assertRaises(LedgerUnreadable):
            b.finish_down(new_fleet(L, "screen", "a" * 40, T0), noop)
        self.assertIsNone(s3.doc("state-b", FLEET_KEY)["fleet"])  # written first

    def dispatch_down(self, b, clock, store, head=None):
        out, gh = io.StringIO(), FakeGitHub()
        with redirect_stdout(out), redirect_stderr(out), \
                mock.patch.object(ancestry, "remote_main_head", return_value=head or self.c["head"]):
            rc = fleet.main(["dispatch-down"], env=event_env(FLEET_LIVE="true", **ENV), backend=b, store=store,
                            transport=gh, clock=clock, sleep=clock.sleep, repo=self.repo)
        self.posted = gh.bodies()
        return rc, out.getvalue()










    def test_up_failing_with_any_error_sets_the_flag(self):
        class SecondWriteFails(FakeS3):  # the slot write (not ApplyFailed) fails
            def put_object(self, Bucket, Key, Body, IfNoneMatch=None):
                if Key == FLEET_KEY and any(k == FLEET_KEY for _, k, _ in self.puts):
                    raise S3Error("InternalError")
                super().put_object(Bucket, Key, Body, IfNoneMatch)
        b, s3, api, tf = backend(s3=SecondWriteFails())
        b._clear_mismatch()
        with self.assertRaises(S3Error):
            b.create(new_fleet(L, "screen", "a" * 40, T0), noop)
        self.assertEqual(tf.calls, ["init", "apply", "output"])
        self.assertIs(s3.doc("control-b", MISMATCH_KEY)["mismatch"], True)
        # LockLost after the first write sets it too (the flag write itself is unfenced: fail closed).
        b, s3, *_ = backend()
        b._clear_mismatch()
        calls = []

        def fence():
            calls.append(1)
            if len(calls) == 2:
                raise LockLost("lost")
        with self.assertRaises(LockLost):
            b.create(new_fleet(L, "screen", "a" * 40, T0), fence)
        self.assertIs(s3.doc("control-b", MISMATCH_KEY)["mismatch"], True)

    def test_up_that_fails_after_its_first_write_sets_the_flag(self):
        for fail in ("output", "apply"):
            with self.subTest(fail=fail):
                b, s3, api, tf = backend(tf=FakeTerraform(fail={fail}))
                b._clear_mismatch()
                with self.assertRaises(apply.ApplyFailed):
                    b.create(new_fleet(L, "screen", "a" * 40, T0), noop)
                self.assertIn(("state-b", FLEET_KEY), s3.objects)  # the expiry is recorded
                self.assertIs(s3.doc("control-b", MISMATCH_KEY)["mismatch"], True)  # down-only until down

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
    def setUp(self):
        repo, _ = make_repo()
        patch = mock.patch.object(apply, "TF_DIR", Path(repo) / "infra/akamai/terraform")
        patch.start()
        self.addCleanup(patch.stop)

    def test_pinned_tf_dir(self):
        self.assertEqual(apply.pinned_tf_dir(None), apply.TF_DIR)
        head = ancestry.head_of("HEAD", str(apply.TF_DIR.parents[2]))
        self.assertEqual(apply.pinned_tf_dir(head), apply.TF_DIR)
        calls = []
        with self.assertRaises(apply.ApplyFailed):  # not an ancestor of main: no worktree
            apply.pinned_tf_dir("f" * 40, run=lambda *a, **k: calls.append(a))
        self.assertEqual(calls, [])

    def test_worktree_calls_are_bounded(self):
        calls = []

        def run(argv, **kw):
            calls.append((argv[3], kw.get("timeout")))
        with mock.patch.object(ancestry, "is_ancestor", return_value=True):
            apply.pinned_tf_dir("f" * 40, run=run)
        apply.unpin_tf_dir(Path("/tmp/fleet-pin-x/infra/akamai/terraform"), run=run)
        self.assertEqual(calls, [("worktree", 30), ("worktree", 30)])

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

    def run_cli(self, sub, body, b, store=None, clock=None, **over):
        out, gh, clock = io.StringIO(), FakeGitHub(), clock or Clock()
        env = event_env(body, FLEET_LIVE="true", **{**ENV, **over})
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
        _, out = self.run_cli("act", "/fleet run arm-switch main", b)
        self.assertIn("step not built yet (OP-3b)", out)
        _, out = self.run_cli("act", "/fleet down", b)
        self.assertIn("independently authorized owner cleanup workflow", out)
        self.assertIn("roles", s3.doc("state-b", FLEET_KEY))

    def test_delivery_chain_keeps_then_releases_the_lock(self):
        api = self.empty_account()
        b, s3, _, _ = backend(api=api)
        store, outfile = MemoryStore(), Path(tempfile.mkdtemp()) / "out"
        self.addCleanup(lambda: outfile.unlink(missing_ok=True))
        rc, out = self.run_cli("act", "/fleet up screen", b, store=store, GITHUB_OUTPUT=str(outfile))
        self.assertEqual(rc, 0)
        self.assertIn(LOCK_KEY, store.objects)  # kept for deliver-pia
        self.assertEqual(outfile.read_text(), "deliver=true\nlive=true\n")
        outfile.write_text("")
        rc, out = self.run_cli("pia-fence", "/fleet up screen", b, store=store, GITHUB_OUTPUT=str(outfile))
        self.assertEqual((rc, out.strip()), (0, "pia-fence: this run holds the fleet lock"))
        self.assertEqual(outfile.read_text(), "lock_expires=" + json.loads(store.objects[LOCK_KEY][0])["expires_at"] + "\n")
        rc, out = self.run_cli("pia-release", "/fleet up screen", b, store=store)
        self.assertEqual(rc, 0)
        self.assertIn("pia-release: released", out)
        self.assertNotIn(LOCK_KEY, store.objects)
        # renew-vpn chains the same way; a plain command releases at once.
        outfile.write_text("")
        self.run_cli("act", "/fleet run renew-vpn worker", b, store=store, GITHUB_OUTPUT=str(outfile))
        self.assertIn(LOCK_KEY, store.objects)
        self.assertEqual(outfile.read_text(), "deliver=true\nlive=true\n")
        self.run_cli("pia-release", "/fleet run renew-vpn worker", b, store=store)
        outfile.write_text("")
        self.run_cli("act", "/fleet status", b, store=store, GITHUB_OUTPUT=str(outfile))
        self.assertNotIn(LOCK_KEY, store.objects)
        self.assertEqual(outfile.read_text(), "")

    def test_act_releases_if_it_cant_hand_the_lock_on(self):
        api = self.empty_account()
        b, *_ = backend(api=api)
        store = MemoryStore()
        rc, out = self.run_cli("act", "/fleet up screen", b, store=store, GITHUB_OUTPUT="/nonexistent/dir/out")
        self.assertEqual(rc, 1)
        self.assertIn("the lock couldn't be handed to deliver-pia, so nothing was delivered; use run renew-vpn", out)
        self.assertNotIn(LOCK_KEY, store.objects)

    def test_a_refused_up_never_keeps_the_lock(self):
        api = self.empty_account()
        b, *_ = backend(api=api)
        b.read_fleet()  # the first tombstone
        api.linodes["13"] = {"id": 13, "tags": ["lodestar"], "created": "2026-11-08T20:00:00"}  # up refuses
        store = MemoryStore()
        _, out = self.run_cli("act", "/fleet up screen", b, store=store)
        self.assertIn("refused: up screen", out)
        self.assertNotIn(LOCK_KEY, store.objects)

    def test_pia_fence_fails_closed(self):
        api = self.empty_account()
        b, s3, _, _ = backend(api=api)
        store = MemoryStore()
        self.run_cli("act", "/fleet up screen", b, store=store)
        cases = (
            ("another holder", lambda: FleetLock(store, 90, Clock()).preempt("9-1"),
             "this run no longer holds the fleet lock; nothing delivered"),
            ("a mismatch", lambda: b.set_mismatch(["x"]), "the fleet is unreadable, gone, overdue or mismatched"),
        )
        for name, mutate, reason in cases:
            with self.subTest(name):
                store.objects.clear()
                FleetLock(store, 90, Clock()).acquire("42-1")
                b._clear_mismatch()
                mutate()
                rc, out = self.run_cli("pia-fence", "/fleet up screen", b, store=store)
                self.assertEqual(rc, 1)
                self.assertIn(f"deliver-pia: refused: {reason}", out)
        for over, body in (({"GITHUB_RUN_ATTEMPT": "2"}, "/fleet up screen"), ({}, "/fleet status"), ({}, "/fleet down")):
            rc, out = self.run_cli("pia-fence", body, b, store=store, **over)
            self.assertEqual((rc, out.strip()), (1, "pia-fence: refused"))
        # A tombstoned fleet (down ran meanwhile): nothing to deliver to.
        store.objects.clear()
        FleetLock(store, 90, Clock()).acquire("42-1")
        b._clear_mismatch()
        s3.objects[("state-b", FLEET_KEY)] = json.dumps({"version": 1, "fleet": None, "at": "2026-11-08T20:00:00Z"}).encode()
        rc, out = self.run_cli("pia-fence", "/fleet up screen", b, store=store)
        self.assertEqual(rc, 1)
        self.assertIn("deliver-pia: refused: the fleet is unreadable, gone, overdue or mismatched", out)

    def test_pia_release_only_releases_its_own_lock(self):
        b, *_ = backend()
        store = MemoryStore()
        FleetLock(store, 90, Clock()).acquire("42-2")
        rc, out = self.run_cli("pia-release", "/fleet up screen", b, store=store, GITHUB_RUN_ATTEMPT="2")
        self.assertEqual((rc, out.strip()), (1, "pia-release: refused"))  # a re-run touches nothing
        self.assertIn(LOCK_KEY, store.objects)
        store = MemoryStore()
        FleetLock(store, 90, Clock()).acquire("9-1")
        rc, out = self.run_cli("pia-release", "/fleet up screen", b, store=store)
        self.assertEqual(rc, 0)
        self.assertIn("not this run's lock any more", out)
        self.assertEqual(json.loads(store.objects[LOCK_KEY][0])["run_id"], "9-1")

        class NoDelete(MemoryStore):
            def delete_if_match(self, key, etag):
                raise RuntimeError("S3 503")
        store = NoDelete()
        FleetLock(store, 90, Clock()).acquire("42-1")
        rc, out = self.run_cli("pia-release", "/fleet up screen", b, store=store)
        self.assertEqual(rc, 1)
        self.assertIn("pia-release: couldn't release the fleet lock; it expires on its own; escalate", out)

    def test_unreadable_state_allows_only_down(self):
        b, *_ = backend(s3=FakeS3(deny={("state-b", FLEET_KEY)}))
        _, out = self.run_cli("act", "/fleet up screen", b)
        self.assertIn("fleet state unreadable: only down runs", out)
        self.assertIn("a fleet may exist", out)


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
