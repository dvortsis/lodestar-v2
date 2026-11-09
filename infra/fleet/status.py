"""The status comment, edited in place (plan §2; #627 rule 6).

Role names, ages, expiry and compute cost so far. Never a Linode label,
fingerprint, address, firewall ID, run ID or any other ID.
"""

from __future__ import annotations

from datetime import datetime
from typing import Mapping

from limits import hourly_usd
from timefmt import parse_ts

MARKER = "<!-- fleet-status v1 -->"


def _span(seconds: float) -> str:
    m = max(0, int(seconds // 60))
    return f"{m // 60} h {m % 60:02d} min"


def _utc(t: datetime) -> str:
    return t.strftime("%Y-%m-%d %H:%MZ")


def render(fleet: Mapping | None, limits: Mapping, now: datetime, dry_run: bool, unknown: bool = False) -> str:
    head = [MARKER, f"**Fleet status** ({'dry-run, ' if dry_run else ''}updated {_utc(now)})", ""]
    if unknown:
        return "\n".join(head + ["Fleet state unreadable: a fleet may exist. Only `down` runs."]) + "\n"
    if fleet is None:
        return "\n".join(head + ["No fleet."]) + "\n"
    created, expires = parse_ts(fleet["created_at"]), parse_ts(fleet["expires_at"])
    age = _span((now - created).total_seconds())
    billed_h = max(0.0, (min(now, expires) - created).total_seconds() / 3600)
    cost = hourly_usd(limits, fleet["preset"]) * billed_h
    rows = ["| Role | Age |", "|---|---|"]
    rows += [f"| {role} | {age} |" for role in limits["presets"][fleet["preset"]]["roles"] if role in fleet["roles"]]
    left = (expires - now).total_seconds()
    when = f"in {_span(left)}" if left > 0 else f"{_span(-left)} ago"
    tail = ["", f"Preset: {fleet['preset']}", f"Expires: {_utc(expires)} ({when})", f"Compute cost so far: ${cost:.2f}"]
    return "\n".join(head + rows + tail) + "\n"
