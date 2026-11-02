#!/usr/bin/env python3
"""Judge crawler-run evidence by the frozen CR-SPEC rules.

docs/decisions/2026-11-02-CR-SPEC.md is the rulebook; rule numbers below are its.
Reads the Linux collector's evidence (linux/collect.py --run-kind ab|comb|comparator)
and never contacts the crawler.

  segment RUN [--output F] [--publish DIR]   one segment: window, rates, checks, verdict
  effect --treatment T --block R R R R ...   block checks and the effect fit (rules 6.1-6.3)
  route-baseline REPORT ...                  rule 5.3: run CR-AB-ROUTE, or record the skip
  gate COMB [--comparator COMP]              rule 6.8: the v0.6 crawler gate
  checkpoint1 --utp E --adm E ...            rule 6.9: the Checkpoint 1 projection

`effect`, `gate` and `checkpoint1` read the JSON reports `segment` and `effect` write.
Standard library only.
"""

import argparse
from datetime import datetime, timedelta, timezone
import json
import math
from pathlib import Path
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent / "linux"))
import collect  # noqa: E402  (one source for label enums, switches and D6's open rule)

SPEC = "docs/decisions/2026-11-02-CR-SPEC.md"
KINDS = collect.SPEC_KINDS
HOUR = 3600
WARMUP = 3600                  # rule 1.1
T_OPEN_LIMIT = 7 * 3600        # rule 1.1
BOUNDARY_LATE = 90             # rule 1.4
MAX_GAP = 180                  # rule 6.6
EDGES = tuple(int(e) for e in collect.LATENCY_EDGES if e != "+Inf")
MARGIN = 2                     # rule 3.2
DROPPED_CARRY = 7200           # rule 3.2
COVERAGE_MIN = 0.98            # rule 4.2
P95_SECONDS = 600              # rule 4.3 (owner-set: 10 min)
RECONCILE = 0.02               # rule 4.5
PERMIT_STRETCH = 3600          # rule 5.1
NTP_OFFSET = 1.0               # D1a
DRIFT = 5.0                    # D1a
SWITCH_SECONDS = 600           # rule 2.3
COMPARATOR_CLEAR = 12 * 3600   # rule 2.2
GATE_MEAN, GATE_HOUR = 100.0, 70.0  # owner-set (§2.25)
EFFECT_MARGIN = math.log(1.05)      # rule 6.3: +/- 5 %
OTHER_SWITCHES = ("coordinated_admission", "responder_hints", "streaming_peers", "target_prefixes",
                  "utp", "route_backoff")
ADMISSION_COUNTERS = ("fetch_admission_waits_total", "fetch_admission_wait_cancelled_total",
                      "fetch_admission_wakeups_total", "fetch_admission_grants_total",
                      "fetch_admission_refusals_total", "fetch_admission_holds_total",
                      "fetch_admission_permit_returns_total")
ADMISSION = ADMISSION_COUNTERS + ("fetch_admission_waiters", "fetch_admission_permits_in_use",
                                  "dht_peer_permit_returns_total")
CORE = ("writer_stored_total", "writer_errors_total", "fetch_attempts_total", "fetch_success_total",
        "dht_peer_admitted_total", "dht_peer_rejected_total", "dht_lookups_completed_total")
# Rule 5.2's guardrails (and rule 6.4's report of them).
GUARDRAILS = (("utp_retransmits_total", None), ("utp_connection_timeouts_total", None),
              ("dht_timeouts_total", None), ("dht_throttled_total", None),
              ("dht_sample_deferred_total", None),
              ("dht_peer_rejection_reasons_total", {"reason": "capacity"}),
              ("dht_peer_admitted_total", None), ("dht_peer_rejected_total", None),
              ("fetch_admission_refusals_total", None), ("fetch_connect_timeouts_total", None))
# Rule 7.1's published breakdowns, by their one label.
BREAKDOWNS = ("metadata_excluded_total", "fetch_lost_total", "fetch_failures_total",
              "fetch_attempts_total", "fetch_success_total", "fetch_connect_timeouts_total",
              "fetch_family_attempts_total", "fetch_family_success_total", "fetch_no_route_total",
              "fetch_route_probe_down_total", "fetch_route_skipped_total")
SERIES = re.compile(r"^lodestar_([a-z_0-9]+)(\{[^{}\r\n]*\})? ([0-9]+(?:\.[0-9]+)?)$")
LABEL = re.compile(r'([a-z_]+)="([A-Za-z0-9_+]*)"')


# ----------------------------------------------------------------- loading
def read_json(path):
    with path.open(encoding="utf-8") as stream:
        value = json.load(stream)
    if not isinstance(value, dict):
        raise ValueError(f"Expected an object in {path.name}")
    return value


def number(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value)


def parse_snapshot(text):
    """{name: {((key, value), ...): value}} from a sanitized snapshot; any malformed
    or duplicated line rejects the whole snapshot."""
    out = {}
    for line in text.splitlines():
        if not line:
            continue
        match = SERIES.fullmatch(line)
        if not match:
            raise ValueError("Malformed snapshot line")
        name, labels, value = match.groups()
        key = tuple(LABEL.findall(labels)) if labels else ()
        if labels and "{" + ",".join(f'{k}="{v}"' for k, v in key) + "}" != labels:
            raise ValueError("Malformed labels")
        series = out.setdefault(name, {})
        if key in series:
            raise ValueError("Duplicate series")
        series[key] = float(value)
    return out


def get(snap, name, **labels):
    """Sum of `name`'s series whose labels include `labels`; None if none match."""
    series = (snap or {}).get(name)
    if not series:
        return None
    hits = [v for k, v in series.items() if all((key, val) in k for key, val in labels.items())]
    return sum(hits) if hits else None


def parse_utc(text):
    return datetime.fromisoformat(str(text).replace("Z", "+00:00"))


def load_rows(root, kind):
    lines = (root / "samples.jsonl").read_bytes().decode("utf-8").splitlines(True)
    rows = []
    for index, line in enumerate(lines):
        try:
            row = json.loads(line)
        except json.JSONDecodeError:
            if index == len(lines) - 1 and not line.endswith("\n"):
                break  # a tail the collector was still writing
            raise ValueError(f"Malformed samples.jsonl line {index + 1}") from None
        if (not isinstance(row, dict) or row.get("sequence") != index or not number(row.get("elapsed_seconds"))
                or not isinstance(row.get("values"), dict)):
            raise ValueError(f"Invalid sample at line {index + 1}")
        scrape = {"e": float(row["elapsed_seconds"]), "utc": parse_utc(row["time"]), "row": row,
                  "values": row["values"], "snap": None, "ok": row.get("metrics_ok") is True}
        if scrape["ok"] and kind != "comparator":
            try:
                scrape["snap"] = parse_snapshot((root / "metrics" / f"{index:05d}.prom").read_text(encoding="utf-8"))
            except (OSError, ValueError):
                scrape["ok"] = False  # missing evidence, never a partial read
        if scrape["ok"] and kind == "comparator" and not number(row["values"].get("comparator_torrents")):
            scrape["ok"] = False
        rows.append(scrape)
    return rows


def load_activity(root):
    path = root / "activity.jsonl"
    if not path.exists():
        return []
    out = []
    for line in path.read_text(encoding="utf-8").splitlines():
        try:
            entry = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(entry, dict) and number(entry.get("elapsed_seconds")):
            out.append(entry)
    return out


# ----------------------------------------------------------------- checks
class Checks:
    """Each rule's check, its result and its effect on the verdict (D7)."""

    def __init__(self):
        self.items = []

    def add(self, rule, name, ok, effect="invalid", detail=None):
        item = {"rule": rule, "check": name, "ok": bool(ok), "effect": effect}
        if detail is not None:
            item["detail"] = detail
        self.items.append(item)
        return ok

    def verdict(self):
        bad = [c for c in self.items if not c["ok"]]
        invalid = [c["check"] for c in bad if c["effect"] in ("invalid", "missing")]
        void = [c["check"] for c in bad if c["effect"] == "void"]
        if invalid:
            return "invalid", sorted(set(invalid))
        if void:
            return "void", sorted(set(void))
        return "valid", []


