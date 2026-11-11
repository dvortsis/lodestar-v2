"""Independent cleanup broker. No provisioning, Terraform, or caller-selected targets.

The owner installs two functions with fixed modes: expiration and emergency.
Only the owner-dispatch OIDC role may invoke emergency. EventBridge invokes
expiration without GitHub, environment approvals, or provisioning credentials.
"""
from __future__ import annotations

import json
import logging
import os
import re
import time
import urllib.error
import urllib.request
import ipaddress
from datetime import datetime, timedelta, timezone
from audit import actions_ranges, audit_firewall, run_is_live, JOB_LABEL

AUTH_KEY = "fleet/cleanup-authorization.json"
FLAG_KEY = "locks/fleet-mismatch.json"
TAGS = {"lodestar", "test", "managed-terraform"}
TYPES = {"main": "g6-standard-4", "worker": "g6-nanode-1"}
MAX_NODES = 2
LOG = logging.getLogger(__name__)
LOG.setLevel(logging.INFO)


class Refused(RuntimeError):
    pass


def timestamp(value, api=False):
    if not isinstance(value, str):
        raise Refused("timestamp missing")
    try:
        stamp = datetime.fromisoformat(value.replace("Z", "+00:00"))
        if stamp.tzinfo is None:
            if not api:
                raise ValueError
            stamp = stamp.replace(tzinfo=timezone.utc)  # Linode API timestamps are UTC.
        return stamp.astimezone(timezone.utc)
    except ValueError:
        raise Refused("timestamp invalid") from None


def positive_id(value):
    if type(value) is not int or value <= 0:
        raise Refused("resource ID invalid")
    return value


def validate_authorization(doc, account, activation, now):
    if (not isinstance(doc, dict) or doc.get("version") != 1 or doc.get("account_euuid") != account
            or doc.get("region") != "us-east" or doc.get("deployment_id") != "fleet"):
        raise Refused("authorization account, region or ownership invalid")
    start, end = timestamp(doc.get("created_at")), timestamp(doc.get("expires_at"))
    if not activation <= start <= now or not start < end <= start + timedelta(hours=44):
        raise Refused("authorization lifetime outside approved bounds")
    nodes, walls = doc.get("linodes"), doc.get("firewalls")
    if not isinstance(nodes, list) or not 1 <= len(nodes) <= MAX_NODES or not isinstance(walls, list) or not 1 <= len(walls) <= MAX_NODES:
        raise Refused("authorization resource count invalid")
    ids, fids, roles = set(), set(), set()
    for node in nodes:
        if not isinstance(node, dict):
            raise Refused("authorization resource invalid")
        ident = positive_id(node.get("id"))
        if ident in ids or node.get("role") not in TYPES or node["role"] in roles:
            raise Refused("duplicate resource or unsupported role")
        ids.add(ident)
        roles.add(node["role"])
        role = node["role"]
        label = "lodestar-main-fleet" if role == "main" else "lodestar-worker-01-fleet"
        made = timestamp(node.get("created"), api=True)
        if node.get("label") != label or node.get("type") != TYPES[role] or not start <= made <= min(now, start + timedelta(hours=1)) or made >= end:
            raise Refused("resource ownership or creation boundary invalid")
    for wall in walls:
        if not isinstance(wall, dict):
            raise Refused("firewall authorization invalid")
        ident = positive_id(wall.get("id"))
        members = wall.get("linode_ids")
        if ident in fids or not isinstance(members, list) or not members or any(type(n) is not int for n in members) or not set(members) <= ids:
            raise Refused("firewall ownership invalid")
        fids.add(ident)
        if wall.get("label") not in ("lodestar-main-fw-fleet", "lodestar-worker-fw-fleet"):
            raise Refused("firewall label invalid")
        made = timestamp(wall.get("created"), api=True)
        if not start <= made <= min(now, start + timedelta(hours=1)):
            raise Refused("firewall creation boundary invalid")
        admin = wall.get("admin_ipv4")
        if not isinstance(admin, list) or not admin:
            raise Refused("firewall admin authorization missing")
        try:
            if any(ipaddress.IPv4Network(cidr).prefixlen != 32 for cidr in admin):
                raise ValueError
        except (ValueError, TypeError):
            raise Refused("firewall admin authorization invalid") from None
    # Every node must have exactly one authorized firewall; no unbound orphan deletion.
    members = [n for wall in walls for n in wall["linode_ids"]]
    if len(members) != len(ids) or set(members) != ids:
        raise Refused("firewall membership incomplete")
    return end


