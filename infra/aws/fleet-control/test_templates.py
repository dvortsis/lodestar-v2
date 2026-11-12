"""Policy tests over the fleet's CloudFormation templates (OP-2a).

Standard library only, over the parsed JSON. No cloud call. Run:
    python3 -m unittest discover -s infra/aws/fleet-control -p 'test_*.py'
"""

from __future__ import annotations

import json
import re
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(ROOT / "infra" / "fleet"))
sys.path.insert(0, str(ROOT / "infra" / "operator"))

import build_templates as bt  # noqa: E402
import lock as fleet_lock  # noqa: E402
import oplock as operator_lock  # noqa: E402
import state as fleet_state  # noqa: E402

REPO = "dvortsis/lodestar-v2"
SUB = re.compile(
    rf"^repo:dvortsis@83323977/lodestar-v2@1427502271:environment:(?P<env>[a-z-]+):workflow_ref:{re.escape(REPO)}"
    r"/\.github/workflows/(?P<wf>[a-z-]+\.yml)@refs/heads/main$"
)
EXPECTED_SUBS = {
    "LodestarFleetOperator": {("cloud-fleet", "fleet.yml")},
    "LodestarOperatorBridge": {("cloud-operator", w) for w in ("operator-wake.yml", "operator-apply.yml", "operator-injection.yml")},
    "LodestarPiaDelivery": {("pia-delivery", "fleet.yml")},
}


def load(name):
    return json.loads((HERE / name).read_text(encoding="utf-8"))


FC, PIA = load("fleet-control.json"), load("pia-delivery.json")
TEMPLATES = {"fleet-control.json": FC, "pia-delivery.json": PIA}


def resources(t, type_):
    return {k: v for k, v in t["Resources"].items() if v["Type"] == type_}


def roles():
    out = {}
    for t in TEMPLATES.values():
        out.update(resources(t, "AWS::IAM::Role"))
    return out


def allow_statements(role):
    return [s for p in role["Properties"]["Policies"] for s in p["PolicyDocument"]["Statement"]]


def as_list(x):
    return x if isinstance(x, list) else [x]


def s3_key(resource):
    """The object key (or prefix) of an `obj()` ARN, or None for a non-S3-object ARN."""
    if isinstance(resource, dict) and "Fn::Sub" in resource and isinstance(resource["Fn::Sub"], list):
        tmpl = resource["Fn::Sub"][0]
        if tmpl.startswith("arn:${AWS::Partition}:s3:::${B}/"):
            return (resource["Fn::Sub"][1]["B"]["Ref"], tmpl.split("${B}/", 1)[1])
    return None


def granted(role_name):
    """{(bucket logical/param, key): set(actions)} for one role's S3 object grants."""
    out = {}
    for s in allow_statements(roles()[role_name]):
        for r in as_list(s["Resource"]):
            k = s3_key(r)
            if k:
                out.setdefault(k, set()).update(as_list(s["Action"]))
    return out


class Built(unittest.TestCase):
    def test_committed_templates_are_what_the_builder_makes(self):
        self.assertEqual(bt.main(["--check"]), 0)

    def test_keys_match_the_control_plane(self):
        self.assertEqual(bt.FLEET_LOCK_KEY, fleet_lock.LOCK_KEY)
        self.assertEqual(bt.FLEET_JSON_KEY, fleet_state.FLEET_KEY)
        self.assertEqual(bt.SPEND_LEDGER_KEY, fleet_state.LEDGER_KEY)
        self.assertEqual(bt.MISMATCH_KEY, fleet_state.MISMATCH_KEY)
        self.assertEqual(bt.OPERATOR_LOCK_KEY, operator_lock.LOCK_KEY)
        self.assertEqual(bt.CLAUDE_LOCK_KEY, operator_lock.CLAUDE_LOCK_KEY)

    def test_bootstrap_guidance_preserves_existing_secrets(self):
        readme = (HERE / "README.md").read_text(encoding="utf-8")
        self.assertNotIn("force-delete-without-recovery", readme)
        self.assertNotIn("delete-secret", readme)
        self.assertIn("stop and preserve it", readme)
        self.assertIn("resource-import/adoption", readme)


