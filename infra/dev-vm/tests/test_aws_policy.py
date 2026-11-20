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


def sub(arn):
    return {"Fn::Sub": arn}


INSTANCES = sub("arn:aws:ec2:us-east-1:${AWS::AccountId}:instance/*")
VOLUMES = sub("arn:aws:ec2:us-east-1:${AWS::AccountId}:volume/*")
SUBNET = sub("arn:aws:ec2:us-east-1:${AWS::AccountId}:subnet/${SubnetId}")
EXPECTED = {
    "ReadInventoryAndCost": {
        "Effect": "Allow",
        "Action": ["ec2:Describe*", "ec2:GetConsoleOutput", "ssm:DescribeInstanceInformation",
                   "ssm:DescribeInstanceProperties", "ssm:GetCommandInvocation", "ssm:ListCommandInvocations",
                   "ssm:ListCommands", "cloudformation:DescribeStacks", "cloudformation:ListStacks",
                   "iam:GetInstanceProfile", "iam:ListInstanceProfiles", "ce:GetCostAndUsage",
                   "budgets:ViewBudget", "servicequotas:GetServiceQuota"],
        "Resource": "*"},
    "ReadLatestAmiIds": {
        "Effect": "Allow", "Action": "ssm:GetParameter",
        "Resource": ["arn:aws:ssm:us-east-1::parameter/aws/service/ami-windows-latest/*",
                     "arn:aws:ssm:us-east-1::parameter/aws/service/ami-amazon-linux-latest/*",
                     "arn:aws:ssm:us-east-1::parameter/aws/service/canonical/ubuntu/server/*"]},
    "RunOnlyTheGuardedInstance": {
        "Effect": "Allow", "Action": "ec2:RunInstances", "Resource": INSTANCES,
        "Condition": {
            "StringEquals": {
                "ec2:InstanceType": "c6a.xlarge", "ec2:MetadataHttpTokens": "required",
                "aws:RequestTag/Project": "Lodestar", "aws:RequestTag/Environment": "Test",
                "aws:RequestTag/ManagedBy": "Terraform", "aws:RequestTag/Purpose": "X-test-vm",
                "ec2:MetadataHttpProtocolIpv6": "disabled", "ec2:Tenancy": "default",
                "aws:RequestTag/LaunchedBy": "${aws:userid}"},
            "NumericLessThanEquals": {"ec2:MetadataHttpPutResponseHopLimit": "1"},
            "Null": {"aws:RequestTag/ExpiresAt": "false"},
            "ForAllValues:StringEquals": {"aws:TagKeys": ["Project", "Environment", "ManagedBy", "Purpose",
                                                          "ExpiresAt", "LaunchedBy", "Name"]}}},
    "RunOnlyEncryptedBoundedVolumes": {
        "Effect": "Allow", "Action": "ec2:RunInstances", "Resource": VOLUMES,
        "Condition": {"Bool": {"ec2:Encrypted": "true"}, "NumericLessThanEquals": {"ec2:VolumeSize": "100"},
                      "StringEquals": {"aws:RequestTag/Project": "Lodestar"}}},
    "RunOnlyInTheLodestarNetwork": {
        "Effect": "Allow", "Action": "ec2:RunInstances",
        "Resource": [SUBNET, sub("arn:aws:ec2:us-east-1:${AWS::AccountId}:security-group/${SecurityGroupId}")]},
    "RunNetworkInterfaceOnlyInThatSubnet": {
        "Effect": "Allow", "Action": "ec2:RunInstances",
        "Resource": sub("arn:aws:ec2:us-east-1:${AWS::AccountId}:network-interface/*"),
        "Condition": {"StringEquals": {"ec2:Subnet": SUBNET}}},
    "RunOnlyAmazonOrCanonicalImages": {
        "Effect": "Allow", "Action": "ec2:RunInstances",
        "Resource": ["arn:aws:ec2:us-east-1::image/*", "arn:aws:ec2:us-east-1::snapshot/*"],
        "Condition": {"StringEquals": {"ec2:Owner": ["amazon", {"Ref": "UbuntuImageOwnerId"}]}}},
    "TagOnlyAtLaunch": {
        "Effect": "Allow", "Action": "ec2:CreateTags", "Resource": [INSTANCES, VOLUMES],
        "Condition": {"StringEquals": {"ec2:CreateAction": "RunInstances"}}},
    "PassOnlyTheStackTestRolesToEc2": {
        "Effect": "Allow", "Action": "iam:PassRole",
        "Resource": [{"Fn::GetAtt": ["WindowsTestVmRole", "Arn"]}, {"Fn::GetAtt": ["LinuxTestVmRole", "Arn"]}],
        "Condition": {"StringEquals": {"iam:PassedToService": "ec2.amazonaws.com"}}},
    "ManageOnlyOwnLodestarTestInstances": {
        "Effect": "Allow",
        "Action": ["ec2:TerminateInstances", "ec2:StopInstances", "ec2:StartInstances", "ec2:RebootInstances"],
        "Resource": INSTANCES,
        "Condition": {"StringEquals": {
            "ec2:ResourceTag/Project": "Lodestar", "ec2:ResourceTag/Environment": "Test",
            "ec2:InstanceType": "c6a.xlarge", "ec2:ResourceTag/Purpose": "X-test-vm",
            "ec2:ResourceTag/LaunchedBy": "${aws:userid}"}}},
    "RunCommandsOnlyOnOwnLodestarTestInstances": {
        "Effect": "Allow", "Action": "ssm:SendCommand", "Resource": INSTANCES,
        "Condition": {"StringEquals": {
            "ssm:resourceTag/Project": "Lodestar", "ssm:resourceTag/Environment": "Test",
            "ssm:resourceTag/Purpose": "X-test-vm", "ssm:resourceTag/LaunchedBy": "${aws:userid}"}}},
    "RunCommandsOnlyWithTheStandardScriptDocuments": {
        "Effect": "Allow", "Action": "ssm:SendCommand",
        "Resource": ["arn:aws:ssm:us-east-1::document/AWS-RunPowerShellScript",
                     "arn:aws:ssm:us-east-1::document/AWS-RunShellScript"]},
    "CancelOwnCommands": {"Effect": "Allow", "Action": "ssm:CancelCommand", "Resource": "*"},
    "NothingOutsideUsEast1": {
        "Effect": "Deny", "Action": ["ec2:*", "ssm:*"], "Resource": "*",
        "Condition": {"StringNotEquals": {"aws:RequestedRegion": "us-east-1"}}},
    "NoSecretsManager": {"Effect": "Deny", "Action": "secretsmanager:*", "Resource": "*"},
    "NoInstanceAttributeReads": {"Effect": "Deny", "Action": "ec2:DescribeInstanceAttribute", "Resource": "*"},
}


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
        mgmt = next(s for s in sts if s["Sid"] == "ManageOnlyOwnLodestarTestInstances")
        self.assertEqual(mgmt["Condition"]["StringEquals"]["ec2:ResourceTag/Project"], "Lodestar")
        self.assertEqual(mgmt["Condition"]["StringEquals"]["ec2:ResourceTag/Environment"], "Test")
        cmd = next(s for s in sts if s["Sid"] == "RunCommandsOnlyOnOwnLodestarTestInstances")
        self.assertEqual(cmd["Condition"]["StringEquals"]["ssm:resourceTag/Project"], "Lodestar")

    def test_owner_run_instances_with_the_same_tags_are_out_of_reach(self):
        # The owner's fleet instances carry Project=Lodestar and Environment=Test
        # too (Deploy-AwsWindows.ps1); only Purpose=X-test-vm marks Claude's own.
        _, _, sts = statements()
        mgmt = next(s for s in sts if s["Sid"] == "ManageOnlyOwnLodestarTestInstances")
        self.assertEqual(mgmt["Condition"]["StringEquals"]["ec2:ResourceTag/Purpose"], "X-test-vm")
        cmd = next(s for s in sts if s["Sid"] == "RunCommandsOnlyOnOwnLodestarTestInstances")
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

    def test_pass_role_names_only_the_two_stack_roles(self):
        # B2: every passable role is defined here, holding only what its lane
        # needs; never a role the template doesn't define (the owner's own).
        t, _, sts = statements()
        prs = [s for s in sts if "iam:PassRole" in actions(s)]
        self.assertEqual(len(prs), 1)
        self.assertEqual(prs[0]["Resource"], [{"Fn::GetAtt": ["WindowsTestVmRole", "Arn"]},
                                              {"Fn::GetAtt": ["LinuxTestVmRole", "Arn"]}])
        self.assertEqual(prs[0]["Condition"], {"StringEquals": {"iam:PassedToService": "ec2.amazonaws.com"}})
        self.assertNotIn("InstanceRoleName", t["Parameters"])
        self.assertNotIn("InstanceRoleName", TEMPLATE.read_text())
        self.assertNotIn(":role/", json.dumps(prs[0]))
        roles = sorted(k for k, v in t["Resources"].items() if v["Type"] == "AWS::IAM::Role")
        self.assertEqual(roles, ["LinuxTestVmRole", "WindowsTestVmRole"])

    def test_windows_role_is_ssm_core_only(self):
        t, _, _ = statements()
        res = t["Resources"]
        role = res["WindowsTestVmRole"]["Properties"]
        self.assertEqual(role["ManagedPolicyArns"], ["arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore"])
        self.assertEqual(sorted(role), ["AssumeRolePolicyDocument", "Description", "ManagedPolicyArns",
                                        "RoleName", "Tags"])
        self.assertEqual(role["AssumeRolePolicyDocument"]["Statement"],
                         [{"Effect": "Allow", "Principal": {"Service": "ec2.amazonaws.com"},
                           "Action": "sts:AssumeRole"}])
        self.assertFalse([k for k, v in res.items() if v["Type"] == "AWS::IAM::Policy"])
        self.assertEqual(res["WindowsTestVmInstanceProfile"]["Properties"]["Roles"], [{"Ref": "WindowsTestVmRole"}])

    def test_linux_role_is_ssm_core_and_the_one_secret(self):
        t, _, _ = statements()
        role = t["Resources"]["LinuxTestVmRole"]["Properties"]
        self.assertEqual(role["ManagedPolicyArns"], ["arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore"])
        self.assertEqual(sorted(role), ["AssumeRolePolicyDocument", "Description", "ManagedPolicyArns",
                                        "Policies", "RoleName", "Tags"])
        (inline,) = role["Policies"]
        self.assertEqual(inline["PolicyDocument"]["Statement"],
                         [{"Effect": "Allow", "Action": "secretsmanager:GetSecretValue",
                           "Resource": {"Ref": "PiaLoginSecret"}}])

    def test_mutations_need_the_launching_principal(self):
        # I1: stop, start, reboot, delete and Run Command reach only instances
        # this principal launched; the tag is forced at launch and can't change.
        _, _, sts = statements()
        inst = next(s for s in sts if s["Sid"] == "RunOnlyTheGuardedInstance")
        self.assertEqual(inst["Condition"]["StringEquals"]["aws:RequestTag/LaunchedBy"], "${aws:userid}")
        self.assertIn("LaunchedBy", inst["Condition"]["ForAllValues:StringEquals"]["aws:TagKeys"])
        mgmt = next(s for s in sts if s["Sid"] == "ManageOnlyOwnLodestarTestInstances")
        self.assertEqual(mgmt["Condition"]["StringEquals"]["ec2:ResourceTag/LaunchedBy"], "${aws:userid}")
        cmd = next(s for s in sts if s["Sid"] == "RunCommandsOnlyOnOwnLodestarTestInstances")
        self.assertEqual(cmd["Condition"]["StringEquals"]["ssm:resourceTag/LaunchedBy"], "${aws:userid}")
        mutating = {"ec2:TerminateInstances", "ec2:StopInstances", "ec2:StartInstances", "ec2:RebootInstances",
                    "ec2:ModifyInstanceAttribute", "ec2:CreateTags", "ec2:DeleteTags", "ssm:SendCommand",
                    "ssm:StartSession"}
        for st in sts:
            if st["Effect"] != "Allow" or not mutating & set(actions(st)):
                continue
            with self.subTest(sid=st["Sid"]):
                if st["Sid"] == "TagOnlyAtLaunch":
                    self.assertEqual(st["Condition"], {"StringEquals": {"ec2:CreateAction": "RunInstances"}})
                elif st["Sid"] == "RunCommandsOnlyWithTheStandardScriptDocuments":
                    self.assertTrue(all(r.startswith("arn:aws:ssm:us-east-1::document/") for r in st["Resource"]))
                else:
                    cond = st["Condition"]["StringEquals"]
                    self.assertIn("${aws:userid}", cond.values())
        self.assertFalse(any("ec2:DeleteTags" in actions(s) for s in sts if s["Effect"] == "Allow"))

    def test_launch_needs_default_tenancy(self):
        # I4: no dedicated hosts or instances.
        _, _, sts = statements()
        inst = next(s for s in sts if s["Sid"] == "RunOnlyTheGuardedInstance")
        self.assertEqual(inst["Condition"]["StringEquals"]["ec2:Tenancy"], "default")

    def test_no_instance_attribute_reads(self):
        # O4: Describe* stays, without instance attribute reads.
        _, _, sts = statements()
        deny = next(s for s in sts if s["Sid"] == "NoInstanceAttributeReads")
        self.assertEqual(deny, {"Sid": "NoInstanceAttributeReads", "Effect": "Deny",
                                "Action": "ec2:DescribeInstanceAttribute", "Resource": "*"})

    def test_every_statement_is_pinned_exactly(self):
        # I2: any widening (an action, a resource, a dropped or loosened
        # condition, a new statement) fails here and needs a reviewed change.
        _, _, sts = statements()
        self.assertEqual([s["Sid"] for s in sts], list(EXPECTED))
        for st in sts:
            with self.subTest(sid=st["Sid"]):
                self.assertEqual(st, dict(EXPECTED[st["Sid"]], Sid=st["Sid"]))

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
