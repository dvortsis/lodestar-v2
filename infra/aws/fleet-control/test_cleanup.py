"""Cleanup boundary tests: fake API only, no network, credentials or resources."""
import copy
import io
import json
import os
import sys
import unittest
from datetime import datetime, timedelta, timezone
from unittest import mock
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "fleet"))
import cleanup_handler as ch

START = datetime(2026, 11, 8, 20, tzinfo=timezone.utc)
END = START + timedelta(hours=44)


def auth():
    return {"version": 1, "account_euuid": "EUUID-FLEET", "region": "us-east", "deployment_id": "fleet",
            "created_at": START.isoformat(), "expires_at": END.isoformat(),
            "linodes": [{"id": 11, "role": "main", "label": "lodestar-main-fleet", "type": "g6-standard-4", "created": START.isoformat()}],
            "firewalls": [{"id": 21, "label": "lodestar-main-fw-fleet", "created": START.isoformat(), "linode_ids": [11], "admin_ipv4": ["203.0.113.4/32"]}]}


class API:
    def __init__(self):
        self.node = {**auth()["linodes"][0], "tags": sorted(ch.TAGS), "region": "us-east"}
        self.wall = {**auth()["firewalls"][0], "tags": sorted(ch.TAGS), "status": "enabled"}
        self.rules = {"inbound_policy": "DROP", "inbound": [{"label": "ssh-admin-only", "action": "ACCEPT", "protocol": "TCP", "ports": "22", "addresses": {"ipv4": ["203.0.113.4/32"]}}]}
        self.attached = [{"entity": {"type": "linode", "id": 11}}]
        self.account, self.calls, self.confirm = "EUUID-FLEET", [], True

    def __call__(self, method, path):
        self.calls.append((method, path))
        if path == "/account":
            return {"euuid": self.account}
        if "/devices?" in path:
            return {"page": 1, "pages": 1, "data": copy.deepcopy(self.attached)}
        if path.endswith("/rules"):
            return copy.deepcopy(self.rules)
        if method == "DELETE":
            if path == "/linode/instances/11" and self.confirm:
                self.node, self.attached = None, []
            elif path == "/networking/firewalls/21" and self.confirm:
                self.wall = None
            return {}
        if path == "/linode/instances/11":
            return copy.deepcopy(self.node)
        if path == "/networking/firewalls/21":
            return copy.deepcopy(self.wall)
        raise AssertionError("unexpected API path")


AUTH_ETAG = '"auth-1"'
ENV = {"AWS_ACCOUNT_ID": "111122223333", "AWS_REGION": "us-east-1", "STATE_BUCKET": "state-b", "CONTROL_BUCKET": "control-b",
       "CLEANUP_SECRET_ARN": "cleanup-secret", "LINODE_ACCOUNT_EUUID": "EUUID-FLEET", "MIN_CREATED_UTC": START.isoformat()}


class Conflict(Exception):
    """Shaped like botocore's ClientError for a failed S3 precondition."""
    def __init__(self, code="PreconditionFailed"):
        super().__init__(code)
        self.response = {"Error": {"Code": code}}