def check_node(actual, expected, activation):
    if actual is None:
        return
    if (actual.get("id") != expected["id"] or actual.get("region") != "us-east"
            or actual.get("type") != expected["type"] or actual.get("label") != expected["label"]
            or not TAGS <= set(actual.get("tags") or [])
            or timestamp(actual.get("created"), api=True) != timestamp(expected["created"], api=True)
            or timestamp(actual.get("created"), api=True) < activation):
        raise Refused("Linode identity, region, tags or creation changed")


def devices(api, fid):
    result = api("GET", f"/networking/firewalls/{fid}/devices?page=1&page_size=100")
    # At most two authorized nodes can fit behind a firewall. A second page
    # necessarily exceeds that bound or represents inconsistent inventory.
    if (not isinstance(result, dict) or type(result.get("pages")) is not int or result["pages"] != 1
            or result.get("page") != 1 or not isinstance(result.get("data"), list) or len(result["data"]) > MAX_NODES):
        raise Refused("firewall device inventory incomplete or outside bounds")
    return result["data"]


def check_wall(api, expected, activation):
    actual = api("GET", f"/networking/firewalls/{expected['id']}")
    if actual is None:
        return None
    if (actual.get("id") != expected["id"] or actual.get("label") != expected["label"]
            or not TAGS <= set(actual.get("tags") or [])
            or timestamp(actual.get("created"), api=True) != timestamp(expected["created"], api=True)
            or timestamp(actual.get("created"), api=True) < activation):
        raise Refused("firewall ownership or creation changed")
    for device in devices(api, expected["id"]):
        entity = device.get("entity", {})
        if entity.get("type") != "linode" or type(entity.get("id")) is not int or entity["id"] not in expected["linode_ids"]:
            raise Refused("firewall has an unapproved attached resource")
    return actual


def firewall_drift(api, wall, now, github):
    actual = api("GET", f"/networking/firewalls/{wall['id']}")
    if actual is None:
        return True
    rules = api("GET", f"/networking/firewalls/{wall['id']}/rules")
    if not isinstance(rules, dict) or not isinstance(rules.get("inbound"), list):
        raise Refused("firewall rules unreadable")
    jobs = [r for r in rules["inbound"] if JOB_LABEL.fullmatch(str(r.get("label", "")))]
    ranges = actions_ranges(github("/meta")) if jobs else []
    problems = audit_firewall(actual, rules, devices(api, wall["id"]), wall["admin_ipv4"], ranges, set(map(str, wall["linode_ids"])))
    for rule in jobs:
        run_id, attempt = JOB_LABEL.fullmatch(rule["label"]).groups()
        if not run_is_live(github(f"/repos/dvortsis/lodestar-v2/actions/runs/{run_id}"), attempt, now):
            problems.append("stale job ingress")
    return bool(problems)


