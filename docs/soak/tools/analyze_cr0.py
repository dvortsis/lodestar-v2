#!/usr/bin/env python3
"""Read aggregate CR-0 collector evidence; never contact or modify the crawler."""

import argparse
from datetime import datetime, timezone
from collections import Counter
import json
import math
from pathlib import Path
import re
import sys

PHASES = ("startup", "warmup", "baseline")
COUNTERS = tuple("dht_" + name + "_total" for name in (
    "peer_admitted", "peer_rejected", "peer_cancelled", "peer_rejection_reasons", "peer_ack_failed", "peer_command_full", "peer_command_closed", "lookups_completed",
    "lookup_deadlines", "sample_stale", "sample_deferred", "queries_out",
    "responses_in", "timeouts", "packets_in", "packets_out", "harvested",
    "harvest_dropped",
)) + tuple(name + "_total" for name in (
    "fetch_attempts", "fetch_success", "fetch_failures", "fetch_lost",
    "fetch_finder_full", "fetch_finder_closed", "fetch_lookup_unaccepted", "fetch_retry_unaccepted", "fetch_evicted_service",
    "writer_stored", "writer_errors", "writer_transactions", "search_errors",
))
BOUNDS = {
    "peer_requests": (("dht_peer_active", "dht_peer_queued"), 32),
    "sampler_states": (("dht_sample_cooling", "dht_sample_inflight"), 200000),
    "sample_frontier": (("dht_sample_frontier",), 20000),
    "outstanding_rpcs": (("dht_outstanding",), None),
    "pool_lookups": (("fetch_lookups",), 256),
}
# The actor's outstanding-RPC cap per intensity preset (`Intensity::max_outstanding`).
OUTSTANDING_RPCS = {"low": 256, "balanced": 1024, "aggressive": 4096}
GAUGES = ("working_set_bytes", "private_bytes", "handles", "threads", "cpu_seconds")
CORE_COUNTERS = ("writer_stored_total", "writer_errors_total", "fetch_attempts_total",
                 "fetch_success_total", "dht_peer_admitted_total", "dht_peer_rejected_total",
                 "dht_lookups_completed_total")
STOP_REASONS = {
    "baseline_window_complete", "operator_stop", "collector_error", "evidence_write_error",
    "disk_monitor_failed", "disk_reserve_reached", "redirected_logs_limit",
    "embedded_ui_mismatch",
    "overall_deadline_without_complete_baseline", "three_consecutive_health_failures",
    "three_consecutive_metrics_failures", "three_consecutive_capacity_violations",
    "rss_over_1GiB_for_five_samples", "active_peer_requests_without_completion_for_five_minutes",
}
# The binding memory gate (owner, 2026-10-30, #189; docs/decisions/2026-10-30-CR-0-acceptance.md):
# the OLS slope of anonymous private memory over baseline hours 12-24, judged by
# the upper bound of its 95% confidence interval.
MEMORY_FIT = {"field": "private_bytes", "from_hour": 12.0, "to_hour": 24.0,
              "bar_mib_per_hour": 2.0, "min_samples": 30}
SERIES = re.compile(r"^lodestar_([a-z_]+)(\{[^\r\n]*\})?\s+(\S+)(?:\s+\d+)?$")


# Only fixed, documented enums may leave the private evidence directory.
BREAKDOWNS = {
    "fetch_failures_total": ("kind", {"no_peers", "timed_out", "refused", "encryption_failed", "oversize", "invalid", "unreachable"}),
    "fetch_lost_total": ("reason", {"no_peers", "timed_out", "refused", "encryption_failed", "oversize", "invalid", "blocked", "evicted", "gave_up", "unreachable"}),
    "dht_peer_rejection_reasons_total": ("reason", {"capacity", "paused", "caller_closed"}),
    "fetch_evicted_service_total": ("stage", {"unaccepted", "lookup", "peers", "fetch"}),
}


def safe_label(name, labels):
    if name not in BREAKDOWNS:
        return None
    key, allowed = BREAKDOWNS[name]
    match = re.fullmatch(r'\{' + key + r'="([a-z_]+)"\}', labels)
    return match[1] if match and match[1] in allowed else None


def freshness(rows, now):
    if not rows:
        return "unknown"
    try:
        stamp = datetime.fromisoformat(rows[-1]["time"].replace("Z", "+00:00"))
        age = (now - stamp).total_seconds()
        return "fresh_observations" if 0 <= age <= 180 else "stale_observations"
    except (KeyError, ValueError, TypeError):
        return "unknown"


