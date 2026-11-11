"""An independent allowlist audit of the fleet templates, plus mutation tests (OP-2a).

`audit()` re-states, here and not via build_templates.py, every resource
type, every role's whole shape and statements, every bucket policy and the
only strings allowed to hold `*` or `?`. The committed templates must give no
problems; each mutation below (widened grants the runner's review found
slipping past the first tests, among others) must give at least one.
Standard library only; no cloud call.
"""

from __future__ import annotations

import copy
import json
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
OIDC = "token.actions.githubusercontent.com"
REPO = "dvortsis/lodestar-v2"


def load(name):
    return json.loads((HERE / name).read_text(encoding="utf-8"))


def sub(env, wf):
    return f"repo:{REPO}:environment:{env}:job_workflow_ref:{REPO}/.github/workflows/{wf}@refs/heads/main"


PROVIDER = {"Fn::If": ["CreateOidcProvider", {"Ref": "GitHubOidcProvider"}, {"Ref": "ExistingOidcProviderArn"}]}


def trust(subs):
    return {"Version": "2012-10-17", "Statement": [{
        "Effect": "Allow", "Principal": {"Federated": PROVIDER}, "Action": "sts:AssumeRoleWithWebIdentity",
        "Condition": {"StringEquals": {f"{OIDC}:aud": "sts.amazonaws.com", f"{OIDC}:sub": subs}}}]}


TYPES = {
    "fleet-control.json": {
        "GitHubOidcProvider": "AWS::IAM::OIDCProvider",
        "StateBucket": "AWS::S3::Bucket", "StateBucketPolicy": "AWS::S3::BucketPolicy",
        "EvidenceBucket": "AWS::S3::Bucket", "EvidenceBucketPolicy": "AWS::S3::BucketPolicy",
        "ControlBucket": "AWS::S3::Bucket", "ControlBucketPolicy": "AWS::S3::BucketPolicy",
        "AkamaiFleetToken": "AWS::SecretsManager::Secret",
        "LodestarFleetOperator": "AWS::IAM::Role", "LodestarOperatorBridge": "AWS::IAM::Role",
        "LodestarFleetDenyAll": "AWS::IAM::ManagedPolicy",
    },
    "pia-delivery.json": {
        "GitHubOidcProvider": "AWS::IAM::OIDCProvider",
        "AkamaiFirewallToken": "AWS::SecretsManager::Secret",
        "LodestarPiaDelivery": "AWS::IAM::Role",
        "LodestarPiaDenyAll": "AWS::IAM::ManagedPolicy",
    },
}

GET, PUT, DEL, LIST, SECRET = "s3:GetObject", "s3:PutObject", "s3:DeleteObject", "s3:ListBucket", "secretsmanager:GetSecretValue"
NO_COND = "null"


def cond(c):
    return json.dumps(c, sort_keys=True)


# Each role: (trust subjects, [(effect, actions, resources, condition)]) in order.
ROLES = {
    "LodestarFleetOperator": (sub("cloud-fleet", "fleet.yml"), [
        ("Allow", (SECRET,), ("ref:AkamaiFleetToken",), NO_COND),
        ("Allow", (GET, PUT), ("s3:StateBucket/terraform/lodestar.tfstate", "s3:StateBucket/terraform/lodestar.tfstate.tflock",
                               "s3:StateBucket/fleet/fleet.json", "s3:StateBucket/fleet/spend-ledger.json"), NO_COND),
        ("Allow", (DEL,), ("s3:StateBucket/terraform/lodestar.tfstate.tflock",), NO_COND),
        ("Allow", (LIST,), ("s3:StateBucket",), NO_COND),
        ("Allow", (GET, PUT), ("s3:EvidenceBucket/fleet/*",), NO_COND),
        ("Allow", (DEL, GET, PUT), ("s3:ControlBucket/locks/fleet.json",), NO_COND),
        ("Allow", (GET, PUT), ("s3:ControlBucket/locks/fleet-mismatch.json",), NO_COND),
        ("Allow", (LIST,), ("s3:ControlBucket",), cond({"StringEquals": {"s3:prefix": ["locks/fleet.json"]}})),
    ]),
    "LodestarOperatorBridge": ([sub("cloud-operator", w) for w in ("operator-wake.yml", "operator-apply.yml", "operator-injection.yml")], [
        ("Allow", (DEL, GET, PUT), ("s3:ControlBucket/locks/operator.json",), NO_COND),
        ("Allow", (GET, PUT), ("s3:ControlBucket/operator/claude-lock.json",), NO_COND),
        ("Allow", (LIST,), ("s3:ControlBucket",), cond({"StringEquals": {"s3:prefix": ["locks/operator.json", "operator/claude-lock.json"]}})),
        ("Allow", (GET,), ("s3:StateBucket/fleet/fleet.json",), NO_COND),
    ]),
    "LodestarPiaDelivery": (sub("pia-delivery", "fleet.yml"), [
        ("Allow", (SECRET,), ("ref:PiaSecretArn", "ref:AkamaiFirewallToken"), NO_COND),
        ("Allow", (GET,), ("s3:StateBucketName/fleet/fleet.json",),
         cond({"StringEquals": {"s3:ResourceAccount": {"Ref": "FleetAccountId"}}})),
    ]),
}

