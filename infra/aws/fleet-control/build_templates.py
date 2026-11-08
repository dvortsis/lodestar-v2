"""Builds the fleet's CloudFormation templates (OP-2a).

Plan: docs/plan/2026-11-08-cloud-operator.md §3-§4 and "Owner setup" step 4.
Decisions: docs/decisions/2026-11-08-OP-2a.md.

    python3 infra/aws/fleet-control/build_templates.py           # write
    python3 infra/aws/fleet-control/build_templates.py --check   # CI

Two stacks, both deployed by the owner, never by CI or an agent:
- fleet-control.json: the GitHub OIDC provider (unless one exists), the
  LodestarFleetOperator and LodestarOperatorBridge roles, the state, evidence
  and control buckets, the `lodestar/akamai-fleet-token` secret shell and the
  unattached deny-all policy.
- pia-delivery.json: LodestarPiaDelivery, in the account that holds
  `lodestar/pia-vpn`, with the `lodestar/akamai-firewall-token` secret shell
  and its own deny-all policy.

No account ID, ARN with an account ID, or resource ID is written here: they
come from AWS pseudo-parameters, stack parameters and references.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
OIDC_HOST = "token.actions.githubusercontent.com"
REPO = "dvortsis/lodestar-v2"

# The exact object keys the control plane uses (docs/decisions/2026-11-08-OP-1.md,
# and this slice's decisions for the Terraform state key).
FLEET_LOCK_KEY = "locks/fleet.json"
FLEET_JSON_KEY = "fleet/fleet.json"
TF_STATE_KEY = "terraform/lodestar.tfstate"
TF_LOCK_KEY = TF_STATE_KEY + ".tflock"  # Terraform >= 1.10 use_lockfile
OPERATOR_LOCK_PREFIX = "locks/operator"  # OP-4 names the exact keys beneath it
CLAUDE_LOCK_KEY = "operator/claude-lock.json"


def sub(environment: str, workflow: str) -> str:
    """The OIDC `sub` with the repo's customized claim template
    (repo, context, job_workflow_ref; owner setup step 2.8)."""
    return f"repo:{REPO}:environment:{environment}:job_workflow_ref:{REPO}/.github/workflows/{workflow}@refs/heads/main"


def trust(provider_arn, subs: list) -> dict:
    return {
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Federated": provider_arn},
            "Action": "sts:AssumeRoleWithWebIdentity",
            "Condition": {"StringEquals": {
                f"{OIDC_HOST}:aud": "sts.amazonaws.com",
                f"{OIDC_HOST}:sub": subs[0] if len(subs) == 1 else subs,
            }},
        }],
    }


def obj(bucket_ref, key: str) -> dict:
    return {"Fn::Sub": ["arn:${AWS::Partition}:s3:::${B}/" + key, {"B": bucket_ref}]}


def bucket_arn(bucket_ref) -> dict:
    return {"Fn::Sub": ["arn:${AWS::Partition}:s3:::${B}", {"B": bucket_ref}]}


def list_prefixes(bucket_ref, prefixes: list[str]) -> dict:
    """ListBucket limited to exact prefixes, so a missing key reads as 404,
    not 403 (the lock and Terraform need to tell the two apart)."""
    return {
        "Effect": "Allow",
        "Action": "s3:ListBucket",
        "Resource": bucket_arn(bucket_ref),
        "Condition": {"StringEquals": {"s3:prefix": prefixes}},
    }


def policy(statements: list) -> dict:
    return {"Version": "2012-10-17", "Statement": statements}


DENY_ALL = policy([{"Effect": "Deny", "Action": "*", "Resource": "*"}])


def deny_all(name: str) -> dict:
    return {
        "Type": "AWS::IAM::ManagedPolicy",
        "Properties": {
            "ManagedPolicyName": name,
            "Description": "Revocation: attach to the fleet roles to cut all access (plan §4, kill switch step 4). Unattached by design.",
            "PolicyDocument": DENY_ALL,
        },
    }


def tls_only(bucket_logical: str, extra: list | None = None) -> dict:
    statements = [{
        "Sid": "DenyNonTls",
        "Effect": "Deny",
        "Principal": "*",
        "Action": "s3:*",
        "Resource": [{"Fn::GetAtt": [bucket_logical, "Arn"]}, {"Fn::Sub": "${" + bucket_logical + ".Arn}/*"}],
        "Condition": {"Bool": {"aws:SecureTransport": "false"}},
    }]
    return {
        "Type": "AWS::S3::BucketPolicy",
        "Properties": {"Bucket": {"Ref": bucket_logical}, "PolicyDocument": policy(statements + (extra or []))},
    }


def bucket(versioned: bool, expire_days: int | None = None) -> dict:
    props = {
        "BucketEncryption": {"ServerSideEncryptionConfiguration": [
            {"ServerSideEncryptionByDefault": {"SSEAlgorithm": "AES256"}, "BucketKeyEnabled": True}]},
        "PublicAccessBlockConfiguration": {
            "BlockPublicAcls": True, "BlockPublicPolicy": True, "IgnorePublicAcls": True, "RestrictPublicBuckets": True},
        "OwnershipControls": {"Rules": [{"ObjectOwnership": "BucketOwnerEnforced"}]},
    }
    rules = [{"Id": "AbortIncompleteUploads", "Status": "Enabled", "AbortIncompleteMultipartUpload": {"DaysAfterInitiation": 1}}]
    if versioned:
        props["VersioningConfiguration"] = {"Status": "Enabled"}
        rules.append({"Id": "ExpireOldVersions", "Status": "Enabled", "NoncurrentVersionExpiration": {"NoncurrentDays": 30}})
    if expire_days:
        rules.append({"Id": "Expire", "Status": "Enabled", "ExpirationInDays": expire_days})
    props["LifecycleConfiguration"] = {"Rules": rules}
    return {"Type": "AWS::S3::Bucket", "DeletionPolicy": "Retain", "UpdateReplacePolicy": "Retain", "Properties": props}


OIDC_PARAMS = {
    "ExistingOidcProviderArn": {
        "Type": "String",
        "Default": "",
        "AllowedPattern": "^$|^arn:aws[a-z-]*:iam::[0-9]{12}:oidc-provider/token\\.actions\\.githubusercontent\\.com$",
        "Description": "Leave empty to create the GitHub OIDC provider. If this account already has one (an account holds one per URL), give its ARN.",
    },
}
OIDC_CONDITION = {"CreateOidcProvider": {"Fn::Equals": [{"Ref": "ExistingOidcProviderArn"}, ""]}}
OIDC_PROVIDER = {
    "Type": "AWS::IAM::OIDCProvider",
    "Condition": "CreateOidcProvider",
    "Properties": {"Url": f"https://{OIDC_HOST}", "ClientIdList": ["sts.amazonaws.com"]},
}
PROVIDER_ARN = {"Fn::If": ["CreateOidcProvider", {"Ref": "GitHubOidcProvider"}, {"Ref": "ExistingOidcProviderArn"}]}


def role(name: str, description: str, subs: list, statements: list) -> dict:
    return {
        "Type": "AWS::IAM::Role",
        "Properties": {
            "RoleName": name,
            "Description": description,
            "MaxSessionDuration": 3600,
            "AssumeRolePolicyDocument": trust(PROVIDER_ARN, subs),
            "Policies": [{"PolicyName": f"{name}Access", "PolicyDocument": policy(statements)}],
        },
    }


def fleet_control() -> dict:
    state, evidence, control = {"Ref": "StateBucket"}, {"Ref": "EvidenceBucket"}, {"Ref": "ControlBucket"}
    fleet_operator = role(
        "LodestarFleetOperator",
        "fleet.yml in the cloud-fleet environment at refs/heads/main. No EC2, SSM or IAM.",
        [sub("cloud-fleet", "fleet.yml")],
        [
            {"Sid": "FleetToken", "Effect": "Allow", "Action": "secretsmanager:GetSecretValue",
             "Resource": {"Ref": "AkamaiFleetToken"}},
            {"Sid": "TerraformStateAndFleetJson", "Effect": "Allow",
             "Action": ["s3:GetObject", "s3:PutObject"],
             "Resource": [obj(state, TF_STATE_KEY), obj(state, TF_LOCK_KEY), obj(state, FLEET_JSON_KEY)]},
            {"Sid": "DeleteTerraformLockAndFleetJson", "Effect": "Allow", "Action": "s3:DeleteObject",
             "Resource": [obj(state, TF_LOCK_KEY), obj(state, FLEET_JSON_KEY)]},
            {"Sid": "ListStateKeys", **list_prefixes(state, [TF_STATE_KEY, TF_LOCK_KEY, FLEET_JSON_KEY])},
            {"Sid": "Evidence", "Effect": "Allow", "Action": ["s3:GetObject", "s3:PutObject"],
             "Resource": obj(evidence, "fleet/*")},
            {"Sid": "FleetLock", "Effect": "Allow", "Action": ["s3:GetObject", "s3:PutObject", "s3:DeleteObject"],
             "Resource": obj(control, FLEET_LOCK_KEY)},
            {"Sid": "ListFleetLock", **list_prefixes(control, [FLEET_LOCK_KEY])},
        ],
    )
    bridge = role(
        "LodestarOperatorBridge",
        "operator-wake, operator-apply and operator-injection in the cloud-operator environment at refs/heads/main.",
        [sub("cloud-operator", w) for w in ("operator-wake.yml", "operator-apply.yml", "operator-injection.yml")],
        [
            {"Sid": "OperatorLocks", "Effect": "Allow", "Action": ["s3:GetObject", "s3:PutObject", "s3:DeleteObject"],
             "Resource": obj(control, OPERATOR_LOCK_PREFIX + "*")},
            {"Sid": "ClaudeLock", "Effect": "Allow", "Action": ["s3:GetObject", "s3:PutObject"],
             "Resource": obj(control, CLAUDE_LOCK_KEY)},
            {"Sid": "ListOperatorKeys", "Effect": "Allow", "Action": "s3:ListBucket", "Resource": bucket_arn(control),
             "Condition": {"StringLike": {"s3:prefix": [OPERATOR_LOCK_PREFIX + "*", CLAUDE_LOCK_KEY]}}},
            {"Sid": "ReadFleetJsonOnly", "Effect": "Allow", "Action": "s3:GetObject",
             "Resource": obj(state, FLEET_JSON_KEY)},
        ],
    )
    cross_account = {
        "Fn::If": ["GrantPiaDelivery", {
            "Sid": "PiaDeliveryReadsFleetJson",
            "Effect": "Allow",
            "Principal": {"AWS": {"Ref": "PiaDeliveryRoleArn"}},
            "Action": "s3:GetObject",
            "Resource": {"Fn::Sub": "${StateBucket.Arn}/" + FLEET_JSON_KEY},
        }, {"Ref": "AWS::NoValue"}],
    }
    return {
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "Lodestar cloud fleet control plane: OIDC provider, LodestarFleetOperator, LodestarOperatorBridge, state/evidence/control buckets, fleet token secret shell, deny-all policy (OP-2a).",
        "Parameters": {
            **OIDC_PARAMS,
            "PiaDeliveryRoleArn": {
                "Type": "String",
                "Default": "",
                "AllowedPattern": "^$|^arn:aws[a-z-]*:iam::[0-9]{12}:role/LodestarPiaDelivery$",
                "Description": "Only if LodestarPiaDelivery sits in another account: its role ARN, granted s3:GetObject on fleet/fleet.json alone. Leave empty when it is in this account.",
            },
        },
        "Conditions": {**OIDC_CONDITION, "GrantPiaDelivery": {"Fn::Not": [{"Fn::Equals": [{"Ref": "PiaDeliveryRoleArn"}, ""]}]}},
        "Resources": {
            "GitHubOidcProvider": OIDC_PROVIDER,
            "StateBucket": bucket(versioned=True),
            "StateBucketPolicy": tls_only("StateBucket", [cross_account]),
            "EvidenceBucket": bucket(versioned=False, expire_days=14),
            "EvidenceBucketPolicy": tls_only("EvidenceBucket"),
            "ControlBucket": bucket(versioned=False),
            "ControlBucketPolicy": tls_only("ControlBucket"),
            "AkamaiFleetToken": {
                "Type": "AWS::SecretsManager::Secret",
                "DeletionPolicy": "Retain",
                "UpdateReplacePolicy": "Retain",
                "Properties": {
                    "Name": "lodestar/akamai-fleet-token",
                    "Description": "Linode token for the fleet's dedicated Akamai account (plan §4). Empty: the owner sets the value in the console. Never the watchdog's lodestar/akamai-token.",
                },
            },
            "LodestarFleetOperator": fleet_operator,
            "LodestarOperatorBridge": bridge,
            "LodestarFleetDenyAll": deny_all("LodestarFleetDenyAll"),
        },
        "Outputs": {
            "StateBucketName": {"Value": {"Ref": "StateBucket"}, "Description": "GitHub secret STATE_BUCKET (cloud-fleet, pia-delivery)"},
            "EvidenceBucketName": {"Value": {"Ref": "EvidenceBucket"}, "Description": "GitHub secret EVIDENCE_BUCKET (cloud-fleet)"},
            "ControlBucketName": {"Value": {"Ref": "ControlBucket"}, "Description": "GitHub secret CONTROL_BUCKET (cloud-fleet, cloud-operator)"},
            "FleetOperatorRoleArn": {"Value": {"Fn::GetAtt": ["LodestarFleetOperator", "Arn"]}, "Description": "GitHub secret AWS_FLEET_ROLE_ARN (cloud-fleet)"},
            "BridgeRoleArn": {"Value": {"Fn::GetAtt": ["LodestarOperatorBridge", "Arn"]}, "Description": "GitHub secret AWS_BRIDGE_ROLE_ARN (cloud-operator)"},
        },
    }


def pia_delivery() -> dict:
    return {
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "Lodestar PIA delivery role, in the account that holds lodestar/pia-vpn: LodestarPiaDelivery, the firewall token secret shell, deny-all policy (OP-2a).",
        "Parameters": {
            **OIDC_PARAMS,
            "PiaSecretArn": {
                "Type": "String",
                "AllowedPattern": "^arn:aws[a-z-]*:secretsmanager:[a-z0-9-]+:[0-9]{12}:secret:lodestar/pia-vpn-[A-Za-z0-9]{6}$",
                "Description": "The exact ARN of lodestar/pia-vpn, with its six-character suffix. Never a wildcard.",
            },
            "StateBucketName": {
                "Type": "String",
                "AllowedPattern": "^[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]$",
                "Description": "The fleet-control stack's StateBucketName output.",
            },
        },
        "Conditions": OIDC_CONDITION,
        "Resources": {
            "GitHubOidcProvider": OIDC_PROVIDER,
            "AkamaiFirewallToken": {
                "Type": "AWS::SecretsManager::Secret",
                "DeletionPolicy": "Retain",
                "UpdateReplacePolicy": "Retain",
                "Properties": {
                    "Name": "lodestar/akamai-firewall-token",
                    "Description": "Linode token with Firewalls read/write only, for deliver-pia's own /32 (plan §4). Empty: the owner sets the value in the console.",
                },
            },
            "LodestarPiaDelivery": role(
                "LodestarPiaDelivery",
                "fleet.yml's deliver-pia job in the pia-delivery environment at refs/heads/main. Two named secrets and fleet.json, nothing else.",
                [sub("pia-delivery", "fleet.yml")],
                [
                    {"Sid": "TwoNamedSecrets", "Effect": "Allow", "Action": "secretsmanager:GetSecretValue",
                     "Resource": [{"Ref": "PiaSecretArn"}, {"Ref": "AkamaiFirewallToken"}]},
                    {"Sid": "FleetJsonOnly", "Effect": "Allow", "Action": "s3:GetObject",
                     "Resource": obj({"Ref": "StateBucketName"}, FLEET_JSON_KEY)},
                ],
            ),
            "LodestarPiaDenyAll": deny_all("LodestarPiaDenyAll"),
        },
        "Outputs": {
            "PiaDeliveryRoleArn": {"Value": {"Fn::GetAtt": ["LodestarPiaDelivery", "Arn"]},
                                   "Description": "GitHub secret AWS_PIA_ROLE_ARN (pia-delivery); and the fleet-control stack's PiaDeliveryRoleArn if cross-account"},
        },
    }


TEMPLATES = {"fleet-control.json": fleet_control, "pia-delivery.json": pia_delivery}


def render(build) -> str:
    return json.dumps(build(), indent=2, sort_keys=False) + "\n"


def main(argv: list[str]) -> int:
    check = argv == ["--check"]
    stale = []
    for name, build in TEMPLATES.items():
        path, text = HERE / name, render(build)
        if check:
            if not path.exists() or path.read_text(encoding="utf-8") != text:
                stale.append(name)
        else:
            path.write_text(text, encoding="utf-8")
    if stale:
        print("stale templates (run build_templates.py): " + ", ".join(stale), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
