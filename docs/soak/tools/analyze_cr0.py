#!/usr/bin/env python3
"""Read aggregate CR-0 collector evidence; never contact or modify the crawler."""

import argparse
from collections import Counter
import json
import math
from pathlib import Path
import re
import sys

PHASES = ("startup", "warmup", "baseline")
COUNTERS = tuple("dht_" + name + "_total" for name in (
    "peer_admitted", "peer_rejected", "peer_cancelled", "lookups_completed",
    "lookup_deadlines", "sample_stale", "sample_deferred", "queries_out",
    "responses_in", "timeouts", "packets_in", "packets_out", "harvested",
    "harvest_dropped",
)) + tuple(name + "_total" for name in (
    "fetch_attempts", "fetch_success", "fetch_failures", "fetch_lost",
    "writer_stored", "writer_errors", "writer_transactions", "search_errors",
))
BOUNDS = {
    "peer_requests": (("dht_peer_active", "dht_peer_queued"), 32),
    "sampler_states": (("dht_sample_cooling", "dht_sample_inflight"), 200000),
    "sample_frontier": (("dht_sample_frontier",), 20000),
    "outstanding_rpcs": (("dht_outstanding",), 1024),
    "pool_lookups": (("fetch_lookups",), 256),
}
STOP_REASONS = {
    "baseline_window_complete", "operator_stop", "collector_error",
    "overall_deadline_without_complete_baseline", "three_consecutive_health_failures",
    "three_consecutive_metrics_failures", "three_consecutive_capacity_violations",
    "rss_over_1GiB_for_five_samples", "active_peer_requests_without_completion_for_five_minutes",
}
SERIES = re.compile(r"^lodestar_([a-z_]+)(\{[^\r\n]*\})?\s+(\S+)(?:\s+\d+)?$")


def number(value):
    return (isinstance(value, (int, float)) and not isinstance(value, bool)
            and math.isfinite(value) and value >= 0)


def read_json(path):
    with path.open(encoding="utf-8-sig") as stream:
        value = json.load(stream)
    if not isinstance(value, dict):
        raise ValueError(f"Expected an object in {path.name}")
    return value


def parse_prom(text):
    """Keep only known counters; labels stay internal and never enter reports."""
    counters = {}
    for line in text.splitlines():
        match = SERIES.fullmatch(line)
        if not match or match[1] not in COUNTERS:
            continue
        name, labels, raw = match.groups()
        value = float(raw)
        if not number(value):
            raise ValueError("Nonfinite or negative counter")
        series = counters.setdefault(name, {})
        if (labels or "") in series:
            raise ValueError("Duplicate counter series")
        series[labels or ""] = value
    return counters


def load_rows(root, issues):
    # One read captures a bounded snapshot even while the collector appends.
    lines = (root / "samples.jsonl").read_bytes().decode("utf-8-sig").splitlines(True)
    rows = []
    for index, line in enumerate(lines):
        try:
            row = json.loads(line)
        except json.JSONDecodeError:
            if index == len(lines) - 1 and not line.endswith("\n"):
                issues["incomplete_tail_ignored"] += 1
                break
            raise ValueError(f"Malformed samples.jsonl line {index + 1}") from None
        if (not isinstance(row, dict) or row.get("phase") not in PHASES
                or not number(row.get("elapsed_seconds"))
                or not isinstance(row.get("values"), dict)
                or not number(row.get("process_id"))):
            raise ValueError(f"Invalid sample schema at line {index + 1}")
        values = row["values"]
        row["counters"] = {name: {"": values[name]} for name in COUNTERS
                           if number(values.get(name))}
        prom = root / "metrics" / f"{index:05d}.prom"
        if row.get("metrics_ok") is True:
            if prom.exists():
                try:
                    extra = parse_prom(prom.read_text(encoding="utf-8-sig"))
                    # Scalar counters recorded in JSONL and Prometheus must agree.
                    for name in row["counters"].keys() & extra.keys():
                        if row["counters"][name] != extra[name]:
                            raise ValueError("Snapshot mismatch")
                    row["counters"].update(extra)
                except (ValueError, OSError):
                    issues["invalid_prom_snapshots"] += 1
                    # Keep the collector's JSON values, not partial Prom data.
            else:
                issues["missing_prom_snapshots"] += 1
        rows.append(row)
    return rows


def identity(row):
    return row["process_id"], row["values"].get("start_time_seconds")


def usable(row):
    return row.get("metrics_ok") is True and number(identity(row)[1])


