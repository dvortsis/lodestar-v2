import json
import logging
import os
import time
import urllib.error
import urllib.request
from datetime import datetime, timedelta, timezone

import boto3
from botocore.exceptions import ClientError

LOG = logging.getLogger()
LOG.setLevel(logging.INFO)
SNS = boto3.client("sns")
SECRETS = boto3.client("secretsmanager")
BASE = "https://api.linode.com/v4"
REQUIRED_TAGS = {"lodestar", "test"}
APPROVED_TYPES = {"g6-standard-4", "g6-nanode-1"}
EXPECTED_REGION = "us-east"
MAX_DELETIONS = 4
MAX_PAGES = 20
MAX_ITEMS = 500


def iso(value):
    if not isinstance(value, str) or not value:
        raise ValueError("Missing timestamp")
    parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    return parsed.replace(tzinfo=timezone.utc) if parsed.tzinfo is None else parsed.astimezone(timezone.utc)


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


OPENER = urllib.request.build_opener(NoRedirect())


def api(method, path, token, allow_404=False):
    if not path.startswith("/") or "//" in path or ".." in path or "?" in path and not path.startswith("/linode/instances?page="):
        raise ValueError("Unexpected API path")
    request = urllib.request.Request(BASE + path, method=method, headers={
        "Authorization": "Bearer " + token,
        "Accept": "application/json",
        "User-Agent": "Lodestar-Akamai-Expiration-Guard/1.0",
    })
    for attempt in range(4):
        try:
            with OPENER.open(request, timeout=12) as response:
                raw = response.read(2_000_001)
                if len(raw) > 2_000_000:
                    raise RuntimeError("Akamai API response too large")
                if not raw:
                    return {}
                return json.loads(raw.decode("utf-8"))
        except urllib.error.HTTPError as exc:
            if exc.code == 404 and allow_404:
                return None
            if exc.code == 429 or exc.code in (500, 502, 503, 504):
                if attempt < 3:
                    time.sleep(min(2 ** attempt, 6))
                    continue
            raise RuntimeError("Akamai API HTTP status " + str(exc.code) + " on " + method + " " + path.split("?")[0]) from None
        except (urllib.error.URLError, TimeoutError):
            if attempt < 3:
                time.sleep(min(2 ** attempt, 6))
                continue
            raise RuntimeError("Akamai API network error on " + method + " " + path.split("?")[0]) from None
    raise RuntimeError("Akamai API retry budget exhausted")


def list_paginated(collection, token):
    all_items = []
    for page in range(1, MAX_PAGES + 1):
        data = api("GET", collection + "?page=" + str(page) + "&page_size=100", token)
        if not isinstance(data, dict) or not isinstance(data.get("data"), list):
            raise RuntimeError("Akamai API paginated data invalid")
        pages = data.get("pages")
        if not isinstance(pages, int) or pages < 1 or pages > MAX_PAGES:
            raise RuntimeError("Unexpected Akamai pagination size")
        if data.get("page") != page:
            raise RuntimeError("Unexpected Akamai page number")
        all_items.extend(data["data"])
        if len(all_items) > MAX_ITEMS:
            raise RuntimeError("Too many Akamai resources to scan safely")
        if page >= pages:
            return all_items
    raise RuntimeError("Akamai pagination incomplete")


def scoped(resource):
    if not isinstance(resource, dict):
        return False
    tags = resource.get("tags")
    return (
        isinstance(tags, list)
        and REQUIRED_TAGS.issubset(set(tags))
        and isinstance(resource.get("label"), str)
        and resource["label"].lower().startswith("lodestar-")
        and resource.get("region") == EXPECTED_REGION
        and resource.get("type") in APPROVED_TYPES
        and isinstance(resource.get("id"), int)
        and resource["id"] > 0
    )