class FakeS3:
    """S3 with real conditional semantics: 412 when If-None-Match: * finds an
    object, or If-Match names a missing object or a different ETag. A missing
    key reads as AccessDenied, as it does without s3:ListBucket."""
    def __init__(self, owner="111122223333"):
        self.owner, self.version, self.calls, self.on_get, self.fail, self.racing = owner, 0, [], None, set(), set()
        self.objects = {("state-b", ch.AUTH_KEY): (json.dumps(auth()).encode(), AUTH_ETAG)}

    def _enter(self, op, bucket, key, owner, cond):
        self.calls.append((op, bucket, key, owner, cond))
        if owner != self.owner:
            raise RuntimeError("AccessDenied: bucket owner")
        if (op, key) in self.fail:
            raise RuntimeError("ServiceUnavailable")
        if set(cond) - {"IfNoneMatch", "IfMatch"}:
            raise AssertionError(f"unexpected S3 arguments {cond}")
        for header in set(cond) & self.racing:  # 409: another conditional write to this key is in flight
            self.racing.discard(header)
            raise Conflict("ConditionalRequestConflict")
        current = self.objects.get((bucket, key))
        if "IfNoneMatch" in cond and (cond["IfNoneMatch"] != "*" or current is not None):
            raise Conflict()
        if "IfMatch" in cond and (current is None or current[1] != cond["IfMatch"]):
            raise Conflict()
        return current

    def put_object(self, *, Bucket, Key, ExpectedBucketOwner, Body, **cond):
        self._enter("put", Bucket, Key, ExpectedBucketOwner, cond)
        self.version += 1
        self.objects[(Bucket, Key)] = (bytes(Body), f'"v{self.version}"')
        return {"ETag": f'"v{self.version}"'}

    def get_object(self, *, Bucket, Key, ExpectedBucketOwner, **cond):
        current = self._enter("get", Bucket, Key, ExpectedBucketOwner, cond)
        if current is None:
            raise RuntimeError("AccessDenied")
        hook, self.on_get = self.on_get, None
        if hook and Key in ch.LEASE_KEYS.values():
            hook()  # another run acts between this read and our conditional write
        return {"Body": io.BytesIO(current[0]), "ETag": current[1]}

    def lease(self, mode):
        body, etag = self.objects[("control-b", ch.LEASE_KEYS[mode])]
        return json.loads(body), etag


def invoke(mode, when, event, s3, api, request_id="req-1"):
    """The real handler against FakeS3 and a fake Linode API; no network."""
    sts, secrets, session, boto = mock.Mock(), mock.Mock(), mock.Mock(), mock.Mock()
    sts.get_caller_identity.return_value = {"Account": "111122223333"}
    secrets.get_secret_value.return_value = {"SecretString": "LEAKMARK-not-a-real-token"}
    session.client.side_effect = lambda name, **kw: {"sts": sts, "s3": s3, "secretsmanager": secrets}[name]
    boto.Session.return_value = session
    modules = {"boto3": boto, "botocore": mock.Mock(), "botocore.config": mock.Mock()}
    context = mock.Mock(aws_request_id=request_id)
    context.get_remaining_time_in_millis.return_value = 300000
    with mock.patch.dict(sys.modules, modules), mock.patch.dict(os.environ, {**ENV, "CLEANUP_MODE": mode}), \
            mock.patch.object(ch, "datetime", wraps=datetime) as clock, mock.patch.object(ch, "transport", return_value=api):
        clock.now.return_value = when
        return ch.handler(event, context)