def window_params(kind, settings):
    """Rule 1.3's lengths; a rehearsal may shorten them, and says so."""
    hours, hour_s, warm = collect.SPEC_HOURS[kind], HOUR, WARMUP
    if settings.get("rehearsal") is True:
        hours = settings.get("baseline_hours", hours)
        hour_s = settings.get("hour_seconds", hour_s)
        warm = settings.get("warmup_minutes", 60) * 60
        if not (number(hours) and hours == int(hours) and hours >= 1 and number(hour_s) and hour_s >= 60
                and number(warm) and warm >= 0):
            raise ValueError("Invalid rehearsal window")
    return int(hours), float(hour_s), float(warm)


def suppression_reason(snap):
    series = (snap or {}).get("dht_sample_suppression_reason") or {}
    on = [dict(k).get("reason") for k, v in series.items() if v == 1]
    return on[0] if len(on) == 1 else None


def required_series(kind, treatment, switches):
    """Rule 6.10, as (name, labels) pairs every window scrape must carry."""
    if kind == "comparator":
        return []
    req = [("searchable_new_total", {"origin": "network"}), ("metadata_new_verified_total", {"origin": "network"})]
    req += [("discovery_to_searchable_seconds_bucket", {"le": e}) for e in collect.LATENCY_EDGES]
    req += [("discovery_to_searchable_seconds_count", {}), ("discovery_to_searchable_dropped_total", {})]
    req += [(n, {}) for n in ("metadata_excluded_total", "fetch_failures_total", "fetch_lost_total")]
    req += [("fetch_attempts_total", {"transport": "tcp"})]
    req += [(n, {}) for n in ("fetch_success_total", "fetch_family_attempts_total", "fetch_family_success_total",
                              "fetch_no_route_total", "dht_sample_suppression_seconds",
                              "dht_sample_suppression_reason", "dht_peer_active", "dht_peer_queued",
                              "fetch_lookups") + CORE]
    if treatment == "adm" or switches.get("coordinated_admission"):
        req += [(n, {}) for n in ADMISSION]
    if switches.get("utp"):
        req += [("dht_utp_in_total", {}), ("fetch_attempts_total", {"transport": "utp"}),
                ("utp_retransmits_total", {}), ("utp_connection_timeouts_total", {})]
    else:
        req += [("dht_utp_in_total", {})]
    if treatment == "route" or switches.get("route_backoff"):
        req += [("fetch_route_skipped_total", {}), ("fetch_route_probe_down_total", {})]
    return req


# ----------------------------------------------------------------- latency and carry-in
def carry_in_bound(a, b, a_i):
    """Rule 3.2 for one scrape interval: Δcount − Δbucket{le=e_i} + Δdropped (a_i < 7200 s)."""
    edge = max((x for x in EDGES if x <= a_i - MARGIN), default=None)
    bound = delta(a, b, "discovery_to_searchable_seconds_count")
    if edge is not None:
        bound -= delta(a, b, "discovery_to_searchable_seconds_bucket", le=str(edge))
    if a_i < DROPPED_CARRY:
        bound += delta(a, b, "discovery_to_searchable_dropped_total")
    return bound


def delta(a, b, name, **labels):
    left, right = get(a["snap"], name, **labels), get(b["snap"], name, **labels)
    if left is None and right is None:
        return 0.0
    if left is None or right is None:
        raise KeyError(name)
    return right - left


def latency(a, b):
    """Rule 4 over the scrapes a..b: bucket deltas, coverage, the pass test and the p95."""
    buckets = {e: delta(a, b, "discovery_to_searchable_seconds_bucket", le=str(e)) for e in EDGES}
    count = delta(a, b, "discovery_to_searchable_seconds_count")
    dropped = delta(a, b, "discovery_to_searchable_dropped_total")
    total = count + dropped
    coverage = count / total if total > 0 else None
    measured = coverage is not None and coverage >= COVERAGE_MIN
    p95 = None
    if measured:
        p95 = next((e for e in EDGES if buckets[e] >= 0.95 * total), "+Inf")
    return {"bucket_deltas": {str(e): buckets[e] for e in EDGES}, "count_delta": count,
            "dropped_delta": dropped, "coverage": coverage, "measured": measured,
            "p95_seconds": p95,
            "passes_p95_600": bool(measured and buckets[P95_SECONDS] >= 0.95 * total)}


def breakdown(a, b, name):
    out = {}
    for key in sorted(((b["snap"] or {}).get(name) or {})):
        if key not in ((a["snap"] or {}).get(name) or {}):
            raise KeyError(name)
        labels = dict(key)
        if len(labels) != 1:
            continue
        (label, value), = labels.items()
        schema = dict(collect.LABELLED.get(name, ()))
        if label in schema and value in schema[label]:
            out[value] = delta(a, b, name, **{label: value})
    return out


