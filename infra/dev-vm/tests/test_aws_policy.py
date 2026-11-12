"""Pins the guard rails of the IAM user Claude signs in as (aws-claude-testvm.json)."""
import json
import re
import unittest
from pathlib import Path

TEMPLATE = Path(__file__).resolve().parents[1] / "aws-claude-testvm.json"


def statements():
    t = json.loads(TEMPLATE.read_text())
    user = t["Resources"]["ClaudeTestVmUser"]["Properties"]
    return t, user, user["Policies"][0]["PolicyDocument"]["Statement"]


def actions(st):
    a = st["Action"]
    return [a] if isinstance(a, str) else a


class PolicyTests(unittest.TestCase):
    def test_no_account_or_resource_ids_in_the_public_repo(self):
        text = TEMPLATE.read_text()
        self.assertIsNone(re.search(r"\b\d{12}\b", text))
        self.assertIsNone(re.search(r"\b(subnet|sg|vpc|ami|i)-[0-9a-f]{8,17}\b", text))

    def test_only_the_mcp_sign_in_managed_policy(self):
        _, user, _ = statements()
        self.assertEqual(user["ManagedPolicyArns"], ["arn:aws:iam::aws:policy/AWSMCPSignInOAuthAccessPolicy"])
        self.assertNotIn("Groups", user)

    def test_no_wildcard_writes(self):
        _, _, sts = statements()
        for st in sts:
            if st["Effect"] != "Allow":
                continue
            for a in actions(st):
                self.assertNotEqual(a, "*")
                self.assertFalse(a.endswith(":*"), a)
                if "*" in a:  # only read-only wildcards
                    self.assertTrue(a.split(":")[1].startswith(("Describe", "List", "Get")), a)

    def test_iam_is_pass_role_only_and_to_ec2(self):
        _, _, sts = statements()
        iam = [(a, st) for st in sts if st["Effect"] == "Allow" for a in actions(st) if a.startswith("iam:")]
        writes = [(a, st) for a, st in iam if not a.startswith(("iam:Get", "iam:List"))]
        self.assertEqual([a for a, _ in writes], ["iam:PassRole"])
        self.assertEqual(writes[0][1]["Condition"]["StringEquals"]["iam:PassedToService"], "ec2.amazonaws.com")

    def test_launch_is_pinned_to_the_guarded_shape(self):
        _, _, sts = statements()
        inst = next(s for s in sts if s["Sid"] == "RunOnlyTheGuardedInstance")
        eq = inst["Condition"]["StringEquals"]
        self.assertEqual(eq["ec2:InstanceType"], "c6a.xlarge")
        self.assertEqual(eq["ec2:MetadataHttpTokens"], "required")
        for k, v in {"Project": "Lodestar", "Environment": "Test", "ManagedBy": "Terraform"}.items():
            self.assertEqual(eq[f"aws:RequestTag/{k}"], v)
        self.assertEqual(inst["Condition"]["Null"], {"aws:RequestTag/ExpiresAt": "false"})
        vol = next(s for s in sts if s["Sid"] == "RunOnlyEncryptedBoundedVolumes")
        self.assertEqual(vol["Condition"]["Bool"], {"ec2:Encrypted": "true"})
        self.assertEqual(vol["Condition"]["NumericLessThanEquals"], {"ec2:VolumeSize": "100"})

    def test_every_run_instances_statement_is_scoped(self):
        _, _, sts = statements()
        for st in sts:
            if "ec2:RunInstances" in actions(st):
                res = st["Resource"] if isinstance(st["Resource"], list) else [st["Resource"]]
                self.assertNotIn("*", res, st["Sid"])

    def test_destructive_and_command_actions_need_the_lodestar_tags(self):
        _, _, sts = statements()
        mgmt = next(s for s in sts if s["Sid"] == "ManageOnlyLodestarTestInstances")
        self.assertEqual(mgmt["Condition"]["StringEquals"]["ec2:ResourceTag/Project"], "Lodestar")
        self.assertEqual(mgmt["Condition"]["StringEquals"]["ec2:ResourceTag/Environment"], "Test")
        cmd = next(s for s in sts if s["Sid"] == "RunCommandsOnlyOnLodestarTestInstances")
        self.assertEqual(cmd["Condition"]["StringEquals"]["ssm:resourceTag/Project"], "Lodestar")
        doc = next(s for s in sts if s["Sid"] == "RunCommandsOnlyWithThePowerShellDocument")
        self.assertEqual(doc["Resource"], "arn:aws:ssm:us-east-1::document/AWS-RunPowerShellScript")

    def test_region_fence(self):
        _, _, sts = statements()
        deny = next(s for s in sts if s["Effect"] == "Deny")
        self.assertEqual(deny["Condition"], {"StringNotEquals": {"aws:RequestedRegion": "us-east-1"}})


if __name__ == "__main__":
    unittest.main()