class CleanupBoundaries(unittest.TestCase):
    def go(self, api=None, doc=None, mode="expiration", now=END, **kwargs):
        return ch.cleanup(auth() if doc is None else doc, "EUUID-FLEET", START, mode, now,
                          API() if api is None else api, kwargs.pop("unchanged", lambda: None),
                          kwargs.pop("mark", lambda: None), pause=lambda _: None, **kwargs)

    def test_expiration_and_idempotent_replay(self):
        api, mark = API(), mock.Mock()
        self.assertEqual(self.go(api=api, mark=mark), {"status": "complete", "linodes": 1, "firewalls": 0, "retained_firewalls": 1})
        # An independently deleted/retired firewall makes a replay completely idle.
        api.wall = None
        self.assertEqual(self.go(api=api, mark=mark), {"status": "complete", "linodes": 0, "firewalls": 0})
        mark.assert_called_once()
        self.assertEqual([p for m, p in api.calls if m == "DELETE"], ["/linode/instances/11"])

    def test_expiration_boundary_and_separate_early_emergency_authority(self):
        api = API()
        self.assertEqual(self.go(api=api, now=END - timedelta(seconds=1))["status"], "not_expired")
        self.assertFalse(any(m == "DELETE" for m, _ in api.calls))
        self.assertEqual(self.go(mode="emergency", now=START)["status"], "complete")

    def test_dry_run_never_marks_or_deletes(self):
        api, mark = API(), mock.Mock()
        self.assertEqual(self.go(api=api, mark=mark, dry_run=True)["status"], "dry_run")
        mark.assert_not_called()
        self.assertFalse(any(m == "DELETE" for m, _ in api.calls))

    def test_foreign_or_invalid_resources_fail_before_any_write(self):
        mutations = {
            "account": lambda a, d: setattr(a, "account", "OTHER"),
            "registry account": lambda a, d: d.update(account_euuid="OTHER"),
            "registry region": lambda a, d: d.update(region="eu-west"),
            "API region": lambda a, d: a.node.update(region="eu-west"),
            "type": lambda a, d: a.node.update(type="g6-standard-8"),
            "tags": lambda a, d: a.node.update(tags=["lodestar"]),
            "label": lambda a, d: a.node.update(label="foreign"),
            "creation changed": lambda a, d: a.node.update(created=(START - timedelta(seconds=1)).isoformat()),
            "activation floor": lambda a, d: d.update(created_at=(START - timedelta(seconds=1)).isoformat()),
            "future authorization": lambda a, d: d.update(created_at=(END + timedelta(seconds=1)).isoformat()),
            "unbounded lifetime": lambda a, d: d.update(expires_at=(END + timedelta(seconds=1)).isoformat()),
            "naive timestamp": lambda a, d: d.update(created_at="2026-11-08T20:00:00"),
            "boolean ID": lambda a, d: d["linodes"][0].update(id=True),
            "foreign association": lambda a, d: d["firewalls"][0].update(linode_ids=[99]),
            "wall retagged": lambda a, d: a.wall.update(tags=[]),
            "wall relabeled": lambda a, d: a.wall.update(label="foreign"),
            "foreign attachment": lambda a, d: a.attached.append({"entity": {"type": "linode", "id": 99}}),
            "other device type": lambda a, d: a.attached.append({"entity": {"type": "nodebalancer", "id": 11}}),
            "empty authorization": lambda a, d: d.update(linodes=[]),
        }
        for label, mutate in mutations.items():
            with self.subTest(boundary=label):
                api, doc, mark = API(), auth(), mock.Mock()
                mutate(api, doc)
                with self.assertRaises(ch.Refused):
                    self.go(api=api, doc=doc, mark=mark)
                mark.assert_not_called()
                self.assertFalse(any(m == "DELETE" for m, _ in api.calls))

    def test_revocation_or_failed_mismatch_write_prevents_deletion(self):
        for where in ("unchanged", "mark"):
            api = API()
            with self.subTest(where=where), self.assertRaises(RuntimeError):
                self.go(api=api, **{where: mock.Mock(side_effect=RuntimeError("refused"))})
            self.assertFalse(any(m == "DELETE" for m, _ in api.calls))

    def test_unconfirmed_vm_deletion_never_deletes_its_firewall(self):
        api, mark = API(), mock.Mock()
        api.confirm = False
        with self.assertRaisesRegex(ch.Refused, "unconfirmed"):
            self.go(api=api, mark=mark)
        mark.assert_called_once()
        self.assertEqual([p for m, p in api.calls if m == "DELETE"], ["/linode/instances/11"])

    def test_target_changed_after_plan_is_rechecked(self):
        api = API()
        with self.assertRaises(ch.Refused):
            self.go(api=api, mark=lambda: api.node.update(region="eu-west"))
        self.assertFalse(any(m == "DELETE" for m, _ in api.calls))

    def test_incomplete_device_pages_are_not_empty(self):
        api = API()
        def bad(method, path):
            return {"data": []} if "/devices?" in path else api(method, path)
        with self.assertRaises(ch.Refused):
            self.go(api=bad)
        self.assertFalse(any(m == "DELETE" for m, _ in api.calls))

    def test_expiring_execution_budget_cannot_start_deletion(self):
        api, mark = API(), mock.Mock()
        with self.assertRaisesRegex(ch.Refused, "deadline"):
            self.go(api=api, mark=mark, remaining=lambda: 20)
        mark.assert_not_called()
        self.assertFalse(any(m == "DELETE" for m, _ in api.calls))

    def test_definite_security_drift_is_independently_authorized_early_teardown(self):
        api = API()
        api.rules["inbound_policy"] = "ACCEPT"
        self.assertEqual(self.go(api=api, now=START)["linodes"], 1)

    def test_unreadable_job_audit_never_authorizes_early_deletion_or_blocks_expiration(self):
        api, mark, github = API(), mock.Mock(), mock.Mock(side_effect=RuntimeError("unavailable"))
        api.rules["inbound"].append({"label": "job-5-1", "action": "ACCEPT", "protocol": "TCP", "ports": "22", "addresses": {"ipv4": ["198.51.100.1/32"]}})
        with self.assertRaisesRegex(ch.Refused, "audit unavailable"):
            self.go(api=api, now=START, mark=mark, github=github)
        mark.assert_called_once()
        self.assertFalse(any(m == "DELETE" for m, _ in api.calls))
        github.reset_mock()
        self.assertEqual(self.go(api=api, now=END, github=github)["linodes"], 1)
        github.assert_not_called()

    def test_transport_cannot_create_update_or_select_arbitrary_endpoints(self):
        with mock.patch.object(ch.urllib.request, "build_opener") as build:
            api = ch.transport("LEAKMARK-TOKEN")
            for method, path in (("POST", "/linode/instances"), ("PUT", "/networking/firewalls/21"),
                                 ("DELETE", "/networking/firewalls/21"), ("DELETE", "/account"), ("DELETE", "/volumes/11"), ("GET", "https://evil.invalid"),
                                 ("GET", "/linode/instances/../99")):
                with self.assertRaises(ch.Refused):
                    api(method, path)
            build.return_value.open.assert_not_called()

    def test_handler_reports_failure_without_sensitive_body(self):
        session = mock.Mock()
        session.client.return_value.get_caller_identity.return_value = {"Account": "111122223333"}
        session.client.return_value.put_object.return_value = {"ETag": '"lease-1"'}
        session.client.return_value.get_object.side_effect = RuntimeError("LEAKMARK-SECRET")
        boto = mock.Mock()
        boto.Session.return_value = session
        modules = {"boto3": boto, "botocore": mock.Mock(), "botocore.config": mock.Mock()}
        env = {"AWS_ACCOUNT_ID": "111122223333", "AWS_REGION": "us-east-1", "STATE_BUCKET": "state-b", "CONTROL_BUCKET": "control-b", "CLEANUP_MODE": "emergency"}
        with mock.patch.dict(sys.modules, modules), mock.patch.dict(os.environ, env), \
                self.assertLogs(ch.LOG, level="ERROR") as logs, self.assertRaisesRegex(RuntimeError, "cleanup failed"):
            ch.handler({"mode": "emergency", "targets": [99]}, mock.Mock(aws_request_id="req-1"))
        self.assertIn("cleanup_failed", str(logs.output))
        self.assertIn('"stage": "authorization"', str(logs.output))
        self.assertNotIn("LEAKMARK", str(logs.output))

    def handler_fixture(self, mode, when, event, s3=None, api=None, request_id="req-1"):
        api, s3 = API() if api is None else api, FakeS3() if s3 is None else s3
        return invoke(mode, when, event, s3, api, request_id), api, s3

    def test_request_payload_cannot_escalate_expiration_to_emergency_or_select_targets(self):
        result, api, s3 = self.handler_fixture("expiration", START, {"mode": "emergency", "targets": [99]})
        self.assertEqual(result["status"], "not_expired")
        self.assertNotIn(("control-b", ch.FLAG_KEY), s3.objects)
        self.assertFalse(any(m == "DELETE" for m, _ in api.calls))

    def test_real_handler_dry_run_and_owner_bound_s3_calls(self):
        result, api, s3 = self.handler_fixture("emergency", START, {"dry_run": True})
        self.assertEqual(result["status"], "dry_run")
        self.assertNotIn(("control-b", ch.FLAG_KEY), s3.objects)
        self.assertFalse(any(m == "DELETE" for m, _ in api.calls))
        self.assertEqual({owner for _, _, _, owner, _ in s3.calls}, {"111122223333"})  # FakeS3 refuses any other owner
        result, api, s3 = self.handler_fixture("expiration", END, {})
        self.assertEqual(result["status"], "complete")
        self.assertTrue(json.loads(s3.objects[("control-b", ch.FLAG_KEY)][0])["mismatch"])

    def test_emergency_is_pinned_to_fresh_request_and_exact_registry_version(self):
        result, api, s3 = self.handler_fixture("emergency", START, {"operation": "prepare", "requested_at": START.isoformat()})
        self.assertEqual(result, {"status": "prepared", "authorization_version": AUTH_ETAG})
        self.assertNotIn(("control-b", ch.FLAG_KEY), s3.objects)
        self.assertEqual(api.calls, [])
        result, api, _ = self.handler_fixture("emergency", START, {"requested_at": START.isoformat(), "authorization_version": AUTH_ETAG})
        self.assertEqual(result["linodes"], 1)
        for event, now in (({"requested_at": START.isoformat(), "authorization_version": "wrong-version"}, START),
                           ({"requested_at": START.isoformat(), "authorization_version": AUTH_ETAG}, START + timedelta(minutes=11)),
                           ({"requested_at": (START - timedelta(seconds=1)).isoformat(), "authorization_version": AUTH_ETAG}, START)):
            with self.subTest(event=event), self.assertRaises(RuntimeError):
                self.handler_fixture("emergency", now, event)


