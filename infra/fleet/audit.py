"""Job SSH rules, the stale-rule sweep and the hourly firewall audit (OP-2; plan §3).

Pure functions over Linode's firewall documents. Callers fetch and write them
through linode.py and fence every whole-rule-set write on the fleet lock.
Problems are fixed strings: no address, ID or label goes into one.

A firewall passes only when it is enabled, attached only to fleet Linodes,
`inbound_policy` is DROP, and its inbound rules are exactly the admin rule
(the /32s recorded at `up`) plus well-formed job rules: ACCEPT, TCP/22, one
IPv4 /32 inside GitHub's Actions ranges (from /meta), labelled
`job-<run_id>-<attempt>`, at most one per run. Stale job rules are swept first.
"""

from __future__ import annotations

import ipaddress
import re
from datetime import datetime, timedelta
from typing import Callable, Iterable

JOB_LABEL = re.compile(r"^job-([0-9]{1,20})-([0-9]{1,4})$")
ADMIN_LABEL = "ssh-admin-only"
CHAIN = timedelta(minutes=90)  # the lock's whole chain (plan §3)
LIVE_STATUSES = {"queued", "in_progress", "waiting", "requested", "pending"}
FLEET_WORKFLOW = ".github/workflows/fleet.yml"  # only its runs open job rules


def actions_ranges(meta: dict) -> list[ipaddress.IPv4Network]:
    """GitHub's Actions IPv4 ranges from GET /meta. Empty means fail closed."""
    nets = []
    for cidr in meta.get("actions") or []:
        try:
            net = ipaddress.ip_network(cidr, strict=False)
        except ValueError:
            continue
        if isinstance(net, ipaddress.IPv4Network):
            nets.append(net)
    if not nets:
        raise ValueError("no Actions IPv4 ranges in /meta")
    return nets


def _v4_v6(rule: dict) -> tuple[list, list]:
    a = rule.get("addresses") or {}
    return list(a.get("ipv4") or []), list(a.get("ipv6") or [])


def _is_ssh(rule: dict) -> bool:
    return rule.get("action") == "ACCEPT" and rule.get("protocol") == "TCP" and str(rule.get("ports")) == "22"


def in_ranges(cidr: str, ranges: Iterable[ipaddress.IPv4Network]) -> bool:
    try:
        net = ipaddress.ip_network(cidr, strict=True)
    except ValueError:
        return False
    return isinstance(net, ipaddress.IPv4Network) and net.prefixlen == 32 and any(net.subnet_of(r) for r in ranges)


def job_rule_ok(rule: dict, ranges) -> bool:
    v4, v6 = _v4_v6(rule)
    return bool(JOB_LABEL.match(str(rule.get("label", "")))) and _is_ssh(rule) and not v6 \
        and len(v4) == 1 and in_ranges(v4[0], ranges)


def admin_rule_ok(rule: dict, admin: list[str]) -> bool:
    v4, v6 = _v4_v6(rule)
    return rule.get("label") == ADMIN_LABEL and _is_ssh(rule) and not v6 and sorted(v4) == sorted(admin)


def job_rule(run_id: str, attempt: str, cidr: str) -> dict:
    return {"label": f"job-{run_id}-{attempt}", "action": "ACCEPT", "protocol": "TCP", "ports": "22",
            "addresses": {"ipv4": [cidr]}}


def run_is_live(run: dict | None, attempt: str, now: datetime) -> bool:
    """Live: a fleet.yml run, at the attempt the label names, queued or running,
    and started within the lock's 90-minute chain."""
    if not run or run.get("status") not in LIVE_STATUSES:
        return False
    path = str(run.get("path", ""))
    if (path != FLEET_WORKFLOW and not path.startswith(FLEET_WORKFLOW + "@")) or str(run.get("run_attempt")) != str(attempt):
        return False
    try:
        started = datetime.fromisoformat(str(run.get("run_started_at")).replace("Z", "+00:00"))
    except ValueError:
        return False
    return now - started <= CHAIN


def sweep(rules: dict, lookup_run: Callable[[str], dict | None], now: datetime) -> tuple[dict, int, int]:
    """Drop job rules whose run isn't live (see run_is_live). Anything else is
    kept for the audit to judge. A lookup that fails counts as not live, so its
    rule is swept (fail closed: it costs a live job only its SSH), and is
    counted so the reaper escalates. Returns (rules, swept, failed lookups)."""
    kept, swept, failed = [], 0, 0
    for rule in rules.get("inbound") or []:
        m = JOB_LABEL.match(str(rule.get("label", "")))
        if m:
            try:
                live = run_is_live(lookup_run(m.group(1)), m.group(2), now)
            except Exception:  # noqa: BLE001
                live, failed = False, failed + 1
            if not live:
                swept += 1
                continue
        kept.append(rule)
    return dict(rules, inbound=kept), swept, failed


def open_rule(rules: dict, run_id: str, attempt: str, cidr: str, ranges) -> dict:
    if not in_ranges(cidr, ranges):
        raise ValueError("refused: this runner's address isn't in GitHub's Actions ranges")
    label = f"job-{run_id}-{attempt}"
    others = [r for r in rules.get("inbound") or [] if r.get("label") != label]
    return dict(rules, inbound=others + [job_rule(run_id, attempt, cidr)])


def close_rule(rules: dict, run_id: str, attempt: str) -> dict:
    label = f"job-{run_id}-{attempt}"
    return dict(rules, inbound=[r for r in rules.get("inbound") or [] if r.get("label") != label])


def audit_firewall(firewall: dict, rules: dict, devices: list[dict], admin: list[str] | None,
                   ranges, fleet_linode_ids: set[str]) -> list[str]:
    problems = []
    if admin is None:
        problems.append("a tagged firewall isn't recorded in fleet.json")
        admin = []
    if firewall.get("status") != "enabled":
        problems.append("a fleet firewall isn't enabled")
    if rules.get("inbound_policy") != "DROP":
        problems.append("a fleet firewall's inbound policy isn't DROP")
    if not devices:
        problems.append("a fleet firewall isn't attached")
    for d in devices:
        e = d.get("entity") or {}
        if e.get("type") != "linode" or str(e.get("id")) not in fleet_linode_ids:
            problems.append("a fleet firewall is attached to something outside the fleet")
    inbound = rules.get("inbound") or []
    if sum(1 for r in inbound if admin_rule_ok(r, admin)) != 1:
        problems.append("a fleet firewall's admin rule isn't exactly the recorded one")
    runs = []
    for r in inbound:
        if admin_rule_ok(r, admin):
            continue
        if not job_rule_ok(r, ranges):
            problems.append("a fleet firewall has a rule outside the allowed shapes")
        else:
            runs.append(JOB_LABEL.match(r["label"]).group(1))
    if len(runs) != len(set(runs)):
        problems.append("a fleet firewall has more than one job rule for a run")
    return problems


def attachment_problems(linode_firewalls: dict[str, list[str]], tagged: set[str], recorded: set[str]) -> list[str]:
    """Each fleet Linode sits behind exactly one firewall, a recorded and tagged
    one; and every recorded firewall still exists and is tagged. This catches a
    fleet firewall deleted, untagged or detached, which the per-firewall audit
    (which sees only tagged firewalls) can't."""
    problems = []
    for fws in linode_firewalls.values():
        if len(fws) != 1:
            problems.append("a fleet Linode isn't behind exactly one firewall")
        elif fws[0] not in tagged or fws[0] not in recorded:
            problems.append("a fleet Linode's firewall isn't a recorded, tagged fleet firewall")
    if recorded - tagged:
        problems.append("a recorded fleet firewall is missing or untagged")
    return problems
