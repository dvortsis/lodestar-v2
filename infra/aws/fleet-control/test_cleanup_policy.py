"""Independent cleanup IAM/control-plane allowlist and policy mutation tests."""
import copy
import json
import unittest
import importlib.util
import tempfile
from pathlib import Path
import build_templates

HERE = Path(__file__).resolve().parent
TEMPLATE = json.loads((HERE / "cleanup.json").read_text())
TYPES = {"CleanupToken": "AWS::SecretsManager::Secret", "Executor": "AWS::IAM::Role",
         "ExpirationLog": "AWS::Logs::LogGroup", "Expiration": "AWS::Lambda::Function", "ExpirationErrors": "AWS::CloudWatch::Alarm",
         "EmergencyLog": "AWS::Logs::LogGroup", "Emergency": "AWS::Lambda::Function", "EmergencyErrors": "AWS::CloudWatch::Alarm",
         "ExpirationSchedule": "AWS::Events::Rule", "SchedulePermission": "AWS::Lambda::Permission",
         "LodestarFleetCleanupInvoker": "AWS::IAM::Role", "HeartbeatMetric": "AWS::Logs::MetricFilter", "HeartbeatAlarm": "AWS::CloudWatch::Alarm"}


def object_arn(bucket, key):
    return {"Fn::Sub": ["arn:${AWS::Partition}:s3:::${B}/" + key, {"B": {"Ref": bucket}}]}