def mutant(old, new=""):
    """The handler module with one lease precondition removed."""
    source = Path(ch.__file__).read_text(encoding="utf-8")
    assert source.count(old) == 1, old
    module = type(sys)("cleanup_mutant")
    module.__dict__.update(actions_ranges=None, audit_firewall=None, run_is_live=None, JOB_LABEL=None)
    exec(compile(source.replace(old, new).replace("from audit import", "# from audit import"), "cleanup_mutant", "exec"), module.__dict__)
    return module


def fresh_runs_act_once(m):
    """Two runs racing for an absent lease: exactly one holds it."""
    s3, won = FakeS3(), []
    for holder in ("a", "b"):
        try:
            won.append(m.acquire_lease(s3, "control-b", "111122223333", "expiration", holder, START))
        except m.Busy:
            pass
    return len(won) == 1


def stale_takeover_happens_once(m):
    """Two runs read the same expired lease, then both try to take it over."""
    s3 = FakeS3()
    m.acquire_lease(s3, "control-b", "111122223333", "expiration", "crashed", START)
    later, won = START + timedelta(seconds=m.LEASE_SECONDS + 1), []
    def take(holder):
        try:
            won.append(m.acquire_lease(s3, "control-b", "111122223333", "expiration", holder, later))
        except m.Busy:
            pass
    s3.on_get = lambda: take("b")  # b reads, writes and wins while a holds its stale read
    take("a")
    return len(won) == 1 and json.loads(s3.objects[("control-b", m.LEASE_KEYS["expiration"])][0])["holder"] == "b"