# ----------------------------------------------------------------- one segment
def analyze_segment(root):
    root = Path(root)
    run = read_json(root / "run.json")
    settings = run.get("settings") or {}
    kind = settings.get("run_kind")
    if kind not in KINDS:
        raise ValueError("Not a CR-SPEC run: settings.run_kind is not ab, comb or comparator")
    treatment = settings.get("treatment")
    result = read_json(root / "result.json") if (root / "result.json").exists() else None
    rows = load_rows(root, kind)
    hours, hour_s, warm = window_params(kind, settings)
    rehearsal = settings.get("rehearsal") is True
    report = {"schema": 1, "spec": SPEC, "kind": kind, "treatment": treatment, "rehearsal": rehearsal,
              "window_hours": hours, "hour_seconds": hour_s}
    if result is None:  # rule 6.11: nothing that shows a rate while the window may be open
        report.update(state="window_open", samples=len(rows),
                      message="The collector hasn't finished; rates are shown only after the window ends.")
        return report
    shutdown = result.get("shutdown") or {}
    report["clean_stop"] = (result.get("stop_mode") == "exited_0" and shutdown.get("exit_code") == 0
                            and shutdown.get("signal_delivered") is True and not shutdown.get("errors"))
    checks = Checks()
    ok = [r for r in rows if r["ok"]]
    config = run.get("config") if kind != "comparator" else None
    switches = (config or {}).get("switches") or {}
    report["identity"] = identity(run, config)
    if not ok:
        checks.add("6.6", "no_usable_scrape", False, "missing")
        return finish(report, checks)
    origin = rows[0]["utc"] - timedelta(seconds=rows[0]["e"])

    # D1a, rule 6.5: NTP at the start and UTC-to-elapsed drift.
    ntp = (run.get("clock") or {}).get("ntp_start") or {}
    checks.add("D1a", "ntp_synchronized", ntp.get("synchronized") is True and number(ntp.get("offset_seconds"))
               and abs(ntp["offset_seconds"]) <= NTP_OFFSET, detail={"source": ntp.get("source")})

    # Process start (D4) and T_open (D6).
    if kind == "comparator":
        try:
            start_unix = parse_utc(run["process_started_utc"]).timestamp()
        except (KeyError, ValueError):
            checks.add("7.2", "process_start_recorded", False, "missing")
            return finish(report, checks)
    else:
        start_unix = ok[0]["values"].get("start_time_seconds")
        if not number(start_unix):
            checks.add("6.6", "process_start_recorded", False, "missing")
            return finish(report, checks)
    p_e = start_unix - origin.timestamp()
    t_open = None
    if kind == "comparator":
        ws = p_e + warm
    else:
        t_open = next((r for r in ok if collect.open_eligible(
            {"dht_sample_suppression_seconds": r["values"].get("dht_sample_suppression_seconds"),
             "suppression_reason": suppression_reason(r["snap"])}, r["utc"].timestamp() - start_unix)), None)
        if not checks.add("1.1", "t_open_within_7h", t_open is not None and t_open["e"] - p_e <= T_OPEN_LIMIT):
            report["timeline"] = {"process_start_elapsed": p_e}
            return finish(report, checks)
        ws = t_open["e"] + warm

    # Rule 1.4: boundary scrapes.
    bounds, late = [], []
    for k in range(hours + 1):
        b = ws + k * hour_s
        scrape = next((r for r in ok if r["e"] >= b), None)
        if scrape is None:
            break
        if scrape["e"] - b > BOUNDARY_LATE:
            late.append(k)
        bounds.append(scrape)
    complete = len(bounds) == hours + 1
    checks.add("1.3", "window_complete", complete, "missing", {"boundaries_found": len(bounds)})
    checks.add("1.4", "boundary_scrapes_on_time", not late, "missing", {"late_hours": late} if late else None)
    end_e = bounds[-1]["e"] if complete else rows[-1]["e"]
    span = [r for r in rows if r["e"] <= end_e]
    span_ok = [r for r in span if r["ok"]]
    local_offset = run.get("utc_offset_seconds") if number(run.get("utc_offset_seconds")) else 0
    report["timeline"] = {
        "process_start_elapsed": round(p_e, 1),
        "t_open_elapsed": round(t_open["e"], 1) if t_open else None,
        "ws_elapsed": round(ws, 1),
        "t_s_elapsed": bounds[0]["e"] if bounds else None,
        "end_elapsed": end_e if complete else None,
        "host_local_start_hour": (origin + timedelta(seconds=p_e + local_offset)).hour,
        "process_start_utc": iso(origin + timedelta(seconds=p_e)),
        "window_start_utc": iso(origin + timedelta(seconds=bounds[0]["e"])) if bounds else None,
        "window_end_utc": iso(origin + timedelta(seconds=end_e)) if complete else None,
        "last_scrape_utc": iso(rows[-1]["utc"]),
    }

    # Rule 6.6: gaps from process start to the end, failed scrapes and series in the window.
    marks = [p_e] + [r["e"] for r in span]
    gaps = [b - a for a, b in zip(marks, marks[1:])]
    checks.add("6.6", "scrape_gaps_within_180s", all(g <= MAX_GAP for g in gaps), "missing",
               {"max_gap_seconds": round(max(gaps), 1) if gaps else None})
    window = [r for r in span if bounds and r["e"] >= bounds[0]["e"]]
    checks.add("6.6", "no_failed_scrape_in_window", all(r["ok"] for r in window), "missing",
               {"failed": sum(not r["ok"] for r in window)})
    checks.add("6.6", "snapshots_not_capped", not any(r["row"].get("series_capped") for r in span), "missing")
    req = required_series(kind, treatment, switches)
    absent = sorted({n + (str(lbl) if lbl else "") for r in window if r["ok"] for n, lbl in req
                     if get(r["snap"], n, **lbl) is None})
    resources_absent = [r["e"] for r in window if r["ok"] and not (
        (number(r["row"].get("private_bytes")) or number(r["row"].get("working_set_bytes")))
        and number(r["row"].get("cpu_seconds")))]
    checks.add("6.10", "required_series_present", not absent and not resources_absent, "missing",
               {"absent": absent[:20], "scrapes_without_memory_or_cpu": len(resources_absent)} if absent or resources_absent else None)

    # Rule 6.5: interruptions, identity changes and counter resets.
    identities = {(r["row"].get("process_identity"), r["values"].get("start_time_seconds")) for r in span_ok}
    checks.add("6.5", "one_process_identity", len(identities) == 1)
    resets = counter_resets(span_ok, kind)
    checks.add("6.5", "no_counter_reset", not resets, detail={"series": resets[:10]} if resets else None)
    exited = result.get("reason", "").startswith("process_exited") and not complete
    checks.add("6.5", "no_interruption", not exited and "identity_change" not in result)

    # D1a: drift between UTC and elapsed over the segment.
    drift = max((abs((r["utc"] - span[0]["utc"]).total_seconds() - (r["e"] - span[0]["e"])) for r in span), default=0)
    checks.add("D1a", "utc_elapsed_drift_within_5s", drift <= DRIFT, detail={"max_drift_seconds": round(drift, 3)})

    # Rule 6.11: host activity, written at the time.
    activity = load_activity(root)
    counted = [a for a in activity if a.get("category") in collect.ACTIVITY_CATEGORIES and a.get("same_boot") is True
               and not a.get("collector_finished") and p_e <= a["elapsed_seconds"] <= end_e]
    notes = [a for a in activity if a not in counted]
    checks.add("6.11", "no_host_activity", not counted,
               detail={"entries": [{"category": a["category"], "elapsed_seconds": a["elapsed_seconds"]} for a in counted]}
               if counted else None)
    report["activity_notes"] = [{"category": a.get("category") if a.get("category") in collect.ACTIVITY_CATEGORIES
                                 else "other", "elapsed_seconds": a["elapsed_seconds"]} for a in notes]

    # Rule 6.5: the public IP at t_s, every boundary and the end.
    ip_rows = [r for r in span if "public_ip" in r["row"]]
    changed = any(r["row"]["public_ip"] == "changed" for r in ip_rows)
    has_reference = any(r["row"]["public_ip"] == "reference" for r in ip_rows if bounds and r["e"] < bounds[0]["e"])
    unchecked = [k for k in range(len(bounds)) if not any(
        r["row"]["public_ip"] == "unchanged" and ws + k * hour_s <= r["e"] <= ws + k * hour_s + BOUNDARY_LATE
        for r in ip_rows)]
    checks.add("6.5", "public_ip_unchanged", not changed)
    checks.add("6.5", "public_ip_checked_each_boundary", has_reference and not unchecked, "missing",
               {"unchecked_hours": unchecked} if unchecked else None)

    # Rule 6.5: the host checklist; rule 2.3: the witness.
    host_checklist(checks, run, kind, switches, config)

    if kind == "comparator":
        comparator_checks(checks, run, ok, bounds, complete)
    else:
        lodestar_checks(checks, run, config, span, span_ok, bounds, complete, treatment, kind)

    if complete:
        try:
            report.update(measure(kind, ok, bounds, t_open, span_ok, p_e, switches, checks, run))
        except KeyError as error:  # a series absent at one end of a delta
            checks.add("6.10", "series_present_at_both_ends", False, "missing", {"series": str(error).strip("'")})
    return finish(report, checks)


def counter_resets(scrapes, kind):
    out = set()
    for a, b in zip(scrapes, scrapes[1:]):
        if kind == "comparator":
            continue
        for name, series in a["snap"].items():
            if not name.endswith(("_total", "_bucket", "_count", "_sum")):
                continue
            after = b["snap"].get(name) or {}
            for key, value in series.items():
                if key not in after or after[key] < value:
                    out.add(name)
    return sorted(out)


def host_checklist(checks, run, kind, switches, config):
    items = run.get("checklist") or {}
    docker = (run.get("settings") or {}).get("mode") == "docker"
    missing = [i for i in collect.CHECKLIST_ITEMS if i not in items
               or (items[i].get("state") == "excluded" and not items[i].get("next_run_utc"))
               or (docker and items[i].get("state") != "excluded")]
    checks.add("6.5", "host_checklist_complete", not missing, detail={"missing": missing} if missing else None)
    if kind == "ab" or switches.get("safe_restart"):
        witness = (config or {}).get("witness") or {}
        checks.add("2.3", "witness_host_mounted_outside_data_dir",
                   witness.get("host_mounted") is True and witness.get("outside_data_dir") is True)