def audit(t):
    errors, r = [], t["Resources"]
    if {k: v["Type"] for k, v in r.items()} != TYPES:
        return ["resource inventory differs"]
    invoker = r["LodestarFleetCleanupInvoker"]["Properties"]
    expected_trust = {"Version": "2012-10-17", "Statement": [{"Effect": "Allow", "Action": "sts:AssumeRoleWithWebIdentity",
        "Principal": {"Federated": {"Fn::Sub": "arn:${AWS::Partition}:iam::${AWS::AccountId}:oidc-provider/token.actions.githubusercontent.com"}},
        "Condition": {"StringEquals": {"token.actions.githubusercontent.com:aud": "sts.amazonaws.com",
            "token.actions.githubusercontent.com:sub": "repo:dvortsis@83323977/lodestar-v2@1427502271:environment:fleet-cleanup:workflow_ref:dvortsis/lodestar-v2/.github/workflows/fleet-emergency-down.yml@refs/heads/main",
            "token.actions.githubusercontent.com:actor_id": "83323977", "token.actions.githubusercontent.com:ref": "refs/heads/main",
            "token.actions.githubusercontent.com:repository_id": "1427502271", "token.actions.githubusercontent.com:environment": "fleet-cleanup"}}}]}
    if invoker != {"RoleName": "LodestarFleetCleanupInvoker", "MaxSessionDuration": 3600, "AssumeRolePolicyDocument": expected_trust,
                   "Policies": [{"PolicyName": "InvokeEmergencyOnly", "PolicyDocument": {"Version": "2012-10-17", "Statement": [
                       {"Effect": "Allow", "Action": "lambda:InvokeFunction", "Resource": {"Fn::GetAtt": ["Emergency", "Arn"]}}]}}]}:
        errors.append("invoker has additional authority or mismatched trust")
    role = r["Executor"]["Properties"]
    same_owner = {"StringEquals": {"s3:ResourceAccount": {"Ref": "AWS::AccountId"}}}
    expected = {"AssumeRolePolicyDocument": {"Version": "2012-10-17", "Statement": [{"Effect": "Allow", "Principal": {"Service": "lambda.amazonaws.com"}, "Action": "sts:AssumeRole"}]},
        "Policies": [{"PolicyName": "ScopedCleanup", "PolicyDocument": {"Version": "2012-10-17", "Statement": [
            {"Effect": "Allow", "Action": "s3:GetObject", "Resource": object_arn("StateBucketName", "fleet/cleanup-authorization.json"), "Condition": same_owner},
            {"Effect": "Allow", "Action": "s3:PutObject", "Resource": object_arn("ControlBucketName", "locks/fleet-mismatch.json"), "Condition": same_owner},
            {"Effect": "Allow", "Action": "secretsmanager:GetSecretValue", "Resource": {"Ref": "CleanupToken"}},
            {"Effect": "Allow", "Action": ["logs:CreateLogStream", "logs:PutLogEvents"], "Resource": [
                {"Fn::Sub": "arn:${AWS::Partition}:logs:${AWS::Region}:${AWS::AccountId}:log-group:/aws/lambda/${AWS::StackName}-expiration:*"},
                {"Fn::Sub": "arn:${AWS::Partition}:logs:${AWS::Region}:${AWS::AccountId}:log-group:/aws/lambda/${AWS::StackName}-emergency:*"}]},
        ]}}]}
    if role != expected:
        errors.append("executor authority differs")
    if t["Parameters"]["EnableExpiration"] != {"Type": "String", "Default": "false", "AllowedValues": ["false", "true"],
            "Description": "Owner enables only after credential, scope, dry-run, alerts and canary verification."}:
        errors.append("expiration enabled without owner authorization")
    if t["Conditions"] != {"ExpirationEnabled": {"Fn::Equals": [{"Ref": "EnableExpiration"}, "true"]}}:
        errors.append("enable condition differs")
    if r["ExpirationSchedule"]["Properties"] != {"ScheduleExpression": "rate(15 minutes)", "State": {"Fn::If": ["ExpirationEnabled", "ENABLED", "DISABLED"]},
            "Targets": [{"Arn": {"Fn::GetAtt": ["Expiration", "Arn"]}, "Id": "expiration", "RetryPolicy": {"MaximumEventAgeInSeconds": 900, "MaximumRetryAttempts": 1}}]}:
        errors.append("schedule scope differs")
    if r["SchedulePermission"]["Properties"] != {"Action": "lambda:InvokeFunction", "FunctionName": {"Ref": "Expiration"},
            "Principal": "events.amazonaws.com", "SourceAccount": {"Ref": "AWS::AccountId"}, "SourceArn": {"Fn::GetAtt": ["ExpirationSchedule", "Arn"]}}:
        errors.append("schedule invocation boundary differs")
    for name, mode in (("Expiration", "expiration"), ("Emergency", "emergency")):
        props = r[name]["Properties"]
        if (props["Environment"]["Variables"] != {"AWS_ACCOUNT_ID": {"Ref": "AWS::AccountId"}, "STATE_BUCKET": {"Ref": "StateBucketName"},
                "CONTROL_BUCKET": {"Ref": "ControlBucketName"}, "CLEANUP_SECRET_ARN": {"Ref": "CleanupToken"},
                "LINODE_ACCOUNT_EUUID": {"Ref": "LinodeAccountEuuid"}, "MIN_CREATED_UTC": {"Ref": "MinCreatedUtc"}, "CLEANUP_MODE": mode}
                or props["Role"] != {"Fn::GetAtt": ["Executor", "Arn"]} or props["Timeout"] != 300 or props["ReservedConcurrentExecutions"] != 1
                or props["Code"] != {"ZipFile": build_templates.cleanup_code()}):
            errors.append("function mode/source/account boundary differs")
        if (r[name + "Errors"]["Properties"]["AlarmActions"] != [{"Fn::Sub": "arn:${AWS::Partition}:sns:${AWS::Region}:${AWS::AccountId}:${NotificationTopicName}"}]
                or r[name + "Log"]["Properties"]["RetentionInDays"] != 14):
            errors.append("failure notification/retention missing")
    if (r["HeartbeatAlarm"]["Properties"]["TreatMissingData"] != "breaching"
            or r["HeartbeatAlarm"]["Properties"]["AlarmActions"] != [{"Fn::Sub": "arn:${AWS::Partition}:sns:${AWS::Region}:${AWS::AccountId}:${NotificationTopicName}"}]):
        errors.append("missing cleanup heartbeat does not notify")
    if (r["CleanupToken"]["DeletionPolicy"] != "Retain" or r["CleanupToken"]["UpdateReplacePolicy"] != "Retain"
            or set(r["CleanupToken"]["Properties"]) != {"Name", "Description"}):
        errors.append("credential must be an empty retained shell")
    return errors


