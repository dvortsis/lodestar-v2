"""fleet.json (plan §3 "One commit per fleet") and the dry-run backend.

fleet.json lives in the state bucket beside the Terraform state. It is never
written to or read back from a comment. Addresses, fingerprints and firewall
IDs are slots that live mode (OP-2) fills; nothing here prints them.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime, timedelta
from typing import Mapping

from command import HEX40
from timefmt import fmt, parse_ts

FLEET_KEY = "fleet/fleet.json"
SLOT_KEYS = ("address", "fingerprint", "firewall_id")


def new_fleet(limits: Mapping, preset: str, commit: str, now: datetime) -> dict:
    roles = limits["presets"][preset]["roles"]
    fleet = {
        "version": 1,
        "preset": preset,
        "commit": commit,
        "created_at": fmt(now),
        "expires_at": fmt(now + timedelta(hours=limits["max_lifetime_hours"])),
        "roles": {r: dict.fromkeys(SLOT_KEYS) for r in roles},
    }
    validate(fleet, limits)
    return fleet


def validate(fleet: Mapping, limits: Mapping) -> None:
    """Raises ValueError (with no field values in it) unless fleet.json is sound."""
    if fleet.get("version") != 1 or fleet.get("preset") not in limits["presets"]:
        raise ValueError("fleet.json: bad version or preset")
    if not isinstance(fleet.get("commit"), str) or not HEX40.match(fleet["commit"]):
        raise ValueError("fleet.json: bad commit")
    created, expires = parse_ts(fleet.get("created_at")), parse_ts(fleet.get("expires_at"))
    if not created < expires <= created + timedelta(hours=min(44, limits["max_lifetime_hours"])):
        raise ValueError("fleet.json: expiry outside 44 h")
    roles = fleet.get("roles")
    if not isinstance(roles, dict) or set(roles) != set(limits["presets"][fleet["preset"]]["roles"]):
        raise ValueError("fleet.json: roles don't match the preset")
    for slot in roles.values():
        if not isinstance(slot, dict) or set(slot) != set(SLOT_KEYS):
            raise ValueError("fleet.json: bad role slot")
        if any(v is not None and not isinstance(v, str) for v in slot.values()):
            raise ValueError("fleet.json: bad role slot")


@dataclass
class DryRunBackend:
    """Inventory and state with no cloud behind them. Live mode is OP-2's."""

    fleet: dict | None = None
    linodes: list[dict] = field(default_factory=list)  # {"id": str, "created": iso Z}
    spend_usd: float = 0.0
    mismatch_flag: bool = False
    log: list[str] = field(default_factory=list)

    def read_fleet(self):
        return self.fleet

    def write_fleet(self, fleet):
        self.fleet = fleet
        self.log.append("write_fleet")

    def destroy(self):
        self.log.append("destroy")
        self.fleet = None

    def delete_linodes(self, ids):
        self.log.append(f"delete_linodes:{len(ids)}")
        self.linodes = [n for n in self.linodes if n["id"] not in ids]

    def lodestar_linodes(self):
        return list(self.linodes)

    def month_spend_usd(self, now):
        return self.spend_usd

    def mismatch(self):
        return self.mismatch_flag
