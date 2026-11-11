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
import ast
import io
import tokenize
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
OIDC_HOST = "token.actions.githubusercontent.com"
REPO = "dvortsis/lodestar-v2"
GITHUB_POLICY = json.loads((HERE / "github-deployment-policy.json").read_text(encoding="utf-8"))
OWNER, NAME = REPO.split("/")
SUB_PREFIX = f"repo:{OWNER}@{GITHUB_POLICY['owner_id']}/{NAME}@{GITHUB_POLICY['repository_id']}"

# The exact object keys the control plane uses (docs/decisions/2026-11-08-OP-1.md,
# and this slice's decisions for the Terraform state key).
FLEET_LOCK_KEY = "locks/fleet.json"
FLEET_JSON_KEY = "fleet/fleet.json"
SPEND_LEDGER_KEY = "fleet/spend-ledger.json"   # OP-2: the monthly fleet-compute ledger
MISMATCH_KEY = "locks/fleet-mismatch.json"     # OP-2: the recorded audit mismatch
TF_STATE_KEY = "terraform/lodestar.tfstate"
TF_LOCK_KEY = TF_STATE_KEY + ".tflock"  # Terraform >= 1.10 use_lockfile
OPERATOR_LOCK_KEY = "locks/operator.json"  # OP-4: one operator session at a time
CLAUDE_LOCK_KEY = "operator/claude-lock.json"


def sub(environment: str, workflow: str) -> str:
    """Immutable identity plus the environment and direct workflow at main.

    workflow_ref is present for direct jobs; job_workflow_ref is reusable-only.
    The owner must separately apply the reviewed GitHub claim configuration.
    """
    if workflow not in GITHUB_POLICY["environments"].get(environment, []):
        raise ValueError("workflow/environment is not in the deployment policy")
    return f"{SUB_PREFIX}:environment:{environment}:workflow_ref:{REPO}/.github/workflows/{workflow}@refs/heads/main"


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
    """ListBucket limited to exact prefixes. It lets ListObjectsV2 with that
    prefix answer "does this key exist"; it does not turn a GetObject on a
    missing key into 404 (that request carries no s3:prefix)."""
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
    """Deny any request without TLS, or with TLS older than 1.2. Retained with
    the bucket, so deleting the stack can't strip TLS-only from a kept bucket."""
    both = [{"Fn::GetAtt": [bucket_logical, "Arn"]}, {"Fn::Sub": "${" + bucket_logical + ".Arn}/*"}]
    statements = [
        {"Sid": "DenyNonTls", "Effect": "Deny", "Principal": "*", "Action": "s3:*", "Resource": both,
         "Condition": {"Bool": {"aws:SecureTransport": "false"}}},
        {"Sid": "DenyTlsBelow12", "Effect": "Deny", "Principal": "*", "Action": "s3:*", "Resource": both,
         "Condition": {"NumericLessThan": {"s3:TlsVersion": "1.2"}}},
    ]
    return {
        "Type": "AWS::S3::BucketPolicy",
        "DeletionPolicy": "Retain",
        "UpdateReplacePolicy": "Retain",
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
        # Each .tflock delete leaves a marker (fleet.json is tombstoned, never
        # deleted); drop it once no older version remains.
        rules.append({"Id": "DropDeleteMarkers", "Status": "Enabled", "ExpiredObjectDeleteMarker": True})
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
             "Resource": [obj(state, TF_STATE_KEY), obj(state, TF_LOCK_KEY), obj(state, FLEET_JSON_KEY),
                          obj(state, SPEND_LEDGER_KEY), obj(state, "fleet/cleanup-authorization.json")]},
            # fleet.json is never deleted: `down` overwrites it with a tombstone
            # (the runner's refinement), so only the lock file may be deleted.
            {"Sid": "DeleteTerraformLock", "Effect": "Allow", "Action": "s3:DeleteObject",
             "Resource": obj(state, TF_LOCK_KEY)},
            # Unconditional: a HeadObject or GetObject carries no s3:prefix, so only
            # a plain ListBucket turns a missing key into 404. Terraform needs that
            # on a fresh state, and the role already reads and writes every object
            # that matters here; listing shows key names only.
            {"Sid": "ListStateBucket", "Effect": "Allow", "Action": "s3:ListBucket", "Resource": bucket_arn(state)},
            {"Sid": "Evidence", "Effect": "Allow", "Action": ["s3:GetObject", "s3:PutObject"],
             "Resource": obj(evidence, "fleet/*")},
            {"Sid": "FleetLock", "Effect": "Allow", "Action": ["s3:GetObject", "s3:PutObject", "s3:DeleteObject"],
             "Resource": obj(control, FLEET_LOCK_KEY)},
            # The flag is overwritten, never deleted, and its meaning never rests on a 404.
            {"Sid": "MismatchFlag", "Effect": "Allow", "Action": ["s3:GetObject", "s3:PutObject"],
             "Resource": obj(control, MISMATCH_KEY)},
            {"Sid": "ListFleetLock", **list_prefixes(control, [FLEET_LOCK_KEY])},
        ],
    )
    bridge = role(
        "LodestarOperatorBridge",
        "operator-wake, operator-apply and operator-injection in the cloud-operator environment at refs/heads/main.",
        [sub("cloud-operator", w) for w in ("operator-wake.yml", "operator-apply.yml", "operator-injection.yml")],
        [
            {"Sid": "OperatorLock", "Effect": "Allow", "Action": ["s3:GetObject", "s3:PutObject", "s3:DeleteObject"],
             "Resource": obj(control, OPERATOR_LOCK_KEY)},
            {"Sid": "ClaudeLock", "Effect": "Allow", "Action": ["s3:GetObject", "s3:PutObject"],
             "Resource": obj(control, CLAUDE_LOCK_KEY)},
            {"Sid": "ListOperatorKeys", **list_prefixes(control, [OPERATOR_LOCK_KEY, CLAUDE_LOCK_KEY])},
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
        "Metadata": {"AWSToolsMetrics": {"AWSAgentToolkit": "aws-cloudformation@3"},
                     "com.aws.cloudformation.Context": {"ref": [{"at": "docs/operations/fleet-control-bootstrap.md"}]}},
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
        "Metadata": {"AWSToolsMetrics": {"AWSAgentToolkit": "aws-cloudformation@3"},
                     "com.aws.cloudformation.Context": {"ref": [{"at": "docs/operations/fleet-control-bootstrap.md"}]}},
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
            "FleetAccountId": {
                "Type": "String",
                "AllowedPattern": "^[0-9]{12}$",
                "Description": "The account that owns the state bucket. The fleet.json grant holds only in that account, so a re-registered bucket name elsewhere can't feed deliver-pia.",
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
                     "Resource": obj({"Ref": "StateBucketName"}, FLEET_JSON_KEY),
                     "Condition": {"StringEquals": {"s3:ResourceAccount": {"Ref": "FleetAccountId"}}}},
                ],
            ),
            "LodestarPiaDenyAll": deny_all("LodestarPiaDenyAll"),
        },
        "Outputs": {
            "PiaDeliveryRoleArn": {"Value": {"Fn::GetAtt": ["LodestarPiaDelivery", "Arn"]},
                                   "Description": "GitHub secret AWS_PIA_ROLE_ARN (pia-delivery); and the fleet-control stack's PiaDeliveryRoleArn if cross-account"},
        },
    }