def number(value):
    return (isinstance(value, (int, float)) and not isinstance(value, bool)
            and math.isfinite(value) and value >= 0)


def read_json(path):
    with path.open(encoding="utf-8-sig") as stream:
        try:
            value = json.load(stream)
        except RecursionError:
            raise ValueError(f"Malformed {path.name}: nested too deeply") from None
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
        except RecursionError:
            raise ValueError(f"Malformed samples.jsonl line {index + 1}") from None
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
        if "sequence" in row and (type(row["sequence"]) is not int or row["sequence"] != index):
            raise ValueError("Invalid sample sequence")
        values = row["values"]
        row["counters"] = {name: {"": values[name]} for name in COUNTERS
                           if number(values.get(name))}
        prom = root / "metrics" / f"{row.get('sequence', index):05d}.prom"
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
    return row["process_id"], row["values"].get("start_time_seconds"), row.get("process_identity")


def usable(row):
    return row.get("metrics_ok") is True and number(identity(row)[1])


def bounds_for(intensity):
    limits = {name: limit for name, (_, limit) in BOUNDS.items()}
    limits["outstanding_rpcs"] = OUTSTANDING_RPCS[intensity]
    return limits


def run_intensity(run, override, issues):
    """The preset from --intensity, else run.json's settings. Evidence without
    one is from the collector that always ran Balanced, and is flagged."""
    value = override if override is not None else run.get("settings", {}).get("intensity")
    if value is None:
        issues["intensity_assumed_balanced"] += 1
        return "balanced"
    if not isinstance(value, str) or value.lower() not in OUTSTANDING_RPCS:
        raise ValueError("Unknown intensity preset")
    return value.lower()


