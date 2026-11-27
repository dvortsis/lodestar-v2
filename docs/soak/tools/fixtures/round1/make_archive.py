#!/usr/bin/env python3
"""Synthetic round-1 archives for `analyze_cr.py gate-round` (X-cr-round1-gate).

Builds a round's declaration, the booked comparator's report, each day's arm reports (the
fields `segment --declaration` writes that gate-round reads), the day archive (exit
assignment, egress readings, activity, leak tests, events) and the 10 s host samples, then
a manifest with every file's SHA-256. Nothing here is real: hashes are of fixed labels,
and no address, hostname or identifier is recorded.

  python3 make_archive.py OUT_DIR [--layout L3|L2|L2b|S]

Tests import `archive(layout)` to get the structure, change it, and `write(root, a)` it.
Standard library only.
"""

import argparse
from datetime import datetime, timedelta, timezone
import hashlib
import json
from pathlib import Path

SWITCHES = ("coordinated_admission", "responder_hints", "streaming_peers", "target_prefixes", "utp",
            "route_backoff", "lookup_end", "preset_slots", "hint_first", "sampler_cooldown_split",
            "sample_backoff_escalate", "soft_timeout")
ALL_ON = {s: s != "route_backoff" for s in SWITCHES}  # O3: route_backoff declared off
ALL_OFF = {s: False for s in SWITCHES}
DIGEST = "lodestar@sha256:" + "d" * 64  # name@sha256: the declaration's form (X-cr-round1-collect-fixes)
SPEC_COMMIT = "c" * 40
DEVICE = "nvme0n1"  # the /data device's kernel name, as a declaration names it (§6.3)
COMMIT = "e" * 40


def h(label):
    """A stand-in for a salted hash: the SHA-256 of a fixed label."""
    return hashlib.sha256(label.encode()).hexdigest()


HOST = h("host")
NETWORK = {"uplink_hash": h("uplink"), "provider": "pia", "protocol": "wireguard"}
EXITS = {"X": h("exit-x"), "Y": h("exit-y"), "Z": h("exit-z")}
LAYOUTS = {
    "L3": {1: {"B": "X", "A": "Y", "C": "Z"}, 2: {"B": "Y", "A": "Z", "C": "X"}},
    "L2": {1: {"B": "X", "A": "Y"}, 2: {"B": "Y", "A": "X"}},
    "L2b": {1: {"B": "X", "C": "Y"}, 2: {"B": "Y", "C": "X"}},
    "L2c": {1: {"A": "X", "C": "Y"}, 2: {"A": "Y", "C": "X"}},
    "S": {1: {"B": "X"}, 2: {"B": "Y"}},
}
ROLE = {"B": "candidate", "A": "candidate", "C": "diagnostic"}
PRESET = {"B": "balanced", "A": "aggressive", "C": "balanced"}
COMP_START = datetime(2026, 11, 27, 5, 0, tzinfo=timezone.utc)
COMP_WS = COMP_START + timedelta(hours=1)
COMP_END = COMP_WS + timedelta(hours=24)
DAY_START = {1: COMP_END + timedelta(hours=12), 2: COMP_END + timedelta(hours=38)}
STAGGER = {"B": 0, "A": 3, "C": 6}  # minutes: staggered starts and finishes (§6.3)


def iso(t):
    return t.astimezone(timezone.utc).isoformat().replace("+00:00", "Z")


def entry(attempt, day, arm, exit_label):
    return {"round": 1, "attempt": attempt, "day": day, "arm": arm, "role": ROLE[arm], "preset": PRESET[arm],
            "switches": dict(ALL_OFF if arm == "C" else ALL_ON), "exit": exit_label, "image_digest": DIGEST}


def report(e, run_id, start, rate=120.0, hourly=None, p95=60, memory_ok=True):
    ws = start + timedelta(minutes=61)
    we = ws + timedelta(hours=24)
    hourly = hourly or [rate] * 24
    return {
        "schema": 1, "kind": "comb", "treatment": None, "rehearsal": False, "window_hours": 24,
        "hour_seconds": 3600.0, "verdict": "valid", "verdict_reasons": [],
        "identity": {"image_digest": e["image_digest"], "source_commit": COMMIT, "spec_commit": SPEC_COMMIT,
                     "switches": dict(e["switches"], safe_restart=True), "preset": e["preset"]},
        "binding": {"run_id": run_id, "host_identity": HOST, "network_identity": dict(NETWORK),
                    "placement": {k: e[k] for k in ("round", "attempt", "day", "arm")},
                    "ended_utc": iso(we + timedelta(seconds=40))},
        "round1": {"declared": dict(e), "fresh_store": True},
        "timeline": {"process_start_utc": iso(start), "window_start_utc": iso(ws), "window_end_utc": iso(we),
                     "last_scrape_utc": iso(we + timedelta(seconds=30))},
        "numerator": {"window": {"mean_net_per_min": sum(hourly) / len(hourly)},
                      "hourly": [{"hour": k, "net_per_min": v} for k, v in enumerate(hourly)],
                      "worst_hour_net_per_min": min(hourly)},
        "latency": {"measured": True, "coverage": 0.995, "p95_seconds": p95, "passes_p95_600": p95 <= 600},
        "memory": {"series": "anon", "meets_bar": memory_ok, "window_complete": True, "refused": None,
                   "slope_mib_per_hour": 0.4 if memory_ok else 2.4,
                   "ci95_mib_per_hour": [0.1, 0.7] if memory_ok else [2.1, 2.7]},
    }


