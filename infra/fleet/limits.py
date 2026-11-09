"""Loads limits.yaml and applies the spend and one-fleet limits (plan §3)."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Mapping

LIMITS_PATH = Path(__file__).with_name("limits.yaml")


def load(path: Path = LIMITS_PATH) -> dict:
    # limits.yaml is JSON with full-line `#` comments (still valid YAML).
    lines = [ln for ln in path.read_text(encoding="utf-8").splitlines() if not ln.lstrip().startswith("#")]
    limits = json.loads("\n".join(lines))
    cap = limits.get("monthly_compute_cap_usd")
    if (limits.get("version") != 1 or not 0 < limits["max_lifetime_hours"] <= 44
            or not 0 < limits["lock_ttl_minutes"] <= 90 or not 0 < limits["reap_grace_minutes"] <= 15
            or isinstance(cap, bool) or not isinstance(cap, (int, float)) or not 0 < cap <= 40
            or "max_fleets" in limits):
        # The owner set the $40 cap; 44 h, 90 min and 15 min are the plan's (§3).
        raise ValueError("limits.yaml is outside the plan's bounds")
    return limits


def hourly_usd(limits: Mapping, preset: str) -> float:
    return sum(r["hourly_usd"] for r in limits["presets"][preset]["roles"].values())


def projected_usd(limits: Mapping, preset: str) -> float:
    return hourly_usd(limits, preset) * limits["max_lifetime_hours"]


def check_up(limits: Mapping, preset: str, backend, now) -> str | None:
    """None if `up <preset>` is within limits, else a fixed refusal reason."""
    if backend.lodestar_linodes():
        return "a Lodestar Linode exists"
    if backend.read_fleet() is not None:
        return "a fleet is recorded"
    if backend.month_spend_usd(now) + projected_usd(limits, preset) > limits["monthly_compute_cap_usd"]:
        return "the monthly fleet-compute cap would be exceeded"
    return None