DENY_ALL = {"Version": "2012-10-17", "Statement": [{"Effect": "Deny", "Action": "*", "Resource": "*"}]}


def tls_statements(bucket):
    both = [{"Fn::GetAtt": [bucket, "Arn"]}, {"Fn::Sub": "${" + bucket + ".Arn}/*"}]
    return [
        {"Sid": "DenyNonTls", "Effect": "Deny", "Principal": "*", "Action": "s3:*", "Resource": both,
         "Condition": {"Bool": {"aws:SecureTransport": "false"}}},
        {"Sid": "DenyTlsBelow12", "Effect": "Deny", "Principal": "*", "Action": "s3:*", "Resource": both,
         "Condition": {"NumericLessThan": {"s3:TlsVersion": "1.2"}}},
    ]


CROSS_ACCOUNT = {"Fn::If": ["GrantPiaDelivery", {
    "Sid": "PiaDeliveryReadsFleetJson", "Effect": "Allow", "Principal": {"AWS": {"Ref": "PiaDeliveryRoleArn"}},
    "Action": "s3:GetObject", "Resource": {"Fn::Sub": "${StateBucket.Arn}/fleet/fleet.json"}}, {"Ref": "AWS::NoValue"}]}

BUCKET_POLICIES = {
    "StateBucketPolicy": ("StateBucket", tls_statements("StateBucket") + [CROSS_ACCOUNT]),
    "EvidenceBucketPolicy": ("EvidenceBucket", tls_statements("EvidenceBucket")),
    "ControlBucketPolicy": ("ControlBucket", tls_statements("ControlBucket")),
}

# The only strings in any resource that may hold `*` or `?`.
WILDCARD_OK = {
    "arn:${AWS::Partition}:s3:::${B}/fleet/*",
}


def resource_name(r):
    """`ref:X`, or `s3:<bucket ref>[/key]` for an ARN built from one bucket Ref; else unrecognised."""
    if isinstance(r, dict) and set(r) == {"Ref"} and isinstance(r["Ref"], str):
        return "ref:" + r["Ref"]
    if isinstance(r, dict) and set(r) == {"Fn::Sub"} and isinstance(r["Fn::Sub"], list) and len(r["Fn::Sub"]) == 2:
        tmpl, args = r["Fn::Sub"]
        prefix = "arn:${AWS::Partition}:s3:::${B}"
        if (isinstance(tmpl, str) and tmpl.startswith(prefix) and isinstance(args, dict) and set(args) == {"B"}
                and isinstance(args["B"], dict) and set(args["B"]) == {"Ref"}):
            return "s3:" + args["B"]["Ref"] + tmpl[len(prefix):]
    return "UNRECOGNISED " + json.dumps(r, sort_keys=True)


def as_list(x):
    return x if isinstance(x, list) else [x]


def statement_shape(s):
    if not isinstance(s, dict) or not set(s) <= {"Sid", "Effect", "Action", "Resource", "Condition"}:
        return ("BAD KEYS", json.dumps(s, sort_keys=True))
    return (s.get("Effect"), tuple(sorted(as_list(s.get("Action")))),
            tuple(resource_name(r) for r in as_list(s.get("Resource"))), cond(s.get("Condition")))