def lodestar_checks(checks, run, config, span, span_ok, bounds, complete, treatment, kind):
    """Rules 5.5, 6.1, 6.6 (identity) and 7.2 for a Lodestar segment."""
    switches = (config or {}).get("switches") or {}
    api = (config or {}).get("settings_api") or {}
    switch_ok = set(switches) == set(collect.SWITCH_ENV) and all(isinstance(v, bool) for v in switches.values())
    digest = run.get("image_digest") or run.get("sha256")
    spec_commit = (run.get("settings") or {}).get("spec_commit")
    have = bool(config) and switch_ok and api.get("intensity") in collect.PRESETS and bool(digest) \
        and bool(run.get("commit")) and bool(spec_commit) and config.get("env_preset") is not None
    checks.add("6.6", "configuration_identity_recorded", have, "missing")
    if not have:
        return
    preset = api["intensity"]
    agrees = config["env_preset"] in ("unset", preset)
    checks.add("7.2", "env_and_settings_preset_agree", agrees, "missing")
    checks.add("5.5", "balanced_preset", preset == "balanced", "void")
    if kind == "ab":
        checks.add("2.3", "safe_restart_on", switches["safe_restart"] is True)
        others = [s for s in OTHER_SWITCHES if s != collect.TREATMENTS[treatment] and switches[s]]
        checks.add("6.1", "other_switches_off", not others, detail={"on": others} if others else None)
    # Boundary API checks: the settings at each boundary, imports and pauses since the start.
    calls = [r["row"].get("api") for r in span if r["row"].get("boundary")]
    settings_seen = [c.get("settings") for c in calls if isinstance(c, dict) and isinstance(c.get("settings"), dict)]
    every = complete and len(settings_seen) == len(calls) and len(calls) >= 1 and \
        sum(len(r["row"]["boundary"]) for r in span if r["row"].get("boundary")) == len(bounds)
    checks.add("5.5", "settings_checked_each_boundary", every, "missing")
    checks.add("5.5", "preset_stayed_balanced", all(s.get("intensity") == "balanced" for s in settings_seen), "void")
    checks.add("6.5", "crawler_never_paused", all(s.get("paused") is False for s in settings_seen))
    final = calls[-1] if calls and complete else None
    imports = final.get("imports") if isinstance(final, dict) else None
    start_unix = span_ok[0]["values"].get("start_time_seconds")
    end_unix = (bounds[-1]["utc"].timestamp() if complete else None)
    covered = (isinstance(imports, dict) and not imports.get("truncated")
               and number(imports.get("from_unix")) and number(start_unix) and imports["from_unix"] <= start_unix
               and end_unix is not None and imports.get("to_unix", 0) >= end_unix)
    if checks.add("5.5", "import_annotations_checked", covered, "missing"):
        checks.add("5.5", "no_import_job", imports["import_started"] == 0 and imports["import_finished"] == 0, "void")
        checks.add("5.5", "no_preset_change", imports["intensity_changes"] == 0, "void")
        checks.add("6.5", "no_pause_annotation", imports["paused"] == 0)
    rendered = any(get(r["snap"], "metadata_new_verified_total", origin="import") is not None for r in span_ok)
    checks.add("5.5", "import_origin_not_rendered", not rendered, "void")


def comparator_checks(checks, run, ok, bounds, complete):
    """Rule 5.4 (rule 2.2's spacing is checked across runs, in `gate` and `checkpoint1`)."""
    checks.add("5.4", "image_pinned_by_digest", bool(run.get("image_digest")), "void")
    checks.add("5.4", "store_empty_at_start", ok[0]["values"]["comparator_torrents"] == 0, "void")
    if complete:
        flat = [k for k in range(len(bounds) - 1)
                if bounds[k + 1]["values"]["comparator_torrents"] <= bounds[k]["values"]["comparator_torrents"]]
        checks.add("5.4", "count_rises_every_hour", not flat, "void", {"flat_hours": flat} if flat else None)
    checks.add("6.6", "comparator_identity_recorded", bool((run.get("comparator") or {}).get("query")), "missing")


def measure(kind, ok, bounds, t_open, span_ok, p_e, switches, checks, run):
    """Rules 1.5-1.7, 3, 4, 5.1-5.3 and 7.1 over a complete window."""
    t_s, end = bounds[0], bounds[-1]
    window = [r for r in ok if t_s["e"] <= r["e"] <= end["e"]]
    out = {}
    if kind == "comparator":
        hourly = []
        for k, (a, b) in enumerate(zip(bounds, bounds[1:])):
            minutes = (b["e"] - a["e"]) / 60
            gain = b["values"]["comparator_torrents"] - a["values"]["comparator_torrents"]
            hourly.append({"hour": k, "minutes": round(minutes, 3), "stored": gain, "rate_per_min": gain / minutes})
        minutes = (end["e"] - t_s["e"]) / 60
        gain = end["values"]["comparator_torrents"] - t_s["values"]["comparator_torrents"]
        out["numerator"] = {"source": "comparator_count", "query": (run.get("comparator") or {}).get("query"),
                            "window": {"stored": gain, "minutes": round(minutes, 3), "mean_per_min": gain / minutes},
                            "hourly": hourly,
                            "worst_hour_per_min": min(h["rate_per_min"] for h in hourly)}
        out["resources"] = resources(window)
        return out
    # Rule 3.2: the carry-in bound per scrape interval, summed per hour.
    bound_by_hour = [0.0] * (len(bounds) - 1)
    k = 0
    for a, b in zip(window, window[1:]):
        while b["e"] > bounds[k + 1]["e"]:
            k += 1
        bound_by_hour[k] += carry_in_bound(a, b, math.floor(a["e"] - t_s["e"]))
    hourly = []
    for k, (a, b) in enumerate(zip(bounds, bounds[1:])):
        minutes = (b["e"] - a["e"]) / 60
        gross = delta(a, b, "searchable_new_total", origin="network")
        net = gross - bound_by_hour[k]
        lat = latency(a, b)
        hourly.append({"hour": k, "minutes": round(minutes, 3), "gross": gross, "carry_in_bound": bound_by_hour[k],
                       "carry_in_share": bound_by_hour[k] / gross if gross else None, "net": net,
                       "net_per_min": net / minutes, "gross_per_min": gross / minutes,
                       "stored_per_min": delta(a, b, "metadata_new_verified_total", origin="network") / minutes,
                       "latency_coverage": lat["coverage"], "latency_bucket_deltas": lat["bucket_deltas"],
                       "dropped_delta": lat["dropped_delta"],
                       "breakdowns": {n: breakdown(a, b, n) for n in BREAKDOWNS},
                       "guardrails": guardrails(a, b)})
    minutes = (end["e"] - t_s["e"]) / 60
    gross = delta(t_s, end, "searchable_new_total", origin="network")
    bound = sum(bound_by_hour)
    out["numerator"] = {
        "series": 'searchable_new_total{origin="network"}',
        "window": {"gross": gross, "carry_in_bound": bound, "carry_in_share": bound / gross if gross else None,
                   "net": gross - bound, "minutes": round(minutes, 3), "mean_net_per_min": (gross - bound) / minutes,
                   "mean_gross_per_min": gross / minutes,
                   "stored_per_min": delta(t_s, end, "metadata_new_verified_total", origin="network") / minutes},
        "hourly": hourly,
        "worst_hour_net_per_min": min(h["net_per_min"] for h in hourly),
    }
    lat = latency(t_s, end)
    out["latency"] = lat
    total = lat["count_delta"] + lat["dropped_delta"]
    reconciled = (total == 0) if gross == 0 else abs(total - gross) <= RECONCILE * gross
    checks.add("4.5", "latency_reconciles_with_numerator", reconciled, "missing",
               {"samples_plus_dropped": total, "gross": gross})
    attempts = delta(t_s, end, "fetch_attempts_total")
    out["fetch"] = {"attempts": attempts, "successes": delta(t_s, end, "fetch_success_total"),
                    "success_per_attempt": delta(t_s, end, "fetch_success_total") / attempts if attempts else None,
                    "connects_per_second": attempts / (end["e"] - t_s["e"])}
    out["breakdowns"] = {n: breakdown(t_s, end, n) for n in BREAKDOWNS}
    out["guardrails"] = guardrails(t_s, end)
    out["mechanism"] = mechanism(checks, window, span_ok, t_s, end, switches)
    out["resources"] = resources(window)
    # Rule 1.7: the startup period and the warmup, with their own rates.
    first = span_ok[0]
    out["startup"] = {
        "suppression_reason_at_start": suppression_reason(first["snap"]),
        "suppression_reason_at_open": suppression_reason(t_open["snap"]),
        "suppressed_seconds": round(t_open["e"] - p_e, 1),
        "suppressed_gross_per_min": rate(first, t_open, "searchable_new_total", origin="network"),
        "warmup_seconds": round(t_s["e"] - t_open["e"], 1),
        "warmup_gross_per_min": rate(t_open, t_s, "searchable_new_total", origin="network"),
    }
    # Rule 3.4: the backlog at the end scrape.
    snap = end["snap"]
    verified = get(snap, "metadata_new_verified_total", origin="network") or 0
    out["end_backlog"] = {
        "stored_not_searchable": verified - (get(snap, "searchable_new_total", origin="network") or 0),
        "fetch_admission_waiters": get(snap, "fetch_admission_waiters"),
        "fetch_lookups": get(snap, "fetch_lookups"),
        "dht_peer_active_plus_queued": (get(snap, "dht_peer_active") or 0) + (get(snap, "dht_peer_queued") or 0),
    }
    out["already_stored_and_rediscovered"] = {
        r: delta(t_s, end, "metadata_excluded_total", reason=r) for r in ("already_stored", "rediscovered")}
    return out


