"""The hourly reaper's decisions (OP-1; plan §3 "Lifetime, enforced twice").

- `down` once the fleet passes the expiry in fleet.json;
- the emergency path (a "reaping" marker over the lock, no waiting) on an
  audit mismatch or more than 15 minutes past expiry;
- Lodestar Linodes older than 44 h by creation time, recorded or not, are
  deleted through the Linode API (live mode, OP-2).
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime, timedelta
from typing import Callable, Mapping

from timefmt import parse_ts

NONE, DOWN, EMERGENCY = "none", "down", "emergency"


def audit_not_built(fleet: Mapping) -> bool:
    """Hook for the hourly firewall audit (OP-2): True means a mismatch.

    OP-2 replaces this with the sweep-then-check of plan §3 (attached, enabled,
    inbound_policy DROP, admin rules plus well-formed job rules only).
    """
    return False


@dataclass(frozen=True)
class Decision:
    action: str
    reason: str
    age_reap: tuple[str, ...]


def only_down(fleet: Mapping | None, mismatch: bool, now: datetime) -> bool:
    """Past expiry, or a recorded mismatch: every command but `down` is refused."""
    return mismatch or (fleet is not None and now >= parse_ts(fleet["expires_at"]))


def decide(fleet: Mapping | None, linodes: list, mismatch: bool, now: datetime, limits: Mapping,
           audit: Callable[[Mapping], bool] = audit_not_built) -> Decision:
    cutoff = now - timedelta(hours=limits["max_lifetime_hours"])
    old = tuple(n["id"] for n in linodes if parse_ts(n["created"]) <= cutoff)
    if fleet is not None and (mismatch or audit(fleet)):
        return Decision(EMERGENCY, "firewall audit mismatch", old)
    if fleet is not None:
        expires = parse_ts(fleet["expires_at"])
        if now > expires + timedelta(minutes=limits["reap_grace_minutes"]):
            return Decision(EMERGENCY, "more than 15 minutes past expiry", old)
        if now >= expires:
            return Decision(DOWN, "past expiry", old)
    return Decision(NONE, "within lifetime" if fleet else "no fleet", old)