def strings(node, path=""):
    if isinstance(node, str):
        yield path, node
    elif isinstance(node, dict):
        for k, v in node.items():
            yield from strings(k, f"{path}.<key>")
            yield from strings(v, f"{path}.{k}")
    elif isinstance(node, list):
        for i, v in enumerate(node):
            yield from strings(v, f"{path}[{i}]")


def audit(templates: dict) -> list[str]:
    problems = []
    for tname, t in templates.items():
        res = t.get("Resources", {})
        got = {k: v.get("Type") for k, v in res.items()}
        if got != TYPES[tname]:
            problems.append(f"{tname}: resources {got} != allowlist")
        for name, r in res.items():
            p, typ = r.get("Properties", {}), r.get("Type")
            if typ == "AWS::IAM::Role":
                if name not in ROLES:
                    problems.append(f"{name}: unexpected role")
                    continue
                subs, want = ROLES[name]
                if set(p) != {"RoleName", "Description", "MaxSessionDuration", "AssumeRolePolicyDocument", "Policies"}:
                    problems.append(f"{name}: role keys {sorted(p)}")
                if p.get("RoleName") != name or p.get("MaxSessionDuration") != 3600:
                    problems.append(f"{name}: name or session length")
                if p.get("AssumeRolePolicyDocument") != trust(subs):
                    problems.append(f"{name}: trust differs")
                pols = p.get("Policies", [])
                if len(pols) != 1 or set(pols[0]) != {"PolicyName", "PolicyDocument"} \
                        or set(pols[0]["PolicyDocument"]) != {"Version", "Statement"} \
                        or pols[0]["PolicyDocument"]["Version"] != "2012-10-17":
                    problems.append(f"{name}: not exactly one inline policy with Version 2012-10-17")
                stmts = [s for pol in pols for s in pol.get("PolicyDocument", {}).get("Statement", [])]
                if [statement_shape(s) for s in stmts] != want:
                    problems.append(f"{name}: statements differ from the allowlist")
            elif typ == "AWS::IAM::ManagedPolicy":
                if set(p) != {"ManagedPolicyName", "Description", "PolicyDocument"} or p.get("PolicyDocument") != DENY_ALL:
                    problems.append(f"{name}: not the unattached deny-all")
                continue  # the deny-all is the one place `*` belongs
            elif typ == "AWS::S3::BucketPolicy":
                bucket, want = BUCKET_POLICIES.get(name, (None, None))
                if p != {"Bucket": {"Ref": bucket}, "PolicyDocument": {"Version": "2012-10-17", "Statement": want}}:
                    problems.append(f"{name}: bucket policy differs from the allowlist")
                if r.get("DeletionPolicy") != "Retain" or r.get("UpdateReplacePolicy") != "Retain":
                    problems.append(f"{name}: not retained")
                continue  # its `*`s are pinned exactly above
            for path, s in strings(r):
                if ("*" in s or "?" in s) and s not in WILDCARD_OK:
                    problems.append(f"{name}{path}: wildcard {s!r}")
    return problems


TEMPLATES = {"fleet-control.json": load("fleet-control.json"), "pia-delivery.json": load("pia-delivery.json")}


def mutate(fn):
    t = copy.deepcopy(TEMPLATES)
    fn(t)
    return t


def role(t, name):
    for tmpl in t.values():
        if name in tmpl["Resources"]:
            return tmpl["Resources"][name]["Properties"]
    raise KeyError(name)


def add(name, stmt):
    return lambda t: role(t, name)["Policies"][0]["PolicyDocument"]["Statement"].append(stmt)