def rate(a, b, name, **labels):
    seconds = b["e"] - a["e"]
    return delta(a, b, name, **labels) / (seconds / 60) if seconds > 0 else None


def guardrails(a, b):
    out = {}
    for name, labels in GUARDRAILS:
        if get(a["snap"], name, **(labels or {})) is None or get(b["snap"], name, **(labels or {})) is None:
            continue
        key = name + ("{" + ",".join(f'{k}="{v}"' for k, v in labels.items()) + "}" if labels else "")
        out[key] = delta(a, b, name, **(labels or {}))
    return out


def resources(window):
    rows = [r["row"] for r in window]
    seconds = window[-1]["e"] - window[0]["e"]
    out = {"peak_private_bytes": max((r["private_bytes"] for r in rows if number(r.get("private_bytes"))), default=None),
           "peak_working_set_bytes": max((r["working_set_bytes"] for r in rows if number(r.get("working_set_bytes"))),
                                         default=None)}
    if number(rows[0].get("cpu_seconds")) and number(rows[-1].get("cpu_seconds")) and seconds > 0:
        out["cpu_cores_mean"] = (rows[-1]["cpu_seconds"] - rows[0]["cpu_seconds"]) / seconds
    nets = [r.get("net") for r in rows]
    if all(isinstance(n, dict) for n in nets) and seconds > 0:
        first, last = nets[0], nets[-1]
        out["net"] = {"rx_bytes": last["rx_bytes"] - first["rx_bytes"], "tx_bytes": last["tx_bytes"] - first["tx_bytes"],
                      "packets_per_second": (last["rx_packets"] + last["tx_packets"]
                                             - first["rx_packets"] - first["tx_packets"]) / seconds,
                      "peak_sockets_used": max(n.get("sockets_used", 0) for n in nets)}
    else:
        out["net"] = None  # not available on this host: reported, not invented
    return out


def mechanism(checks, window, span_ok, t_s, end, switches):
    """Rules 5.1-5.3: a treatment that didn't take effect makes the segment void."""
    out = {}
    if switches.get("coordinated_admission") or get(t_s["snap"], "fetch_admission_grants_total") is not None:
        longest, since = 0.0, None
        for r in span_ok:
            s = r["snap"]
            parts = [get(s, "fetch_admission_grants_total"), get(s, "dht_peer_permit_returns_total"),
                     get(s, "fetch_admission_permits_in_use")]
            if any(p is None for p in parts):
                continue
            gap = parts[0] - parts[1] - parts[2] - (get(s, "fetch_admission_permit_returns_total", boundary="finder_full") or 0) \
                - (get(s, "fetch_admission_permit_returns_total", boundary="finder_closed") or 0)
            if gap != 0:
                since = r["e"] if since is None else since
                longest = max(longest, r["e"] - since)
            else:
                since = None
        grants = delta(t_s, end, "fetch_admission_grants_total")
        out["coordinated_admission"] = {"on": bool(switches.get("coordinated_admission")), "grants": grants,
                                        "longest_nonzero_permit_gap_seconds": longest,
                                        "refusals": delta(t_s, end, "fetch_admission_refusals_total"),
                                        "capacity_rejections": delta(t_s, end, "dht_peer_rejection_reasons_total",
                                                                     reason="capacity")}
        if switches.get("coordinated_admission"):
            checks.add("5.1", "admission_granted", grants > 0, "void")
            checks.add("5.1", "permit_identity_returns_to_zero_within_60min", longest < PERMIT_STRETCH, "void",
                       {"longest_seconds": longest})
        else:
            moved = [n for n in ADMISSION_COUNTERS if delta(t_s, end, n) != 0]
            held = any((get(r["snap"], "fetch_admission_permits_in_use") or 0) != 0 for r in window)
            checks.add("5.1", "admission_idle_when_off", not moved and not held, "void")
    utp_in = delta(t_s, end, "dht_utp_in_total")
    utp_attempts = delta(t_s, end, "fetch_attempts_total", transport="utp")
    out["utp"] = {"on": bool(switches.get("utp")), "utp_in": utp_in, "utp_attempts": utp_attempts}
    if switches.get("utp"):
        checks.add("5.2", "utp_in_use", utp_in > 0 and utp_attempts > 0, "void")
    else:
        rendered = any(get(r["snap"], n) is not None for r in window
                       for n in ("utp_retransmits_total", "utp_connection_timeouts_total"))
        checks.add("5.2", "utp_idle_when_off", utp_in == 0 and utp_attempts == 0 and not rendered, "void")
    if switches.get("route_backoff") or get(t_s["snap"], "fetch_route_skipped_total") is not None:
        skipped = delta(t_s, end, "fetch_route_skipped_total")
        probes = delta(t_s, end, "fetch_route_probe_down_total")
        out["route_backoff"] = {"on": bool(switches.get("route_backoff")), "skipped": skipped, "probe_down": probes}
        if switches.get("route_backoff"):
            checks.add("5.3", "no_opportunity", skipped > 0, "void")
        else:
            checks.add("5.3", "route_backoff_idle_when_off",
                       skipped == 0 and (switches.get("utp") or probes == 0), "void")
    out["no_route_ipv6"] = delta(t_s, end, "fetch_no_route_total", family="ipv6")
    return out


def identity(run, config):
    """Rule 7.2, publishable fields only."""
    settings = run.get("settings") or {}
    out = {"image_digest": run.get("image_digest"), "binary_sha256": run.get("sha256"),
           "source_commit": run.get("commit"), "spec": SPEC, "spec_commit": settings.get("spec_commit"),
           "network_mode": (run.get("network") or {}).get("mode"), "host_class": settings.get("mode"),
           "collector_sha256": run.get("collector_sha256")}
    if config:
        api = config.get("settings_api") or {}
        out.update(switches=config.get("switches"), preset=api.get("intensity"),
                   preset_source=api.get("intensity_source"),
                   witness_host_mounted=(config.get("witness") or {}).get("host_mounted"))
    return out


def finish(report, checks):
    verdict, reasons = checks.verdict()
    report.update(verdict=verdict, verdict_reasons=reasons, checks=checks.items)
    return report


def iso(stamp):
    return stamp.astimezone(timezone.utc).isoformat().replace("+00:00", "Z")


# ----------------------------------------------------------------- publication (rule 7)
IPV4 = re.compile(r"(?<![0-9.])(?:[0-9]{1,3}\.){3}[0-9]{1,3}(?![0-9.])")
IPV6 = re.compile(r"(?i)(?<![0-9a-f:])[0-9a-f]{1,4}(?::[0-9a-f]{0,4}){2,7}(?![0-9a-f:])")
HEX40 = re.compile(r"(?i)\b[0-9a-f]{40}\b")
HASH_KEYS = {"source_commit", "spec_commit", "commit"}


def publication_problems(value, key=None):
    """Rule 7.4: no addresses, no infohash-shaped strings outside commit fields."""
    problems = []
    if isinstance(value, dict):
        for k, v in value.items():
            problems += publication_problems(k, None) + publication_problems(v, k)
    elif isinstance(value, list):
        for v in value:
            problems += publication_problems(v, key)
    elif isinstance(value, str):
        if IPV4.search(value) or (IPV6.search(value) and not re.fullmatch(r"\d{4}-\d\d-\d\dT[\d:.]+Z", value)):
            problems.append(f"address-like text under {key}")
        if HEX40.search(value) and key not in HASH_KEYS:
            problems.append(f"infohash-shaped text under {key}")
    return problems