def lost_lease_release_refused(m):
    """A run whose lease was taken over must not release the new holder's lease."""
    s3 = FakeS3()
    old = m.acquire_lease(s3, "control-b", "111122223333", "expiration", "a", START)
    later = START + timedelta(seconds=m.LEASE_SECONDS + 1)
    m.acquire_lease(s3, "control-b", "111122223333", "expiration", "b", later)
    try:
        m.release_lease(s3, "control-b", "111122223333", "expiration", "a", old, later)
    except Conflict:
        return json.loads(s3.objects[("control-b", m.LEASE_KEYS["expiration"])][0])["holder"] == "b"
    return False


# Each scenario, and the source edit that drops the precondition it depends on.
def racing_takeover_is_busy(m):
    """S3 answers 409 when a conditional write races another in flight: that is busy, never a win."""
    s3 = FakeS3()
    m.acquire_lease(s3, "control-b", "111122223333", "expiration", "crashed", START)
    s3.racing.add("IfMatch")
    try:
        m.acquire_lease(s3, "control-b", "111122223333", "expiration", "a", START + timedelta(seconds=m.LEASE_SECONDS + 1))
    except m.Busy:
        return json.loads(s3.objects[("control-b", m.LEASE_KEYS["expiration"])][0])["holder"] == "crashed"
    except Exception:
        return False
    return False


