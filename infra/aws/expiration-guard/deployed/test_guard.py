"""Unit tests for the AWS expiration guard Lambda (index.py). They never contact AWS.

Run: python3 -m unittest test_guard -v   (from this folder)
"""
import importlib.util
import os
import sys
import types
import unittest
from datetime import datetime, timedelta, timezone
from unittest.mock import MagicMock, patch

VPC = "vpc-0aaaaaaaaaaaaaaaa"
SG = "sg-0bbbbbbbbbbbbbbbb"
TOPIC = "arn:aws:sns:us-east-1:111122223333:lodestar-expiration-alerts"

# Stub the AWS SDK only while importing the Lambda under test.
_boto3 = types.ModuleType("boto3")
_boto3.client = lambda service, **kwargs: MagicMock(name=service)
_botocore = types.ModuleType("botocore")
_config = types.ModuleType("botocore.config")
_config.Config = lambda **kwargs: None
_botocore.config = _config
with patch.dict(sys.modules, {"boto3": _boto3, "botocore": _botocore, "botocore.config": _config}), \
        patch.dict(os.environ, {"EXPECTED_VPC": VPC, "EXPECTED_SECURITY_GROUP": SG, "ALERT_TOPIC_ARN": TOPIC}):
    spec = importlib.util.spec_from_file_location("guard", os.path.join(os.path.dirname(__file__), "index.py"))
    mod = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = mod
    spec.loader.exec_module(mod)


def fmt(dt):
    return dt.strftime("%Y-%m-%dT%H:%M:%SZ")