def cleanup(doc, account, activation, mode, now, api, unchanged, mark, pause=time.sleep, dry_run=False, remaining=lambda: float("inf"), github=None):
    """All reads/authorization precede writes. Recheck every target before DELETE."""
    if mode not in ("expiration", "emergency") or not account:
        raise Refused("cleanup mode or account missing")
    if api("GET", "/account").get("euuid") != account:
        raise Refused("Linode account mismatch")
    end = validate_authorization(doc, account, activation, now)
    # Validate the entire deletion set, including shared-firewall attachments, first.
    present = False
    for node in doc["linodes"]:
        current = api("GET", f"/linode/instances/{node['id']}")
        check_node(current, node, activation)
        present = present or current is not None
    for wall in doc["firewalls"]:
        current = check_wall(api, wall, activation)
        present = present or current is not None
    unchanged()
    if mode == "expiration" and now < end:
        try:
            cache = {}
            def lookup(path):
                if path not in cache:
                    cache[path] = github(path)
                return cache[path]
            drift = any(firewall_drift(api, wall, now, lookup) for wall in doc["firewalls"])
        except Exception:
            if not dry_run:
                mark()  # Unknown audit blocks work, but never authorizes early deletion.
            raise Refused("firewall audit unavailable; owner escalation required") from None
        if not drift:
            return {"status": "not_expired", "linodes": 0, "firewalls": 0}
        LOG.warning('{"event":"cleanup_security_tripwire"}')
    if dry_run:
        return {"status": "dry_run", "linodes": 0, "firewalls": 0}
    if not present:
        return {"status": "complete", "linodes": 0, "firewalls": 0}
    if remaining() < 45:
        raise Refused("cleanup deadline too near")
    mark()  # Blocks provisioning before any deletion; never clear it or touch tfstate.
    count = {"status": "complete", "linodes": 0, "firewalls": 0}
    for node in doc["linodes"]:
        unchanged()
        if api("GET", "/account").get("euuid") != account:
            raise Refused("Linode account changed")
        current = api("GET", f"/linode/instances/{node['id']}")
        check_node(current, node, activation)
        # A foreign device added during this invocation also stops VM deletion.
        for wall in doc["firewalls"]:
            check_wall(api, wall, activation)
        if current is not None:
            if remaining() < 40:
                raise Refused("cleanup deadline too near for confirmed deletion")
            api("DELETE", f"/linode/instances/{node['id']}")
            for attempt in range(4):
                if api("GET", f"/linode/instances/{node['id']}") is None:
                    break
                if attempt == 3:
                    raise Refused("Linode deletion unconfirmed")
                pause(1)
            count["linodes"] += 1
            LOG.info(json.dumps({"event": "cleanup_delete_confirmed", "kind": "linode", "id": node["id"]}))
    # Linode firewalls are not region-scoped and attachment checks are not an
    # atomic DELETE precondition. Retain them for separately approved cleanup;
    # the broker's credential needs firewall:read_only, never firewall writes.
    count["retained_firewalls"] = len(doc["firewalls"])
    return count


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


def transport(token):
    opener = urllib.request.build_opener(NoRedirect())
    allowed = re.compile(r"^/(account|linode/instances/[1-9][0-9]*|networking/firewalls/[1-9][0-9]*(/devices\?page=[1-5]&page_size=100|/rules)?)$")
    def call(method, path):
        if (method not in ("GET", "DELETE") or not allowed.fullmatch(path)
                or method == "DELETE" and not re.fullmatch(r"/linode/instances/[1-9][0-9]*", path)):
            raise Refused("unsupported cleanup API operation")
        request = urllib.request.Request("https://api.linode.com/v4" + path, method=method,
                                         headers={"Authorization": "Bearer " + token, "Accept": "application/json"})
        try:
            with opener.open(request, timeout=5) as response:
                raw = response.read(1000001)
                if len(raw) > 1000000:
                    raise Refused("Linode response too large")
                return json.loads(raw) if raw else {}
        except urllib.error.HTTPError as exc:
            if exc.code == 404:
                return None
            raise Refused("Linode request failed") from None
        except Exception:
            raise Refused("Linode request failed") from None
    return call


def github_get(path):
    if not re.fullmatch(r"/meta|/repos/dvortsis/lodestar-v2/actions/runs/[1-9][0-9]*", path):
        raise Refused("unsupported GitHub audit path")
    request = urllib.request.Request("https://api.github.com" + path, headers={"User-Agent": "lodestar-cleanup", "Accept": "application/vnd.github+json"})
    try:
        with urllib.request.build_opener(NoRedirect()).open(request, timeout=5) as response:
            raw = response.read(1000001)
            if len(raw) > 1000000:
                raise Refused("GitHub audit response too large")
            return json.loads(raw)
    except Exception:
        raise Refused("GitHub audit unavailable") from None