def summarize(rows, max_gap):
    issues = Counter()
    phases = {}
    for phase in PHASES:
        selected = [r for r in rows if r["phase"] == phase]
        phases[phase] = {
            "samples": len(selected),
            "first_elapsed_seconds": selected[0]["elapsed_seconds"] if selected else None,
            "last_elapsed_seconds": selected[-1]["elapsed_seconds"] if selected else None,
            "observed_span_seconds": (selected[-1]["elapsed_seconds"] - selected[0]["elapsed_seconds"]
                                      if len(selected) > 1 else 0),
            "valid_interval_seconds": 0,
            "health_failed_samples": sum(r.get("health") != 200 for r in selected),
            "metrics_failed_samples": sum(not usable(r) for r in selected),
            "rss_max_mib": max((r["working_set_bytes"] / 2**20 for r in selected
                                if number(r.get("working_set_bytes"))), default=None),
            "rss_change_mib": None,
            "rss_change_mib_per_hour": None,
            "bounds": {name: {"limit": limit, "observed_samples": 0, "max": None,
                              "at_capacity_samples": 0, "over_capacity_samples": 0}
                       for name, (_, limit) in BOUNDS.items()},
            "counters": {name: {"delta": 0, "covered_seconds": 0, "per_hour": None}
                         for name in COUNTERS},
        }
        p = phases[phase]
        for row in selected:
            if not usable(row):
                continue
            suppression = row["values"].get("dht_sample_suppression_seconds")
            if phase == "baseline" and number(suppression) and suppression > 0:
                issues["baseline_during_suppression"] += 1
            for name, (fields, limit) in BOUNDS.items():
                if all(number(row["values"].get(f)) for f in fields):
                    total = sum(row["values"][f] for f in fields)
                    bound = p["bounds"][name]
                    bound["observed_samples"] += 1
                    bound["max"] = max(bound["max"] or 0, total)
                    bound["at_capacity_samples"] += total == limit
                    bound["over_capacity_samples"] += total > limit

    for before, after in zip(rows, rows[1:]):
        dt = after["elapsed_seconds"] - before["elapsed_seconds"]
        if dt <= 0:
            issues["nonmonotonic_intervals"] += 1
            continue
        if dt > max_gap:
            issues["sampling_gaps"] += 1
        if identity(before) != identity(after):
            issues["identity_changes"] += 1
            continue
        if PHASES.index(after["phase"]) < PHASES.index(before["phase"]):
            issues["phase_regressions"] += 1
        # Check resets even across gaps and phase transitions. Never bridge them.
        reset = set()
        if usable(before) and usable(after):
            for name in before["counters"].keys() & after["counters"].keys():
                left, right = before["counters"][name], after["counters"][name]
                if left.keys() != right.keys() or any(right[k] < left[k] for k in left):
                    reset.add(name)
            issues["counter_discontinuities"] += len(reset)
        if (before["phase"] != after["phase"] or dt > max_gap
                or not usable(before) or not usable(after)):
            continue
        p = phases[after["phase"]]
        p["valid_interval_seconds"] += dt
        for name in before["counters"].keys() & after["counters"].keys() - reset:
            counter = p["counters"][name]
            counter["delta"] += sum(after["counters"][name].values()) - sum(before["counters"][name].values())
            counter["covered_seconds"] += dt

    for phase, p in phases.items():
        selected = [r for r in rows if r["phase"] == phase]
        # A first-to-last RSS change is descriptive, not a leak diagnosis.
        span = p["observed_span_seconds"]
        if (span > 0 and math.isclose(p["valid_interval_seconds"], span)
                and all(number(r.get("working_set_bytes")) for r in selected)):
            change = (selected[-1]["working_set_bytes"] - selected[0]["working_set_bytes"]) / 2**20
            p["rss_change_mib"] = change
            p["rss_change_mib_per_hour"] = change * 3600 / span
        for counter in p["counters"].values():
            seconds = counter["covered_seconds"]
            if seconds:
                counter["per_hour"] = counter["delta"] * 3600 / seconds
            else:
                counter["delta"] = None
    return phases, issues