class Trust(unittest.TestCase):
    def test_all_source_aws_oidc_roles_are_in_this_policy_audit(self):
        found = set()
        for path in (ROOT / "infra" / "aws").rglob("*.json"):
            template = json.loads(path.read_text(encoding="utf-8"))
            for name, resource in template.get("Resources", {}).items():
                if resource.get("Type") != "AWS::IAM::Role":
                    continue
                trust = resource.get("Properties", {}).get("AssumeRolePolicyDocument", {})
                if "sts:AssumeRoleWithWebIdentity" in json.dumps(trust):
                    self.assertIn(path.resolve(), {(HERE / name).resolve() for name in (*TEMPLATES, "cleanup.json")})
                    found.add(name)
        self.assertEqual(found, set(EXPECTED_SUBS) | {"LodestarFleetCleanupInvoker"})

    def test_every_role_pins_aud_and_an_exact_sub_at_main(self):
        self.assertEqual(set(roles()), set(EXPECTED_SUBS))
        for name, role in roles().items():
            with self.subTest(role=name):
                stmts = role["Properties"]["AssumeRolePolicyDocument"]["Statement"]
                self.assertEqual(len(stmts), 1)
                s = stmts[0]
                self.assertEqual((s["Effect"], s["Action"]), ("Allow", "sts:AssumeRoleWithWebIdentity"))
                self.assertEqual(set(s["Principal"]), {"Federated"})
                self.assertEqual(set(s["Condition"]), {"StringEquals"})  # never StringLike
                cond = s["Condition"]["StringEquals"]
                self.assertEqual(set(cond), {f"{bt.OIDC_HOST}:aud", f"{bt.OIDC_HOST}:sub"})
                self.assertEqual(cond[f"{bt.OIDC_HOST}:aud"], "sts.amazonaws.com")
                found = set()
                for sub in as_list(cond[f"{bt.OIDC_HOST}:sub"]):
                    self.assertIsInstance(sub, str)
                    self.assertNotIn("*", sub)
                    self.assertNotIn("?", sub)
                    m = SUB.match(sub)
                    self.assertIsNotNone(m, sub)
                    found.add((m["env"], m["wf"]))
                self.assertEqual(found, EXPECTED_SUBS[name])

    def test_sessions_are_short(self):
        for name, role in roles().items():
            self.assertLessEqual(role["Properties"]["MaxSessionDuration"], 3600, name)


