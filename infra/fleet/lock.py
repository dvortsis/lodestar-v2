"""The fleet lock (OP-1; plan §3 "Busy"), against a small store interface.

An S3 object written with If-None-Match holds the run ID and an expiry (the
90-minute chain). A taken lock gives "busy"; an expired one is taken over with
If-Match on its ETag. Release is a conditional delete that succeeds only while
the lock holds this run's ID. The reaper and the owner's dispatched `down`
pre-empt it with a "reaping" marker. MemoryStore is the dry-run and test
store; S3Store is the live one, refused unless live mode is on (OP-2).
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from datetime import datetime, timedelta
from typing import Callable, Protocol

from timefmt import fmt, parse_ts

LOCK_KEY = "locks/fleet.json"
ACQUIRED, BUSY, REAPING = "acquired", "busy", "reaping"


class Store(Protocol):
    def get(self, key: str) -> tuple[bytes, str] | None: ...
    def put_if_absent(self, key: str, body: bytes) -> bool: ...
    def put_if_match(self, key: str, body: bytes, etag: str) -> bool: ...
    def put(self, key: str, body: bytes) -> None: ...
    def delete_if_match(self, key: str, etag: str) -> bool: ...


class MemoryStore:
    def __init__(self):
        self.objects: dict[str, tuple[bytes, str]] = {}
        self.calls: list[str] = []
        self._n = 0

    def _etag(self) -> str:
        self._n += 1
        return f'"{self._n}"'

    def get(self, key):
        self.calls.append("get")
        return self.objects.get(key)

    def put_if_absent(self, key, body):
        self.calls.append("put_if_absent")
        if key in self.objects:
            return False
        self.objects[key] = (body, self._etag())
        return True

    def put_if_match(self, key, body, etag):
        self.calls.append("put_if_match")
        if key not in self.objects or self.objects[key][1] != etag:
            return False
        self.objects[key] = (body, self._etag())
        return True

    def put(self, key, body):
        self.calls.append("put")
        self.objects[key] = (body, self._etag())

    def delete_if_match(self, key, etag):
        self.calls.append("delete_if_match")
        if key not in self.objects or self.objects[key][1] != etag:
            return False
        del self.objects[key]
        return True


class S3Store:  # pragma: no cover - live only; nothing constructs it in CI
    """Live store. Errors carry no bucket name or key beyond the fixed prefix."""

    def __init__(self, bucket: str, live: bool):
        if not live:
            raise RuntimeError("S3Store is live-only (FLEET_LIVE=true, OP-2)")
        import boto3  # noqa: PLC0415 - only on the live path
        from botocore.exceptions import ClientError  # noqa: PLC0415

        self._s3, self._bucket, self._err = boto3.client("s3"), bucket, ClientError

    def _cond(self, fn, **kw) -> bool:
        try:
            fn(Bucket=self._bucket, **kw)
            return True
        except self._err as e:
            if e.response.get("Error", {}).get("Code") in ("PreconditionFailed", "ConditionalRequestConflict", "NoSuchKey"):
                return False
            raise RuntimeError("S3 lock request failed") from None

    def get(self, key):
        try:
            r = self._s3.get_object(Bucket=self._bucket, Key=key)
        except self._err as e:
            if e.response.get("Error", {}).get("Code") == "NoSuchKey":
                return None
            raise RuntimeError("S3 lock read failed") from None
        return r["Body"].read(), r["ETag"]

    def put_if_absent(self, key, body):
        return self._cond(self._s3.put_object, Key=key, Body=body, IfNoneMatch="*")

    def put_if_match(self, key, body, etag):
        return self._cond(self._s3.put_object, Key=key, Body=body, IfMatch=etag)

    def put(self, key, body):
        self._s3.put_object(Bucket=self._bucket, Key=key, Body=body)

    def delete_if_match(self, key, etag):
        return self._cond(self._s3.delete_object, Key=key, IfMatch=etag)


class LockLost(RuntimeError):
    """The lock no longer holds this run's ID: stop before any write."""


@dataclass(frozen=True)
class Holder:
    kind: str  # "run" or "reaping"
    run_id: str
    expires_at: datetime


def _decode(body: bytes) -> Holder | None:
    try:
        d = json.loads(body)
        if d.get("kind") not in ("run", "reaping") or not isinstance(d.get("run_id"), str):
            return None
        return Holder(d["kind"], d["run_id"], parse_ts(d["expires_at"]))
    except (ValueError, TypeError, KeyError, AttributeError):
        return None


class FleetLock:
    def __init__(self, store: Store, ttl_minutes: int, clock: Callable[[], datetime]):
        self.store, self.ttl, self.clock = store, timedelta(minutes=ttl_minutes), clock

    def _body(self, kind: str, run_id: str) -> bytes:
        return json.dumps({"kind": kind, "run_id": run_id, "expires_at": fmt(self.clock() + self.ttl)}).encode()

    def acquire(self, run_id: str) -> str:
        body = self._body("run", run_id)
        if self.store.put_if_absent(LOCK_KEY, body):
            return ACQUIRED
        cur = self.store.get(LOCK_KEY)
        if cur is None:  # released between the two calls; one more try
            return ACQUIRED if self.store.put_if_absent(LOCK_KEY, body) else BUSY
        holder = _decode(cur[0])
        if holder is None:  # unreadable: fail closed; the reaper can pre-empt it
            return BUSY
        if holder.expires_at > self.clock():
            return REAPING if holder.kind == "reaping" else BUSY
        return ACQUIRED if self.store.put_if_match(LOCK_KEY, body, cur[1]) else BUSY

    def holds(self, run_id: str) -> bool:
        cur = self.store.get(LOCK_KEY)
        holder = _decode(cur[0]) if cur else None
        return holder is not None and holder.run_id == run_id

    def fence(self, run_id: str) -> None:
        if not self.holds(run_id):
            raise LockLost("fleet lock lost")

    def fenced(self, run_id: str, write: Callable[[], object]):
        """Every rule-set write goes through here (OP-2 supplies `write`)."""
        self.fence(run_id)
        return write()

    def preempt(self, run_id: str) -> None:
        """The emergency path: overwrite whatever holds the lock."""
        self.store.put(LOCK_KEY, self._body("reaping", run_id))

    def release(self, run_id: str) -> bool:
        cur = self.store.get(LOCK_KEY)
        holder = _decode(cur[0]) if cur else None
        if holder is None or holder.run_id != run_id:
            return False
        return self.store.delete_if_match(LOCK_KEY, cur[1])