def summarize(rows, max_gap, intensity="balanced"):
    limits = bounds_for(intensity)
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
            "maximum_gap_seconds": 0,
            "measurement_coverage": {name: 0 for name in GAUGES},
            "health_failed_samples": sum(r.get("health") != 200 for r in selected),
            "metrics_failed_samples": sum(not usable(r) for r in selected),
            "rss_max_mib": max((r["working_set_bytes"] / 2**20 for r in selected
                                if number(r.get("working_set_bytes"))), default=None),
            "rss_change_mib": None,
            "rss_change_mib_per_hour": None,
            "bounds": {name: {"limit": limits[name], "observed_samples": 0, "max": None,
                              "at_capacity_samples": 0, "over_capacity_samples": 0}
                       for name in BOUNDS},
            "breakdowns": {}, "hourly": {},
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
            for name, (fields, _) in BOUNDS.items():
                limit = limits[name]
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
        if before["phase"] == after["phase"]:
            p = phases[after["phase"]]
            p["maximum_gap_seconds"] = max(p["maximum_gap_seconds"], dt)
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
        for name in GAUGES:
            if number(before.get(name)) and number(after.get(name)):
                p["measurement_coverage"][name] += dt
        for name in before["counters"].keys() & after["counters"].keys() - reset:
            counter = p["counters"][name]
            counter["delta"] += sum(after["counters"][name].values()) - sum(before["counters"][name].values())
            counter["covered_seconds"] += dt
            for label, value in after["counters"][name].items():
                enum = safe_label(name, label)
                if enum is not None:
                    item = p["breakdowns"].setdefault(name, {}).setdefault(enum, {"delta": 0, "covered_seconds": 0})
                    item["delta"] += value - before["counters"][name][label]
                    item["covered_seconds"] += dt
            # Assign an interval to its ending hour; don't fabricate within-interval events.
            hour = str(int((after["elapsed_seconds"] - p["first_elapsed_seconds"]) // 3600))
            item = p["hourly"].setdefault(hour, {}).setdefault(name, {"delta": 0, "covered_seconds": 0})
            item["delta"] += sum(after["counters"][name].values()) - sum(before["counters"][name].values())
            item["covered_seconds"] += dt

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
        p["measurement_coverage"] = {
            name: {"covered_seconds": seconds, "fraction": seconds / span if span > 0 else None}
            for name, seconds in p["measurement_coverage"].items()
        }
        for name, counter in p["counters"].items():
            p["measurement_coverage"][name] = {
                "covered_seconds": counter["covered_seconds"],
                "fraction": counter["covered_seconds"] / span if span > 0 else None,
            }
    return phases, issues


def memory_fit(rows):
    """OLS slope of private memory (MiB/h) over baseline hours 12-24, with a 95% CI.

    Hours count from the first baseline sample. Samples from another process
    identity are refused rather than fitted across a restart.
    """
    f = MEMORY_FIT
    baseline = [r for r in rows if r["phase"] == "baseline"]
    fit = {"series": f["field"], "from_hour": f["from_hour"], "to_hour": f["to_hour"],
           "bar_upper_ci_mib_per_hour": f["bar_mib_per_hour"], "samples": 0,
           "slope_mib_per_hour": None, "standard_error": None, "ci95_mib_per_hour": None,
           "window_complete": False, "meets_bar": None, "refused": None}
    peaks = [r[f["field"]] for r in rows if number(r.get(f["field"]))]
    fit["peak_mib"] = max(peaks) / 2**20 if peaks else None
    if not baseline:
        fit["refused"] = "no_baseline"
        return fit
    t0 = baseline[0]["elapsed_seconds"]
    points = [((r["elapsed_seconds"] - t0) / 3600, r) for r in baseline]
    window = [(h, r) for h, r in points if f["from_hour"] <= h <= f["to_hour"]]
    # A failed metrics read lacks the start time, so compare it only where present.
    processes = {(r["process_id"], r.get("process_identity")) for _, r in window}
    starts = {identity(r)[1] for _, r in window if usable(r)}
    if len(processes) > 1 or len(starts) > 1:
        fit["refused"] = "identity_change"
        return fit
    xy = [(h, r[f["field"]] / 2**20) for h, r in window if number(r.get(f["field"]))]
    fit["samples"] = n = len(xy)
    # Within one sampling interval of each end of the window.
    fit["window_complete"] = bool(xy) and xy[0][0] <= f["from_hour"] + 0.05 and xy[-1][0] >= f["to_hour"] - 0.05
    if n < f["min_samples"]:
        fit["refused"] = "too_few_samples"
        return fit
    mx = sum(x for x, _ in xy) / n
    my = sum(y for _, y in xy) / n
    sxx = sum((x - mx) ** 2 for x, _ in xy)
    if sxx == 0:
        fit["refused"] = "no_time_spread"
        return fit
    slope = sum((x - mx) * (y - my) for x, y in xy) / sxx
    residual = sum((y - my - slope * (x - mx)) ** 2 for x, y in xy)
    se = math.sqrt(residual / (n - 2) / sxx)
    # n >= 30, so the normal quantile stands in for Student's t.
    ci = [slope - 1.96 * se, slope + 1.96 * se]
    fit.update(slope_mib_per_hour=slope, standard_error=se, ci95_mib_per_hour=ci)
    if fit["window_complete"]:
        fit["meets_bar"] = ci[1] <= f["bar_mib_per_hour"]
    return fit


def result_state(result):
    if result is None:
        return "completion_unknown"
    if result.get("schema_version", 1) >= 2:
        shutdown = result.get("shutdown")
        return "stopped" if isinstance(shutdown, dict) and shutdown.get("exit_confirmed") is True else "completion_unknown"
    # Legacy collectors confirmed these exit modes; malformed values aren't proof.
    mode = result.get("stop_mode", "")
    return "stopped" if re.fullmatch(r"(?:already_)?exited_-?\d+", str(mode)) else "completion_unknown"


def clean_shutdown(result):
    if result is None:
        return None
    if result_state(result) != "stopped":
        return False
    if result.get("schema_version", 1) == 1:
        return result.get("stop_mode") in ("exited_0", "already_exited_0")
    shutdown = result["shutdown"]
    return (result.get("stop_mode") == "exited_0"
            and shutdown.get("exit_code") == 0
            and shutdown.get("signal_delivered") is True
            and not shutdown.get("errors"))


def disk_summary(root, issues):
    path = root / "disk.jsonl"
    if not path.exists():
        return None
    summary = {"observations": 0, "read_failure_observations": 0, "minimum_free_bytes": None, "max_redirected_log_bytes": None, "hourly": {}}
    try:
        with path.open(encoding="utf-8-sig") as stream:
            for line in stream:
                try:
                    row = json.loads(line)
                    if not isinstance(row, dict):
                        raise ValueError("Disk row")
                    volumes = row.get("volumes", [])
                    if not isinstance(volumes, list):
                        raise ValueError("Disk volumes")
                    for volume in volumes:
                        free = volume.get("free_bytes") if isinstance(volume, dict) else None
                        if number(free):
                            old = summary["minimum_free_bytes"]
                            summary["minimum_free_bytes"] = free if old is None else min(old, free)
                    size = row.get("redirected_log_bytes")
                    if number(size):
                        summary["max_redirected_log_bytes"] = max(summary["max_redirected_log_bytes"] or 0, size)
                    elapsed = row.get("elapsed_seconds")
                    if number(elapsed):
                        hour = str(int(elapsed // 3600))
                        bucket = summary["hourly"].setdefault(hour, {
                            "observations": 0, "read_failure_observations": 0,
                            "minimum_free_bytes": None, "max_redirected_log_bytes": None})
                        bucket["observations"] += 1
                        bucket["read_failure_observations"] += bool(row.get("read_failures"))
                        frees = [v["free_bytes"] for v in volumes
                                 if isinstance(v, dict) and number(v.get("free_bytes"))]
                        if frees:
                            old = bucket["minimum_free_bytes"]
                            bucket["minimum_free_bytes"] = min(frees) if old is None else min(old, *frees)
                        if number(size):
                            bucket["max_redirected_log_bytes"] = max(bucket["max_redirected_log_bytes"] or 0, size)
                    summary["read_failure_observations"] += bool(row.get("read_failures"))
                    summary["observations"] += 1
                except (ValueError, TypeError, RecursionError):
                    issues["invalid_disk_observations"] += 1
    except OSError:
        issues["unreadable_disk_observations"] += 1
    return summary


# Baseline coverage each leg must measure before its run can be gate evidence.
GATE_HOURS = {"main": 24, "gluetun": 2}


def run_identity(run):
    """Which kind of run this is, from run.json alone. A field the collector
    didn't record is None, never guessed."""
    settings = run.get("settings", {})
    leg = settings.get("leg")
    rehearsal = settings.get("rehearsal")
    hours = settings.get("baseline_hours")
    intensity = settings.get("intensity")
    if "python" in run or settings.get("mode") in ("docker", "native"):
        collector = "linux"
    elif "common_sha256" in run or str(run.get("os", "")).startswith("Microsoft Windows"):
        collector = "windows"
    else:
        collector = None
    return {
        "leg": leg if leg in GATE_HOURS else None,
        "rehearsal": rehearsal if isinstance(rehearsal, bool) else None,
        "baseline_required_hours": hours if number(hours) else None,
        "intensity": intensity.lower() if isinstance(intensity, str) and intensity.lower() in OUTSTANDING_RPCS else None,
        "collector": collector,
    }


def gate_verdict(identity_, covered_seconds):
    """Informational only: whether this run could count as release-gate
    evidence. It changes no pass/fail computation."""
    reasons = []
    if identity_["rehearsal"] is None:
        reasons.append("rehearsal_not_recorded")
    elif identity_["rehearsal"]:
        reasons.append("rehearsal")
    leg = identity_["leg"]
    if leg is None:
        reasons.append("leg_not_recorded")
    elif covered_seconds < GATE_HOURS[leg] * 3600:
        reasons.append("coverage_below_required")
    if identity_["intensity"] is None:
        reasons.append("intensity_not_recorded")
    return not reasons, reasons


def analyze(root, max_gap=90, now=None, intensity=None):
    run = read_json(root / "run.json")
    if not isinstance(run.get("settings", {}), dict):
        raise ValueError("Invalid run settings")
    # Read termination first: a newly completed run during this read is still
    # conservatively presented as completion unknown, never prematurely done.
    result_path = root / "result.json"
    issues = Counter()
    intensity = run_intensity(run, intensity, issues)
    try:
        result = read_json(result_path) if result_path.exists() else None
        if result is not None and not isinstance(result.get("reason"), str):
            raise ValueError("Invalid termination reason")
        if result is not None and (type(result.get("schema_version", 1)) is not int
                                   or result.get("schema_version", 1) not in (1, 2)):
            raise ValueError("Unsupported result schema")
    except (ValueError, OSError):
        result = None
        issues["invalid_result"] += 1
    if result is not None and result.get("schema_version", 1) >= 2:
        if result.get("finalization_complete") is not True:
            issues["finalization_incomplete"] += 1
        if result.get("publication_failures"):
            issues["publication_failures"] += 1
    rows = load_rows(root, issues)
    phases, intervals = summarize(rows, max_gap, intensity)
    issues.update(intervals)
    hours = run.get("settings", {}).get("baseline_hours", 24)
    if not number(hours) or hours == 0:
        raise ValueError("Invalid configured baseline duration")
    baseline = phases["baseline"]
    if baseline["samples"] == 0:
        status = "not_started"
    elif result is None:
        status = "completion_unknown"
    elif result.get("reason") != "baseline_window_complete":
        status = "interrupted" if result_state(result) == "stopped" else "interruption_exit_unknown"
    else:
        status = "collector_window_complete_review_required"
    if result is not None and result.get("samples") != len(rows):
        issues["result_sample_count_mismatch"] += 1
    if (status == "collector_window_complete_review_required"
            and baseline["observed_span_seconds"] < hours * 3600):
        issues["baseline_evidence_short"] += 1
    if not rows:
        issues["no_samples"] += 1
    # Only known scalar provenance is exported, never settings, paths or logs.
    provenance = {k: run.get(k) for k in ("commit", "sha256", "collector_sha256")
                  if re.fullmatch(r"[0-9a-fA-F]{7,64}", str(run.get(k, "")))}
    reason = result.get("reason") if result else None
    if reason is not None and reason not in STOP_REASONS:
        reason = "process_exited" if str(reason).startswith("process_exited_") else "unknown"
    core_coverage = {name: baseline["measurement_coverage"][name]["fraction"]
                     for name in CORE_COUNTERS + GAUGES}
    run_info = run_identity(run)
    fit = memory_fit(rows)
    evidence, reasons = gate_verdict(run_info, baseline["valid_interval_seconds"])
    run_info.update(baseline_measured_hours=baseline["valid_interval_seconds"] / 3600,
                    gate_evidence=evidence, gate_evidence_reasons=reasons)
    return {
        "run": run_info,
        "schema_version": 2, "provenance": provenance, "intensity": intensity,
        "run_state": result_state(result),
        "observation_state": freshness(rows, now or datetime.now(timezone.utc)),
        "disk": disk_summary(root, issues),
        "baseline_coverage_fraction": baseline["valid_interval_seconds"] / (hours * 3600),
        "collection_quality": {
            "required_coverage_fraction": 0.99, "maximum_allowed_gap_seconds": 180,
            "full_duration_observed": baseline["observed_span_seconds"] >= hours * 3600,
            "core_coverage": core_coverage,
            "core_coverage_sufficient": all(v is not None and v >= 0.99 for v in core_coverage.values()),
            "gap_limit_met": baseline["samples"] > 1 and baseline["maximum_gap_seconds"] <= 180,
        },
        "baseline_status": status, "baseline_required_hours": hours,
        "memory_fit": fit,
        "collector_completed_window": bool(result and result.get("reason") == "baseline_window_complete"),
        "stop_reason": reason,
        "clean_shutdown": clean_shutdown(result),
        "max_gap_seconds": max_gap, "issues": dict(sorted((k, v) for k, v in issues.items() if v)), "phases": phases,
        "limitations": [
            "Review evidence; this report never declares soak acceptance or a performance improvement.",
            "Rates use adjacent same-phase, same-process samples within the gap limit; coverage is per counter.",
            "Health failures remain counted and do not remove valid metric intervals or unsuccessful work.",
            "Counter resets or changed label sets invalidate that counter interval; absent data is unknown.",
            "Stored torrents are not verified searchable yield; no cohort latency or kernel-drop proof is available.",
            "Capacity equality is backpressure; over-capacity observations require review.",
            "RSS change is first-to-last within a continuous phase, not evidence of a leak.",
            "The memory fit is plain OLS on samples that are not independent; it applies the owner's bar as stated, and a pass is still reviewed.",
            "Final shutdown metrics have no monotonic sample timestamp and are excluded from rates.",
        ],
    }


def markdown(report):
    def fmt(value):
        return "unknown" if value is None else f"{value:,.2f}"
    shutdown = {None: "not recorded", True: "yes", False: "no"}[report["clean_shutdown"]]
    run = report["run"]

    def recorded(value):
        return "not recorded" if value is None else str(value).lower() if isinstance(value, bool) else str(value)
    gate = ("yes" if run["gate_evidence"] else "no: " + ", ".join(run["gate_evidence_reasons"]))
    leg = recorded(run["leg"])
    if run["leg"] == "gluetun":
        leg += " (2 h leg)"
    out = ["# CR-0 soak evidence", "", "## Run", "",
           f"- Collector: {recorded(run['collector'])}",
           f"- Leg: {leg}",
           f"- Rehearsal: {recorded(run['rehearsal'])}",
           f"- Baseline hours configured: {recorded(run['baseline_required_hours'])}",
           f"- Baseline hours measured (valid coverage): {fmt(run['baseline_measured_hours'])}",
           f"- Intensity: {recorded(run['intensity'])}",
           f"- Gate evidence: {gate}", "",
           f"Commit: `{report['provenance'].get('commit', 'unknown')}`.",
           f"Binary SHA-256: `{report['provenance'].get('sha256', 'unknown')}`.", "",
           f"Run: {report['run_state']}. Baseline: {report['baseline_status']}.", "",
           f"Intensity: {report['intensity']}.", "",
           f"Stop reason: {report['stop_reason'] or 'not recorded'}. Clean shutdown: {shutdown}.", "",
           "| Phase | Samples | Span (h) | Covered (h) | Health failures | Metrics failures | Peak RSS (MiB) | RSS change (MiB/h) |",
           "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"]
    for phase, p in report["phases"].items():
        out.append(f"| {phase} | {p['samples']} | {fmt(p['observed_span_seconds']/3600)} | "
                   f"{fmt(p['valid_interval_seconds']/3600)} | {p['health_failed_samples']} | "
                   f"{p['metrics_failed_samples']} | {fmt(p['rss_max_mib'])} | {fmt(p['rss_change_mib_per_hour'])} |")
    fit = report["memory_fit"]
    verdict = {None: "not judged", True: "meets the bar", False: "misses the bar"}[fit["meets_bar"]]
    ci = fit["ci95_mib_per_hour"]
    out += ["", "## Memory gate", "",
            f"- Series: `{fit['series']}`, baseline hours {fit['from_hour']:g}-{fit['to_hour']:g} "
            f"({fit['samples']} samples, window complete: {str(fit['window_complete']).lower()})",
            f"- OLS slope: {fmt(fit['slope_mib_per_hour'])} MiB/h, 95% CI "
            + (f"[{ci[0]:,.2f}, {ci[1]:,.2f}]" if ci else "unknown") + " MiB/h",
            f"- Bar: upper 95% bound <= {fit['bar_upper_ci_mib_per_hour']:g} MiB/h: {verdict}"
            + (f" ({fit['refused']})" if fit["refused"] else ""),
            f"- Peak private memory: {fmt(fit['peak_mib'])} MiB"]
    out += ["", f"Observation freshness: {report['observation_state']}.",
            f"Baseline valid coverage: {report['baseline_coverage_fraction']:.2%}.",
            f"Disk summary: {json.dumps(report['disk'])}", "", "## Evidence issues", ""]
    out += [f"- {name}: {count}" for name, count in report["issues"].items() if count] or ["No collection issues detected in this snapshot."]
    for phase, p in report["phases"].items():
        if not p["samples"]:
            continue
        out += ["", f"## {phase.capitalize()}", "",
                "| Counter | Observed delta | Per hour | Covered seconds |",
                "| --- | ---: | ---: | ---: |"]
        for name, c in p["counters"].items():
            out.append(f"| {name} | {fmt(c['delta'])} | {fmt(c['per_hour'])} | {fmt(c['covered_seconds'])} |")
        for name, labels in p["breakdowns"].items():
            out += ["", f"Reason breakdown: {name}", "", "| Reason | Delta | Covered seconds |", "| --- | ---: | ---: |"]
            out += [f"| {label} | {fmt(v['delta'])} | {fmt(v['covered_seconds'])} |" for label, v in sorted(labels.items())]
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
    parser.add_argument("--intensity", choices=sorted(OUTSTANDING_RPCS),
                        help="Preset whose outstanding-RPC cap applies (default: run.json settings)")
    args = parser.parse_args()
    if not math.isfinite(args.max_gap_seconds) or args.max_gap_seconds <= 0:
        parser.error("--max-gap-seconds must be positive and finite")
    if args.output and args.output.resolve().is_relative_to(args.root.resolve()):
        parser.error("Report output must be outside the evidence directory")
    try:
        report = analyze(args.root, args.max_gap_seconds, intensity=args.intensity)
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