SCENARIOS = {fresh_runs_act_once: (', IfNoneMatch="*"', ""), stale_takeover_happens_once: (', IfMatch=found["ETag"]', ""),
             lost_lease_release_refused: ("now), IfMatch=etag)", "now))"),
             racing_takeover_is_busy: ('"PreconditionFailed", "ConditionalRequestConflict"', '"PreconditionFailed"')}


class CleanupLease(unittest.TestCase):
    def test_lease_scenarios_hold(self):
        for scenario in SCENARIOS:
            with self.subTest(scenario=scenario.__name__):
                self.assertTrue(scenario(ch))

    def test_dropping_any_condition_header_breaks_a_scenario(self):
        for scenario, (old, new) in SCENARIOS.items():
            with self.subTest(header=old):
                self.assertFalse(scenario(mutant(old, new)))

    def test_concurrent_runs_of_one_mode_act_exactly_once(self):
        s3, api, second = FakeS3(), API(), {}
        def overlap(method, path):
            if method == "DELETE" and not second:  # the first run is mid-deletion, holding its lease
                inner, before = API(), len(s3.calls)
                with self.assertLogs(ch.LOG, level="WARNING") as logs:
                    second["result"] = invoke("expiration", END, {}, s3, inner, "req-2")
                second.update(logs=logs.output, calls=inner.calls, s3_keys={c[2] for c in s3.calls[before:]})
                # Leases are per mode: an emergency dry run is not blocked by expiration.
                second["other_mode"] = invoke("emergency", END, {"dry_run": True}, s3, API(), "req-3")["status"]
            return api(method, path)
        self.assertEqual(invoke("expiration", END, {}, s3, overlap, "req-1")["linodes"], 1)
        self.assertEqual(second["result"], {"status": "busy", "event": "cleanup_busy", "mode": "expiration"})
        self.assertEqual(len(second["logs"]), 1)
        self.assertIn('"event": "cleanup_busy"', second["logs"][0])
        self.assertEqual(second["calls"], [])  # no Linode call, and in S3 nothing beyond the lease
        self.assertEqual(second["s3_keys"], {ch.LEASE_KEYS["expiration"]})
        self.assertEqual(second["other_mode"], "dry_run")
        self.assertEqual([p for m, p in api.calls if m == "DELETE"], ["/linode/instances/11"])
        lease, _ = s3.lease("expiration")
        self.assertEqual(lease["holder"], "req-1")
        self.assertLessEqual(ch.timestamp(lease["expires_at"]), END)  # released
        # Released, so the next run takes it over by ETag and rechecks: nothing left to delete.
        third = invoke("expiration", END, {}, s3, api, "req-4")
        self.assertEqual((third["status"], third["linodes"]), ("complete", 0))
        self.assertEqual(s3.lease("expiration")[0]["holder"], "req-4")

    def test_live_lease_is_busy_and_expired_lease_is_taken_over(self):
        s3 = FakeS3()
        ch.acquire_lease(s3, "control-b", "111122223333", "emergency", "crashed", START)
        for offset, busy in ((ch.LEASE_SECONDS - 1, True), (ch.LEASE_SECONDS, False)):
            with self.subTest(offset=offset):
                api = API()
                result = invoke("emergency", START + timedelta(seconds=offset), {"dry_run": True}, s3, api, f"req-{offset}")
                self.assertEqual(result["status"], "busy" if busy else "dry_run")
                self.assertEqual(api.calls == [], busy)
        self.assertGreater(ch.LEASE_SECONDS, 300)  # outlives the function timeout
        self.assertLess(ch.LEASE_SECONDS, 900)     # a crashed run never skips two schedules

    def test_lease_errors_fail_closed_with_an_alert(self):
        for name, setup in {
            "create error": lambda s3: s3.fail.add(("put", ch.LEASE_KEYS["expiration"])),
            "read error": lambda s3: (ch.acquire_lease(s3, "control-b", "111122223333", "expiration", "x", START), s3.fail.add(("get", ch.LEASE_KEYS["expiration"]))),
            "malformed lease": lambda s3: s3.objects.update({("control-b", ch.LEASE_KEYS["expiration"]): (b"not json", '"bad"')}),
            "other mode's lease": lambda s3: s3.objects.update({("control-b", ch.LEASE_KEYS["expiration"]): (ch.lease_body("emergency", "x", START), '"bad"')}),
            "oversized lease": lambda s3: s3.objects.update({("control-b", ch.LEASE_KEYS["expiration"]): (b" " * 4097, '"big"')}),
        }.items():
            with self.subTest(case=name):
                s3, api = FakeS3(), API()
                setup(s3)
                s3.calls.clear()
                with self.assertLogs(ch.LOG, level="ERROR") as logs, self.assertRaisesRegex(RuntimeError, "cleanup failed"):
                    invoke("expiration", END, {}, s3, api)
                self.assertIn('"stage": "lease"', logs.output[-1])
                self.assertEqual(api.calls, [])
                self.assertFalse([c for c in s3.calls if c[2] != ch.LEASE_KEYS["expiration"]])  # no authorization read, no flag

    def test_racing_first_create_fails_closed(self):
        # 409 on the very first create: the object may not exist yet, so the read is
        # refused (no ListBucket) and the run stops at the lease with an alert.
        s3, api = FakeS3(), API()
        s3.racing.add("IfNoneMatch")
        with self.assertLogs(ch.LOG, level="ERROR") as logs, self.assertRaises(RuntimeError):
            invoke("expiration", END, {}, s3, api)
        self.assertIn('"stage": "lease"', logs.output[-1])
        self.assertEqual(api.calls, [])
        self.assertEqual({c[2] for c in s3.calls}, {ch.LEASE_KEYS["expiration"]})

    def test_failed_run_releases_and_lost_lease_alerts(self):
        s3, api = FakeS3(), API()
        api.account = "OTHER"
        with self.assertLogs(ch.LOG, level="ERROR"), self.assertRaises(RuntimeError):
            invoke("expiration", END, {}, s3, api)
        self.assertLessEqual(ch.timestamp(s3.lease("expiration")[0]["expires_at"]), END)
        # A run whose lease was taken over after it acted raises, so the Errors alarm fires.
        s3 = FakeS3()
        def steal(method, path):
            if method == "DELETE":
                s3.objects[("control-b", ch.LEASE_KEYS["expiration"])] = (ch.lease_body("expiration", "thief", END), '"stolen"')
            return api2(method, path)
        api2 = API()
        with self.assertLogs(ch.LOG, level="ERROR") as logs, self.assertRaises(RuntimeError):
            invoke("expiration", END, {}, s3, steal)
        self.assertIn('"stage": "lease_release"', logs.output[-1])
        self.assertEqual(s3.lease("expiration")[0]["holder"], "thief")


if __name__ == "__main__":
    unittest.main()