def cleanup_code():
    # Package the existing pure auditor into the inline module. No dynamic code
    # execution, external package or credential-bearing deployment artifact.
    audit_source = (HERE.parents[1] / "fleet/audit.py").read_text(encoding="utf-8")
    handler = (HERE / "cleanup_handler.py").read_text(encoding="utf-8")
    handler = handler.replace("from audit import actions_ranges, audit_firewall, run_is_live, JOB_LABEL\n", "")
    text = "from __future__ import annotations\n" + audit_source.replace("from __future__ import annotations\n", "") + "\n" + handler.replace("from __future__ import annotations\n", "")
    # Keep original source spelling: ast.unparse changes across Python versions.
    # AST positions select only unused helpers and non-executable docstrings.
    tree = ast.parse(text)
    unused = {"job_rule", "sweep", "open_rule", "close_rule", "attachment_problems"}
    omit = set()
    for node in ast.walk(tree):
        if (isinstance(node, ast.FunctionDef) and node.name in unused
                or isinstance(node, ast.Expr) and isinstance(node.value, ast.Constant) and isinstance(node.value.value, str)):
            omit.update(range(node.lineno, node.end_lineno + 1))
    lines = text.splitlines()
    for token in tokenize.generate_tokens(io.StringIO(text).readline):
        if token.type == tokenize.COMMENT:
            lines[token.start[0] - 1] = lines[token.start[0] - 1][:token.start[1]]
    return "\n".join(line.rstrip() for i, line in enumerate(lines, 1) if i not in omit and line.strip()) + "\n"


