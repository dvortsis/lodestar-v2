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
CONFIRM_DEADLINE_S = 90  # wait for 404s on all deletions of a run together
CONFIRM_POLL_S = 5
REPORT_RESERVE_S = 20  # time kept back for the SNS message and summary log
DELETE_RESERVE_S = 60  # don't start another deletion with less time left
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


def in_window(resource, now):
    """Created on or after MIN_CREATED_UTC and not more than 10 minutes ahead."""
    created = iso(resource.get("created"))
    activation = iso(os.environ["MIN_CREATED_UTC"])
    return activation <= created <= now + timedelta(minutes=10)


def aged(resource, now):
    created = iso(resource.get("created"))
    activation = iso(os.environ["MIN_CREATED_UTC"])
    if created < activation or created > now + timedelta(minutes=10):
        return False
    return now >= created + timedelta(hours=48)


def remaining_s(context):
    """Seconds left before the Lambda times out (no limit outside Lambda)."""
    getter = getattr(context, "get_remaining_time_in_millis", None)
    return getter() / 1000.0 if callable(getter) else float("inf")


def confirm_deleted(ids, token, deadline_s=CONFIRM_DEADLINE_S):
    """Poll until each Linode returns 404 or the shared deadline passes.
    Returns the set of ids confirmed deleted. A failed status check keeps that
    id pending (and is logged) instead of discarding confirmations already made."""
    pending = set(ids)
    confirmed = set()
    deadline = time.monotonic() + max(0.0, deadline_s)
    while pending:
        for id_ in sorted(pending):
            if time.monotonic() >= deadline:
                break
            try:
                if api("GET", "/linode/instances/" + str(id_), token, allow_404=True) is None:
                    confirmed.add(id_)
            except Exception as exc:
                LOG.warning(json.dumps({"event": "confirm_check_failed", "id": id_, "error_type": type(exc).__name__}))
        pending -= confirmed
        if not pending or time.monotonic() >= deadline:
            break
        time.sleep(CONFIRM_POLL_S)
    return confirmed


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
    out_of_scope = 0
    for item in items:
        if not scoped(item):
            tags = item.get("tags") if isinstance(item, dict) else None
            if isinstance(tags, list) and "lodestar" in tags:
                # Tagged lodestar but outside what the watchdog may delete (region,
                # type, label or tags): it would never be reaped. The OutOfScope
                # alarm counts this log line.
                LOG.error(json.dumps({"event": "out_of_scope_linode", "id": item.get("id")}))
                out_of_scope += 1
            continue
        try:
            if not in_window(item, now):
                # In scope but created before the watchdog's activation time, or
                # dated in the future: it would never be reaped. Counted by the
                # same OutOfScope metric filter.
                LOG.error(json.dumps({"event": "out_of_scope_linode", "id": item.get("id"), "reason": "created_outside_window"}))
                out_of_scope += 1
                continue
            if aged(item, now):
                selected.append(item)
        except (ValueError, TypeError):
            LOG.error(json.dumps({"event": "invalid_created_timestamp", "id": item.get("id")}))
            raise RuntimeError("Invalid creation timestamp for in-scope Linode") from None
    if len(selected) > 10:
        raise RuntimeError("Safety limit exceeded: more than 10 eligible Linodes")

    requested = []
    failures = []
    for instance in selected[:MAX_DELETIONS]:
        instance_id = instance["id"]
        if dry_run:
            LOG.info(json.dumps({"event": "would_delete", "id": instance_id}))
            continue
        if remaining_s(context) < DELETE_RESERVE_S:
            # Leave time to confirm what was already deleted and to report.
            LOG.warning(json.dumps({"event": "deferred_low_time", "id": instance_id}))
            failures.append({"id": instance_id, "error_type": "DeferredLowTime"})
            continue
        try:
            current = api("GET", "/linode/instances/" + str(instance_id), token, allow_404=True)
            if current is None:
                continue
            if not scoped(current) or not aged(current, datetime.now(timezone.utc)):
                LOG.warning(json.dumps({"event": "scope_changed_skip", "id": instance_id}))
                continue
            api("DELETE", "/linode/instances/" + str(instance_id), token)
            requested.append(instance_id)
        except Exception as exc:
            failures.append({"id": instance_id, "error_type": type(exc).__name__})
            LOG.exception("Akamai deletion failed for id=%s", instance_id)

    deleted = []
    if requested:
        # The shared deadline never runs past the Lambda's own timeout.
        budget = min(CONFIRM_DEADLINE_S, remaining_s(context) - REPORT_RESERVE_S)
        try:
            confirmed = confirm_deleted(requested, token, deadline_s=budget)
        except Exception as exc:
            confirmed = set()
            LOG.exception("Akamai deletion confirmation failed")
            failures.append({"id": None, "error_type": type(exc).__name__})
        for instance_id in requested:
            if instance_id in confirmed:
                deleted.append(instance_id)
                LOG.info(json.dumps({"event": "deleted_verified", "id": instance_id}))
            else:
                failures.append({"id": instance_id, "error_type": "DeletionNotVerified"})

    if deleted:
        SNS.publish(
            TopicArn=os.environ["ALERT_TOPIC_ARN"],
            Subject="Lodestar Akamai expiry: instances deleted",
            Message=json.dumps({"verified_deleted": deleted, "utc": now.isoformat()}),
        )
    LOG.info(json.dumps({"event": "scan_complete", "scanned": len(items), "eligible": len(selected), "out_of_scope_count": out_of_scope, "verified_deleted": len(deleted), "dry_run": dry_run}))
    if failures:
        raise RuntimeError("Akamai deletions failed: " + json.dumps(failures))
    return {"status": "ok", "scanned": len(items), "eligible": len(selected), "out_of_scope": out_of_scope,
            "deleted": len(deleted), "dry_run": dry_run}
