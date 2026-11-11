"""Negative deployment boundary tests; fake settings and tokens, no network/AWS."""

import base64
import json
import sys
import unittest
from pathlib import Path
from unittest import mock

import apply
import deployment_policy as dp

ROOT = Path(__file__).resolve().parents[3]
REPO = "dvortsis/lodestar-v2"
PREFIX = "repo:dvortsis@83323977/lodestar-v2@1427502271"


def settings():
    data = {
        f"/repos/{REPO}": {"id": 1427502271, "owner": {"id": 83323977}, "full_name": REPO, "default_branch": "main"},
        f"/repos/{REPO}/actions/oidc/customization/sub": {
            "use_default": False, "use_immutable_subject": True,
            "include_claim_keys": ["repo", "context", "workflow_ref"], "sub_claim_prefix": PREFIX},
    }
    for name in ("cloud-fleet", "pia-delivery", "cloud-operator", "fleet-cleanup"):
        data[f"/repos/{REPO}/environments/{name}"] = {
            "deployment_branch_policy": {"protected_branches": False, "custom_branch_policies": True},
            "can_admins_bypass": False,
            "protection_rules": [{"type": "required_reviewers", "prevent_self_review": False,
                                  "reviewers": [{"type": "User", "reviewer": {"id": 83323977}}]}],
        }
        if name == "fleet-cleanup":
            data[f"/repos/{REPO}/environments/{name}"]["protection_rules"] = []
        data[f"/repos/{REPO}/environments/{name}/deployment-branch-policies?per_page=100"] = {
            "total_count": 1, "branch_policies": [{"name": "main", "type": "branch"}]}
    return data


def job(workflow="fleet.yml"):
    return {"GITHUB_REPOSITORY": REPO, "GITHUB_REPOSITORY_ID": "1427502271",
            "GITHUB_REPOSITORY_OWNER_ID": "83323977", "GITHUB_REF": "refs/heads/main",
            "GITHUB_EVENT_NAME": "workflow_dispatch", "GITHUB_ACTOR_ID": "83323977", "GITHUB_RUN_ATTEMPT": "1",
            "GITHUB_WORKFLOW_REF": f"{REPO}/.github/workflows/{workflow}@refs/heads/main"}


def claims(environment="cloud-fleet", workflow="fleet.yml"):
    return {"iss": "https://token.actions.githubusercontent.com", "aud": "sts.amazonaws.com",
            "sub": f"{PREFIX}:environment:{environment}:workflow_ref:{REPO}/.github/workflows/{workflow}@refs/heads/main",
            "repository": REPO, "repository_id": "1427502271", "repository_owner_id": "83323977",
            "environment": environment, "ref": "refs/heads/main", "workflow_ref": job(workflow)["GITHUB_WORKFLOW_REF"]}


def token(payload):
    # Unsigned fixture; only STS authenticates a real token. No credential here.
    part = base64.urlsafe_b64encode(json.dumps(payload).encode()).decode().rstrip("=")
    return f"test.{part}.signature"