def aged(resource, now):
    created = iso(resource.get("created"))
    activation = iso(os.environ["MIN_CREATED_UTC"])
    if created < activation or created > now + timedelta(minutes=10):
        return False
    return now >= created + timedelta(hours=48)


def confirm_deleted(id_, token):
    for attempt in range(6):
        data = api("GET", "/linode/instances/" + str(id_), token, allow_404=True)
        if data is None:
            return True
        if attempt < 5:
            time.sleep(3)
    return False


def token_value():
    try:
        result = SECRETS.get_secret_value(SecretId=os.environ["SECRET_ARN"])
    except SECRETS.exceptions.ResourceNotFoundException:
        return None
    secret = result.get("SecretString")
    if not secret:
        return None
    if secret.startswith("{"):
        secret = json.loads(secret).get("token")
    if not isinstance(secret, str) or not (20 <= len(secret) <= 512) or any(c.isspace() for c in secret):
        raise RuntimeError("Akamai secret value invalid: check Secrets Manager")
    return secret


def handler(event, context):
    dry_run = isinstance(event, dict) and event.get("dry_run") is True
    now = datetime.now(timezone.utc)
    token = token_value()
    if token is None:
        if os.environ.get("TOKEN_REQUIRED") == "true":
            # Once the token is installed, a missing or empty secret means the 48-hour
            # cleanup has silently stopped: fail so the Errors alarm fires.
            LOG.error(json.dumps({"event": "token_missing", "dry_run": dry_run}))
            raise RuntimeError("Akamai API token missing from Secrets Manager; cleanup is not running")
        LOG.warning(json.dumps({"event": "unconfigured", "message": "Akamai API token not installed", "dry_run": dry_run}))
        return {"status": "unconfigured", "scanned": 0, "eligible": 0, "deleted": 0, "dry_run": dry_run}

    items = list_paginated("/linode/instances", token)
    selected = []
    for item in items:
        if not scoped(item):
            continue
        try:
            if aged(item, now):
                selected.append(item)
        except (ValueError, TypeError):
            LOG.error(json.dumps({"event": "invalid_created_timestamp", "id": item.get("id")}))
            raise RuntimeError("Invalid creation timestamp for in-scope Linode") from None
    if len(selected) > 10:
        raise RuntimeError("Safety limit exceeded: more than 10 eligible Linodes")

    deleted = []
    failures = []
    for instance in selected[:MAX_DELETIONS]:
        instance_id = instance["id"]
        if dry_run:
            LOG.info(json.dumps({"event": "would_delete", "id": instance_id}))
            continue
        try:
            current = api("GET", "/linode/instances/" + str(instance_id), token, allow_404=True)
            if current is None:
                continue
            if not scoped(current) or not aged(current, datetime.now(timezone.utc)):
                LOG.warning(json.dumps({"event": "scope_changed_skip", "id": instance_id}))
                continue
            api("DELETE", "/linode/instances/" + str(instance_id), token)
            if not confirm_deleted(instance_id, token):
                raise RuntimeError("Deletion was not verified")
            deleted.append(instance_id)
            LOG.info(json.dumps({"event": "deleted_verified", "id": instance_id}))
        except Exception as exc:
            failures.append({"id": instance_id, "error_type": type(exc).__name__})
            LOG.exception("Akamai deletion failed for id=%s", instance_id)

    if deleted:
        SNS.publish(
            TopicArn=os.environ["ALERT_TOPIC_ARN"],
            Subject="Lodestar Akamai expiry: instances deleted",
            Message=json.dumps({"verified_deleted": deleted, "utc": now.isoformat()}),
        )
    LOG.info(json.dumps({"event": "scan_complete", "scanned": len(items), "eligible": len(selected), "verified_deleted": len(deleted), "dry_run": dry_run}))
    if failures:
        raise RuntimeError("Akamai deletions failed: " + json.dumps(failures))
    return {"status": "ok", "scanned": len(items), "eligible": len(selected), "deleted": len(deleted), "dry_run": dry_run}