class LeastPrivilege(unittest.TestCase):
    def test_no_wildcard_action_or_resource_outside_deny_all(self):
        for tname, t in TEMPLATES.items():
            text = json.dumps(t)
            for name, r in t["Resources"].items():
                docs = []
                if r["Type"] == "AWS::IAM::Role":
                    docs = [p["PolicyDocument"] for p in r["Properties"]["Policies"]]
                elif r["Type"] == "AWS::IAM::ManagedPolicy":
                    self.assertEqual(r["Properties"]["PolicyDocument"], bt.DENY_ALL, name)
                    continue
                for doc in docs:
                    for s in doc["Statement"]:
                        with self.subTest(template=tname, resource=name, sid=s.get("Sid")):
                            self.assertEqual(s["Effect"], "Allow")
                            for a in as_list(s["Action"]):
                                self.assertNotIn("*", a)
                            for res in as_list(s["Resource"]):
                                self.assertNotEqual(res, "*")
            self.assertNotIn('"Action": "*"', text.replace(json.dumps(bt.DENY_ALL["Statement"][0])[1:-1], ""))

    def test_deny_all_is_unattached(self):
        for tname, t in TEMPLATES.items():
            self.assertEqual(len(resources(t, "AWS::IAM::ManagedPolicy")), 1, tname)  # each stack has its own
            for name, p in resources(t, "AWS::IAM::ManagedPolicy").items():
                for k in ("Roles", "Users", "Groups"):
                    self.assertNotIn(k, p["Properties"], name)

    def test_fleet_operator_grants(self):
        g = granted("LodestarFleetOperator")
        self.assertEqual(g, {
            ("StateBucket", bt.TF_STATE_KEY): {"s3:GetObject", "s3:PutObject"},
            ("StateBucket", bt.TF_LOCK_KEY): {"s3:GetObject", "s3:PutObject", "s3:DeleteObject"},
            ("StateBucket", "fleet/fleet.json"): {"s3:GetObject", "s3:PutObject"},  # tombstoned, never deleted
            ("StateBucket", "fleet/spend-ledger.json"): {"s3:GetObject", "s3:PutObject"},
            ("StateBucket", "fleet/cleanup-authorization.json"): {"s3:GetObject", "s3:PutObject"},
            ("EvidenceBucket", "fleet/*"): {"s3:GetObject", "s3:PutObject"},
            ("ControlBucket", "locks/fleet.json"): {"s3:GetObject", "s3:PutObject", "s3:DeleteObject"},
            ("ControlBucket", "locks/fleet-mismatch.json"): {"s3:GetObject", "s3:PutObject"},  # overwritten, never deleted
        })
        actions = {a for s in allow_statements(roles()["LodestarFleetOperator"]) for a in as_list(s["Action"])}
        self.assertFalse({a for a in actions if a.split(":")[0] in ("ec2", "ssm", "iam", "sts", "kms")})
        secrets = [s for s in allow_statements(roles()["LodestarFleetOperator"]) if s["Action"] == "secretsmanager:GetSecretValue"]
        self.assertEqual([s["Resource"] for s in secrets], [{"Ref": "AkamaiFleetToken"}])

    def test_bridge_grants(self):
        g = granted("LodestarOperatorBridge")
        self.assertEqual(g, {
            ("ControlBucket", "locks/operator.json"): {"s3:GetObject", "s3:PutObject", "s3:DeleteObject"},
            ("ControlBucket", "operator/claude-lock.json"): {"s3:GetObject", "s3:PutObject"},
            ("StateBucket", "fleet/fleet.json"): {"s3:GetObject"},
        })
        actions = {a for s in allow_statements(roles()["LodestarOperatorBridge"]) for a in as_list(s["Action"])}
        self.assertFalse({a for a in actions if a.startswith("secretsmanager:")})

    def test_bridge_names_exact_keys_and_no_wildcard(self):
        # OP-4 narrowed the plan's `locks/operator*` to the two keys the bridge uses.
        text = json.dumps(roles()["LodestarOperatorBridge"]["Properties"]["Policies"])
        self.assertNotIn("*", text)
        self.assertNotIn("?", text)
        self.assertNotIn("StringLike", text)
        keys = {k for _, k in granted("LodestarOperatorBridge")}
        self.assertEqual(keys, {"locks/operator.json", "operator/claude-lock.json", "fleet/fleet.json"})

    def test_pia_delivery_is_two_named_secrets_and_fleet_json(self):
        stmts = allow_statements(roles()["LodestarPiaDelivery"])
        self.assertEqual(len(stmts), 2)
        secrets, fleet_json = stmts
        self.assertEqual(secrets["Action"], "secretsmanager:GetSecretValue")
        self.assertEqual(secrets["Resource"], [{"Ref": "PiaSecretArn"}, {"Ref": "AkamaiFirewallToken"}])
        self.assertEqual(granted("LodestarPiaDelivery"), {("StateBucketName", "fleet/fleet.json"): {"s3:GetObject"}})
        # Only in the fleet's account: a re-registered bucket name elsewhere can't feed deliver-pia.
        self.assertEqual(fleet_json["Condition"], {"StringEquals": {"s3:ResourceAccount": {"Ref": "FleetAccountId"}}})
        self.assertEqual(PIA["Parameters"]["FleetAccountId"]["AllowedPattern"], "^[0-9]{12}$")
        self.assertNotIn("Default", PIA["Parameters"]["FleetAccountId"])
        pat = re.compile(PIA["Parameters"]["PiaSecretArn"]["AllowedPattern"])
        fake = "arn:aws:secretsmanager:us-east-1:111122223333:secret:"  # a placeholder account
        self.assertTrue(pat.fullmatch(fake + "lodestar/pia-vpn-AbC123"))
        for bad in ("lodestar/pia-vpn-*", "lodestar/pia-vpn-??????", "lodestar/*", "*", "lodestar/pia-vpn",
                    "lodestar/other-AbC123"):
            self.assertIsNone(pat.fullmatch(fake + bad), bad)

    def lists(self, role_name):
        out = {}
        for s in allow_statements(roles()[role_name]):
            if "s3:ListBucket" in as_list(s["Action"]):
                self.assertEqual(as_list(s["Action"]), ["s3:ListBucket"])
                bucket = s["Resource"]["Fn::Sub"][1]["B"]["Ref"]
                out[bucket] = s.get("Condition")
        return out

    def test_list_grants_are_exact(self):
        self.assertEqual(self.lists("LodestarFleetOperator"), {
            "StateBucket": None,  # unconditional: Terraform needs 404 on a fresh state
            "ControlBucket": {"StringEquals": {"s3:prefix": ["locks/fleet.json"]}},
        })
        self.assertEqual(self.lists("LodestarOperatorBridge"), {
            "ControlBucket": {"StringEquals": {"s3:prefix": ["locks/operator.json", "operator/claude-lock.json"]}},
        })
        self.assertEqual(self.lists("LodestarPiaDelivery"), {})

    def test_cross_account_grant_is_one_object_and_conditional(self):
        stmt = FC["Resources"]["StateBucketPolicy"]["Properties"]["PolicyDocument"]["Statement"][2]
        cond, grant, none = stmt["Fn::If"]
        self.assertEqual((cond, none), ("GrantPiaDelivery", {"Ref": "AWS::NoValue"}))
        self.assertEqual(grant["Principal"], {"AWS": {"Ref": "PiaDeliveryRoleArn"}})
        self.assertEqual(grant["Action"], "s3:GetObject")
        self.assertEqual(grant["Resource"], {"Fn::Sub": "${StateBucket.Arn}/fleet/fleet.json"})
        self.assertTrue(FC["Parameters"]["PiaDeliveryRoleArn"]["AllowedPattern"].endswith(":role/LodestarPiaDelivery$"))


