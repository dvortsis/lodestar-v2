"""One operator session at a time (plan §2): an S3 conditional-write lock.

The bridge's exact key `locks/operator.json` in the control bucket, on the
fleet lock's store interface (infra/fleet/lock.py: put_if_absent is
If-None-Match, put_if_match and delete_if_match are If-Match). GitHub's
`concurrency` isn't used: a newer pending run would cancel an older one and its
instruction would vanish. A taken lock means "busy, retry"; an expired one is
taken over with If-Match on its ETag; release deletes only this run's lock.
Apply takes the same lock, so the agent never changes under a live session.
"""

from __future__ import annotations

import json
from datetime import datetime, timedelta
from typing import Callable

from timefmt import fmt, parse_ts

LOCK_KEY = "locks/operator.json"
CLAUDE_LOCK_KEY = "operator/claude-lock.json"
TTL_MINUTES = 60  # above the wake job's 45-minute timeout


def _holder(cur) -> tuple[str, datetime] | None:
    try:
        d = json.loads(cur[0])
        return str(d["run_id"]), parse_ts(d["expires_at"])
    except (ValueError, TypeError, KeyError, IndexError):
        return None


class OperatorLock:
    def __init__(self, store, clock: Callable[[], datetime], ttl_minutes: int = TTL_MINUTES):
        self.store, self.clock, self.ttl = store, clock, timedelta(minutes=ttl_minutes)

    def _body(self, run_id: str) -> bytes:
        return json.dumps({"run_id": run_id, "expires_at": fmt(self.clock() + self.ttl)}).encode()

    def acquire(self, run_id: str) -> bool:
        if self.store.put_if_absent(LOCK_KEY, self._body(run_id)):
            return True
        cur = self.store.get(LOCK_KEY)
        if cur is None:
            return self.store.put_if_absent(LOCK_KEY, self._body(run_id))
        h = _holder(cur)
        if h is None:  # unreadable: fail closed until it's cleared by hand
            return False
        if h[1] > self.clock():
            return False  # held, ours included: a run never takes the lock twice
        return self.store.put_if_match(LOCK_KEY, self._body(run_id), cur[1])

    def release(self, run_id: str) -> bool:
        cur = self.store.get(LOCK_KEY)
        h = _holder(cur) if cur else None
        if h is None or h[0] != run_id:
            return False
        return self.store.delete_if_match(LOCK_KEY, cur[1])