MUTATIONS = {
    "owner actor binding removed": lambda t: t["Resources"]["LodestarFleetCleanupInvoker"]["Properties"]["AssumeRolePolicyDocument"]["Statement"][0]["Condition"]["StringEquals"].pop("token.actions.githubusercontent.com:actor_id"),
    "different owner actor": lambda t: t["Resources"]["LodestarFleetCleanupInvoker"]["Properties"]["AssumeRolePolicyDocument"]["Statement"][0]["Condition"]["StringEquals"].update({"token.actions.githubusercontent.com:actor_id": "1"}),
    "branch claim widened": lambda t: t["Resources"]["LodestarFleetCleanupInvoker"]["Properties"]["AssumeRolePolicyDocument"]["Statement"][0]["Condition"]["StringEquals"].update({"token.actions.githubusercontent.com:ref": "*"}),
    "invoker reads credentials": lambda t: t["Resources"]["LodestarFleetCleanupInvoker"]["Properties"]["Policies"][0]["PolicyDocument"]["Statement"].append({"Effect": "Allow", "Action": "secretsmanager:GetSecretValue", "Resource": "*"}),
    "invoker calls any Lambda": lambda t: t["Resources"]["LodestarFleetCleanupInvoker"]["Properties"]["Policies"][0]["PolicyDocument"]["Statement"][0].update(Resource="*"),
    "executor reads tfstate": lambda t: t["Resources"]["Executor"]["Properties"]["Policies"][0]["PolicyDocument"]["Statement"][0].update(Resource=object_arn("StateBucketName", "terraform/lodestar.tfstate")),
    "executor writes state": lambda t: t["Resources"]["Executor"]["Properties"]["Policies"][0]["PolicyDocument"]["Statement"][0].update(Action="s3:PutObject"),
    "executor wildcard secret": lambda t: t["Resources"]["Executor"]["Properties"]["Policies"][0]["PolicyDocument"]["Statement"][2].update(Resource="*"),
    "schedule becomes emergency": lambda t: t["Resources"]["ExpirationSchedule"]["Properties"]["Targets"][0].update(Arn={"Fn::GetAtt": ["Emergency", "Arn"]}),
    "no source account": lambda t: t["Resources"]["SchedulePermission"]["Properties"].pop("SourceAccount"),
    "no source ARN": lambda t: t["Resources"]["SchedulePermission"]["Properties"].pop("SourceArn"),
    "caller-controlled mode": lambda t: t["Resources"]["Expiration"]["Properties"]["Environment"]["Variables"].update(CLEANUP_MODE="emergency"),
    "enabled by default": lambda t: t["Parameters"]["EnableExpiration"].update(Default="true"),
    "no error alert": lambda t: t["Resources"]["EmergencyErrors"]["Properties"].update(AlarmActions=[]),
    "no heartbeat alert": lambda t: t["Resources"]["HeartbeatAlarm"]["Properties"].update(TreatMissingData="notBreaching"),
    "public function URL": lambda t: t["Resources"].update(PublicURL={"Type": "AWS::Lambda::Url", "Properties": {}}),
}


class CleanupPolicy(unittest.TestCase):
    def test_packaged_lambda_runs_the_same_scope_and_expiration_checks(self):
        from test_cleanup import API, auth, START, END
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "index.py"
            path.write_text(TEMPLATE["Resources"]["Expiration"]["Properties"]["Code"]["ZipFile"])
            spec = importlib.util.spec_from_file_location("cleanup_packaged_fixture", path)
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)
            self.assertEqual(module.cleanup(auth(), "EUUID-FLEET", START, "expiration", START, API(), lambda: None, lambda: None)["status"], "not_expired")
            self.assertEqual(module.cleanup(auth(), "EUUID-FLEET", START, "expiration", END, API(), lambda: None, lambda: None)["linodes"], 1)
            api = API()
            api.node["region"] = "eu-west"
            with self.assertRaises(module.Refused):
                module.cleanup(auth(), "EUUID-FLEET", START, "emergency", START, api, lambda: None, lambda: None)
            self.assertFalse(any(m == "DELETE" for m, _ in api.calls))

    def test_exact_cleanup_authority(self):
        self.assertEqual(audit(TEMPLATE), [])

    def test_all_authority_widening_mutations_fail(self):
        for name, mutate in MUTATIONS.items():
            with self.subTest(mutation=name):
                changed = copy.deepcopy(TEMPLATE)
                mutate(changed)
                self.assertTrue(audit(changed))