class GuardTests(unittest.TestCase):
    def setUp(self):
        self.now = datetime.now(timezone.utc)
        self.ec2 = MagicMock()
        self.sns = MagicMock()
        patcher_ec2 = patch.object(mod, "ec2", self.ec2)
        patcher_sns = patch.object(mod, "sns", self.sns)
        patcher_ec2.start()
        patcher_sns.start()
        self.addCleanup(patcher_ec2.stop)
        self.addCleanup(patcher_sns.stop)
        self.ec2.terminate_instances.side_effect = lambda InstanceIds: {
            "TerminatingInstances": [{"InstanceId": InstanceIds[0], "CurrentState": {"Name": "shutting-down"}}]
        }

    def instance(self, iid="i-0123456789abcdef0", launched_hours_ago=1, expires_in_hours=None,
                 root_attached_hours_ago=None, **over):
        tags = {"Project": "Lodestar", "Environment": "Test", "ManagedBy": "Terraform"}
        if expires_in_hours is not None:
            tags["ExpiresAt"] = fmt(self.now + timedelta(hours=expires_in_hours))
        tags.update(over.pop("tags", {}))
        inst = {
            "InstanceId": iid,
            "VpcId": VPC,
            "InstanceType": "c6a.xlarge",
            "SecurityGroups": [{"GroupId": SG}],
            "LaunchTime": self.now - timedelta(hours=launched_hours_ago),
            "Tags": [{"Key": k, "Value": v} for k, v in tags.items()],
            "RootDeviceName": "/dev/sda1",
            "BlockDeviceMappings": [{
                "DeviceName": "/dev/sda1",
                "Ebs": {"AttachTime": self.now - timedelta(
                    hours=launched_hours_ago if root_attached_hours_ago is None else root_attached_hours_ago)},
            }],
        }
        inst.update(over)
        return inst

    def serve(self, *instances):
        paginator = MagicMock()
        paginator.paginate.return_value = [{"Reservations": [{"Instances": list(instances)}]}]
        self.ec2.get_paginator.return_value = paginator
        return paginator

    def terminated_ids(self):
        return [c.kwargs["InstanceIds"][0] for c in self.ec2.terminate_instances.call_args_list]

    def test_scans_every_lodestar_instance(self):
        # Only Project=Lodestar is filtered server side, so an instance outside
        # the guard's scope is seen and reported instead of never listed.
        paginator = self.serve()
        mod.handler({}, None)
        names = {f["Name"]: f["Values"] for f in paginator.paginate.call_args.kwargs["Filters"]}
        self.assertEqual(names["tag:Project"], ["Lodestar"])
        self.assertNotIn("vpc-id", names)
        self.assertNotIn("tag:Environment", names)

    def test_stop_start_does_not_reset_the_48h_clock(self):
        # Started 49 h ago (root volume attached then), stopped and started 1 h ago.
        self.serve(self.instance(launched_hours_ago=1, root_attached_hours_ago=49))
        result = mod.handler({}, None)
        self.assertEqual(result["requested"], 1)

    def test_recent_root_attach_does_not_extend_life(self):
        # The earliest of the two times wins.
        self.serve(self.instance(launched_hours_ago=49, root_attached_hours_ago=1))
        self.assertEqual(mod.handler({}, None)["requested"], 1)

    def test_attach_time_of_non_root_volume_is_ignored(self):
        inst = self.instance(launched_hours_ago=1)
        inst["BlockDeviceMappings"].append({"DeviceName": "/dev/xvdf", "Ebs": {"AttachTime": self.now - timedelta(hours=60)}})
        self.serve(inst)
        self.assertEqual(mod.handler({}, None)["expired"], 0)

    def test_falls_back_to_launch_time_without_root_attach_time(self):
        for mappings in ([], [{"DeviceName": "/dev/sda1", "Ebs": {}}]):
            self.ec2.terminate_instances.reset_mock()
            inst = self.instance(launched_hours_ago=49)
            inst["BlockDeviceMappings"] = mappings
            self.serve(inst)
            self.assertEqual(mod.handler({}, None)["requested"], 1, mappings)
        inst = self.instance(launched_hours_ago=49)
        del inst["BlockDeviceMappings"], inst["RootDeviceName"]
        self.serve(inst)
        self.assertEqual(mod.handler({}, None)["requested"], 1)

    def test_expires_at_still_caps_before_48h(self):
        self.serve(self.instance(launched_hours_ago=1, root_attached_hours_ago=2, expires_in_hours=-0.1))
        self.assertEqual(mod.handler({}, None)["requested"], 1)

    def test_terminates_past_expires_at(self):
        self.serve(self.instance(expires_in_hours=-1))
        result = mod.handler({}, None)
        self.assertEqual(result["requested"], 1)
        self.assertEqual(self.terminated_ids(), ["i-0123456789abcdef0"])
        self.sns.publish.assert_called_once()

    def test_keeps_unexpired(self):
        self.serve(self.instance(expires_in_hours=2))
        result = mod.handler({}, None)
        self.assertEqual(result["expired"], 0)
        self.ec2.terminate_instances.assert_not_called()
        self.sns.publish.assert_not_called()

    def test_48h_ceiling_overrides_later_tag(self):
        self.serve(self.instance(launched_hours_ago=49, expires_in_hours=10))
        self.assertEqual(mod.handler({}, None)["requested"], 1)

    def test_missing_or_bad_tag_falls_back_to_48h(self):
        self.serve(self.instance(launched_hours_ago=47, tags={"ExpiresAt": "tomorrow"}),
                   self.instance(iid="i-0000000000000000b", launched_hours_ago=48.5))
        result = mod.handler({}, None)
        self.assertEqual(self.terminated_ids(), ["i-0000000000000000b"])
        self.assertEqual(result["expired"], 1)

    def test_out_of_scope_never_terminated(self):
        expired = dict(expires_in_hours=-1)
        self.serve(
            self.instance(iid="i-00000000000000001", InstanceType="c6a.2xlarge", **expired),
            self.instance(iid="i-00000000000000002", VpcId="vpc-0other", **expired),
            self.instance(iid="i-00000000000000003", SecurityGroups=[{"GroupId": "sg-0other"}], **expired),
            self.instance(iid="i-00000000000000004", tags={"Environment": "Prod"}, **expired),
        )
        with self.assertLogs(mod.log, level="ERROR") as logs:
            result = mod.handler({}, None)
        self.assertEqual(result["expired"], 0)
        self.assertEqual(result["out_of_scope"], 4)
        self.assertEqual(sum('"out_of_scope_instance"' in line for line in logs.output), 4)
        self.ec2.terminate_instances.assert_not_called()

    def test_dry_run_terminates_nothing(self):
        self.serve(self.instance(expires_in_hours=-1))
        result = mod.handler({"dry_run": True}, None)
        self.assertEqual(result["expired"], 1)
        self.assertEqual(result["requested"], 0)
        self.ec2.terminate_instances.assert_not_called()
        self.sns.publish.assert_not_called()

    def test_at_most_15_per_run(self):
        self.serve(*[self.instance(iid="i-%017x" % n, expires_in_hours=-1) for n in range(20)])
        result = mod.handler({}, None)
        self.assertEqual(result["expired"], 20)
        self.assertEqual(self.ec2.terminate_instances.call_count, 15)

    def test_unacknowledged_termination_raises(self):
        self.serve(self.instance(expires_in_hours=-1))
        self.ec2.terminate_instances.side_effect = lambda InstanceIds: {"TerminatingInstances": []}
        with self.assertRaises(RuntimeError):
            mod.handler({}, None)
        self.sns.publish.assert_not_called()

    def test_notification_failure_raises(self):
        self.serve(self.instance(expires_in_hours=-1))
        self.sns.publish.side_effect = RuntimeError("sns down")
        with self.assertRaises(RuntimeError):
            mod.handler({}, None)


if __name__ == "__main__":
    unittest.main(verbosity=2)