MUTATIONS = {
    # The six the runner's review found slipping past the first tests:
    "GetObject on arn:aws:s3:::*/*": add("LodestarFleetOperator", {"Effect": "Allow", "Action": "s3:GetObject", "Resource": "arn:aws:s3:::*/*"}),
    "GetObject on an Fn::Sub of every bucket": add("LodestarFleetOperator", {
        "Effect": "Allow", "Action": "s3:GetObject", "Resource": {"Fn::Sub": "arn:${AWS::Partition}:s3:::*"}}),
    "logs on every log group": add("LodestarOperatorBridge", {"Effect": "Allow", "Action": "logs:PutLogEvents", "Resource": "arn:aws:logs:*:*:*"}),
    "iam:PassRole on the bridge": add("LodestarOperatorBridge", {"Effect": "Allow", "Action": "iam:PassRole", "Resource": {"Ref": "AkamaiFleetToken"}}),
    "a second inline policy": lambda t: role(t, "LodestarFleetOperator")["Policies"].append(
        {"PolicyName": "Extra", "PolicyDocument": {"Version": "2012-10-17", "Statement": [
            {"Effect": "Allow", "Action": "s3:PutBucketPolicy", "Resource": {"Ref": "StateBucket"}}]}}),
    "AdministratorAccess attached": lambda t: role(t, "LodestarPiaDelivery").__setitem__(
        "ManagedPolicyArns", ["arn:aws:iam::aws:policy/AdministratorAccess"]),
    # Others:
    "the bridge lists everything": lambda t: role(t, "LodestarOperatorBridge")["Policies"][0]["PolicyDocument"]["Statement"][2]
        .__setitem__("Condition", {"StringLike": {"s3:prefix": ["*"]}}),
    "the PIA grant loses its account condition": lambda t: role(t, "LodestarPiaDelivery")["Policies"][0]["PolicyDocument"]["Statement"][1]
        .pop("Condition"),
    "a wildcard subject": lambda t: role(t, "LodestarFleetOperator")["AssumeRolePolicyDocument"]["Statement"][0]["Condition"]
        ["StringEquals"].__setitem__(f"{OIDC}:sub", sub("cloud-fleet", "*")),
    "StringLike trust": lambda t: role(t, "LodestarPiaDelivery")["AssumeRolePolicyDocument"]["Statement"][0].__setitem__(
        "Condition", {"StringLike": role(t, "LodestarPiaDelivery")["AssumeRolePolicyDocument"]["Statement"][0]["Condition"]["StringEquals"]}),
    "a public bucket-policy Allow": lambda t: t["fleet-control.json"]["Resources"]["ControlBucketPolicy"]["Properties"]["PolicyDocument"]
        ["Statement"].append({"Effect": "Allow", "Principal": "*", "Action": "s3:GetObject", "Resource": "arn:aws:s3:::x/*"}),
    "the TLS 1.2 floor removed": lambda t: t["fleet-control.json"]["Resources"]["EvidenceBucketPolicy"]["Properties"]["PolicyDocument"]
        ["Statement"].pop(1),
    "the cross-account grant widened": lambda t: t["fleet-control.json"]["Resources"]["StateBucketPolicy"]["Properties"]["PolicyDocument"]
        ["Statement"][2]["Fn::If"][1].__setitem__("Action", ["s3:GetObject", "s3:PutObject"]),
    "a bucket policy not retained": lambda t: t["fleet-control.json"]["Resources"]["StateBucketPolicy"].pop("DeletionPolicy"),
    "an extra resource type": lambda t: t["pia-delivery.json"]["Resources"].__setitem__(
        "Backdoor", {"Type": "AWS::IAM::User", "Properties": {}}),
    "the deny-all attached": lambda t: t["fleet-control.json"]["Resources"]["LodestarFleetDenyAll"]["Properties"].__setitem__(
        "Roles", [{"Ref": "LodestarFleetOperator"}]),
    "a wildcard in an Fn::Join": add("LodestarOperatorBridge", {"Effect": "Allow", "Action": "s3:GetObject",
        "Resource": {"Fn::Join": ["", ["arn:aws:s3:::", {"Ref": "StateBucket"}, "/*"]]}}),
    "a longer session": lambda t: role(t, "LodestarFleetOperator").__setitem__("MaxSessionDuration", 43200),
    "an old policy language version": lambda t: role(t, "LodestarOperatorBridge")["Policies"][0]["PolicyDocument"].__setitem__(
        "Version", "2008-10-17"),
    "a wider key than the ledger": add("LodestarFleetOperator", {"Effect": "Allow", "Action": "s3:PutObject",
        "Resource": {"Fn::Sub": ["arn:${AWS::Partition}:s3:::${B}/fleet/spend-*", {"B": {"Ref": "StateBucket"}}]}}),
}


class Audit(unittest.TestCase):
    def test_committed_templates_pass(self):
        self.assertEqual(audit(TEMPLATES), [])

    def test_every_mutation_is_caught(self):
        for label, fn in MUTATIONS.items():
            with self.subTest(mutation=label):
                self.assertNotEqual(audit(mutate(fn)), [], f"{label} passed the audit")


if __name__ == "__main__":
    unittest.main()