def comparator():
    return {"schema": 1, "kind": "comparator", "rehearsal": False, "window_hours": 24, "verdict": "valid",
            "binding": {"run_id": "comp-1", "host_identity": HOST, "network_identity": dict(NETWORK),
                        "placement": None, "ended_utc": iso(COMP_END + timedelta(seconds=40))},
            "timeline": {"process_start_utc": iso(COMP_START), "window_start_utc": iso(COMP_WS),
                         "window_end_utc": iso(COMP_END), "last_scrape_utc": iso(COMP_END + timedelta(seconds=30))},
            "numerator": {"window": {"mean_per_min": 40.0}, "worst_hour_per_min": 30.0}}


def day_archive(attempt, day, reports, placed):
    arms, latest = {}, max(datetime.fromisoformat(r["timeline"]["window_end_utc"].replace("Z", "+00:00"))
                           for r in reports.values())
    for arm, rep in reports.items():
        ws = datetime.fromisoformat(rep["timeline"]["window_start_utc"].replace("Z", "+00:00"))
        start = datetime.fromisoformat(rep["timeline"]["process_start_utc"].replace("Z", "+00:00"))
        readings = [{"utc": iso(start + timedelta(seconds=20)), "hash": EXITS[placed[arm]]}]
        readings += [{"utc": iso(ws + timedelta(hours=k, seconds=30)), "hash": EXITS[placed[arm]]} for k in range(25)]
        arms[arm] = {"exit": placed[arm], "egress": readings, "active_from_utc": iso(start + timedelta(seconds=60)),
                     "active_through_utc": iso(latest + timedelta(minutes=2)),
                     "leak_test": {"before": "pass", "after": "pass"}}
    return {"schema": 1, "round": 1, "attempt": attempt, "day": day,
            "exits": {label: EXITS[label] for label in sorted(set(placed.values()))}, "arms": arms, "events": []}


def host_samples(reports, cpu=0.40):
    starts = [datetime.fromisoformat(r["timeline"]["process_start_utc"].replace("Z", "+00:00")) for r in reports.values()]
    ends = [datetime.fromisoformat(r["timeline"]["window_end_utc"].replace("Z", "+00:00")) for r in reports.values()]
    t, last, out, drops = min(starts), max(ends) + timedelta(minutes=2), [], 0
    while t <= last:
        out.append({"utc": iso(t), "cpu": cpu, "steal": 0.01, "disk_write_ms": 4.0, "disk_inflight": 0,
                    "uplink_drops_total": drops, "tunnels": {arm: {"rtt_ms": 31.0, "lost": False} for arm in reports}})
        t += timedelta(seconds=10)
    return {"schema": 1, "interval_seconds": 10, "vcpus": 8, "backing_device": DEVICE,
            "rehearsal_rtt_median_ms": {arm: 30.0 for arm in reports}, "samples": out}


def archive(layout="L3", attempt="r1-a1"):
    """The round's whole archive as one structure, before it's written."""
    days, arms = [], []
    for day in (1, 2):
        placed = LAYOUTS[layout][day]
        reports = {}
        for arm, label in placed.items():
            e = entry(attempt, day, arm, label)
            arms.append(e)
            start = DAY_START[day] + timedelta(minutes=STAGGER[arm])
            reports[arm] = report(e, f"{attempt}-d{day}-{arm}", start)
        days.append({"attempt": attempt, "day": day, "repeat": 0, "reports": reports,
                     "archive": day_archive(attempt, day, reports, placed), "contention": host_samples(reports)})
    decl = {"schema": 1, "kind": "cr_round1_declaration", "round": 1, "spec_commit": SPEC_COMMIT,
            "image_digest": DIGEST, "host_identity": HOST, "network_identity": dict(NETWORK), "backing_device": DEVICE,
            "vcpus": 8,
            "comparator": {"run_id": "comp-1"}, "selection_order": ["B", "A"],
            "attempts": [{"id": attempt, "layout": layout,
                          "candidate_digests": {a: DIGEST for a in LAYOUTS[layout][1] if ROLE[a] == "candidate"}}],
            "arms": arms}
    return {"declaration": decl, "comparator": comparator(), "days": days}


def dump(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    data = (json.dumps(value, indent=1, sort_keys=True) + "\n").encode()
    path.write_bytes(data)
    return {"path": path.as_posix(), "sha256": hashlib.sha256(data).hexdigest()}


def write(root, a):
    """Write `a` under root and return the manifest's path. A part set to None is left out
    of the manifest (a missing input); the manifest lists every file written, by hash."""
    root = Path(root)
    root.mkdir(parents=True, exist_ok=True)

    def put(rel, value):
        ref = dump(root / rel, value)
        return {"path": rel, "sha256": ref["sha256"]}
    man = {"schema": 1, "kind": "cr_round1_manifest", "round": 1,
           "declaration": put("declaration.json", a["declaration"]),
           "comparator": put("comparator.json", a["comparator"]) if a["comparator"] is not None else None,
           "days": []}
    for d in a["days"]:
        tag = f"{d['attempt']}/day{d['day']}" + (f"-repeat{d['repeat']}" if d["repeat"] else "")
        entry_ = {"attempt": d["attempt"], "day": d["day"], "repeat": d["repeat"],
                  "reports": {arm: put(f"{tag}/{arm}.json", r) for arm, r in d["reports"].items()},
                  "archive": put(f"{tag}/archive.json", d["archive"]) if d["archive"] is not None else None,
                  "contention": put(f"{tag}/host-samples.json", d["contention"]) if d["contention"] is not None else None}
        man["days"].append(entry_)
    path = root / "manifest.json"
    path.write_text(json.dumps(man, indent=1, sort_keys=True) + "\n")
    return path


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("out", type=Path)
    parser.add_argument("--layout", choices=sorted(LAYOUTS), default="L3")
    args = parser.parse_args()
    print(write(args.out, archive(args.layout)))


if __name__ == "__main__":
    main()