class Buckets(unittest.TestCase):
    def test_every_bucket_is_private_encrypted_and_tls_only(self):
        buckets = resources(FC, "AWS::S3::Bucket")
        self.assertEqual(set(buckets), {"StateBucket", "EvidenceBucket", "ControlBucket"})
        policies = {p["Properties"]["Bucket"]["Ref"]: p for p in resources(FC, "AWS::S3::BucketPolicy").values()}
        for name, b in buckets.items():
            with self.subTest(bucket=name):
                p = b["Properties"]
                self.assertEqual(set(p["PublicAccessBlockConfiguration"].values()), {True})
                self.assertEqual(len(p["PublicAccessBlockConfiguration"]), 4)
                rule = p["BucketEncryption"]["ServerSideEncryptionConfiguration"][0]
                self.assertEqual(rule["ServerSideEncryptionByDefault"], {"SSEAlgorithm": "AES256"})
                self.assertEqual(p["OwnershipControls"]["Rules"][0]["ObjectOwnership"], "BucketOwnerEnforced")
                self.assertEqual(b["DeletionPolicy"], "Retain")
                stmts = policies[name]["Properties"]["PolicyDocument"]["Statement"]
                tls = stmts[0]
                self.assertEqual((tls["Sid"], tls["Effect"], tls["Principal"], tls["Action"]), ("DenyNonTls", "Deny", "*", "s3:*"))
                self.assertEqual(tls["Condition"], {"Bool": {"aws:SecureTransport": "false"}})
                self.assertEqual(tls["Resource"], [{"Fn::GetAtt": [name, "Arn"]}, {"Fn::Sub": "${" + name + ".Arn}/*"}])
                floor = stmts[1]
                self.assertEqual((floor["Sid"], floor["Effect"], floor["Principal"], floor["Action"]), ("DenyTlsBelow12", "Deny", "*", "s3:*"))
                self.assertEqual(floor["Condition"], {"NumericLessThan": {"s3:TlsVersion": "1.2"}})
                self.assertEqual(floor["Resource"], tls["Resource"])
                self.assertEqual(policies[name]["DeletionPolicy"], "Retain")
                # Nothing else, except the state bucket's one conditional grant.
                self.assertEqual(len(stmts), 3 if name == "StateBucket" else 2)
                for s in stmts:
                    inner = s["Fn::If"][1] if "Fn::If" in s else s
                    if inner["Effect"] == "Allow":
                        self.assertNotEqual(inner["Principal"], "*")
                        self.assertNotEqual(inner.get("Principal", {}).get("AWS"), "*")
        self.assertEqual(buckets["StateBucket"]["Properties"]["VersioningConfiguration"], {"Status": "Enabled"})
        state_rules = buckets["StateBucket"]["Properties"]["LifecycleConfiguration"]["Rules"]
        self.assertIn(True, [r.get("ExpiredObjectDeleteMarker") for r in state_rules])
        self.assertNotIn("VersioningConfiguration", buckets["ControlBucket"]["Properties"])
        rules = buckets["EvidenceBucket"]["Properties"]["LifecycleConfiguration"]["Rules"]
        self.assertIn(14, [r.get("ExpirationInDays") for r in rules])

    def test_bucket_names_are_generated(self):
        for name, b in resources(FC, "AWS::S3::Bucket").items():
            self.assertNotIn("BucketName", b["Properties"], name)

    def test_secret_shells_hold_no_value(self):
        shells = {**resources(FC, "AWS::SecretsManager::Secret"), **resources(PIA, "AWS::SecretsManager::Secret")}
        self.assertEqual({s["Properties"]["Name"] for s in shells.values()},
                         {"lodestar/akamai-fleet-token", "lodestar/akamai-firewall-token"})
        for name, s in shells.items():
            self.assertFalse({"SecretString", "GenerateSecretString"} & set(s["Properties"]), name)