class DeploymentBoundary(unittest.TestCase):
    def test_exact_policy_is_pinned_independently(self):
        self.assertEqual(dp.POLICY, {
            "repository": REPO, "repository_id": 1427502271, "owner_id": 83323977,
            "oidc": {"use_default": False, "use_immutable_subject": True,
                     "include_claim_keys": ["repo", "context", "workflow_ref"]},
            "environments": {"cloud-fleet": ["fleet.yml"], "pia-delivery": ["fleet.yml"],
                             "cloud-operator": ["operator-wake.yml", "operator-apply.yml", "operator-injection.yml"],
                             "fleet-cleanup": ["fleet-emergency-down.yml"]}, "owner_dispatch_environments": ["fleet-cleanup"]})
        self.assertEqual(dp.configuration_errors(settings().__getitem__), [])
        for role, environment in dp.ROLE_ENVIRONMENTS.items():
            for workflow in dp.POLICY["environments"][environment]:
                with self.subTest(role=role, workflow=workflow):
                    self.assertEqual(dp.require_for_run(job(workflow), role, settings().__getitem__), environment)

    def test_every_trust_subject_matches_the_customized_direct_workflow_claims(self):
        subjects = set()
        for name in ("fleet-control.json", "pia-delivery.json", "cleanup.json"):
            template = json.loads((ROOT / "infra/aws/fleet-control" / name).read_text())
            for resource in template["Resources"].values():
                if resource["Type"] == "AWS::IAM::Role" and "sts:AssumeRoleWithWebIdentity" in json.dumps(resource):
                    value = resource["Properties"]["AssumeRolePolicyDocument"]["Statement"][0]["Condition"]["StringEquals"]
                    self.assertEqual(value["token.actions.githubusercontent.com:aud"], "sts.amazonaws.com")
                    subs = value["token.actions.githubusercontent.com:sub"]
                    subjects.update(subs if isinstance(subs, list) else [subs])
        expected = set()
        for environment, workflows in dp.POLICY["environments"].items():
            for workflow in workflows:
                c = claims(environment, workflow)
                if environment == "fleet-cleanup":
                    c.update(actor_id="83323977", event_name="workflow_dispatch", run_attempt="1")
                self.assertEqual(dp.subject(environment, workflow), c["sub"])
                dp.require_token_claims(token(c), job(workflow), environment)
                expected.add(c["sub"])
        self.assertEqual(subjects, expected)

    def test_unsafe_or_missing_live_settings_cannot_pass(self):
        root = f"/repos/{REPO}"
        o = root + "/actions/oidc/customization/sub"
        e = root + "/environments/cloud-fleet"
        b = e + "/deployment-branch-policies?per_page=100"
        mutations = {
            "current default subject": lambda d: d[o].update(use_default=True),
            "mutable subject": lambda d: d[o].update(use_immutable_subject=False),
            "reusable-only claim": lambda d: d[o].update(include_claim_keys=["repo", "context", "job_workflow_ref"]),
            "wrong claim order": lambda d: d[o].update(include_claim_keys=["workflow_ref", "repo", "context"]),
            "recycled repository": lambda d: d[root].update(id=1427502272),
            "wrong owner": lambda d: d[root]["owner"].update(id=1),
            "missing branch protection": lambda d: d[e].update(deployment_branch_policy=None),
            "all protected branches": lambda d: d[e].update(deployment_branch_policy={"protected_branches": True, "custom_branch_policies": False}),
            "main wildcard": lambda d: d[b]["branch_policies"][0].update(name="main*"),
            "main tag": lambda d: d[b]["branch_policies"][0].update(type="tag"),
            "partial rule response": lambda d: d[b].update(total_count=101),
            "extra rule": lambda d: d[b]["branch_policies"].append({"name": "feature", "type": "branch"}),
            "missing reviewer": lambda d: d[e].update(protection_rules=[]),
            "team reviewer": lambda d: d[e]["protection_rules"][0]["reviewers"][0].update(type="Team"),
            "other approver": lambda d: d[e]["protection_rules"][0]["reviewers"].append({"type": "User", "reviewer": {"id": 1}}),
            "admin bypass": lambda d: d[e].update(can_admins_bypass=True),
            "unknown bypass": lambda d: d[e].pop("can_admins_bypass"),
        }
        for label, mutate in mutations.items():
            with self.subTest(case=label):
                data = settings()
                mutate(data)
                self.assertTrue(dp.configuration_errors(data.__getitem__))
                with self.assertRaises(dp.DeploymentBlocked):
                    dp.require_for_run(job(), "AWS_FLEET_ROLE_ARN", data.__getitem__)

    def test_denied_reads_fail_closed_without_sensitive_error_output(self):
        def denied(_):
            raise RuntimeError("LEAKMARK-private-http-body")
        errors = dp.configuration_errors(denied)
        self.assertTrue(errors)
        self.assertNotIn("LEAKMARK", str(errors))

    def test_owner_approval_requires_explicitly_available_self_review(self):
        # A sole owner reviewer cannot approve their own dispatch when this is
        # true. Missing/ill-typed metadata cannot prove approval is available.
        for environment in ("cloud-fleet", "pia-delivery", "cloud-operator"):
            role = next(k for k, v in dp.ROLE_ENVIRONMENTS.items() if v == environment)
            for workflow in dp.POLICY["environments"][environment]:
                for value in (True, None, "false", 0, "missing"):
                    with self.subTest(environment=environment, workflow=workflow, value=value):
                        data = settings()
                        rule = data[f"/repos/{REPO}/environments/{environment}"]["protection_rules"][0]
                        if value == "missing":
                            rule.pop("prevent_self_review")
                        else:
                            rule["prevent_self_review"] = value
                        self.assertIn(f"deployment: {environment} requires prevent_self_review=false for sole-owner approval",
                                      dp.configuration_errors(data.__getitem__, [environment]))
                        with self.assertRaises(dp.DeploymentBlocked):
                            dp.require_for_run(job(workflow), role, data.__getitem__)

    def test_unavailable_owner_self_review_stops_before_oidc_and_sts(self):
        for role, environment in dp.ROLE_ENVIRONMENTS.items():
            if environment == "fleet-cleanup":
                continue  # Independently authorized owner dispatch has no reviewer queue.
            for workflow in dp.POLICY["environments"][environment]:
                with self.subTest(environment=environment, workflow=workflow):
                    data = settings()
                    data[f"/repos/{REPO}/environments/{environment}"]["protection_rules"][0]["prevent_self_review"] = True
                    with mock.patch.object(apply, "github_json", side_effect=lambda _, path: data[path]), \
                            mock.patch("urllib.request.urlopen") as http, \
                            mock.patch.object(apply, "aws_config"), \
                            mock.patch.dict(sys.modules, {"boto3": mock.Mock()}) as modules:
                        with self.assertRaisesRegex(SystemExit, "prevent_self_review=false"):
                            apply.oidc_session({**job(workflow), role: "fake-role",
                                                "ACTIONS_ID_TOKEN_REQUEST_URL": "https://example.invalid/token?x=1",
                                                "ACTIONS_ID_TOKEN_REQUEST_TOKEN": "fake-oidc"}, "1-1", role_env=role)
                        http.assert_not_called()
                        modules["boto3"].client.assert_not_called()

    def test_wrong_run_context_is_blocked_before_any_api_read(self):
        for key, value in (("GITHUB_REF", "refs/heads/feature"), ("GITHUB_REF", "refs/tags/main"),
                           ("GITHUB_REPOSITORY_ID", "1427502272"), ("GITHUB_REPOSITORY_OWNER_ID", "1"),
                           ("GITHUB_WORKFLOW_REF", f"{REPO}/.github/workflows/other.yml@refs/heads/main")):
            with self.subTest(key=key, value=value):
                env, get = job(), mock.Mock()
                env[key] = value
                with self.assertRaises(dp.DeploymentBlocked):
                    dp.require_for_run(env, "AWS_FLEET_ROLE_ARN", get)
                get.assert_not_called()

    def test_wrong_actual_token_claims_fail(self):
        for key in claims():
            payload = claims()
            payload[key] = "LEAKMARK-wrong-value"
            with self.subTest(claim=key), self.assertRaises(dp.DeploymentBlocked) as caught:
                dp.require_token_claims(token(payload), job(), "cloud-fleet")
            self.assertNotIn("LEAKMARK", str(caught.exception))
        payload = claims()
        payload["job_workflow_ref"] = job()["GITHUB_WORKFLOW_REF"]
        with self.assertRaises(dp.DeploymentBlocked):
            dp.require_token_claims(token(payload), job(), "cloud-fleet")
        for malformed in ("invalid", "x.bad.z", token([]), "x" * 65537):
            with self.assertRaises(dp.DeploymentBlocked):
                dp.require_token_claims(malformed, job(), "cloud-fleet")

    def test_both_live_consumers_stop_before_oidc_or_aws_when_approval_is_missing(self):
        for role in ("AWS_FLEET_ROLE_ARN", "AWS_PIA_ROLE_ARN"):
            with self.subTest(role=role), mock.patch.object(dp, "require_for_run", side_effect=dp.DeploymentBlocked("blocked")), \
                    mock.patch("urllib.request.urlopen") as http, mock.patch.dict(sys.modules, {"boto3": mock.Mock()}) as modules:
                with self.assertRaisesRegex(SystemExit, "blocked"):
                    apply.oidc_session({}, "1-1", role_env=role)
                http.assert_not_called()
                modules["boto3"].client.assert_not_called()

    def test_claim_mismatch_never_reaches_sts(self):
        env = {**job(), "ACTIONS_ID_TOKEN_REQUEST_URL": "https://example.invalid/token?x=1",
               "ACTIONS_ID_TOKEN_REQUEST_TOKEN": "LEAKMARK", "AWS_FLEET_ROLE_ARN": "role"}
        response = mock.MagicMock()
        response.__enter__.return_value.read.return_value = json.dumps({"value": token({})}).encode()
        boto = mock.Mock()
        with mock.patch.object(dp, "require_for_run", return_value="cloud-fleet"), \
                mock.patch("urllib.request.urlopen", return_value=response), mock.patch.object(apply, "aws_config"), \
                mock.patch.dict(sys.modules, {"boto3": boto}):
            with self.assertRaises(SystemExit):
                apply.oidc_session(env, "1-1")
        boto.client.assert_not_called()

    def test_anonymous_metadata_request_carries_no_authorization(self):
        response = mock.MagicMock()
        response.__enter__.return_value.read.return_value = b'{}'
        for value in ("", "fake-read-token"):
            with self.subTest(authenticated=bool(value)), mock.patch("urllib.request.urlopen", return_value=response) as http:
                self.assertEqual(apply.github_json(value, f"/repos/{REPO}"), {})
                request = http.call_args.args[0]
                self.assertEqual(request.get_header("Authorization"), f"Bearer {value}" if value else None)

    def test_tokenless_injection_audits_identically_before_oidc_and_sts(self):
        env = {**job("operator-injection.yml"), "ACTIONS_ID_TOKEN_REQUEST_URL": "https://example.invalid/token?x=1",
               "ACTIONS_ID_TOKEN_REQUEST_TOKEN": "fake-oidc", "AWS_BRIDGE_ROLE_ARN": "fake-role"}
        for state in ("protected", "missing-reviewer", "unreadable"):
            with self.subTest(state=state):
                data = settings()
                if state == "missing-reviewer":
                    data[f"/repos/{REPO}/environments/cloud-operator"]["protection_rules"] = []
                def get(value, path):
                    self.assertEqual(value, "")
                    if state == "unreadable":
                        raise RuntimeError("private HTTP body")
                    return data[path]
                response, boto = mock.MagicMock(), mock.Mock()
                response.__enter__.return_value.read.return_value = json.dumps({
                    "value": token(claims("cloud-operator", "operator-injection.yml"))}).encode()
                boto.client.return_value.assume_role_with_web_identity.return_value = {"Credentials": {
                    "AccessKeyId": "fake", "SecretAccessKey": "fake", "SessionToken": "fake"}}
                with mock.patch.object(apply, "github_json", side_effect=get) as metadata, \
                        mock.patch("urllib.request.urlopen", return_value=response) as http, \
                        mock.patch.object(apply, "aws_config"), mock.patch.dict(sys.modules, {"boto3": boto}):
                    if state == "protected":
                        apply.oidc_session(env, "1-1", role_env="AWS_BRIDGE_ROLE_ARN")
                        boto.client.return_value.assume_role_with_web_identity.assert_called_once()
                    else:
                        with self.assertRaises(SystemExit):
                            apply.oidc_session(env, "1-1", role_env="AWS_BRIDGE_ROLE_ARN")
                        http.assert_not_called()
                        boto.client.assert_not_called()
                    self.assertTrue(metadata.called)


if __name__ == "__main__":
    unittest.main()
