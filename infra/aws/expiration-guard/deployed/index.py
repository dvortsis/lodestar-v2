import json
import logging
import os
from datetime import datetime, timezone, timedelta
import boto3
from botocore.config import Config

log = logging.getLogger()
log.setLevel(logging.INFO)
retry_config = Config(retries={"mode": "standard", "max_attempts": 4})
ec2 = boto3.client("ec2", config=retry_config)
sns = boto3.client("sns", config=retry_config)

REQUIRED = {"Project": "Lodestar", "Environment": "Test", "ManagedBy": "Terraform"}
VPC = os.environ["EXPECTED_VPC"]
SG = os.environ["EXPECTED_SECURITY_GROUP"]
TOPIC = os.environ["ALERT_TOPIC_ARN"]

def expires_at(value):
    try:
        return datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc)
    except (ValueError, TypeError):
        return None

def started_at(instance):
    """When the instance first started: the earliest of LaunchTime and the root
    EBS volume's AttachTime. A stop/start resets LaunchTime, but the root volume
    stays attached, so its AttachTime keeps the original start and the 48-hour
    clock can't be reset by stopping and starting the instance."""
    times = [instance["LaunchTime"].astimezone(timezone.utc)]
    root = instance.get("RootDeviceName")
    for mapping in instance.get("BlockDeviceMappings", []):
        attached = mapping.get("Ebs", {}).get("AttachTime")
        if root and mapping.get("DeviceName") == root and attached is not None:
            times.append(attached.astimezone(timezone.utc))
    return min(times)


def handler(event, context):
    dry_run = isinstance(event, dict) and event.get("dry_run") is True
    now = datetime.now(timezone.utc)
    expired = []
    scanned = 0
    out_of_scope = 0
    for page in ec2.get_paginator("describe_instances").paginate(Filters=[
        {"Name": "tag:Project", "Values": ["Lodestar"]},
        {"Name": "instance-state-name", "Values": ["pending", "running", "stopping", "stopped"]},
    ]):
        for reservation in page.get("Reservations", []):
            for instance in reservation.get("Instances", []):
                scanned += 1
                tags = {x["Key"]: x["Value"] for x in instance.get("Tags", [])}
                if not (
                    all(tags.get(k) == v for k,v in REQUIRED.items()) and
                    instance.get("VpcId") == VPC and
                    instance.get("InstanceType") == "c6a.xlarge" and
                    any(s.get("GroupId") == SG for s in instance.get("SecurityGroups", []))
                ):
                    # Tagged Project=Lodestar but not something this guard may
                    # terminate: the OutOfScope alarm counts this log line.
                    log.error(json.dumps({"event": "out_of_scope_instance", "id": instance["InstanceId"]}))
                    out_of_scope += 1
                    continue
                latest = started_at(instance) + timedelta(hours=48)
                tagged = expires_at(tags.get("ExpiresAt"))
                if tagged is None:
                    log.warning(json.dumps({"event": "invalid_or_missing_expiration", "id": instance["InstanceId"]}))
                deadline = min(tagged, latest) if tagged is not None else latest
                if now >= deadline:
                    expired.append(instance["InstanceId"])
    terminated = []
    errors = []
    for instance_id in expired[:15]:
        if dry_run:
            log.info(json.dumps({"event": "would_terminate", "id": instance_id}))
            continue
        try:
            result = ec2.terminate_instances(InstanceIds=[instance_id])
            states = result.get("TerminatingInstances", [])
            if not any(x.get("InstanceId") == instance_id and x.get("CurrentState", {}).get("Name") in ("shutting-down", "terminated") for x in states):
                raise RuntimeError("Termination not acknowledged")
            terminated.append(instance_id)
            log.info(json.dumps({"event": "termination_accepted", "id": instance_id}))
        except Exception as exc:
            log.exception("termination_failed id=%s", instance_id)
            errors.append(instance_id + ":" + type(exc).__name__)
    if terminated:
        try:
            sns.publish(TopicArn=TOPIC, Subject="Lodestar EC2 expiration: termination started",
                        Message=json.dumps({"instance_ids": terminated, "utc": now.isoformat(), "state": "termination initiated, not yet confirmed final"}))
        except Exception:
            log.exception("notification_failed")
            errors.append("notification_failed")
    log.info(json.dumps({"event": "scan_summary", "scanned": scanned, "expired": len(expired),
                         "out_of_scope_count": out_of_scope, "requested": len(terminated), "dry_run": dry_run}))
    if errors:
        raise RuntimeError("Expiration processing failed: " + ",".join(errors))
    return {"scanned": scanned, "expired": len(expired), "out_of_scope": out_of_scope,
            "requested": len(terminated), "dry_run": dry_run}