def cleanup_stack() -> dict:
    """Owner-authorized broker; the GitHub caller never receives a Linode token."""
    account = {"Ref": "AWS::AccountId"}
    same_owner = {"StringEquals": {"s3:ResourceAccount": account}}
    params = {name: {"Type": "String", "AllowedPattern": "^[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]$"}
              for name in ("StateBucketName", "ControlBucketName")}
    params.update({
        "LinodeAccountEuuid": {"Type": "String", "AllowedPattern": "^[A-Za-z0-9-]{1,128}$"},
        "MinCreatedUtc": {"Type": "String", "AllowedPattern": "^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$"},
        "NotificationTopicName": {"Type": "String", "AllowedPattern": "^[A-Za-z0-9_-]{1,256}$"},
        "EnableExpiration": {"Type": "String", "Default": "false", "AllowedValues": ["false", "true"],
                             "Description": "Owner enables only after credential, scope, dry-run, alerts and canary verification."},
    })
    resources = {
        "CleanupToken": {"Type": "AWS::SecretsManager::Secret", "DeletionPolicy": "Retain", "UpdateReplacePolicy": "Retain",
                         "Properties": {"Name": "lodestar/akamai-cleanup-token", "Description": "Owner-installed dedicated cleanup credential; never delivered to GitHub. Empty shell."}},
        "Executor": {"Type": "AWS::IAM::Role", "Properties": {
            "AssumeRolePolicyDocument": policy([{"Effect": "Allow", "Principal": {"Service": "lambda.amazonaws.com"}, "Action": "sts:AssumeRole"}]),
            "Policies": [{"PolicyName": "ScopedCleanup", "PolicyDocument": policy([
                {"Effect": "Allow", "Action": "s3:GetObject", "Resource": obj({"Ref": "StateBucketName"}, "fleet/cleanup-authorization.json"), "Condition": same_owner},
                {"Effect": "Allow", "Action": "s3:PutObject", "Resource": obj({"Ref": "ControlBucketName"}, MISMATCH_KEY), "Condition": same_owner},
                {"Effect": "Allow", "Action": "secretsmanager:GetSecretValue", "Resource": {"Ref": "CleanupToken"}},
                {"Effect": "Allow", "Action": ["logs:CreateLogStream", "logs:PutLogEvents"],
                 "Resource": [{"Fn::Sub": "arn:${AWS::Partition}:logs:${AWS::Region}:${AWS::AccountId}:log-group:/aws/lambda/${AWS::StackName}-expiration:*"},
                              {"Fn::Sub": "arn:${AWS::Partition}:logs:${AWS::Region}:${AWS::AccountId}:log-group:/aws/lambda/${AWS::StackName}-emergency:*"}]},
            ])}],
        }},
    }
    code = cleanup_code()
    for name, mode in (("Expiration", "expiration"), ("Emergency", "emergency")):
        resources[name + "Log"] = {"Type": "AWS::Logs::LogGroup", "DeletionPolicy": "Retain", "UpdateReplacePolicy": "Retain",
                                  "Properties": {"LogGroupName": {"Fn::Sub": "/aws/lambda/${AWS::StackName}-" + mode}, "RetentionInDays": 14}}
        resources[name] = {"Type": "AWS::Lambda::Function", "DependsOn": name + "Log", "Properties": {
            "FunctionName": {"Fn::Sub": "${AWS::StackName}-" + mode}, "Runtime": "python3.13", "Handler": "index.handler",
            "Role": {"Fn::GetAtt": ["Executor", "Arn"]}, "Timeout": 300, "MemorySize": 256,
            "ReservedConcurrentExecutions": 1, "Code": {"ZipFile": code},
            "Environment": {"Variables": {"AWS_ACCOUNT_ID": account, "STATE_BUCKET": {"Ref": "StateBucketName"},
                "CONTROL_BUCKET": {"Ref": "ControlBucketName"}, "CLEANUP_SECRET_ARN": {"Ref": "CleanupToken"},
                "LINODE_ACCOUNT_EUUID": {"Ref": "LinodeAccountEuuid"}, "MIN_CREATED_UTC": {"Ref": "MinCreatedUtc"}, "CLEANUP_MODE": mode}},
        }}
        resources[name + "Errors"] = {"Type": "AWS::CloudWatch::Alarm", "Properties": {
            "Namespace": "AWS/Lambda", "MetricName": "Errors", "Dimensions": [{"Name": "FunctionName", "Value": {"Ref": name}}],
            "Statistic": "Sum", "Period": 300, "EvaluationPeriods": 1, "Threshold": 1, "ComparisonOperator": "GreaterThanOrEqualToThreshold",
            "TreatMissingData": "notBreaching", "AlarmActions": [{"Fn::Sub": "arn:${AWS::Partition}:sns:${AWS::Region}:${AWS::AccountId}:${NotificationTopicName}"}]}}
    resources["ExpirationSchedule"] = {"Type": "AWS::Events::Rule", "Properties": {"ScheduleExpression": "rate(15 minutes)", "State": {"Fn::If": ["ExpirationEnabled", "ENABLED", "DISABLED"]},
        "Targets": [{"Arn": {"Fn::GetAtt": ["Expiration", "Arn"]}, "Id": "expiration", "RetryPolicy": {"MaximumEventAgeInSeconds": 900, "MaximumRetryAttempts": 1}}]}}
    resources["SchedulePermission"] = {"Type": "AWS::Lambda::Permission", "Properties": {
        "Action": "lambda:InvokeFunction", "FunctionName": {"Ref": "Expiration"}, "Principal": "events.amazonaws.com",
        "SourceAccount": account, "SourceArn": {"Fn::GetAtt": ["ExpirationSchedule", "Arn"]}}}
    resources["LodestarFleetCleanupInvoker"] = {"Type": "AWS::IAM::Role", "Properties": {
        "RoleName": "LodestarFleetCleanupInvoker", "MaxSessionDuration": 3600,
        "AssumeRolePolicyDocument": {"Version": "2012-10-17", "Statement": [{"Effect": "Allow", "Action": "sts:AssumeRoleWithWebIdentity",
            "Principal": {"Federated": {"Fn::Sub": "arn:${AWS::Partition}:iam::${AWS::AccountId}:oidc-provider/token.actions.githubusercontent.com"}}, "Condition": {"StringEquals": {
                f"{OIDC_HOST}:aud": "sts.amazonaws.com", f"{OIDC_HOST}:sub": sub("fleet-cleanup", "fleet-emergency-down.yml"),
                f"{OIDC_HOST}:actor_id": str(GITHUB_POLICY["owner_id"]), f"{OIDC_HOST}:ref": "refs/heads/main",
                f"{OIDC_HOST}:repository_id": str(GITHUB_POLICY["repository_id"]), f"{OIDC_HOST}:environment": "fleet-cleanup"}}}]},
        "Policies": [{"PolicyName": "InvokeEmergencyOnly", "PolicyDocument": policy([{"Effect": "Allow", "Action": "lambda:InvokeFunction",
                                                                                     "Resource": {"Fn::GetAtt": ["Emergency", "Arn"]}}])}],
    }}
    resources["HeartbeatMetric"] = {"Type": "AWS::Logs::MetricFilter", "Properties": {"LogGroupName": {"Ref": "ExpirationLog"},
        "FilterPattern": '{ $.event = "cleanup_complete" }', "MetricTransformations": [{"MetricNamespace": "LodestarCleanup", "MetricName": {"Fn::Sub": "${AWS::StackName}-heartbeat"}, "MetricValue": "1"}]}}
    resources["HeartbeatAlarm"] = {"Type": "AWS::CloudWatch::Alarm", "Properties": {"Namespace": "LodestarCleanup",
        "MetricName": {"Fn::Sub": "${AWS::StackName}-heartbeat"}, "Statistic": "Sum", "Period": 3600, "EvaluationPeriods": 1,
        "Threshold": 1, "ComparisonOperator": "LessThanThreshold", "TreatMissingData": "breaching", "AlarmActions": [{"Fn::Sub": "arn:${AWS::Partition}:sns:${AWS::Region}:${AWS::AccountId}:${NotificationTopicName}"}]}}
    return {"AWSTemplateFormatVersion": "2010-09-09", "Description": "Independently authorized expiration and owner-emergency cleanup; no Terraform or Linode credentials in GitHub.",
            "Metadata": {"AWSToolsMetrics": {"AWSAgentToolkit": "aws-cloudformation@3"}, "com.aws.cloudformation.Context": {"ref": [{"at": "docs/operations/fleet-control-cleanup.md"}]}},
            "Parameters": params, "Conditions": {"ExpirationEnabled": {"Fn::Equals": [{"Ref": "EnableExpiration"}, "true"]}}, "Resources": resources, "Outputs": {
                "CleanupRoleArn": {"Value": {"Fn::GetAtt": ["LodestarFleetCleanupInvoker", "Arn"]}},
                "EmergencyFunctionArn": {"Value": {"Fn::GetAtt": ["Emergency", "Arn"]}},
                "CleanupSecretArn": {"Value": {"Ref": "CleanupToken"}},
            }}


TEMPLATES = {"fleet-control.json": fleet_control, "pia-delivery.json": pia_delivery, "cleanup.json": cleanup_stack}


def render(build) -> str:
    text = json.dumps(build(), indent=0 if build is cleanup_stack else 2, sort_keys=False) + "\n"
    if len(text.encode("utf-8")) > 51200:
        raise ValueError("template exceeds inline body limit; owner-reviewed packaging required")
    return text


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