def publish(report, out_dir):
    problems = publication_problems(report)
    if problems:
        raise ValueError("Refusing to publish: " + "; ".join(sorted(set(problems))))
    out_dir = Path(out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    name = "segment" if "verdict" in report and "kind" in report else "report"
    with (out_dir / f"{name}.json").open("x", encoding="utf-8") as stream:
        stream.write(json.dumps(report, indent=2, allow_nan=False) + "\n")
    with (out_dir / f"{name}.md").open("x", encoding="utf-8") as stream:
        stream.write(markdown(report))


def markdown(report):
    def f(v, digits=2):
        return "n/a" if v is None else f"{v:,.{digits}f}" if isinstance(v, float) else str(v)
    lines = [f"# CR-SPEC {report.get('kind', '')} segment", "",
             f"- Spec: `{report.get('spec')}`" + (" (rehearsal: not gate evidence)" if report.get("rehearsal") else ""),
             f"- Verdict: **{report.get('verdict', report.get('state'))}**"
             + (f" ({', '.join(report['verdict_reasons'])})" if report.get("verdict_reasons") else ""), ""]
    num = report.get("numerator")
    if num:
        w = num["window"]
        lines += ["## Numerator", "",
                  f"Window mean (net): {f(w.get('mean_net_per_min', w.get('mean_per_min')))}/min over {f(w['minutes'])} min; "
                  f"worst hour {f(num.get('worst_hour_net_per_min', num.get('worst_hour_per_min')))}/min.", "",
                  "| Hour | Minutes | Gross | Carry-in bound | Net/min |", "| ---: | ---: | ---: | ---: | ---: |"]
        for h in num["hourly"]:
            lines.append(f"| {h['hour']} | {f(h['minutes'])} | {f(h.get('gross', h.get('stored')))} | "
                         f"{f(h.get('carry_in_bound'))} | {f(h.get('net_per_min', h.get('rate_per_min')))} |")
    lat = report.get("latency")
    if lat:
        lines += ["", "## Latency", "", f"Coverage {f(lat['coverage'], 4)}; p95 "
                  + (f"{lat['p95_seconds']} s" if lat["measured"] else "not measured")
                  + f"; passes ≤ 600 s: {str(lat['passes_p95_600']).lower()}."]
    lines += ["", "## Checks", "", "| Rule | Check | Result |", "| --- | --- | --- |"]
    for c in report.get("checks", []):
        lines.append(f"| {c['rule']} | {c['check']} | {'ok' if c['ok'] else c['effect']} |")
    return "\n".join(lines) + "\n"


# ----------------------------------------------------------------- statistics
def betacf(a, b, x):
    tiny, c, d = 1e-300, 1.0, 1.0 - (a + b) * x / (a + 1)
    d = 1 / (d if abs(d) > tiny else tiny)
    h = d
    for m in range(1, 300):
        for num in (m * (b - m) * x / ((a + 2 * m - 1) * (a + 2 * m)),
                    -(a + m) * (a + b + m) * x / ((a + 2 * m) * (a + 2 * m + 1))):
            d = 1 + num * d
            d = 1 / (d if abs(d) > tiny else tiny)
            c = 1 + num / c
            c = c if abs(c) > tiny else tiny
            h *= d * c
        if abs(d * c - 1) < 1e-15:
            break
    return h


def betai(a, b, x):
    if x <= 0:
        return 0.0
    if x >= 1:
        return 1.0
    front = math.exp(math.lgamma(a + b) - math.lgamma(a) - math.lgamma(b) + a * math.log(x) + b * math.log(1 - x))
    if x < (a + 1) / (a + b + 2):
        return front * betacf(a, b, x) / a
    return 1 - front * betacf(b, a, 1 - x) / b


def t_cdf(t, df):
    tail = 0.5 * betai(df / 2, 0.5, df / (df + t * t))
    return 1 - tail if t > 0 else tail


def t_quantile(p, df):
    """Student's t quantile by bisection (p > 0.5)."""
    lo, hi = 0.0, 1e4
    for _ in range(200):
        mid = (lo + hi) / 2
        if t_cdf(mid, df) < p:
            lo = mid
        else:
            hi = mid
    return (lo + hi) / 2


def ols(rows, y):
    """Least squares: coefficients, residual variance, (X'X)^-1 and df."""
    n, p = len(rows), len(rows[0])
    xtx = [[sum(r[i] * r[j] for r in rows) for j in range(p)] for i in range(p)]
    xty = [sum(r[i] * v for r, v in zip(rows, y)) for i in range(p)]
    inv = invert(xtx)
    beta = [sum(inv[i][j] * xty[j] for j in range(p)) for i in range(p)]
    df = n - p
    rss = sum((v - sum(b * x for b, x in zip(beta, r))) ** 2 for r, v in zip(rows, y))
    return beta, (rss / df if df > 0 else None), inv, df


def invert(m):
    n = len(m)
    a = [list(row) + [float(i == j) for j in range(n)] for i, row in enumerate(m)]
    for col in range(n):
        pivot = max(range(col, n), key=lambda r: abs(a[r][col]))
        if abs(a[pivot][col]) < 1e-12:
            raise ValueError("Singular design")
        a[col], a[pivot] = a[pivot], a[col]
        scale = a[col][col]
        a[col] = [v / scale for v in a[col]]
        for r in range(n):
            if r != col and a[r][col]:
                factor = a[r][col]
                a[r] = [v - factor * w for v, w in zip(a[r], a[col])]
    return [row[n:] for row in a]


# ----------------------------------------------------------------- blocks and effects (rules 6.1-6.4)
def segment_view(report):
    num = report.get("numerator") or {}
    w = num.get("window") or {}
    t = report.get("timeline") or {}
    return {"verdict": report.get("verdict"), "reasons": report.get("verdict_reasons", []),
            "switches": (report.get("identity") or {}).get("switches") or {},
            "digest": (report.get("identity") or {}).get("image_digest") or (report.get("identity") or {}).get("binary_sha256"),
            "net": w.get("mean_net_per_min"), "gross": w.get("mean_gross_per_min"), "stored": w.get("stored_per_min"),
            "success": (report.get("fetch") or {}).get("success_per_attempt"),
            "worst_hour": num.get("worst_hour_net_per_min"),
            "p95": (report.get("latency") or {}).get("p95_seconds") if (report.get("latency") or {}).get("measured") else None,
            "p95_measured": bool((report.get("latency") or {}).get("measured")),
            "start": t.get("window_start_utc"), "end": t.get("window_end_utc"),
            "process_start": t.get("process_start_utc"), "last_scrape": t.get("last_scrape_utc"),
            "start_hour": t.get("host_local_start_hour"),
            "reason_at_start": (report.get("startup") or {}).get("suppression_reason_at_start"),
            "clean_stop": report.get("clean_stop"), "guardrails": report.get("guardrails") or {},
            "minutes": w.get("minutes")}


def judge_block(reports, treatment):
    """Rules 2.3, 5, 6.1, 6.2's zero rule and 6.7 for one block of four segments."""
    switch = collect.TREATMENTS[treatment]
    segs = [segment_view(r) for r in reports]
    problems = []
    if len(reports) != 4 or any(r.get("kind") != "ab" or r.get("treatment") != treatment for r in reports):
        return {"valid": False, "problems": ["not_four_segments_of_this_treatment"], "segments": segs}
    arms = "".join("B" if s["switches"].get(switch) else "A" for s in segs)
    if arms not in ("ABBA", "BAAB"):
        problems.append("order_not_abba_or_baab")
    for s in segs:
        if s["verdict"] == "invalid" or (s["verdict"] == "void" and s["reasons"] != ["no_opportunity"]):
            problems.append("segment_" + s["verdict"])
    sets = {tuple(sorted((k, v) for k, v in s["switches"].items() if k != switch)) for s in segs}
    if len(sets) != 1:
        problems.append("switch_set_differs")
    if len({s["digest"] for s in segs}) != 1 or not segs[0]["digest"]:
        problems.append("image_digest_differs")
    for a, b in zip(segs, segs[1:]):  # rule 2.3: a clean stop, then a start within 10 minutes
        try:
            gap = (parse_utc(b["process_start"]) - parse_utc(a["last_scrape"])).total_seconds()
        except (TypeError, ValueError):
            gap = None
        if gap is None or gap > SWITCH_SECONDS or gap < 0:
            problems.append("arm_switch_over_10_minutes")
        if a["clean_stop"] is not True:
            problems.append("arm_switch_not_a_clean_stop")
    a_means = [s["net"] for s, arm in zip(segs, arms) if arm == "A" and s["verdict"] == "valid"]
    if any(m is None or m <= 0 for m in a_means):
        problems.append("control_did_not_crawl")
    loss = (not problems and all(m > 0 for m in a_means)
            and any(s["verdict"] == "valid" and s["net"] is not None and s["net"] <= 0
                    for s, arm in zip(segs, arms) if arm == "B"))
    return {"valid": not problems, "problems": sorted(set(problems)), "order": arms, "loss_without_fit": loss,
            "segments": segs, "host_local_start_hours": [s["start_hour"] for s in segs]}


def fit_effect(blocks, field, treatment_arm="B"):
    """Rule 6.2's fit of ln(field) on block intercepts, per-block trends and τ."""
    rows, y = [], []
    nb = len(blocks)
    for b, block in enumerate(blocks):
        mids = []
        for s in block["segments"]:
            mids.append((parse_utc(s["start"]).timestamp() + parse_utc(s["end"]).timestamp()) / 2 / 3600
                        if s["start"] and s["end"] else None)
        base = min((m for m in mids if m is not None), default=0.0)
        for s, arm, mid in zip(block["segments"], block["order"], mids):
            if s["verdict"] != "valid" or s[field] is None or s[field] <= 0:
                continue
            row = [0.0] * (2 * nb + 1)
            row[2 * b], row[2 * b + 1], row[-1] = 1.0, mid - base, float(arm == treatment_arm)
            rows.append(row)
            y.append(math.log(s[field]))
    if len(rows) <= 2 * nb + 1:
        return None
    try:
        beta, var, inv, df = ols(rows, y)
    except ValueError:
        return None
    if var is None:
        return None
    tau, se = beta[-1], math.sqrt(var * inv[-1][-1])
    q = t_quantile(0.975, df)
    return {"tau": tau, "se": se, "df": df, "t_quantile": q, "ci": [tau - q * se, tau + q * se],
            "effect": math.exp(tau) - 1, "effect_ci": [math.exp(tau - q * se) - 1, math.exp(tau + q * se) - 1],
            "segments": len(rows)}


def outcome(fit, valid_blocks):
    """Rule 6.3, first match wins."""
    if fit is None:
        return "not_estimable"
    lo, hi = fit["ci"]
    if lo > 0 and fit["effect"] >= 0.05:
        return "win"
    if -EFFECT_MARGIN <= lo and hi <= EFFECT_MARGIN:
        return "no_effect"
    if hi < 0:
        return "loss"
    return "inconclusive_run_blocks_3_and_4" if valid_blocks == 2 else "inconclusive_switch_stays_off"


def analyze_effect(treatment, block_reports):
    blocks = [judge_block(reports, treatment) for reports in block_reports]
    valid = [b for b in blocks if b["valid"]]
    out = {"schema": 1, "spec": SPEC, "treatment": treatment, "switch": collect.TREATMENTS[treatment],
           "blocks": blocks, "valid_blocks": len(valid)}
    orders = [b["order"] for b in valid]
    paired = len(valid) in (2, 4) and all(orders[i] == ("ABBA" if i % 2 == 0 else "BAAB") for i in range(len(valid)))
    rehearsal = any(r.get("rehearsal") for reports in block_reports for r in reports)
    out["rehearsal"] = rehearsal
    if treatment == "route":
        on = [s for b in valid for s, arm in zip(b["segments"], b["order"]) if arm == "B"]
        if on and all(s["verdict"] == "void" for s in on):
            out.update(outcome="no_opportunity", decided=True)
            return out
    if not paired:
        out.update(outcome="not_decidable", decided=False,
                   reason="needs 2 or 4 valid blocks, ABBA then BAAB in pairs (rules 6.1, 6.3)")
        return out
    if any(b["loss_without_fit"] for b in valid):
        out.update(outcome="loss", decided=True, reason="a treatment segment's net mean is 0 (rule 6.2)")
        return out
    fit = fit_effect(valid, "net")
    out.update(fit=fit, outcome=outcome(fit, len(valid)), decided=fit is not None)
    out["also_reported"] = {
        "gross": fit_effect(valid, "gross"), "stored": fit_effect(valid, "stored"),
        "fetch_success_per_attempt": fit_effect(valid, "success"),
        "p95_by_arm": {arm: [s["p95"] if s["p95_measured"] else "not_measured"
                             for b in valid for s, a in zip(b["segments"], b["order"]) if a == arm] for arm in "AB"},
        "worst_hour_by_arm": {arm: [s["worst_hour"] for b in valid for s, a in zip(b["segments"], b["order"]) if a == arm]
                              for arm in "AB"},
        "guardrails": guardrail_fits(valid),
    }
    out["control_segments"] = [{"net": s["net"], "p95": s["p95"], "p95_measured": s["p95_measured"]}
                               for b in valid for s, a in zip(b["segments"], b["order"]) if a == "A" and s["verdict"] == "valid"]
    out["treatment_segments"] = [{"net": s["net"], "p95": s["p95"], "p95_measured": s["p95_measured"]}
                                 for b in valid for s, a in zip(b["segments"], b["order"]) if a == "B" and s["verdict"] == "valid"]
    return out


def guardrail_fits(blocks):
    names = sorted({k for b in blocks for s in b["segments"] for k in s["guardrails"]})
    out = {}
    for name in names:
        for b in blocks:
            for s in b["segments"]:
                s["_g"] = (s["guardrails"].get(name) / s["minutes"]) if s["guardrails"].get(name) and s["minutes"] else None
        fit = fit_effect(blocks, "_g")
        out[name] = ({"effect": fit["effect"], "effect_ci": fit["effect_ci"],
                      "ci_excludes_zero": fit["ci"][0] > 0 or fit["ci"][1] < 0} if fit else "not_fitted")
    for b in blocks:
        for s in b["segments"]:
            s.pop("_g", None)
    return out


def route_baseline(reports):
    """Rule 5.3: CR-AB-ROUTE runs only if valid segments with route backoff and
    uTP off saw fetch_no_route_total{family="ipv6"} > 0."""
    used = [r for r in reports if r.get("verdict") == "valid" and r.get("kind") in ("ab", "comb")
            and not ((r.get("identity") or {}).get("switches") or {}).get("route_backoff")
            and not ((r.get("identity") or {}).get("switches") or {}).get("utp")]
    total = sum((r.get("mechanism") or {}).get("no_route_ipv6", 0) for r in used)
    return {"segments": len(used), "no_route_ipv6_sum": total,
            "decision": "run" if total > 0 else ("recorded_skip" if used else "no_baseline")}


def timelines(effect):
    """The segment timelines an effect report carries, for rule 2.2."""
    return [{"process_start_utc": s["process_start"], "window_start_utc": s["start"],
             "window_end_utc": s["end"], "last_scrape_utc": s["last_scrape"]}
            for b in effect.get("blocks") or [] for s in b.get("segments") or []]


def spacing(comparator, lodestar):
    """Rule 2.2: no Lodestar process from 12 h before the comparator's start to its
    window's end, and no Lodestar window starting within 12 h after it. `lodestar`
    holds segment timelines."""
    t = comparator.get("timeline") or {}
    try:
        start, end = parse_utc(t["process_start_utc"]), parse_utc(t["window_end_utc"])
    except (KeyError, TypeError, ValueError):
        return ["comparator_timeline_missing"]
    out = []
    for rt in lodestar:
        try:
            p0, w0 = parse_utc(rt["process_start_utc"]), parse_utc(rt["window_start_utc"])
            w1 = parse_utc(rt.get("window_end_utc") or rt["last_scrape_utc"])
        except (KeyError, TypeError, ValueError):
            out.append("lodestar_timeline_missing")
            continue
        if p0 < end and w1 > start - timedelta(seconds=COMPARATOR_CLEAR):
            out.append("lodestar_ran_within_12h_before_or_during_comparator")
        if end <= w0 < end + timedelta(seconds=COMPARATOR_CLEAR):
            out.append("lodestar_window_within_12h_after_comparator")
    return sorted(set(out))


def gate(comb, comparator=None):
    """Rule 6.8: one valid 24 h CR-COMB window."""
    out = {"schema": 1, "spec": SPEC, "rehearsal": bool(comb.get("rehearsal"))}
    reasons = []
    if comb.get("kind") != "comb" or comb.get("rehearsal") or comb.get("window_hours") != 24:
        reasons.append("not_a_24h_comb_window")
    if comb.get("verdict") != "valid":
        reasons.append("segment_" + str(comb.get("verdict")))
    num, lat = comb.get("numerator") or {}, comb.get("latency") or {}
    mean = (num.get("window") or {}).get("mean_net_per_min")
    hourly = [h["net_per_min"] for h in num.get("hourly") or []]
    mean_ok = mean is not None and mean >= GATE_MEAN
    hours_ok = len(hourly) == 24 and all(h >= GATE_HOUR for h in hourly)
    p95 = "not_measured" if not lat.get("measured") else ("pass" if lat.get("passes_p95_600") else "fail")
    out.update(mean_net_per_min=mean, mean_passes=mean_ok, worst_hour_net_per_min=min(hourly) if hourly else None,
               every_hour_passes=hours_ok, p95=p95, p95_seconds=lat.get("p95_seconds"),
               coverage=lat.get("coverage"))
    if comparator is not None:
        cnum = comparator.get("numerator") or {}
        out["comparator"] = {"verdict": comparator.get("verdict"),
                             "mean_per_min": (cnum.get("window") or {}).get("mean_per_min"),
                             "worst_hour_per_min": cnum.get("worst_hour_per_min"),
                             "spacing_problems": spacing(comparator, [comb.get("timeline") or {}])}
    passed = not reasons and mean_ok and hours_ok and p95 == "pass"
    out["gate"] = "pass" if passed else ("not_judged" if reasons else "fail")
    out["reasons"] = reasons
    return out


def cr0_worst_ratio(report):
    """CR-0 final's worst hour ÷ mean, from analyze_cr0.py's baseline (stored rate)."""
    hourly = (report.get("phases") or {}).get("baseline", {}).get("hourly") or {}
    cells = [v.get("writer_stored_total") for v in hourly.values()]
    cells = [c for c in cells if c and c.get("covered_seconds")]
    if not cells:
        return None
    mean = sum(c["delta"] for c in cells) / sum(c["covered_seconds"] for c in cells)
    worst = min(c["delta"] / c["covered_seconds"] for c in cells)
    return worst / mean if mean > 0 else None


def checkpoint1(utp, others, comparator, cr0):
    """Rule 6.9's projection, with both models and their intervals."""
    out = {"schema": 1, "spec": SPEC, "kind": "projection_not_measurement"}
    base = [s["net"] for s in utp.get("control_segments") or [] if s["net"] is not None]
    if len(base) < 2:
        out["error"] = "the uTP experiment has fewer than two valid control segments"
        return out
    n = len(base)
    r0 = sum(base) / n
    se = math.sqrt(sum((x - r0) ** 2 for x in base) / (n - 1) / n)
    q0 = t_quantile(0.975, n - 1)
    var_ln_r0 = (se / r0) ** 2
    out["base"] = {"R0": r0, "ci": [r0 - q0 * se, r0 + q0 * se], "n": n, "var_ln_R0": var_ln_r0}
    winners = [e for e in [utp] + list(others) if e.get("outcome") == "win" and e.get("fit")]
    gains = [{"treatment": e["treatment"], "g": math.exp(e["fit"]["tau"]),
              "ci": [math.exp(c) for c in e["fit"]["ci"]], "var": e["fit"]["se"] ** 2, "df": e["fit"]["df"]}
             for e in winners]
    q = max([q0] + [t_quantile(0.975, g["df"]) for g in gains])
    mult = r0 * math.prod(g["g"] for g in gains)
    sd_mult = math.sqrt(var_ln_r0 + sum(g["var"] for g in gains))
    add = r0 * (1 + sum(g["g"] - 1 for g in gains))
    sd_add = math.sqrt(add ** 2 * var_ln_r0 + sum((r0 * g["g"]) ** 2 * g["var"] for g in gains))
    m_ci = [mult * math.exp(-q * sd_mult), mult * math.exp(q * sd_mult)]
    a_ci = [add - q * sd_add, add + q * sd_add]
    projection = min(mult, add)
    out.update(gains=gains, t_quantile=q, multiplicative={"point": mult, "ci": m_ci},
               additive={"point": add, "ci": a_ci}, range=[min(m_ci[0], a_ci[0]), max(m_ci[1], a_ci[1])],
               projection=projection)
    cnum = (comparator or {}).get("numerator") or {}
    c_mean = (cnum.get("window") or {}).get("mean_per_min")
    ratios = {}
    if c_mean and cnum.get("worst_hour_per_min") is not None:
        ratios["comparator"] = cnum["worst_hour_per_min"] / c_mean
    if cr0 is not None and cr0_worst_ratio(cr0) is not None:
        ratios["cr0_final"] = cr0_worst_ratio(cr0)
    worst = projection * min(ratios.values()) if len(ratios) == 2 else None
    pool = ([s for e in winners for s in e.get("treatment_segments") or []] if winners
            else utp.get("control_segments") or [])
    if not pool or any(not s["p95_measured"] for s in pool):
        p95 = "not_projected"
    else:
        p95 = max((s["p95"] for s in pool), key=lambda v: math.inf if v == "+Inf" else v)
    out.update(worst_hour={"projection": worst, "ratios": ratios,
                           "assumption": "6 h arms don't bound a 24 h worst hour"},
               p95_seconds=p95, comparator_mean_per_min=c_mean,
               fraction_of_comparator=projection / c_mean if c_mean else None)
    on_track = (worst is not None and projection >= GATE_MEAN and worst >= GATE_HOUR
                and p95 not in ("not_projected", "+Inf") and p95 <= P95_SECONDS)
    out["on_track"] = on_track
    out["range_below_100"] = out["range"][0] < GATE_MEAN
    if comparator is not None:
        out["comparator_spacing_problems"] = spacing(
            comparator, [t for e in [utp] + list(others) for t in timelines(e)])
    return out


# ----------------------------------------------------------------- CLI
def load_report(path):
    return read_json(Path(path))


def write(report, output):
    text = json.dumps(report, indent=2, allow_nan=False) + "\n"
    if output:
        with Path(output).open("x", encoding="utf-8") as stream:
            stream.write(text)
    else:
        print(text, end="")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("segment")
    p.add_argument("root", type=Path)
    p.add_argument("--output", type=Path, help="New JSON report path, outside the run directory")
    p.add_argument("--publish", type=Path, help="New directory for the publishable bundle (rule 7)")
    p = sub.add_parser("effect")
    p.add_argument("--treatment", choices=sorted(collect.TREATMENTS), required=True)
    p.add_argument("--block", nargs=4, action="append", required=True, metavar="REPORT")
    p.add_argument("--output", type=Path)
    p = sub.add_parser("route-baseline")
    p.add_argument("reports", nargs="+")
    p = sub.add_parser("gate")
    p.add_argument("comb")
    p.add_argument("--comparator")
    p = sub.add_parser("checkpoint1")
    p.add_argument("--utp", required=True)
    p.add_argument("--adm", required=True)
    p.add_argument("--other", action="append", default=[])
    p.add_argument("--comparator", required=True)
    p.add_argument("--cr0-report", required=True, help="analyze_cr0.py --format json of CR-0 final")
    args = parser.parse_args(argv)
    try:
        if args.command == "segment":
            for path in (args.output, args.publish):
                if path and path.resolve().is_relative_to(args.root.resolve()):
                    parser.error("Reports go outside the run directory")
            report = analyze_segment(args.root)
            if args.publish:
                publish(report, args.publish)
            write(report, args.output)
        elif args.command == "effect":
            write(analyze_effect(args.treatment, [[load_report(p) for p in block] for block in args.block]),
                  args.output)
        elif args.command == "route-baseline":
            write(route_baseline([load_report(p) for p in args.reports]), None)
        elif args.command == "gate":
            write(gate(load_report(args.comb), load_report(args.comparator) if args.comparator else None), None)
        else:
            write(checkpoint1(load_report(args.utp), [load_report(args.adm)] + [load_report(p) for p in args.other],
                              load_report(args.comparator), load_report(args.cr0_report)), None)
    except (OSError, ValueError, KeyError) as error:
        print(f"Cannot analyze evidence: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
