"""fleet.json (plan §3 "One commit per fleet") and the dry-run backend.

fleet.json lives in the state bucket beside the Terraform state. It is never
written to or read back from a comment. Addresses, host keys, fingerprints,
firewall IDs and admin /32s are slots that live mode (OP-2) fills; nothing here
prints them.

fleet.json is never deleted (the runner, 2026-11-08): once `down` has emptied
the inventory it becomes a tombstone, `{"version": 1, "fleet": null, ...}`.
"No fleet" is read only from a readable tombstone; any read error raises
FleetUnknown, which means a fleet may exist.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime, timedelta
from typing import Mapping

from command import HEX40
from timefmt import fmt, parse_ts

FLEET_KEY = "fleet/fleet.json"
LEDGER_KEY = "fleet/spend-ledger.json"      # state bucket: {"YYYY-MM": usd}
MISMATCH_KEY = "locks/fleet-mismatch.json"  # control bucket: a recorded audit mismatch
SLOT_KEYS = ("address", "host_key", "fingerprint", "firewall_id", "admin_ipv4")


class FleetUnknown(RuntimeError):
    """fleet.json couldn't be read as a fleet or a tombstone: a fleet may exist."""


def tombstone(now: datetime) -> dict:
    return {"version": 1, "fleet": None, "updated_at": fmt(now)}


def is_tombstone(doc) -> bool:
    return isinstance(doc, dict) and doc.get("version") == 1 and "fleet" in doc and doc["fleet"] is None


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
        if any(slot[k] is not None and not isinstance(slot[k], str) for k in SLOT_KEYS if k != "admin_ipv4"):
            raise ValueError("fleet.json: bad role slot")
        admin = slot["admin_ipv4"]
        if admin is not None and (not isinstance(admin, list) or not all(isinstance(a, str) for a in admin)):
            raise ValueError("fleet.json: bad role slot")


@dataclass
class DryRunBackend:
    """Inventory and state with no cloud behind them. Live mode is OP-2's."""

    fleet: dict | None = None
    linodes: list[dict] = field(default_factory=list)  # {"id": str, "created": iso Z}
    spend_usd: float = 0.0
    mismatch_flag: bool = False
    log: list[str] = field(default_factory=list)

    audit_problems: list[str] = field(default_factory=list)

    def read_fleet(self):
        return self.fleet

    def write_fleet(self, fleet):
        self.fleet = fleet
        self.log.append("write_fleet")

    def create(self, fleet, fence):
        fence()
        self.write_fleet(fleet)
        return fleet

    def destroy(self, fence):
        fence()
        self.log.append("destroy")
        self.fleet = None
        self.linodes = []
        return True

    def finish_down(self, fleet, fence):
        fence()
        self.log.append("finish_down")
        self.mismatch_flag = False

    def audit(self, fence):
        return list(self.audit_problems)

    def set_mismatch(self, reasons):
        self.log.append("set_mismatch")
        self.mismatch_flag = True

    def delete_linodes(self, ids):
        self.log.append(f"delete_linodes:{len(ids)}")
        self.linodes = [n for n in self.linodes if n["id"] not in ids]

    def lodestar_linodes(self):
        return list(self.linodes)

    def month_spend_usd(self, now):
        return self.spend_usd

    def mismatch(self):
        return self.mismatch_flag