def analyze(root, max_gap=90):
    run = read_json(root / "run.json")
    if not isinstance(run.get("settings", {}), dict):
        raise ValueError("Invalid run settings")
    # Read termination first: a newly completed run during this read is still
    # conservatively presented as an ongoing snapshot, never prematurely done.
    result_path = root / "result.json"
    result = read_json(result_path) if result_path.exists() else None
    if result is not None and not isinstance(result.get("reason"), str):
        raise ValueError("Invalid termination reason")
    issues = Counter()
    rows = load_rows(root, issues)
    phases, intervals = summarize(rows, max_gap)
    issues.update(intervals)
    hours = run.get("settings", {}).get("baseline_hours", 24)
    if not number(hours) or hours == 0:
        raise ValueError("Invalid configured baseline duration")
    baseline = phases["baseline"]
    if baseline["samples"] == 0:
        status = "not_started"
    elif result is None:
        status = "in_progress"
    elif result.get("reason") != "baseline_window_complete":
        status = "stopped_early"
    else:
        status = "collector_window_complete_review_required"
    if result is not None and result.get("samples") != len(rows):
        issues["result_sample_count_mismatch"] += 1
    if (status == "collector_window_complete_review_required"
            and baseline["observed_span_seconds"] < hours * 3600 - max_gap):
        issues["baseline_evidence_short"] += 1
    if not rows:
        issues["no_samples"] += 1
    # Only known scalar provenance is exported, never settings, paths or logs.
    provenance = {k: run.get(k) for k in ("commit", "sha256", "collector_sha256")
                  if re.fullmatch(r"[0-9a-fA-F]{7,64}", str(run.get(k, "")))}
    reason = result.get("reason") if result else None
    if reason is not None and reason not in STOP_REASONS:
        reason = "process_exited" if str(reason).startswith("process_exited_") else "unknown"
    return {
        "schema_version": 1, "provenance": provenance,
        "run_state": "stopped" if result else "ongoing_snapshot",
        "baseline_status": status, "baseline_required_hours": hours,
        "collector_completed_window": bool(result and result.get("reason") == "baseline_window_complete"),
        "stop_reason": reason,
        "clean_shutdown": (result.get("stop_mode") == "exited_0") if result else None,
        "max_gap_seconds": max_gap, "issues": dict(sorted((k, v) for k, v in issues.items() if v)), "phases": phases,
        "limitations": [
            "Review evidence; this report never declares soak acceptance or a performance improvement.",
            "Rates use adjacent same-phase, same-process samples within the gap limit; coverage is per counter.",
            "Health failures remain counted and do not remove valid metric intervals or unsuccessful work.",
            "Counter resets or changed label sets invalidate that counter interval; absent data is unknown.",
            "Stored torrents are not verified searchable yield; no cohort latency or kernel-drop proof is available.",
            "Capacity equality is backpressure; over-capacity observations require review.",
            "RSS change is first-to-last within a continuous phase, not evidence of a leak.",
            "Final shutdown metrics have no monotonic sample timestamp and are excluded from rates.",
        ],
    }


def markdown(report):
    def fmt(value):
        return "unknown" if value is None else f"{value:,.2f}"
    shutdown = {None: "not recorded", True: "yes", False: "no"}[report["clean_shutdown"]]
    out = ["# CR-0 soak evidence", "",
           f"Commit: `{report['provenance'].get('commit', 'unknown')}`.",
           f"Binary SHA-256: `{report['provenance'].get('sha256', 'unknown')}`.", "",
           f"Run: {report['run_state']}. Baseline: {report['baseline_status']}.", "",
           f"Stop reason: {report['stop_reason'] or 'not recorded'}. Clean shutdown: {shutdown}.", "",
           "| Phase | Samples | Span (h) | Covered (h) | Health failures | Metrics failures | Peak RSS (MiB) | RSS change (MiB/h) |",
           "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"]
    for phase, p in report["phases"].items():
        out.append(f"| {phase} | {p['samples']} | {fmt(p['observed_span_seconds']/3600)} | "
                   f"{fmt(p['valid_interval_seconds']/3600)} | {p['health_failed_samples']} | "
                   f"{p['metrics_failed_samples']} | {fmt(p['rss_max_mib'])} | {fmt(p['rss_change_mib_per_hour'])} |")
    out += ["", "## Evidence issues", ""]
    out += [f"- {name}: {count}" for name, count in report["issues"].items() if count] or ["No collection issues detected in this snapshot."]
    for phase, p in report["phases"].items():
        if not p["samples"]:
            continue
        out += ["", f"## {phase.capitalize()}", "",
                "| Counter | Observed delta | Per hour | Covered seconds |",
                "| --- | ---: | ---: | ---: |"]
        for name, c in p["counters"].items():
            out.append(f"| {name} | {fmt(c['delta'])} | {fmt(c['per_hour'])} | {fmt(c['covered_seconds'])} |")
        out += ["", "| Resource | Observed max | Limit | At capacity samples | Over capacity samples |",
                "| --- | ---: | ---: | ---: | ---: |"]
        for name, b in p["bounds"].items():
            out.append(f"| {name} | {fmt(b['max'])} | {b['limit']} | {b['at_capacity_samples']} | {b['over_capacity_samples']} |")
    out += ["", "## Interpretation", ""] + [f"- {s}" for s in report["limitations"]]
    return "\n".join(out) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path, help="Collector evidence directory (read only)")
    parser.add_argument("--format", choices=("markdown", "json"), default="markdown")
    parser.add_argument("--output", type=Path, help="New report path outside the evidence directory")
    parser.add_argument("--max-gap-seconds", type=float, default=90)
    args = parser.parse_args()
    if not math.isfinite(args.max_gap_seconds) or args.max_gap_seconds <= 0:
        parser.error("--max-gap-seconds must be positive and finite")
    if args.output and args.output.resolve().is_relative_to(args.root.resolve()):
        parser.error("Report output must be outside the evidence directory")
    try:
        report = analyze(args.root, args.max_gap_seconds)
        rendered = (json.dumps(report, indent=2, allow_nan=False) + "\n"
                    if args.format == "json" else markdown(report))
        if args.output:
            with args.output.open("x", encoding="utf-8") as stream:
                stream.write(rendered)
        else:
            print(rendered, end="")
    except (OSError, ValueError) as error:
        print(f"Cannot analyze evidence: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