class NoPrivateLiterals(unittest.TestCase):
    def test_no_account_ids_arns_or_ips(self):
        for name, t in TEMPLATES.items():
            text = json.dumps(t)
            with self.subTest(template=name):
                self.assertIsNone(re.search(r"(?<![\d{])\d{12}(?![\d}])", text), "a 12-digit literal")
                self.assertIsNone(re.search(r"arn:aws[a-z-]*:[a-z0-9-]+:[a-z0-9-]*:\d", text), "an ARN with an account ID")
                self.assertIsNone(re.search(r"\b\d{1,3}(\.\d{1,3}){3}\b", text), "an IP address")
                for p in t["Parameters"].values():
                    if p.get("Default") not in (None, ""):
                        self.fail("a parameter with a literal default")


def ps_code(text: str) -> str:
    """PowerShell without block or full-line comments, and with string literals
    emptied, so only code is scanned (a message mentioning terraform is fine)."""
    code = re.sub(r"<#.*?#>", "", text, flags=re.S)
    code = "\n".join(ln for ln in code.splitlines() if not ln.lstrip().startswith("#"))
    # One left-to-right pass, so an apostrophe inside "..." can't pair wrongly.
    return re.sub(r"'(?:[^']|'')*'|\"(?:[^\"`]|`.)*\"", "''", code)


# An init call however terraform is named: bare, .exe, a path, quoted after
# `&`, or through a variable (`& $tf init`).
_QUOTED_TF = re.compile(r"""&\s*(['"])[^'"]*?terraform(?:\.exe)?\1""", re.I)
_INIT = re.compile(r"(?:\bterraform(?:\.exe)?|&\s*\$[\w:]+)\s+init\b", re.I)


