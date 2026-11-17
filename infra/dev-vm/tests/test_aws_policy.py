"""Pins the guard rails of the IAM user Claude signs in as (aws-claude-testvm.json)."""
import json
import re
import unittest
from pathlib import Path

TEMPLATE = Path(__file__).resolve().parents[1] / "aws-claude-testvm.json"


def statements():
    t = json.loads(TEMPLATE.read_text())
    user = t["Resources"]["ClaudeTestVmUser"]["Properties"]
    return t, user, t["Resources"]["ClaudeTestVmPolicy"]["Properties"]["PolicyDocument"]["Statement"]


def actions(st):
    a = st["Action"]
    return [a] if isinstance(a, str) else a


class PolicyTests(unittest.TestCase):
    def test_no_account_or_resource_ids_in_the_public_repo(self):
        # Not even a public one: Canonical's publisher ID is a parameter the
        # owner looks up at deploy. Every text file here, not just the template.
        for f in sorted(TEMPLATE.parent.rglob("*")):
            if f.is_dir() or "__pycache__" in f.parts:
                continue
            text = f.read_text()
            with self.subTest(file=f.name):
                self.assertEqual(re.findall(r"\d{12}", text), [])
                self.assertIsNone(re.search(r"\b(subnet|sg|vpc|ami|i)-[0-9a-f]{8,17}\b", text))

    def test_only_the_mcp_sign_in_policy_and_our_own(self):
        _, user, _ = statements()
        self.assertEqual(user["ManagedPolicyArns"],
                         ["arn:aws:iam::aws:policy/AWSMCPSignInOAuthAccessPolicy", {"Ref": "ClaudeTestVmPolicy"}])
        self.assertNotIn("Groups", user)
        self.assertNotIn("Policies", user)

    def test_policy_fits_iams_managed_policy_limit(self):
        t, _, _ = statements()
        doc = t["Resources"]["ClaudeTestVmPolicy"]["Properties"]["PolicyDocument"]
        # IAM counts the policy without whitespace; Fn::Sub ARNs grow by the account ID.
        self.assertLess(len(json.dumps(doc, separators=(",", ":"))) + 12 * 20, 6144)

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
        self.assertEqual(eq["ec2:MetadataHttpProtocolIpv6"], "disabled")
        # Bridged containers are kept from the role only by the hop limit.
        self.assertEqual(inst["Condition"]["NumericLessThanEquals"], {"ec2:MetadataHttpPutResponseHopLimit": "1"})
        for k, v in {"Project": "Lodestar", "Environment": "Test", "ManagedBy": "Terraform",
                     "Purpose": "X-test-vm"}.items():
            self.assertEqual(eq[f"aws:RequestTag/{k}"], v)
        self.assertIn("Purpose", inst["Condition"]["ForAllValues:StringEquals"]["aws:TagKeys"])
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

    def test_owner_run_instances_with_the_same_tags_are_out_of_reach(self):
        # The owner's fleet instances carry Project=Lodestar and Environment=Test
        # too (Deploy-AwsWindows.ps1); only Purpose=X-test-vm marks Claude's own.
        _, _, sts = statements()
        mgmt = next(s for s in sts if s["Sid"] == "ManageOnlyLodestarTestInstances")
        self.assertEqual(mgmt["Condition"]["StringEquals"]["ec2:ResourceTag/Purpose"], "X-test-vm")
        cmd = next(s for s in sts if s["Sid"] == "RunCommandsOnlyOnLodestarTestInstances")
        self.assertEqual(cmd["Condition"]["StringEquals"]["ssm:resourceTag/Purpose"], "X-test-vm")
        doc = next(s for s in sts if s["Sid"] == "RunCommandsOnlyWithTheStandardScriptDocuments")
        self.assertEqual(doc["Resource"], ["arn:aws:ssm:us-east-1::document/AWS-RunPowerShellScript",
                                           "arn:aws:ssm:us-east-1::document/AWS-RunShellScript"])

    def test_images_only_from_amazon_or_canonical(self):
        _, _, sts = statements()
        img = next(s for s in sts if s["Sid"] == "RunOnlyAmazonOrCanonicalImages")
        self.assertEqual(img["Condition"], {"StringEquals": {"ec2:Owner": ["amazon", {"Ref": "UbuntuImageOwnerId"}]}})
        t, _, _ = statements()
        param = t["Parameters"]["UbuntuImageOwnerId"]
        self.assertEqual(param["AllowedPattern"], "^[0-9]{12}$")
        self.assertNotIn("Default", param)

    def test_pass_role_names_only_the_two_test_roles(self):
        _, _, sts = statements()
        pr = next(s for s in sts if s["Sid"] == "PassOnlyTheTestInstanceRolesToEc2")
        self.assertEqual(pr["Resource"], [{"Fn::Sub": "arn:aws:iam::${AWS::AccountId}:role/${InstanceRoleName}"},
                                          {"Fn::GetAtt": ["LinuxTestVmRole", "Arn"]}])

    def test_the_user_is_denied_secrets_manager(self):
        _, _, sts = statements()
        self.assertFalse(any(a.startswith("secretsmanager:") for s in sts if s["Effect"] == "Allow"
                             for a in actions(s)))
        deny = next(s for s in sts if s["Sid"] == "NoSecretsManager")
        self.assertEqual((deny["Effect"], deny["Action"], deny["Resource"]), ("Deny", "secretsmanager:*", "*"))

    def test_only_the_linux_role_reads_the_pia_secret(self):
        t, _, _ = statements()
        res = t["Resources"]
        secret = res["PiaLoginSecret"]["Properties"]
        self.assertEqual(json.loads(secret["SecretString"]), {"username": "", "password": ""})
        self.assertNotIn("ResourcePolicy", res)
        role = res["LinuxTestVmRole"]["Properties"]
        trust = role["AssumeRolePolicyDocument"]["Statement"]
        self.assertEqual(trust, [{"Effect": "Allow", "Principal": {"Service": "ec2.amazonaws.com"},
                                  "Action": "sts:AssumeRole"}])
        self.assertEqual(role["ManagedPolicyArns"], ["arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore"])
        (inline,) = role["Policies"]
        self.assertEqual(inline["PolicyDocument"]["Statement"],
                         [{"Effect": "Allow", "Action": "secretsmanager:GetSecretValue",
                           "Resource": {"Ref": "PiaLoginSecret"}}])
        self.assertEqual(res["LinuxTestVmInstanceProfile"]["Properties"]["Roles"], [{"Ref": "LinuxTestVmRole"}])

    def test_deployed_policy_is_updated_in_place(self):
        # A managed policy's Description and Path can't change in place, and its
        # fixed name makes the replacement fail with AlreadyExists (2026-11-15:
        # the stack update stopped at UPDATE_FAILED). Keep both as deployed.
        t, _, _ = statements()
        props = t["Resources"]["ClaudeTestVmPolicy"]["Properties"]
        self.assertEqual(props["Description"], "X-test-vm: what Claude may do in AWS (the Windows test instance only).")
        self.assertNotIn("Path", props)

    def test_region_fence(self):
        _, _, sts = statements()
        deny = next(s for s in sts if s["Sid"] == "NothingOutsideUsEast1")
        self.assertEqual(deny["Effect"], "Deny")
        self.assertEqual(deny["Condition"], {"StringNotEquals": {"aws:RequestedRegion": "us-east-1"}})


if __name__ == "__main__":
    unittest.main()
