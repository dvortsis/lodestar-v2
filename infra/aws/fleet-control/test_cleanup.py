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
        session.client.return_value.get_object.side_effect = RuntimeError("LEAKMARK-SECRET")
        boto = mock.Mock()
        boto.Session.return_value = session
        modules = {"boto3": boto, "botocore": mock.Mock(), "botocore.config": mock.Mock()}
        with mock.patch.dict(sys.modules, modules), mock.patch.dict(os.environ, {"AWS_ACCOUNT_ID": "111122223333", "AWS_REGION": "us-east-1", "STATE_BUCKET": "state-b"}), \
                self.assertLogs(ch.LOG, level="ERROR") as logs, self.assertRaisesRegex(RuntimeError, "cleanup failed"):
            ch.handler({"mode": "emergency", "targets": [99]}, None)
        self.assertIn("cleanup_failed", str(logs.output))
        self.assertNotIn("LEAKMARK", str(logs.output))

    def handler_fixture(self, mode, when, event):
        api, s3, sts, secrets = API(), mock.Mock(), mock.Mock(), mock.Mock()
        sts.get_caller_identity.return_value = {"Account": "111122223333"}
        s3.get_object.side_effect = lambda **kw: {"Body": io.BytesIO(json.dumps(auth()).encode()), "ETag": '"version-1"'}
        secrets.get_secret_value.return_value = {"SecretString": "LEAKMARK-not-a-real-token"}
        session, boto = mock.Mock(), mock.Mock()
        session.client.side_effect = lambda name, **kw: {"sts": sts, "s3": s3, "secretsmanager": secrets}[name]
        boto.Session.return_value = session
        env = {"AWS_ACCOUNT_ID": "111122223333", "AWS_REGION": "us-east-1", "STATE_BUCKET": "state-b", "CONTROL_BUCKET": "control-b",
               "CLEANUP_SECRET_ARN": "cleanup-secret", "LINODE_ACCOUNT_EUUID": "EUUID-FLEET", "MIN_CREATED_UTC": START.isoformat(), "CLEANUP_MODE": mode}
        modules = {"boto3": boto, "botocore": mock.Mock(), "botocore.config": mock.Mock()}
        context = mock.Mock()
        context.get_remaining_time_in_millis.return_value = 300000
        with mock.patch.dict(sys.modules, modules), mock.patch.dict(os.environ, env), \
                mock.patch.object(ch, "datetime", wraps=datetime) as clock, mock.patch.object(ch, "transport", return_value=api):
            clock.now.return_value = when
            result = ch.handler(event, context)
        return result, api, s3

    def test_request_payload_cannot_escalate_expiration_to_emergency_or_select_targets(self):
        result, api, s3 = self.handler_fixture("expiration", START, {"mode": "emergency", "targets": [99]})
        self.assertEqual(result["status"], "not_expired")
        s3.put_object.assert_not_called()
        self.assertFalse(any(m == "DELETE" for m, _ in api.calls))

    def test_real_handler_dry_run_and_owner_bound_s3_calls(self):
        result, api, s3 = self.handler_fixture("emergency", START, {"dry_run": True})
        self.assertEqual(result["status"], "dry_run")
        s3.put_object.assert_not_called()
        self.assertFalse(any(m == "DELETE" for m, _ in api.calls))
        for call in s3.get_object.call_args_list:
            self.assertEqual(call.kwargs["ExpectedBucketOwner"], "111122223333")
        result, api, s3 = self.handler_fixture("expiration", END, {})
        self.assertEqual(result["status"], "complete")
        write = s3.put_object.call_args.kwargs
        self.assertEqual((write["Bucket"], write["Key"], write["ExpectedBucketOwner"]), ("control-b", ch.FLAG_KEY, "111122223333"))
        self.assertTrue(json.loads(write["Body"])["mismatch"])

    def test_emergency_is_pinned_to_fresh_request_and_exact_registry_version(self):
        result, api, s3 = self.handler_fixture("emergency", START, {"operation": "prepare", "requested_at": START.isoformat()})
        self.assertEqual(result, {"status": "prepared", "authorization_version": '"version-1"'})
        s3.put_object.assert_not_called()
        self.assertEqual(api.calls, [])
        result, api, _ = self.handler_fixture("emergency", START, {"requested_at": START.isoformat(), "authorization_version": '"version-1"'})
        self.assertEqual(result["linodes"], 1)
        for event, now in (({"requested_at": START.isoformat(), "authorization_version": "wrong-version"}, START),
                           ({"requested_at": START.isoformat(), "authorization_version": '"version-1"'}, START + timedelta(minutes=11)),
                           ({"requested_at": (START - timedelta(seconds=1)).isoformat(), "authorization_version": '"version-1"'}, START)):
            with self.subTest(event=event), self.assertRaises(RuntimeError):
                self.handler_fixture("emergency", now, event)


if __name__ == "__main__":
    unittest.main()