def init_lines(text: str) -> list[str]:
    code = _QUOTED_TF.sub("& terraform", re.sub(r"<#.*?#>", "", text, flags=re.S))
    return [ln for ln in ps_code(code).splitlines() if _INIT.search(ln)]


class Terraform(unittest.TestCase):
    TF = ROOT / "infra" / "akamai" / "terraform"

    def test_backend_is_partial_with_native_locking(self):
        text = (self.TF / "backend.tf").read_text(encoding="utf-8")
        code = "\n".join(ln for ln in text.splitlines() if not ln.lstrip().startswith("#"))
        self.assertIn('backend "s3"', code)
        self.assertRegex(code, r"use_lockfile\s*=\s*true")
        self.assertRegex(code, r"encrypt\s*=\s*true")
        for arg in ("bucket", "key", "region", "dynamodb_table", "profile", "access_key", "secret_key", "role_arn"):
            self.assertNotRegex(code, rf"\b{arg}\s*=", f"{arg} must come from -backend-config")

    def test_every_script_that_inits_uses_the_shared_state(self):
        scripts = self.TF / "scripts"
        helper = (scripts / "StateBackend.ps1").read_text(encoding="utf-8")
        self.assertIn(f"$key = '{bt.TF_STATE_KEY}'", helper)  # the key fleet.yml's role is granted
        self.assertIn("LODESTAR_TF_STATE_BUCKET", helper)
        self.assertIn("LODESTAR_TF_STATE_REGION", helper)
        initing = [p for p in scripts.glob("*.ps1") if init_lines(p.read_text(encoding="utf-8"))]
        self.assertEqual({p.name for p in initing}, {"Deploy.ps1", "Destroy.ps1", "Preflight.ps1"})
        for p in initing:
            text = p.read_text(encoding="utf-8")
            with self.subTest(script=p.name):
                self.assertIn(". (Join-Path $PSScriptRoot 'StateBackend.ps1')", text)
                self.assertIn("$backend = Get-StateBackendArgs", text)
                for line in init_lines(text):
                    self.assertIn("@backend", line)
                    self.assertNotIn("-backend=false", line)

    def test_init_scan_sees_every_spelling(self):
        for src in ("terraform init", "& terraform init -input=false", "terraform.exe init", "& 'terraform' init",
                    '& "C:\\tools\\terraform.exe" init', "& $tf init", "& $script:tf init", "  TERRAFORM INIT"):
            with self.subTest(src=src):
                self.assertEqual(len(init_lines(src)), 1)
        for src in ("throw 'terraform init failed.'", "# terraform init", "<# terraform init #>",
                    'Audit "run terraform init first"', "terraform validate"):
            with self.subTest(src=src):
                self.assertEqual(init_lines(src), [])

    def test_state_backend_helper_stays_pure(self):
        # CI runs tests/Test-StateBackend.ps1, which dot-sources this helper; the
        # runner's rule is that nothing run in CI can reach a cloud.
        for name in ("scripts/StateBackend.ps1", "tests/Test-StateBackend.ps1"):
            text = (self.TF / name).read_text(encoding="utf-8")
            code = ps_code(text)
            with self.subTest(file=name):
                for banned in (r"\bterraform\b", r"(?-i:\baws\s+[a-z0-9-]+)", r"Invoke-(RestMethod|WebRequest|Expression)", r"Start-Process",
                               r"New-Object\s+System\.Net", r"\[System\.Net\.", r"\b(curl|wget|iwr|irm|iex)\b"):
                    self.assertIsNone(re.search(banned, code, re.I), banned)

    def test_required_version_has_native_s3_locking(self):
        text = (self.TF / "versions.tf").read_text(encoding="utf-8")
        self.assertRegex(text, r'required_version\s*=\s*">=\s*1\.1[0-9]\.')


if __name__ == "__main__":
    unittest.main()
