"""The emergency identity cannot become a provisioning or arbitrary-delete path."""
import io
import json
import unittest
import sys
from datetime import datetime, timezone
from contextlib import redirect_stdout
from pathlib import Path
from unittest import mock

import apply
import deployment_policy as dp
import emergency_cleanup as ec
import fleet
import yaml
from tests.helpers import event_env
from tests.test_deployment_policy import job, settings, token, claims

ROOT = Path(__file__).resolve().parents[3]


class EmergencyAuthorization(unittest.TestCase):
    def test_workflow_is_main_first_attempt_owner_dispatch_with_no_inputs_or_terraform(self):
        text = (ROOT / ".github/workflows/fleet-emergency-down.yml").read_text()
        wf = yaml.safe_load(text)
        self.assertEqual(wf.get("on", wf.get(True)), {"workflow_dispatch": None})
        self.assertEqual(wf["permissions"], {})
        self.assertEqual(set(wf["jobs"]), {"cleanup"})
        j = wf["jobs"]["cleanup"]
        self.assertEqual(j["environment"], "fleet-cleanup")
        for fragment in ("github.event_name == 'workflow_dispatch'", "github.ref == 'refs/heads/main'",
                         "github.actor_id == '83323977'", "github.run_attempt == 1"):
            self.assertIn(fragment, j["if"])
        self.assertEqual(j["permissions"], {"contents": "read", "actions": "read", "id-token": "write"})
        self.assertNotIn("terraform", text.lower())
        self.assertNotIn("FLEET_LIVE", text)
        self.assertNotIn("LINODE_TOKEN", text)
        self.assertNotIn("AWS_FLEET_ROLE_ARN", text)

    def test_cleanup_does_not_accept_review_queues_or_another_actor(self):
        env = job("fleet-emergency-down.yml")
        self.assertEqual(dp.require_for_run(env, "AWS_CLEANUP_ROLE_ARN", settings().__getitem__), "fleet-cleanup")
        for key, value in (("GITHUB_ACTOR_ID", "1"), ("GITHUB_EVENT_NAME", "schedule"), ("GITHUB_RUN_ATTEMPT", "2")):
            get = mock.Mock()
            with self.subTest(key=key), self.assertRaises(dp.DeploymentBlocked):
                dp.require_for_run({**env, key: value}, "AWS_CLEANUP_ROLE_ARN", get)
            get.assert_not_called()
        data = settings()
        data[f"/repos/{dp.REPO}/environments/fleet-cleanup"]["protection_rules"] = [{"type": "required_reviewers"}]
        with self.assertRaises(dp.DeploymentBlocked):
            dp.require_for_run(env, "AWS_CLEANUP_ROLE_ARN", data.__getitem__)

    def test_signed_claims_must_identify_the_owner_and_dispatch(self):
        env = job("fleet-emergency-down.yml")
        c = claims("fleet-cleanup", "fleet-emergency-down.yml")
        c.update(actor_id="83323977", event_name="workflow_dispatch", run_attempt="1")
        dp.require_token_claims(token(c), env, "fleet-cleanup")
        for key, value in (("actor_id", "1"), ("event_name", "schedule"), ("run_attempt", "2")):
            with self.subTest(key=key), self.assertRaises(dp.DeploymentBlocked):
                dp.require_token_claims(token({**c, key: value}), env, "fleet-cleanup")

    def test_only_the_pinned_emergency_function_and_authorization_version_are_used(self):
        env = {"FLEET_CLEANUP_ENABLED": "true", "GITHUB_RUN_ID": "1", "GITHUB_TOKEN": "LEAKMARK-GH",
               "AWS_CLEANUP_ROLE_ARN": "arn:aws:iam::111122223333:role/LodestarFleetCleanupInvoker",
               "AWS_CLEANUP_FUNCTION_ARN": "arn:aws:lambda:us-east-1:111122223333:function:cleanup-emergency"}
        session = mock.Mock()
        session.client.return_value.invoke.side_effect = [
            {"StatusCode": 200, "Payload": io.BytesIO(b'{"status":"prepared","authorization_version":"version-1"}')},
            {"StatusCode": 200, "Payload": io.BytesIO(b'{"status":"complete"}')},
        ]
        created = datetime.now(timezone.utc).isoformat()
        config = mock.Mock()
        with mock.patch.object(apply, "oidc_session", return_value=(session, {})), \
                mock.patch.object(apply, "github_json", return_value={"created_at": created}), \
                mock.patch.dict(sys.modules, {"botocore": mock.Mock(), "botocore.config": config}), redirect_stdout(io.StringIO()):
            self.assertEqual(ec.main(["invoke"], env), 0)
        calls = session.client.return_value.invoke.call_args_list
        self.assertEqual(len(calls), 2)
        self.assertTrue(all(c.kwargs["FunctionName"] == env["AWS_CLEANUP_FUNCTION_ARN"] for c in calls))
        self.assertEqual(json.loads(calls[0].kwargs["Payload"]), {"operation": "prepare", "requested_at": created})
        self.assertEqual(json.loads(calls[1].kwargs["Payload"]), {"requested_at": created, "authorization_version": "version-1"})
        config.Config.assert_called_once_with(connect_timeout=5, read_timeout=330, retries={"total_max_attempts": 1})
        for arn in (env["AWS_CLEANUP_FUNCTION_ARN"].replace("us-east-1", "us-west-2"), env["AWS_CLEANUP_FUNCTION_ARN"].replace("111122223333", "999988887777"), "LEAKMARK-invalid"):
            with mock.patch.object(apply, "oidc_session") as credentials, redirect_stdout(io.StringIO()) as output:
                self.assertEqual(ec.main(["invoke"], {**env, "AWS_CLEANUP_FUNCTION_ARN": arn}), 1)
            credentials.assert_not_called()
            self.assertNotIn("LEAKMARK", output.getvalue())

    def test_a_delayed_owner_request_never_acquires_credentials(self):
        env = {"FLEET_CLEANUP_ENABLED": "true", "GITHUB_RUN_ID": "1", "GITHUB_TOKEN": "LEAKMARK-GH",
               "AWS_CLEANUP_ROLE_ARN": "arn:aws:iam::111122223333:role/LodestarFleetCleanupInvoker",
               "AWS_CLEANUP_FUNCTION_ARN": "arn:aws:lambda:us-east-1:111122223333:function:cleanup-emergency"}
        with mock.patch.object(apply, "github_json", return_value={"created_at": "2020-01-01T00:00:00Z"}), \
                mock.patch.object(apply, "oidc_session") as credentials, redirect_stdout(io.StringIO()):
            self.assertEqual(ec.main(["invoke"], env), 1)
        credentials.assert_not_called()

    def test_legacy_live_commands_do_not_acquire_credentials(self):
        for command, body in (("act", "/fleet down"), ("reap", ""), ("dispatch-down", "")):
            with mock.patch.object(apply, "live_context") as credentials, redirect_stdout(io.StringIO()):
                self.assertEqual(fleet.main([command], env=event_env(body, FLEET_LIVE="true")), 1)
            credentials.assert_not_called()

    def test_disabled_emergency_mode_never_acquires_credentials(self):
        with mock.patch.object(apply, "oidc_session") as credentials, redirect_stdout(io.StringIO()):
            self.assertEqual(ec.main(["invoke"], {}), 0)
        credentials.assert_not_called()