def handler(event, context):
    # Fixed deployment mode; caller payload cannot select targets, credentials or mode.
    import boto3
    from botocore.config import Config
    report = {"status": "failed", "event": "cleanup_failed"}
    stage = "execution_identity"
    try:
        account, region = os.environ["AWS_ACCOUNT_ID"], os.environ["AWS_REGION"]
        if region != "us-east-1" or not re.fullmatch(r"[0-9]{12}", account):
            raise Refused("AWS account/region configuration invalid")
        cfg = Config(connect_timeout=5, read_timeout=10, retries={"max_attempts": 1})
        session = boto3.Session(region_name=region)
        if session.client("sts", config=cfg).get_caller_identity()["Account"] != account:
            raise Refused("AWS execution account mismatch")
        s3 = session.client("s3", config=cfg)
        stage = "authorization"
        bucket = os.environ["STATE_BUCKET"]
        result = s3.get_object(Bucket=bucket, Key=AUTH_KEY, ExpectedBucketOwner=account)
        etag = result["ETag"]
        raw = result["Body"].read(65537)
        if len(raw) > 65536:
            raise Refused("authorization too large")
        doc = json.loads(raw)
        read_only = isinstance(event, dict) and event.get("dry_run") is True
        if os.environ["CLEANUP_MODE"] == "emergency" and not read_only:
            request = event if isinstance(event, dict) else {}
            requested = timestamp(request.get("requested_at"))
            now = datetime.now(timezone.utc)
            validate_authorization(doc, os.environ["LINODE_ACCOUNT_EUUID"], timestamp(os.environ["MIN_CREATED_UTC"]), now)
            if (not now - timedelta(minutes=10) <= requested <= now or timestamp(doc["created_at"]) > requested
                    or any(timestamp(n["created"], api=True) > requested for n in doc["linodes"])):
                raise Refused("emergency request is stale or predates this fleet")
            if request.get("operation") == "prepare":
                return {"status": "prepared", "authorization_version": etag}
            if request.get("authorization_version") != etag:
                raise Refused("emergency authorization version changed")
        def unchanged():
            # IfMatch pins the current authorization; replacement/revocation stops work.
            s3.get_object(Bucket=bucket, Key=AUTH_KEY, ExpectedBucketOwner=account, IfMatch=etag)["Body"].close()
        def mark():
            s3.put_object(Bucket=os.environ["CONTROL_BUCKET"], Key=FLAG_KEY, ExpectedBucketOwner=account,
                          Body=json.dumps({"mismatch": True, "reasons": ["independent cleanup; owner reconciliation required"]}).encode())
        stage = "cleanup_credential"
        secret = session.client("secretsmanager", config=cfg).get_secret_value(SecretId=os.environ["CLEANUP_SECRET_ARN"])["SecretString"]
        token = json.loads(secret)["token"] if secret.startswith("{") else secret
        if not isinstance(token, str) or not 20 <= len(token) <= 512 or any(c.isspace() for c in token):
            raise Refused("cleanup credential invalid")
        stage = "scoped_cleanup"
        report = cleanup(doc, os.environ["LINODE_ACCOUNT_EUUID"], timestamp(os.environ["MIN_CREATED_UTC"]),
                         os.environ["CLEANUP_MODE"], datetime.now(timezone.utc), transport(token), unchanged, mark,
                         dry_run=read_only, remaining=lambda: context.get_remaining_time_in_millis() / 1000, github=github_get)
        report["event"] = "cleanup_read_only" if read_only else "cleanup_complete"
        LOG.info(json.dumps(report))
        return report
    except Exception as exc:
        report.update(status="failed", event="cleanup_failed", stage=stage, reason=str(exc) if isinstance(exc, Refused) else "dependency unavailable or malformed data")
        LOG.error(json.dumps(report))  # No exception/body/credential text; Errors alarm notifies.
        raise RuntimeError("cleanup failed; inspect scoped inventory and authorization; no empty-fleet assumption") from None
