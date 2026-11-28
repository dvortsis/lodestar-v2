#!/usr/bin/env python3
"""Judge crawler-run evidence by the frozen CR-SPEC rules.

docs/decisions/2026-11-02-CR-SPEC.md is the rulebook; rule numbers below are its.
Reads the Linux collector's evidence (linux/collect.py --run-kind ab|comb|comparator)
and never contacts the crawler.

  segment RUN [--output F] [--publish DIR] [--winners W]
                                             one segment: window, rates, checks, verdict
  effect --treatment T --block R R R R ... [--winners W] [--design D ... [--aa]]
                                             block checks and the effect fit (rules 6.1-6.3)
  route-baseline REPORT ...                  rule 5.3: run CR-AB-ROUTE, or record the skip
  gate COMB [--comparator COMP]              rule 6.8: the v0.6 crawler gate
  gate-round MANIFEST [--output F]           round 1: every arm's verdict and the selection
                                             (revised path to 100 §5.2 item 5; `segment
                                             --declaration D` judges a round-1 arm first)
  drop-c SAMPLES --arm A=DIR ...             round 1's rehearsal: §6.3's drop-C rule, keep or drop
  checkpoint1 --utp E --adm E ...            rule 6.9: the Checkpoint 1 projection

`effect`, `gate` and `checkpoint1` read the JSON reports `segment` and `effect` write.
From stage 2 (r5.1 A2, docs/decisions/2026-11-20-path-to-100-cr-spec.md) the control
has every earlier winner on: `--winners` names them in a JSON file, {"winners": [...]}.
`--design abba|baab`, one per `--block` (X-fleet-baab), is each block's recorded order: a
block whose switches give another order is invalid (`order_differs_from_design`). With
`--aa` the run is an A/A one: both arms ran the same switches, so the arms are the design
labels and the fit is reported as the noise floor, never as an effect.
Standard library only.
"""

import argparse
from datetime import datetime, timedelta, timezone
import hashlib
import json
import math
from pathlib import Path
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent / "linux"))
import collect  # noqa: E402  (one source for label enums, switches and D6's open rule)
sys.path.insert(0, str(Path(__file__).resolve().parent))
import analyze_cr0  # noqa: E402  (the memory gate's fit, one copy)

SPEC = "docs/decisions/2026-11-02-CR-SPEC.md"
ROUND1_SPEC = "docs/decisions/2026-11-27-CR-SPEC-round1.md"  # round 1's own spec (item L); SPEC stays for old runs
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
STOP_ALLOWANCE = 90            # the collector's stop path: up to 60 s, plus a sample's lag
GATE_MEAN, GATE_HOUR = 100.0, 70.0  # owner-set (§2.25)
NO_EFFECT = (math.log(0.95), math.log(1.05))  # rule 6.3: τ within [ln 0.95, ln 1.05]
# Every switch an A/B can turn on (safe_restart is rule 2.3's, on in every arm).
EXPERIMENT_SWITCHES = tuple(s for s in collect.SWITCH_ENV if s != "safe_restart")
# r5.1 A1: the lookup-cost pair's mechanism. Each preset's query rate is
# Intensity::queries_per_sec (crates/dht/src/node/mod.rs); pings to queriers spend their
# own 5 % share on top (VERIFY_SHARE, crates/dht/src/node/actor.rs), and both buckets
# hold one second's burst. An interval may also be up to a second long at each end
# against the scrape's own clock, so the bound is rate × 1.05 × (interval + 2 s).
PRESET_QUERY_RATE = {"low": 50, "balanced": 200, "aggressive": 1000}
VERIFY_SHARE = 0.05
RATE_SLACK_SECONDS = 2.0
A1_SLOTS = 32
LEDGER = collect.LEDGER
# Round 1 (docs/plan/2026-11-27-revised-path-to-100.md §5.2, §6, §8; X-cr-round1-gate).
# Each preset's caps: Intensity::max_outstanding and CR-1c's slots (crates/dht/src/node/mod.rs).
PRESET_OUTSTANDING = analyze_cr0.OUTSTANDING_RPCS
PRESET_SLOTS = {"low": 32, "balanced": 80, "aggressive": 256}
# §6.1's evidence table, the rows that are one series above 0. Proposed in the step-0 note
# (2026-11-27); the runner freezes the names in the CR-SPEC decisions file. A gauge row is
# read at every window scrape (its maximum); a counter row as the window's delta.
BUNDLE_COUNTERS = {
    "responder_hints": ("counter", "dht_hints_captured_total", {}),
    "streaming_peers": ("counter", "fetch_stream_batches_total", {}),
    "target_prefixes": ("counter", "dht_target_prefix_queries_total", {}),
    "sample_backoff_escalate": ("gauge", "dht_sample_cooling_reason", {"reason": "first_failure"}),
}
HINT, SPLIT, SOFT = collect.HINT, collect.SPLIT, collect.SOFT
# CR-COL-6: the hint-first tier's series every scrape of a hint segment carries, in both arms.
HINT_SERIES = tuple(HINT.values())
CR_ADM = "coordinated_admission"
# The runner's call (2026-11-21): stage 3's control should have CR-ADM on, since #1003's screen
# shows the T1 tier does little without it. Without it the effect report says so; never a verdict.
SPLIT_NOT_EXERCISED = ("The sampler's table never filled in any treatment segment, so the split was never "
                       "exercised: the outcome says nothing about what it does once the table is full. The "
                       "verdict is unchanged.")
HINT_WITHOUT_ADM = ("The control doesn't have coordinated admission (CR-ADM) on: #1003's screen shows the hint-first "
                    "tier does little without it, so a small or absent effect may come from that. The verdict "
                    "is unchanged.")
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
LABEL = re.compile(r'([a-z_]+)="([A-Za-z0-9_+.]*)"')


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


def stack_args(kind, treatment, winners):
    """r5.1 A2: a stage-2+ treatment needs the earlier winners; stage 1 takes none."""
    if kind != "ab":
        if winners is not None:
            raise ValueError("--winners applies only to an A/B segment")
        return None
    if treatment not in collect.TREATMENT_SWITCHES:
        raise ValueError(f"Unknown treatment {treatment!r}")
    if treatment not in collect.STACKED:
        if winners is not None:
            raise ValueError("--winners applies only from stage 2 (r5.1 A2); stage 1's control is the shipped defaults")
        return None
    if winners is None:
        raise ValueError(f"{treatment} runs against a stacked control (r5.1 A2): give --winners")
    if set(winners) & set(collect.TREATMENT_SWITCHES[treatment]):
        raise ValueError("A winner can't be one of the treatment's own switches")
    return sorted(winners)


def load_winners(path):
    """The stacked control's identity: {"winners": [switch, ...]}, possibly empty."""
    value = read_json(Path(path)).get("winners")
    if (not isinstance(value, list) or len(set(value)) != len(value)
            or any(w not in EXPERIMENT_SWITCHES for w in value)):
        raise ValueError(f"--winners must hold a list of distinct switches from {', '.join(EXPERIMENT_SWITCHES)}")
    return sorted(value)


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
    if treatment == "lc":  # r5.1 A1 and the ledger it's read from, in both arms
        req += [(LEDGER[k], {}) for k in ("residence", "ended", "occupied")]
        req += [(LEDGER["queries"], {"purpose": collect.LEDGER_PEER_PURPOSE}), ("dht_queries_out_total", {})]
    if treatment == "hint":  # CR-COL-6: the hint-first tier's series, in both arms
        req += [(n, {}) for n in HINT_SERIES]
        if switches.get(CR_ADM):  # T1's grants, under coordinated admission only
            req += [("fetch_admission_grants_total", {"class": collect.HINT_GRANT_CLASS})]
    if treatment == "split":  # CR-COL-6: the sampler split's series and the table's size, in both arms
        req += [(SPLIT["full_seconds"], {}), (SPLIT["spill"], {}), ("dht_sample_cooling", {}),
                ("dht_sample_inflight", {})]
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
def analyze_segment(root, winners=None, declaration=None):
    """One segment's report. With `declaration` (round 1, revised path to 100 §5.2 items 2
    and 9), a comb arm is judged against its declared preset and switches and §6.1's bundle
    evidence instead of rule 5.5's Balanced preset and the per-treatment dispatch."""
    root = Path(root)
    run = read_json(root / "run.json")
    settings = run.get("settings") or {}
    kind = settings.get("run_kind")
    if kind not in KINDS:
        raise ValueError("Not a CR-SPEC run: settings.run_kind is not ab, comb or comparator")
    if declaration is not None and kind != "comb":
        raise ValueError("--declaration applies only to a round-1 comb arm")
    treatment = settings.get("treatment")
    winners = stack_args(kind, treatment, winners)
    result = read_json(root / "result.json") if (root / "result.json").exists() else None
    rows = load_rows(root, kind)
    hours, hour_s, warm = window_params(kind, settings)
    rehearsal = settings.get("rehearsal") is True
    report = {"schema": 1, "spec": SPEC, "kind": kind, "treatment": treatment, "rehearsal": rehearsal,
              "window_hours": hours, "hour_seconds": hour_s}
    if winners is not None:
        report["winners"] = winners
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
    if run.get("run_id") is not None:  # a round-1 collector's binding fields (§5.2 items 4-5, 10)
        report["binding"] = binding(run, result)
    entry = None
    if declaration is not None:
        entry = declared_entry(declaration, run)
        report["round1"] = {"declared": entry, "placement": run.get("round1"), "analyzer_sha256": ANALYZER_SHA256,
                            "round1_spec": ROUND1_SPEC}
        checks.add("5.5", "declaration_entry_found", entry is not None, "invalid",
                   None if entry else {"placement": run.get("round1")})
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
    marks = [p_e] + [r["e"] for r in span_ok]  # a failed scrape doesn't close a gap
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
    exited = str(result.get("reason") or "").startswith("process_exited") and not complete
    checks.add("6.5", "no_interruption", not exited and "identity_change" not in result)

    # D1a: drift between UTC and elapsed over the segment.
    drift = max((abs((r["utc"] - span[0]["utc"]).total_seconds() - (r["e"] - span[0]["e"])) for r in span), default=0)
    checks.add("D1a", "utc_elapsed_drift_within_5s", drift <= DRIFT, detail={"max_drift_seconds": round(drift, 3)})

    # Rule 6.11: host activity, written at the time.
    activity = load_activity(root)
    counted = [a for a in activity if a.get("category") in collect.ACTIVITY_CATEGORIES and a.get("same_boot") is True
               and not a.get("collector_finished") and p_e <= a["elapsed_seconds"] <= end_e]
    # Round 1, D3 (plan §8 item 6; owner §2.85; runner, 2026-11-27): a mover run or parity
    # check spares the window only when none of the pools it touches holds the arm's /data
    # device (the declaration maps pools to the VM's kernel device names; an unmapped pool
    # can't be cleared, and a device in no pool is never exempt).
    # Every other entry still counts.
    if declaration is not None:
        pools, device = declaration.get("pools") or {}, declaration.get("backing_device")

        def spares(a):
            touched = a.get("pools")
            return (a.get("category") == "array_operation" and a.get("kind") in D3_KINDS
                    and isinstance(touched, list) and touched
                    and any(device in v for v in pools.values())
                    and all(p in pools and device not in pools[p] for p in touched))
        exempt = [a for a in counted if spares(a)]
        counted = [a for a in counted if a not in exempt]
        report["activity_exempt_d3"] = [{"kind": a["kind"], "pools": a["pools"], "elapsed_seconds": a["elapsed_seconds"]}
                                        for a in exempt]
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
        lodestar_checks(checks, run, config, span, span_ok, bounds, complete, treatment, kind, winners, entry)
    if entry is not None:
        report["round1"]["fresh_store"] = fresh_store(ok)
        checks.add("6.7", "fresh_store_at_start", report["round1"]["fresh_store"], "invalid")

    if complete:
        try:
            if entry is not None:
                switches = entry["switches"]  # checked equal to the effective ones above
            report.update(measure(kind, ok, bounds, t_open, span_ok, p_e, switches, checks, run, treatment, entry))
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
    items = {k: v for k, v in (run.get("checklist") or {}).items() if isinstance(v, dict)}
    docker = (run.get("settings") or {}).get("mode") == "docker"
    missing = [i for i in collect.CHECKLIST_ITEMS if i not in items
               or (items[i].get("state") == "excluded" and not items[i].get("next_run_utc"))
               or (docker and items[i].get("state") != "excluded")]
    checks.add("6.5", "host_checklist_complete", not missing, detail={"missing": missing} if missing else None)
    if kind == "ab" or switches.get("safe_restart"):
        witness = (config or {}).get("witness") or {}
        checks.add("2.3", "witness_host_mounted_outside_data_dir",
                   witness.get("host_mounted") is True and witness.get("outside_data_dir") is True)


def lodestar_checks(checks, run, config, span, span_ok, bounds, complete, treatment, kind, winners=None, entry=None):
    """Rules 5.5, 6.1 (as r5.1 amends it), 6.6 (identity) and 7.2 for a Lodestar segment.
    With a round-1 declaration `entry`, rule 5.5 is the declared preset (§8 item 1)."""
    switches = (config or {}).get("switches") or {}
    api = (config or {}).get("settings_api") or {}
    # A CR-COL-3 collector records the stage-1 switches; a stacked arm needs every switch.
    recorded = set(switches)
    switch_ok = (set(collect.STAGE1_SWITCHES) <= recorded <= set(collect.SWITCH_ENV)
                 and all(isinstance(v, bool) for v in switches.values())
                 and (treatment not in collect.STACKED or recorded == set(collect.SWITCH_ENV)))
    digest = run.get("image_digest") or run.get("sha256")
    spec_commit = (run.get("settings") or {}).get("spec_commit")
    have = bool(config) and switch_ok and api.get("intensity") in collect.PRESETS and bool(digest) \
        and bool(run.get("commit")) and bool(spec_commit) and config.get("env_preset") is not None \
        and config.get("config_file") == "absent"
    checks.add("6.6", "configuration_identity_recorded", have, "missing")
    if not have:
        return
    preset = api["intensity"]
    agrees = config["env_preset"] in ("unset", preset)
    checks.add("7.2", "env_and_settings_preset_agree", agrees, "missing")
    expected = entry["preset"] if entry else "balanced"
    if entry:
        declared_checks(checks, run, config, entry, preset)
    else:
        checks.add("5.5", "balanced_preset", preset == "balanced", "void")
    if kind == "ab":
        checks.add("2.3", "safe_restart_on", switches["safe_restart"] is True)
        own = collect.TREATMENT_SWITCHES[treatment]
        others = [s for s in EXPERIMENT_SWITCHES if s not in own and switches.get(s)]
        if treatment in collect.STACKED:
            on = [s for s in own if switches[s]]
            checks.add("A1", "treatment_switches_together", len(on) in (0, len(own)),
                       detail=None if len(on) in (0, len(own)) else {"on": on})
            others = sorted(others)
            checks.add("A2", "control_is_the_stacked_winners", others == winners,
                       detail=None if others == winners else {"on": others, "winners": winners})
            unknown = config.get("unknown_switch_env") or []
            checks.add("A2", "no_unrecognized_switch_variable", not unknown, "missing",
                       {"variables": unknown} if unknown else None)
        else:
            checks.add("6.1", "other_switches_off", not others, detail={"on": others} if others else None)
    # Boundary API checks: the settings at each boundary, imports and pauses since the start.
    calls = [r["row"].get("api") for r in span if r["row"].get("boundary")]
    settings_seen = [c.get("settings") for c in calls if isinstance(c, dict) and isinstance(c.get("settings"), dict)]
    every = complete and len(settings_seen) == len(calls) and len(calls) >= 1 and \
        sum(len(r["row"]["boundary"]) for r in span if r["row"].get("boundary")) == len(bounds)
    checks.add("5.5", "settings_checked_each_boundary", every, "missing")
    checks.add("5.5", "preset_stayed_declared" if entry else "preset_stayed_balanced",
               all(s.get("intensity") == expected for s in settings_seen), "void")
    if entry:  # §5.2 item 2: the switches too, wherever a boundary reads them
        seen = [c.get("switches") for c in calls if isinstance(c, dict) and c.get("switches") is not None]
        stayed = all(isinstance(w, dict) and {k: w.get(k) for k in entry["switches"]} == entry["switches"]
                     for w in seen)
        checks.add("5.5", "switches_stayed_declared", stayed, "void")
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


def declared_entry(declaration, run):
    """The declaration entry this arm's run.json `round1` placement names, or None."""
    place = run.get("round1") or {}
    for e in declaration["arms"]:
        if all(e.get(k) == place.get(k) for k in ("round", "attempt", "day", "arm")):
            return e
    return None


def declared_checks(checks, run, config, entry, preset):
    """§5.2 item 2: the arm ran its declaration at start (the boundaries are checked with
    the other settings reads). Any mismatch voids the window (§8 item 1)."""
    effective = config.get("effective_switches")
    have = (isinstance(effective, dict) and set(effective) == set(collect.SWITCH_ENV)
            and all(isinstance(v, bool) for v in effective.values()))
    if not checks.add("5.5", "effective_switches_recorded", have, "missing",
                      None if have else {"note": "the switch gauge's 13 values (§5.1), read at start"}):
        return
    checks.add("5.5", "declared_preset", preset == entry["preset"], "void",
               None if preset == entry["preset"] else {"declared": entry["preset"], "seen": preset})
    wrong = sorted(k for k, v in entry["switches"].items() if effective[k] != v)
    checks.add("5.5", "declared_switches", not wrong, "void", {"differ": wrong} if wrong else None)
    checks.add("2.3", "safe_restart_on", effective["safe_restart"] is True, "void")
    digest = run.get("image_digest")
    checks.add("5.5", "declared_image_digest", digest_part(digest) == digest_part(entry["image_digest"]), "void")


def fresh_store(ok):
    """Fresh-store evidence (§6.3): the store held no torrents at the first scrape."""
    return ok[0]["values"].get("torrents") == 0


def binding(run, result):
    """What `gate-round` binds a report to (§5.2 items 5 and 10): its run ID, placement,
    host and network identities (salted hashes, never addresses) and the collector's stop
    time, beside the window end it's checked against."""
    return {"run_id": run.get("run_id"), "placement": run.get("round1"),
            "host_identity": run.get("host_identity"), "network_identity": run.get("network_identity"),
            "ended_utc": (result or {}).get("ended")}


def counter_row(window, kind, name, labels):
    """One §6.1 row's value over the window: a counter's delta or a gauge's maximum; None if
    the series is absent at any scrape it's read from."""
    if kind == "counter":
        return optional_delta(window[0], window[-1], name, **labels)
    seen = [get(r["snap"], name, **labels) for r in window]
    return None if any(v is None for v in seen) else max(seen)


def preset_caps(checks, window, preset):
    """§6.1: an interval over the declared preset's query rate or outstanding cap voids the
    window, in every arm. The rate bound is r5.1 A1's (rate × 1.05 × (interval + 2 s));
    outstanding is a gauge, so each window scrape is read against the cap."""
    rate, cap = PRESET_QUERY_RATE[preset], PRESET_OUTSTANDING[preset]
    over_rate, over_cap, peak_rate, peak_out = [], [], None, None
    for a, b in zip(window, window[1:]):
        sent, seconds = optional_delta(a, b, "dht_queries_out_total"), b["e"] - a["e"]
        if sent is None or seconds <= 0:
            over_rate.append(round(a["e"] - window[0]["e"]))
            continue
        peak_rate = max(peak_rate or 0.0, sent / seconds)
        if sent > rate * (1 + VERIFY_SHARE) * (seconds + RATE_SLACK_SECONDS):
            over_rate.append(round(a["e"] - window[0]["e"]))
    for r in window:
        v = get(r["snap"], "dht_outstanding")
        if v is None or v > cap:
            over_cap.append(round(r["e"] - window[0]["e"]))
        else:
            peak_out = max(peak_out or 0, v)
    checks.add("6.1", "query_rate_within_declared_preset", len(window) > 1 and not over_rate, "void",
               {"intervals_over_or_unread": len(over_rate), "first_at_seconds": over_rate[0]} if over_rate else None)
    checks.add("6.1", "outstanding_within_declared_preset", not over_cap, "void",
               {"scrapes_over_or_unread": len(over_cap), "first_at_seconds": over_cap[0]} if over_cap else None)
    return {"preset": preset, "queries_per_second_cap": rate, "peak_queries_per_second": peak_rate,
            "outstanding_cap": cap, "peak_outstanding": peak_out,
            "intervals_over_rate": len(over_rate), "scrapes_over_outstanding": len(over_cap)}


def bundle_evidence(checks, window, switches, preset, hourly):
    """§5.2 item 9 and §6.1's table: every declared-on switch's evidence, whatever the run
    kind. A required check that fails, or a required series that's absent, voids the window.
    Coordinated admission, uTP and route backoff are rules 5.1-5.3's own checks (mechanism)."""
    rows = {}
    for switch, (kind, name, labels) in BUNDLE_COUNTERS.items():
        value = counter_row(window, kind, name, labels)
        rows[switch] = {"on": bool(switches.get(switch)), "series": name, "value": value}
        if switches.get(switch):
            checks.add("6.1", f"{switch}_evidence", (value or 0) > 0, "void",
                       None if (value or 0) > 0 else {"series": name, "value": value,
                                                      "note": "absent" if value is None else "a measured zero"})
    # lookup_end (plan §6.1): lookups ended on enough values above 0 (CR-LOOKUP-END's own end;
    # its series absent voids), and the lookup-cost pair's checks.
    rows["lookup_end"] = lookup_cost_pair(checks, window, switches, preset, judged=bool(switches.get("lookup_end")))
    if switches.get("lookup_end"):
        early = optional_delta(window[0], window[-1], LEDGER["ended"], reason=collect.LEDGER_EARLY_VALUES)
        rows["lookup_end"]["ended_on_enough_values"] = early
        checks.add("6.1", "lookups_end_on_enough_values", (early or 0) > 0, "void",
                   None if (early or 0) > 0 else {"value": early, "note": "absent" if early is None else "a measured zero"})
    # preset_slots: the slots equal the declared preset's count at every window scrape.
    caps = [get(r["snap"], LEDGER["cap"]) for r in window]
    want = PRESET_SLOTS[preset]
    rows["preset_slots"] = {"on": bool(switches.get("preset_slots")), "capacity": caps[-1], "expected": want}
    if switches.get("preset_slots"):
        good = all(c == want for c in caps)
        checks.add("6.1", "preset_slots_capacity", good, "void",
                   None if good else {"expected": want, "seen": sorted({c for c in caps if c != want}, key=str)})
    rows["hint_first"] = hint_first(checks, window, switches, bool(switches.get("hint_first")))
    split = sampler_split(checks, window, switches, hourly, bool(switches.get("sampler_cooldown_split")))
    if split["on"] and split["absent"]:  # "counter absent": void, not "not exercised"
        checks.add("6.1", "sampler_split_series_present", False, "void", {"absent": split["absent"]})
    soft = soft_timeout(window, switches)
    if soft["on"]:  # a count of 0 is a result, never a void (the existing rule)
        rendered = "soft" not in soft["absent"]
        checks.add("5", "soft_timeout_counter_rendered", rendered, "void",
                   None if rendered else {"series": SOFT["soft"]})
    rows["caps"] = preset_caps(checks, window, preset)
    return {"bundle": rows, "hint_first": rows["hint_first"], "sampler_split": split, "soft_timeout": soft}


def memory_window(ok, t_s, hours, bounds):
    """§5.2 item 6: the `anon` fit over `ws + 12 h` to `ws + 24 h`, from the window's own
    scrapes (Docker mode records the cgroup's anonymous memory as `private_bytes`)."""
    hour_s = (bounds[-1]["e"] - t_s["e"]) / hours if hours else HOUR
    points = [(r["e"], r["row"].get("private_bytes")) for r in ok if r["e"] >= t_s["e"]]
    return analyze_cr0.measured_window_fit(points, t_s["e"], hours, hour_s)


def comparator_checks(checks, run, ok, bounds, complete):
    """Rule 5.4 (rule 2.2's spacing is checked across runs, in `gate` and `checkpoint1`)."""
    checks.add("5.4", "image_pinned_by_digest", bool(run.get("image_digest")), "void")
    checks.add("5.4", "store_empty_at_start", ok[0]["values"]["comparator_torrents"] == 0, "void")
    if complete:
        flat = [k for k in range(len(bounds) - 1)
                if bounds[k + 1]["values"]["comparator_torrents"] <= bounds[k]["values"]["comparator_torrents"]]
        checks.add("5.4", "count_rises_every_hour", not flat, "void", {"flat_hours": flat} if flat else None)
    checks.add("6.6", "comparator_identity_recorded", bool((run.get("comparator") or {}).get("query")), "missing")


def measure(kind, ok, bounds, t_open, span_ok, p_e, switches, checks, run, treatment=None, entry=None):
    """Rules 1.5-1.7, 3, 4, 5.1-5.3 and 7.1 over a complete window."""
    t_s, end = bounds[0], bounds[-1]
    window = [r for r in ok if t_s["e"] <= r["e"] <= end["e"]]
    out = {}
    if kind == "comparator":
        hourly = []
        for k, (a, b) in enumerate(zip(bounds, bounds[1:])):
            minutes = (b["e"] - a["e"]) / 60
            gain = b["values"]["comparator_torrents"] - a["values"]["comparator_torrents"]
            hourly.append({"hour": k, "minutes": round(minutes, 3), "stored": gain, "rate_per_min": gain / minutes,
                       "resources": resources([r for r in window if a["e"] <= r["e"] <= b["e"]])})
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
                       "guardrails": guardrails(a, b), "mechanism": mechanism_deltas(a, b),
                       "connects_per_second": delta(a, b, "fetch_attempts_total") / (b["e"] - a["e"]),
                       "resources": resources([r for r in window if a["e"] <= r["e"] <= b["e"]])})
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
    preset = ((run.get("config") or {}).get("settings_api") or {}).get("intensity")
    if entry is not None:  # round 1: §6.1's table for every on switch, whatever the run kind
        out.update(bundle_evidence(checks, window, switches, entry["preset"], hourly))
        out["memory"] = memory_window(ok, t_s, len(bounds) - 1, bounds)
    else:
        if treatment == "lc":
            out["mechanism"]["lookup_cost_pair"] = lookup_cost_pair(checks, window, switches, preset)
        # CR-COL-6: stage 3's mechanisms, checked in a hint or split segment's treatment arm;
        # reported in every segment, their series listed absent where the binary lacks them.
        out["hint_first"] = hint_first(checks, window, switches, treatment == "hint")
        out["sampler_split"] = sampler_split(checks, window, switches, hourly, treatment == "split")
        if treatment == "soft":  # X-soft-key: CR-SOFT-TIMEOUT's pair over sampling minutes, reported
            out["soft_timeout"] = soft = soft_timeout(window, switches)
            # X-soft-absent-void: with the switch on, a binary without CR-SOFT-TIMEOUT (#1091)
            # renders no soft counter at all, and the arm ran as the control (rule 5). A count
            # of 0 is a result, never a void.
            if soft["on"]:
                rendered = "soft" not in soft["absent"]
                checks.add("5", "soft_timeout_counter_rendered", rendered, "void",
                           None if rendered else {"series": SOFT["soft"],
                                                  "note": "absent with the switch on: the binary predates the soft timeout"})
    # CR-LEDGER's W, q and y; absent series are reported absent (older binaries).
    out["ledger"] = {"window": ledger(t_s, end),
                     "hourly": [ledger(a, b) for a, b in zip(bounds, bounds[1:])]}
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
    # X-sampler-cooldown-split's restart gate, beside sampler-full time (rule 6.4's report).
    out["sampler_split"]["restart_gate"] = {k: out["startup"][k] for k in (
        "suppression_reason_at_start", "suppression_reason_at_open", "suppressed_seconds")}
    out["already_stored_and_rediscovered"] = {
        r: delta(t_s, end, "metadata_excluded_total", reason=r) for r in ("already_stored", "rediscovered")}
    return out


def optional_delta(a, b, name, **labels):
    """A delta, or None when the series is absent at either end (never an error)."""
    left, right = get(a["snap"], name, **labels), get(b["snap"], name, **labels)
    return None if left is None or right is None else right - left


def ledger(a, b):
    """W, q and y over the scrapes a..b, from CR-LEDGER's identities (docs/metrics.md):
    W = residence seconds / lookups ended, q = peer-lookup queries / lookups ended,
    y = stored / lookups ended."""
    parts = {
        "residence_seconds": optional_delta(a, b, LEDGER["residence"]),
        "lookups_ended": optional_delta(a, b, LEDGER["ended"]),
        "peer_lookup_queries": optional_delta(a, b, LEDGER["queries"], purpose=collect.LEDGER_PEER_PURPOSE),
        "stored": optional_delta(a, b, "metadata_new_verified_total", origin="network"),
        "peer_lookup_seconds": optional_delta(a, b, "dht_peer_lookup_seconds_total"),
        "peer_lookups_answered": optional_delta(a, b, "dht_peer_lookups_answered_total"),
    }
    reasons = [optional_delta(a, b, LEDGER["ended"], reason=r) for r in collect.LEDGER_VALUE_REASONS]
    # CR-LOOKUP-END's early end is an end on values too. Older binaries don't render it, so
    # it counts as 0 there and their numbers are unchanged (X-cr-round1-collect-fixes).
    early = optional_delta(a, b, LEDGER["ended"], reason=collect.LEDGER_EARLY_VALUES)
    parts["ended_on_values"] = None if any(r is None for r in reasons) else sum(reasons) + (early or 0)
    ended, answered = parts["lookups_ended"], parts["peer_lookups_answered"]

    def per(value, count):
        return value / count if value is not None and count else None
    return {"W_seconds": per(parts["residence_seconds"], ended), "q": per(parts["peer_lookup_queries"], ended),
            "y": per(parts["stored"], ended),
            "seconds_per_answered": per(parts["peer_lookup_seconds"], answered), **parts,
            "absent": sorted(k for k, v in parts.items() if v is None),
            # Only where rendered, so an older run's report is unchanged.
            **({"ended_on_enough_values": early} if early is not None else {})}


def lookup_cost_pair(checks, window, switches, preset, judged=None):
    """r5.1 A1: with both switches on, lookups end on values, the slots go above 32 at
    least once, and the query rate stays within the preset; otherwise the segment is void."""
    led = ledger(window[0], window[-1])
    occupied = [get(r["snap"], LEDGER["occupied"]) for r in window]
    occupied = [v for v in occupied if v is not None]
    rate = PRESET_QUERY_RATE.get(preset)
    over, peak = [], None
    for a, b in zip(window, window[1:]):
        sent, seconds = optional_delta(a, b, "dht_queries_out_total"), b["e"] - a["e"]
        if sent is None or seconds <= 0 or rate is None:
            over.append(round(a["e"] - window[0]["e"]))
            continue
        peak = max(peak or 0.0, sent / seconds)
        if sent > rate * (1 + VERIFY_SHARE) * (seconds + RATE_SLACK_SECONDS):
            over.append(round(a["e"] - window[0]["e"]))
    out = {"on": all(switches.get(s) for s in collect.TREATMENT_SWITCHES["lc"]),
           "ended_on_values": led["ended_on_values"], "max_slots_occupied": max(occupied, default=None),
           "slots_capacity": get(window[-1]["snap"], LEDGER["cap"]),
           "peak_queries_per_second": peak, "preset_queries_per_second": rate,
           "intervals_over_rate": len(over)}
    if out["on"] if judged is None else judged:
        checks.add("A1", "lookups_end_on_values", (led["ended_on_values"] or 0) > 0, "void")
        checks.add("A1", "slots_exceed_32_once", (out["max_slots_occupied"] or 0) > A1_SLOTS, "void",
                   {"max_slots_occupied": out["max_slots_occupied"]})
        checks.add("A1", "query_rate_within_preset", len(window) > 1 and not over, "void",
                   {"peak_queries_per_second": peak, "intervals_over_or_unread": len(over)} if over else None)
    return out


def hint_first(checks, window, switches, judged):
    """CR-HINT-FIRST's mechanism. In a hint segment's treatment arm (the switch on): marks and
    hinted lookups above 0, and the pool ignored no more marks than it took as keys; otherwise
    the segment is void (rule 5). More ignored than taken means marks are coming from somewhere
    unexpected (another source, the switch off, a private or interactive key)."""
    a, b = window[0], window[-1]
    parts = {k: optional_delta(a, b, HINT[k]) for k in ("marked", "keys", "ignored", "waits", "lookups")}
    skipped = {r: optional_delta(a, b, HINT["skipped"], reason=r) for r in sorted(collect.HINT_SKIPPED_REASONS)}
    left = {r: optional_delta(a, b, HINT["left"], reason=r) for r in sorted(collect.HINT_LEFT_REASONS)}
    grants = optional_delta(a, b, "fetch_admission_grants_total", **{"class": collect.HINT_GRANT_CLASS})
    waiting = [v for v in (get(r["snap"], HINT["waiting"]) for r in window) if v is not None]
    out = {"on": bool(switches.get("hint_first")), **parts, "skipped": skipped, "left_t1": left,
           "hinted_grants": grants, "max_t1_waiting": max(waiting, default=None)}
    out["absent"] = sorted([k for k, v in parts.items() if v is None]
                           + (["skipped"] if None in skipped.values() else [])
                           + (["left_t1"] if None in left.values() else [])
                           + (["hinted_grants"] if grants is None else []) + ([] if waiting else ["t1_waiting"]))
    if judged and out["on"]:
        marked, lookups, keys, ignored = (parts[k] for k in ("marked", "lookups", "keys", "ignored"))
        checks.add("5", "hint_first_marks_hashes", (marked or 0) > 0, "void", {"marked": marked})
        checks.add("5", "hint_first_lookups_sent", (lookups or 0) > 0, "void", {"lookups": lookups})
        within = keys is not None and ignored is not None and ignored <= keys
        checks.add("5", "hint_ignored_within_keys", within, "void",
                   None if within else {"ignored": ignored, "keys": keys,
                                        "note": "more hint marks ignored than taken: marks from an unexpected source"})
    return out


def sampler_split(checks, window, switches, hourly, judged):
    """X-sampler-cooldown-split's mechanism and rule 6.4's report. In a split segment's
    treatment arm (the switch on), once the sampler's table has filled, the spill filter holds
    slots at some scrape; otherwise the segment is void (rule 5). A table that never filled in
    the window didn't exercise the split: reported, not a failure. In every segment, the
    sampler-full time and the worst hours of stored per minute are reported, never decided on."""
    spill = [get(r["snap"], SPLIT["spill"]) for r in window]
    table = [None if c is None or i is None else c + i for c, i in
             ((get(r["snap"], "dht_sample_cooling"), get(r["snap"], "dht_sample_inflight")) for r in window)]
    # The filter is allocated when the table first fills, so slots in use also mean it filled.
    full = [k for k, (t, s) in enumerate(zip(table, spill))
            if (t is not None and t >= collect.SAMPLER_TABLE) or (s or 0) > 0]
    first = full[0] if full else None
    spilled = first is not None and any((s or 0) > 0 for s in spill[first:])
    # A table first seen full at the window's last scrape had no scrape after it in which to
    # spill (a cooldown moves out only when the next node needs a place): not exercised.
    if first is not None and not spilled and first == len(window) - 1:
        first = None
    a, b = window[0], window[-1]
    hours = [{"hour": h["hour"], "stored_per_min": h["stored_per_min"]} for h in hourly]
    out = {"on": bool(switches.get("sampler_cooldown_split")),
           "sampler_full_seconds": optional_delta(a, b, SPLIT["full_seconds"]),
           "max_spill_slots": max((s for s in spill if s is not None), default=None),
           "max_table_entries": max((t for t in table if t is not None), default=None),
           "table_filled_after_seconds": None if not full else round(window[full[0]]["e"] - a["e"]),
           "worst_hours_stored_per_min": sorted(hours, key=lambda h: h["stored_per_min"])[:3]}
    out["absent"] = sorted(([] if out["sampler_full_seconds"] is not None else ["sampler_full_seconds"])
                           + ([] if out["max_spill_slots"] is not None else ["spill_slots"])
                           + ([] if out["max_table_entries"] is not None else ["table_entries"]))
    if judged and out["on"]:
        if first is None:
            out["spill_check"] = "not_exercised"  # the table never filled: nothing for the split to do
        else:
            out["spill_check"] = "spilled" if spilled else "table_full_without_spill"
            checks.add("5", "spill_slots_used_once_table_fills", spilled, "void",
                       None if spilled else {"table_filled_after_seconds": out["table_filled_after_seconds"],
                                             "max_table_entries": out["max_table_entries"]})
    return out


def interval_stall(a, b):
    """X-soft-key (the runner's definition, 2026-11-24): why the scrape interval a..b is
    stalled, or None when it samples. It's stalled when the start suppression
    (`dht_sample_suppression_seconds`) reads above 0 at either end ("suppressed"), or when
    sampler-full time (rule 6.4's `dht_sample_full_seconds_total`) grew in it ("sampler_full").
    With sampler-full time absent, stalled can't be told from sampling, so it samples."""
    if any((get(r["snap"], "dht_sample_suppression_seconds") or 0) > 0 for r in (a, b)):
        return "suppressed"
    if (optional_delta(a, b, SPLIT["full_seconds"]) or 0) > 0:
        return "sampler_full"
    return None


def soft_timeout(window, switches):
    """X-soft-key: CR-SOFT-TIMEOUT's pair (docs/metrics.md) in a soft segment, split into
    sampling and stalled minutes (interval_stall), each interval whole. The headline figures
    (soft and late per hour, late ÷ soft, soft ÷ peer-lookup queries) are over sampling minutes
    only; `stalled` gives the same soft and late figures over the stalled minutes. The figures
    are reported, never decided on; an absent series reads absent, and a part with no minutes
    reads null. Only the soft counter's absence with the switch on voids (X-soft-absent-void)."""
    names = {"soft": (SOFT["soft"], {}), "late": (SOFT["late"], {}),
             "peer_lookup_queries": (LEDGER["queries"], {"purpose": collect.LEDGER_PEER_PURPOSE})}
    sums = {part: {k: 0 for k in names} for part in ("sampling", "stalled")}
    minutes = {"sampling": 0.0, "stalled": 0.0}
    absent = set()
    for a, b in zip(window, window[1:]):
        if optional_delta(a, b, SPLIT["full_seconds"]) is None:
            absent.add("sampler_full_seconds")
        part = "sampling" if interval_stall(a, b) is None else "stalled"
        minutes[part] += (b["e"] - a["e"]) / 60
        for k, (name, labels) in names.items():
            d = optional_delta(a, b, name, **labels)
            if d is None:
                absent.add(k)
            else:
                sums[part][k] += d

    def figures(part):
        # No minute in the part means nothing was observed there: null counts, never zeros.
        got = {k: (None if k in absent or not minutes[part] else v) for k, v in sums[part].items()}
        soft, late, hours = got["soft"], got["late"], minutes[part] / 60
        return got, {"soft_per_hour": soft / hours if soft is not None and hours else None,
                     "late_per_hour": late / hours if late is not None and hours else None,
                     "late_per_soft": late / soft if late is not None and soft else None}
    parts, rates = figures("sampling")
    queries = parts["peer_lookup_queries"]
    st_parts, st_rates = figures("stalled")
    return {"on": bool(switches.get("soft_timeout")), "sampling_minutes": round(minutes["sampling"], 3),
            "stalled_minutes": round(minutes["stalled"], 3), **parts, **rates,
            "soft_per_peer_query": parts["soft"] / queries if parts["soft"] is not None and queries else None,
            "stalled": {"soft": st_parts["soft"], "late": st_parts["late"], **st_rates},
            "absent": sorted(absent)}


def mechanism_deltas(a, b):
    """Rule 5's series over one hour (rule 7.1 publishes them per hour too)."""
    out = {}
    for name, labels in (("fetch_admission_grants_total", {}), ("fetch_admission_refusals_total", {}),
                         ("dht_peer_rejection_reasons_total", {"reason": "capacity"}), ("dht_utp_in_total", {}),
                         ("fetch_attempts_total", {"transport": "utp"}), ("fetch_route_skipped_total", {}),
                         ("fetch_route_probe_down_total", {}), ("fetch_no_route_total", {"family": "ipv6"})):
        if get(a["snap"], name, **labels) is not None and get(b["snap"], name, **labels) is not None:
            out[name + ("{" + ",".join(f'{k}="{v}"' for k, v in labels.items()) + "}" if labels else "")] = \
                delta(a, b, name, **labels)
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
            if gap != 0:  # a stretch lasts until the scrape that reads 0 again
                since = r["e"] if since is None else since
                longest = max(longest, r["e"] - since)
            elif since is not None:
                longest = max(longest, r["e"] - since)
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
    switches = (report.get("identity") or {}).get("switches")
    if switches is not None:
        lines += ["## Switches", "", "On: " + (", ".join(sorted(k for k, v in switches.items() if v)) or "none") + "."]
        if "winners" in report:
            lines.append("Stacked control (r5.1 A2), earlier winners: " + (", ".join(report["winners"]) or "none") + ".")
        lines.append("")
    led = (report.get("ledger") or {}).get("window")
    if led:
        lines += ["## Ledger", "", f"W {f(led['W_seconds'])} s, q {f(led['q'])}, y {f(led['y'], 4)} over "
                  f"{f(led['lookups_ended'])} lookups ended"
                  + (f"; absent: {', '.join(led['absent'])}" if led["absent"] else "") + ".", ""]
    hint, split = report.get("hint_first"), report.get("sampler_split")
    if report.get("treatment") == "hint" and hint:
        lines += ["## Hint-first", "", f"On: {str(hint['on']).lower()}; marked {f(hint['marked'])}, taken as keys "
                  f"{f(hint['keys'])}, ignored {f(hint['ignored'])}, first lookups {f(hint['lookups'])}"
                  + (f"; absent: {', '.join(hint['absent'])}" if hint["absent"] else "") + ".", ""]
    if report.get("treatment") == "split" and split:
        lines += ["## Sampler split", "", f"On: {str(split['on']).lower()}"
                  + (f"; {split['spill_check'].replace('_', ' ')}" if "spill_check" in split else "")
                  + f"; sampler-full {f(split['sampler_full_seconds'])} s; peak spill slots {f(split['max_spill_slots'])}"
                  + (f"; absent: {', '.join(split['absent'])}" if split["absent"] else "") + ".", ""]
    soft = report.get("soft_timeout")
    if report.get("treatment") == "soft" and soft:
        lines += ["## Soft timeout", "", f"On: {str(soft['on']).lower()}; over {f(soft['sampling_minutes'])} sampling "
                  f"min ({f(soft['stalled_minutes'])} stalled): soft {f(soft['soft_per_hour'])}/h, late "
                  f"{f(soft['late_per_hour'])}/h, late ÷ soft {f(soft['late_per_soft'], 4)}, soft ÷ peer-lookup "
                  f"queries {f(soft['soft_per_peer_query'], 4)}"
                  + f"; stalled: soft {f(soft['stalled']['soft_per_hour'])}/h, late {f(soft['stalled']['late_per_hour'])}/h, "
                  f"late ÷ soft {f(soft['stalled']['late_per_soft'], 4)}"
                  + (f"; absent: {', '.join(soft['absent'])}" if soft["absent"] else "") + ".", ""]
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
            "minutes": w.get("minutes"), "winners": report.get("winners"),
            "ledger": (report.get("ledger") or {}).get("window"),
            "hint_first": report.get("hint_first"), "sampler_split": report.get("sampler_split"),
            # X-soft-key: a soft segment's only, so every other view stays byte-identical.
            **({"soft_timeout": report["soft_timeout"]} if "soft_timeout" in report else {})}


# A block's recorded order (block.env's MODE, X-fleet-baab) as the arm letters; infra/fleet's
# scrub.arm_of and block-lib.sh's arm_of give the same arms (a test checks the three agree).
DESIGNS = {"abba": "ABBA", "baab": "BAAB"}


def judge_block(reports, treatment, winners=None, design=None, aa=False):
    """Rules 2.3, 5, 6.1 (with r5.1 A1 and A2), 6.2's zero rule and 6.7 for one block of four segments.

    `design` (X-fleet-baab) is the block's recorded order, "abba" or "baab": the order the
    switches give must match it. With `aa` the block is labelled A/A: all four segments'
    switches must be identical, and the arms are the design's labels."""
    if (aa or design is not None) and design not in DESIGNS:
        raise ValueError("an A/A block needs its design (abba or baab)" if aa else "a design is abba or baab")
    own = collect.TREATMENT_SWITCHES[treatment]
    segs = [segment_view(r) for r in reports]
    problems = []
    if len(reports) != 4 or any(r.get("kind") != "ab" or r.get("treatment") != treatment for r in reports):
        return {"valid": False, "problems": ["not_four_segments_of_this_treatment"], "segments": segs}

    def arm(s):  # a pair is B only with both switches on (A1)
        on = [bool(s["switches"].get(x)) for x in own]
        return "B" if all(on) else "A" if not any(on) else "?"
    arms = "".join(arm(s) for s in segs)
    if aa:  # X-fleet-baab: both arms ran the same switches, so the arms are the recorded labels
        if len({frozenset(k for k, v in s["switches"].items() if v) for s in segs}) != 1:
            problems.append("aa_switches_differ")
        if any(s["switches"].get(x) for s in segs for x in own):  # both arms are the control
            problems.append("aa_treatment_on")
        arms = DESIGNS[design]
    else:
        if "?" in arms:
            problems.append("treatment_switches_split")
        if arms not in ("ABBA", "BAAB"):
            problems.append("order_not_abba_or_baab")
        if "A" not in arms and "?" not in arms:  # X-block-on-only-mode: an on-only block has no control, so no A/B verdict
            problems.append("no_control_arm")
        if design is not None and arms != DESIGNS[design]:
            problems.append("order_differs_from_design")
    for s in segs:
        if s["verdict"] == "invalid" or (s["verdict"] == "void" and s["reasons"] != ["no_opportunity"]):
            problems.append("segment_" + s["verdict"])
    # Compared as the switches on, so a switch an older collector didn't record reads off.
    sets = {frozenset(k for k, v in s["switches"].items() if v and k not in own) for s in segs}
    if len(sets) != 1:
        problems.append("switch_set_differs")
    # Every switch on in the control arm, printed beside the result (r5.1 A2's reporting).
    control = sorted({k for s, a in zip(segs, arms) if a == "A" for k, v in s["switches"].items() if v})
    if treatment in collect.STACKED:
        if [k for k in control if k != "safe_restart"] != winners:
            problems.append("control_not_the_stacked_winners")
        if any(s["winners"] != winners for s in segs):
            problems.append("segment_judged_with_other_winners")
    if len({s["digest"] for s in segs}) != 1 or not segs[0]["digest"]:
        problems.append("image_digest_differs")
    for a, b in zip(segs, segs[1:]):  # rule 2.3: a clean stop, then a start within 10 minutes
        try:
            gap = (parse_utc(b["process_start"]) - parse_utc(a["last_scrape"])).total_seconds()
        except (TypeError, ValueError):
            gap = None
        if gap is None or gap > SWITCH_SECONDS or gap < 0:
            problems.append("arm_switch_over_10_minutes")
        # A clean stop is proven by the collector reading exit 0 (Docker), or by the next start
        # resuming: CR-0d resumes only after a clean stop. Native mode can't read an exit code.
        if a["clean_stop"] is not True and b["reason_at_start"] != "resumed":
            problems.append("arm_switch_not_a_clean_stop")
    a_means = [s["net"] for s, arm in zip(segs, arms) if arm == "A" and s["verdict"] == "valid"]
    if any(m is None or m <= 0 for m in a_means):
        problems.append("control_did_not_crawl")
    loss = (not aa and not problems and all(m > 0 for m in a_means)  # an A/A block never shows a loss
            and any(s["verdict"] == "valid" and s["net"] is not None and s["net"] <= 0
                    for s, arm in zip(segs, arms) if arm == "B"))
    return {"valid": not problems, "problems": sorted(set(problems)), "order": arms, "loss_without_fit": loss,
            "segments": segs, "host_local_start_hours": [s["start_hour"] for s in segs], "control_switches": control}


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
    if NO_EFFECT[0] <= lo and hi <= NO_EFFECT[1]:
        return "no_effect"
    if hi < 0:
        return "loss"
    return "inconclusive_run_blocks_3_and_4" if valid_blocks == 2 else "inconclusive_switch_stays_off"


def analyze_effect(treatment, block_reports, winners=None, designs=None, aa=False):
    winners = stack_args("ab", treatment, winners)
    if designs is not None and (len(designs) != len(block_reports) or any(d not in DESIGNS for d in designs)):
        raise ValueError("give one --design (abba or baab) per --block")
    if aa and designs is None:
        raise ValueError("--aa needs the blocks' --design")
    if aa:
        return noise_floor(treatment, block_reports, winners, designs)
    blocks = [judge_block(reports, treatment, winners, design) for reports, design
              in zip(block_reports, designs or [None] * len(block_reports))]
    valid = [b for b in blocks if b["valid"]]
    own = collect.TREATMENT_SWITCHES[treatment]
    out = {"schema": 1, "spec": SPEC, "treatment": treatment, "switch": "+".join(own),
           "blocks": blocks, "valid_blocks": len(valid)}
    controls = sorted({tuple(b["control_switches"]) for b in valid})
    # The control's switch set, beside the result (r5.1 A2); one set across the blocks.
    out["control_switches"] = list(controls[0]) if len(controls) == 1 else [list(c) for c in controls]
    if winners is not None:
        out["winners"] = winners
    orders = [b["order"] for b in valid]
    paired = len(valid) in (2, 4) and all(orders[i] == ("ABBA" if i % 2 == 0 else "BAAB") for i in range(len(valid)))
    rehearsal = any(r.get("rehearsal") for reports in block_reports for r in reports)
    out["rehearsal"] = rehearsal
    # Rule 6.9 takes R0 from the uTP controls whatever uTP's outcome, so these come first.
    out["control_segments"] = [{"net": s["net"], "p95": s["p95"], "p95_measured": s["p95_measured"]}
                               for b in valid for s, a in zip(b["segments"], b["order"]) if a == "A" and s["verdict"] == "valid"]
    out["treatment_segments"] = [{"net": s["net"], "p95": s["p95"], "p95_measured": s["p95_measured"]}
                                 for b in valid for s, a in zip(b["segments"], b["order"]) if a == "B" and s["verdict"] == "valid"]
    out["ledger_by_arm"] = ledger_by_arm(valid)
    if treatment == "hint":  # CR-COL-6: the tier's counts per arm, and the runner's CR-ADM warning
        out["hint_first_by_arm"] = stage3_by_arm(valid, "hint_first", ("marked", "keys", "ignored", "lookups"))
        if CR_ADM not in (winners or []):
            out["warnings"] = [HINT_WITHOUT_ADM]
    if treatment == "split":  # CR-COL-6: rule 6.4's report, per arm; never decided on
        out["sampler_split_by_arm"] = stage3_by_arm(valid, "sampler_split",
                                                    ("spill_check", "table_filled_after_seconds",
                                                     "sampler_full_seconds", "max_spill_slots",
                                                     "worst_hours_stored_per_min", "restart_gate"))
        idle = [s for s in out["sampler_split_by_arm"]["B"] if s["spill_check"] == "not_exercised"]
        out["split_not_exercised_b_segments"] = len(idle)
        if out["sampler_split_by_arm"]["B"] and len(idle) == len(out["sampler_split_by_arm"]["B"]):
            out["warnings"] = [SPLIT_NOT_EXERCISED]
    if treatment == "soft":  # X-soft-key: CR-SOFT-TIMEOUT's pair per arm, over sampling minutes; the figures never decide
        out["soft_timeout_by_arm"] = stage3_by_arm(valid, "soft_timeout",
                                                   ("sampling_minutes", "stalled_minutes", "soft", "late",
                                                    "soft_per_hour",
                                                    "late_per_hour", "late_per_soft", "soft_per_peer_query",
                                                    "stalled", "absent"))
    if len(controls) > 1:
        out.update(outcome="invalid", decided=False,
                   reason="the control's switch set differs between blocks of this treatment (r5.1 A2)")
        return out
    if not paired:
        out.update(outcome="not_decidable", decided=False,
                   reason="needs 2 or 4 valid blocks, ABBA then BAAB in pairs (rules 6.1, 6.3)")
        return out
    if treatment == "route":  # rule 5.3, decided over the same paired blocks as any outcome
        on = [s for b in valid for s, arm in zip(b["segments"], b["order"]) if arm == "B"]
        if on and all(s["verdict"] == "void" for s in on):
            out.update(outcome="no_opportunity", decided=True)
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
    return out


def noise_floor(treatment, block_reports, winners, designs):
    """X-fleet-baab: an A/A run. Both arms ran the same switches (the stage's control), so the
    blocks' design labels split them and rule 6.2's fit gives the noise floor between legs. It
    never decides anything: there's no `fit` and no win, loss or no_effect."""
    blocks = [judge_block(reports, treatment, winners, design, aa=True)
              for reports, design in zip(block_reports, designs)]
    valid = [b for b in blocks if b["valid"]]
    out = {"schema": 1, "spec": SPEC, "analysis": "noise_floor", "treatment": treatment,
           "designs": list(designs), "blocks": blocks, "valid_blocks": len(valid),
           "rehearsal": any(r.get("rehearsal") for reports in block_reports for r in reports),
           "outcome": "noise_floor", "decided": False}
    if winners is not None:
        out["winners"] = winners
    if not valid:
        out["reason"] = "no valid A/A block"
        return out
    out["noise_floor"] = fit_effect(valid, "net")
    out["also_reported"] = {"gross": fit_effect(valid, "gross"), "stored": fit_effect(valid, "stored"),
                            "fetch_success_per_attempt": fit_effect(valid, "success")}
    return out


NOISE_FLOOR_REFUSED = "an A/A report is a noise floor, not an effect"


def refuse_noise_floor(report):
    if report.get("analysis") == "noise_floor" or report.get("outcome") == "noise_floor":
        raise ValueError(NOISE_FLOOR_REFUSED)
    return report


def ledger_by_arm(blocks):
    """CR-LEDGER's W, q and y per valid segment and arm, with each arm's plain mean."""
    out = {}
    for arm in "AB":
        rows = [{k: (s["ledger"] or {}).get(k) for k in ("W_seconds", "q", "y")}
                for b in blocks for s, a in zip(b["segments"], b["order"]) if a == arm and s["verdict"] == "valid"]
        means = {}
        for k in ("W_seconds", "q", "y"):
            vals = [r[k] for r in rows if r[k] is not None]
            means[k] = sum(vals) / len(vals) if vals and len(vals) == len(rows) else None
        out[arm] = {"segments": rows, "mean": means}
    return out


def stage3_by_arm(blocks, key, fields):
    """Stage 3's per-segment report fields for each valid segment, by arm. Reported, not fitted."""
    return {arm: [{k: (s.get(key) or {}).get(k) for k in fields}
                  for b in blocks for s, a in zip(b["segments"], b["order"]) if a == arm and s["verdict"] == "valid"]
            for arm in "AB"}


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
            w1 = max(parse_utc(x) for x in (rt.get("window_end_utc"), rt.get("last_scrape_utc")) if x) \
                + timedelta(seconds=STOP_ALLOWANCE)
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


# ----------------------------------------------------------------- round 1 (gate-round)
# docs/plan/2026-11-27-revised-path-to-100.md §5.2 items 2, 4-6, 9-12, §6.2-§6.4 and §8;
# the formats and every reading of an open rule are in docs/decisions/2026-11-27-X-cr-round1-gate.md.
ROUND_SWITCHES = EXPERIMENT_SWITCHES  # the 12 a declaration names (safe_restart is on in every arm)
ROLES = ("candidate", "diagnostic")
# §6.3's permitted layouts: arm → exit label on each day. S is one arm on two different exits.
LAYOUTS = {
    "L3": {1: {"B": "X", "A": "Y", "C": "Z"}, 2: {"B": "Y", "A": "Z", "C": "X"}},
    "L2": {1: {"B": "X", "A": "Y"}, 2: {"B": "Y", "A": "X"}},
    "L2b": {1: {"B": "X", "C": "Y"}, 2: {"B": "Y", "C": "X"}},
    # §6.3: a candidate B that fails in L3 leaves A (candidate) and C (diagnostic), which go
    # on in a fresh attempt with the remaining arms (item G); only after an L3 attempt.
    "L2c": {1: {"A": "X", "C": "Y"}, 2: {"A": "Y", "C": "X"}},
    "S": None,
}
EXIT_LABELS = ("X", "Y", "Z")
SELECTION_ORDER = ["B", "A"]  # O2: B Balanced first, A Aggressive the authorized fallback
DIAGNOSES = (None, "pending", "product", "infrastructure")  # an event's diagnosis (§6.3)
RETRY_BUDGET = 2  # §6.3: repeated days per round, not counting a solo confirmation's own
# §6.3's contention thresholds, as proposed; the CR-SPEC decisions file freezes them and a
# declaration's `contention` block may only restate them (gate-round refuses other values).
CONTENTION = {"interval_seconds": 10, "coverage_min": 0.98, "cpu_p95_max": 0.80, "steal_p95_max": 0.05,
              "disk_write_p99_ms_max": 50.0, "tunnel_loss_max": 0.01, "tunnel_rtt_p95_over_rehearsal_max": 2.0,
              "uplink_drops_max": 0}
HASH = re.compile(r"^[0-9a-f]{16,128}$")  # a salted hash, never an address
# The declaration's image: `name@sha256:<hex>` (runner, 2026-11-27). The collector records
# Docker's repo digest without the repository, so reports are compared on the part after `@`.
IMAGE_DIGEST = re.compile(r"^[^@\s]+@sha256:[0-9a-f]{64}$")


def digest_part(value):
    return str(value).rsplit("@", 1)[-1] if value else None
# The analyzer's own SHA-256, in every round-1 output, so a reproduction pins its version.
ANALYZER_SHA256 = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
RUN_ID = re.compile(r"^[A-Za-z0-9_.:-]{1,80}$")


def require(ok, message):
    if not ok:
        raise ValueError(message)


def check_declaration(decl):
    """The declaration's schema (§8 item 1). A malformed declaration is an error, never a
    verdict: it's frozen before the rehearsal, so a bad one is fixed, not judged."""
    require(isinstance(decl, dict) and decl.get("schema") == 1 and decl.get("kind") == "cr_round1_declaration",
            "declaration: schema 1, kind cr_round1_declaration")
    require(isinstance(decl.get("round"), int) and decl["round"] >= 1, "declaration: round")
    for key in ("spec_commit", "image_digest", "host_identity"):
        require(isinstance(decl.get(key), str) and decl[key], f"declaration: {key}")
    require(IMAGE_DIGEST.fullmatch(decl["image_digest"]) is not None,
            "declaration: image_digest is name@sha256:<64 hex> (pulled by tag, recorded by its repo digest)")
    require(HASH.fullmatch(decl["host_identity"]) is not None, "declaration: host_identity must be a salted hash")
    require(isinstance(decl.get("backing_device"), str) and re.fullmatch(r"[a-z0-9_-]{1,32}", decl["backing_device"])
            is not None, "declaration: backing_device (the /data device's kernel name, §6.3)")
    net = decl.get("network_identity")
    require(isinstance(net, dict) and HASH.fullmatch(str(net.get("uplink_hash"))) is not None
            and isinstance(net.get("provider"), str) and isinstance(net.get("protocol"), str),
            "declaration: network_identity {uplink_hash, provider, protocol}")
    comp = decl.get("comparator")
    require(isinstance(comp, dict) and RUN_ID.fullmatch(str(comp.get("run_id"))) is not None,
            "declaration: comparator.run_id (the booked comparator)")
    # O2: Balanced first. The order is the owner's, so a declaration may only restate it.
    require(decl.get("selection_order", SELECTION_ORDER) == SELECTION_ORDER,
            "declaration: selection_order must be B then A (O2, Balanced first)")
    thresholds = decl.get("contention", CONTENTION)
    require(thresholds == CONTENTION, "declaration: contention thresholds differ from the frozen ones")
    pools = decl.get("pools", {})
    require(isinstance(pools, dict) and all(isinstance(k, str) and isinstance(v, list)
                                            and all(isinstance(x, str) for x in v) for k, v in pools.items()),
            "declaration: pools maps a pool name to its devices' kernel names (D3)")
    # Runner, 2026-11-27: the map lists the VM's kernel device names (the names identity
    # records), so the declared /data device must sit in exactly one pool, by exact name.
    require(not pools or sum(decl.get("backing_device") in v for v in pools.values()) == 1,
            "declaration: backing_device must be listed in exactly one pool (D3)")
    # Item J: the thresholds are shares of the declared vCPUs (8); host samples must match.
    require(decl.get("vcpus") == DECLARED_VCPUS, f"declaration: vcpus must be {DECLARED_VCPUS} (the box's, §6.3)")
    attempts = decl.get("attempts")
    require(isinstance(attempts, list) and attempts, "declaration: attempts")
    ids = set()
    for a in attempts:
        require(isinstance(a, dict) and RUN_ID.fullmatch(str(a.get("id"))) is not None and a["id"] not in ids,
                "declaration: each attempt has a distinct id")
        ids.add(a["id"])
        require(a.get("layout") in LAYOUTS, f"declaration: attempt {a['id']}'s layout must be one of {sorted(LAYOUTS)}")
        require(isinstance(a.get("candidate_digests"), dict), f"declaration: attempt {a['id']}'s candidate_digests")
    arms = decl.get("arms")
    require(isinstance(arms, list) and arms, "declaration: arms")
    seen = set()
    for e in arms:
        require(isinstance(e, dict), "declaration: each arm entry is an object")
        key = (e.get("round"), e.get("attempt"), e.get("day"), e.get("arm"))
        require(key not in seen, f"declaration: duplicate entry {key}")
        seen.add(key)
        require(e.get("round") == decl["round"] and e.get("attempt") in ids and e.get("day") in (1, 2)
                and isinstance(e.get("arm"), str) and e["arm"], f"declaration: entry {key}'s placement")
        require(e.get("role") in ROLES, f"declaration: entry {key}'s role")
        require(e.get("preset") in collect.PRESETS, f"declaration: entry {key}'s preset")
        sw = e.get("switches")
        require(isinstance(sw, dict) and set(sw) == set(ROUND_SWITCHES) and all(isinstance(v, bool) for v in sw.values()),
                f"declaration: entry {key} names exactly the 12 switches, each true or false")
        require(e.get("exit") in EXIT_LABELS, f"declaration: entry {key}'s exit label")
        require(isinstance(e.get("image_digest"), str) and IMAGE_DIGEST.fullmatch(e["image_digest"]) is not None,
                f"declaration: entry {key}'s image digest (name@sha256:<64 hex>)")
    # §6.1's arms: B and A all-on but route_backoff (O3), B Balanced and A Aggressive; C the
    # explicit all-off Balanced control, diagnostic. A is a candidate only if O2 authorized it.
    all_on = {s: s != "route_backoff" for s in ROUND_SWITCHES}
    table = {"B": (("candidate",), "balanced", all_on), "A": (ROLES, "aggressive", all_on),
             "C": (("diagnostic",), "balanced", {s: False for s in ROUND_SWITCHES})}
    for e in arms:
        require(e["arm"] in table, f"declaration: arm {e['arm']} isn't one of §6.1's arms (B, A, C)")
        roles, preset, switches = table[e["arm"]]
        require(e["role"] in roles and e["preset"] == preset and e["switches"] == switches,
                f"declaration: arm {e['arm']}'s role, preset or switches differ from §6.1's table")
    for e in arms:  # a role is the arm's for the whole round
        require(all(o["role"] == e["role"] for o in arms if o["arm"] == e["arm"]),
                f"declaration: arm {e['arm']} has one role")
        # §6.3: one pinned image, the same digest for every arm and day.
        require(e["image_digest"] == decl["image_digest"],
                f"declaration: entry {e['arm']} day {e['day']} of {e['attempt']} names another image digest")
        if e["role"] == "candidate":  # each attempt records its candidates' digests (§6.3)
            att = next(a for a in attempts if a["id"] == e["attempt"])
            require(att["candidate_digests"].get(e["arm"]) == decl["image_digest"],
                    f"declaration: attempt {att['id']} records candidate {e['arm']}'s digest as the round's image")
    # Runner, 2026-11-27: layouts only shrink (L3 → L2 or L2b → S), so each later attempt that
    # isn't a solo confirmation drops an arm, and the round can't loop through fresh attempts.
    # A solo confirmation (S) is one arm from the attempt before it, outside that chain.
    # L2c follows the chain's L3 attempt (B dropped); a solo confirmation between them is
    # outside the chain, so L3 → S → L2c holds.
    arms_of = {a["id"]: {e["arm"] for e in arms if e["attempt"] == a["id"]} for a in attempts}
    chain, chain_layout = None, None
    for a in attempts:
        own = arms_of[a["id"]]
        if chain is not None and a["layout"] == "S" and len(own) == 1 and own <= chain:
            continue
        require(a["layout"] != "L2c" or chain_layout == "L3",
                f"declaration: attempt {a['id']}'s layout L2c follows only an L3 attempt (B dropped)")
        require(chain is None or own < chain,
                f"declaration: attempt {a['id']} must drop an arm from the attempt before it (layouts only shrink)")
        chain, chain_layout = own, a["layout"]
    return decl


def load_manifest(path):
    """The manifest (§5.2 item 12): every input file with its SHA-256. A listed file that's
    absent or whose hash differs is an error; an input the manifest doesn't list is judged
    missing. Returns the loaded inputs and each path's hash, in manifest order."""
    path = Path(path)
    man = read_json(path)
    require(isinstance(man, dict) and man.get("schema") == 1 and man.get("kind") == "cr_round1_manifest",
            "manifest: schema 1, kind cr_round1_manifest")
    base = path.resolve().parent
    hashes = {}

    def load(ref):
        require(isinstance(ref, dict) and isinstance(ref.get("path"), str) and isinstance(ref.get("sha256"), str),
                "manifest: each input is {path, sha256}")
        rel = Path(ref["path"])
        target = (base / rel).resolve()
        require(not rel.is_absolute() and target.is_relative_to(base), f"manifest: {ref['path']} is outside the archive")
        data = target.read_bytes()
        digest = hashlib.sha256(data).hexdigest()
        require(digest == ref["sha256"], f"manifest: {ref['path']}'s SHA-256 differs from the manifest's")
        hashes[ref["path"]] = digest
        return json.loads(data.decode("utf-8"))

    out = {"declaration": check_declaration(load(man.get("declaration"))),
           "comparator": load(man["comparator"]) if man.get("comparator") is not None else None, "days": []}
    # Runner, 2026-11-27 (§2.19, flagged to the owner): a comparator run by a collector that
    # predates round 1 has no binding; the coordinator binds it after its window
    # (`bind-comparator`), and the manifest carries that binding. A report's own binding wins.
    out["comparator_binding"] = None
    if man.get("comparator_binding") is not None:
        bind = load(man["comparator_binding"])
        require(isinstance(bind, dict) and bind.get("kind") == "cr_round1_comparator_binding"
                and bind.get("bound_after_window") is True and RUN_ID.fullmatch(str(bind.get("run_id"))) is not None
                and HASH.fullmatch(str(bind.get("host_identity"))) is not None
                and isinstance(bind.get("network_identity"), dict) and isinstance(bind.get("ended_utc"), str),
                "manifest: comparator_binding {kind cr_round1_comparator_binding, run_id, host_identity, "
                "network_identity, ended_utc, bound_after_window: true}")
        out["comparator_binding"] = bind
        if out["comparator"] is not None and not (out["comparator"].get("binding") or {}).get("run_id"):
            out["comparator"] = dict(out["comparator"], binding={
                "run_id": bind["run_id"], "host_identity": bind["host_identity"],
                "network_identity": bind["network_identity"], "ended_utc": bind["ended_utc"],
                "placement": None, "bound_after_window": True})
    declared = {a["id"] for a in out["declaration"]["attempts"]}
    ends = man.get("attempt_ends") or {}
    require(isinstance(ends, dict) and all(k in declared and isinstance(v, dict) and isinstance(v.get("reason"), str)
                                           and isinstance(v.get("days_run"), list)
                                           and all(type(x) is int for x in v["days_run"])
                                           and v["days_run"] in ([], [1], [1, 2]) for k, v in ends.items()),
            "manifest: attempt_ends maps a declared attempt to {reason, days_run: [], [1] or [1, 2]}")
    out["attempt_ends"] = ends
    for d in man.get("days") or []:
        require(isinstance(d, dict) and d.get("day") in (1, 2) and isinstance(d.get("attempt"), str)
                and type(d.get("repeat", 0)) is int and d.get("repeat", 0) >= 0,
                "manifest: each day has its attempt, day and repeat (an integer from 0)")
        require(d["attempt"] in declared, f"manifest: attempt {d['attempt']} isn't in the declaration")
        out["days"].append({
            "attempt": d["attempt"], "day": d["day"], "repeat": d.get("repeat", 0),
            "reports": {arm: load(ref) for arm, ref in sorted((d.get("reports") or {}).items())},
            "archive": load(d["archive"]) if d.get("archive") is not None else None,
            "contention": load(d["contention"]) if d.get("contention") is not None else None})
    listed = {(d["attempt"], d["day"]) for d in out["days"]}
    # Runner (2026-11-27): days_run lists the days the attempt ran, each listed; [] means it
    # never started, so no day of it may be listed. The verdict echoes each entry, and the
    # manifest's hash makes the claim auditable against the run log.
    for k, v in ends.items():
        require(all((k, day) in listed for day in v["days_run"]),
                f"manifest: attempt_ends says {k} ran days {v['days_run']}, but not all are listed")
        require(v["days_run"] or not any(a == k for a, _ in listed),
                f"manifest: attempt_ends says {k} never started, but days of it are listed")
    out["inputs"] = hashes
    out["manifest_sha256"] = hashlib.sha256(path.read_bytes()).hexdigest()
    return out


def utc(text):
    try:
        return parse_utc(text)
    except (TypeError, ValueError):
        return None


def nearest_rank(values, p):
    """Nearest-rank percentile (§6.3); None stands for a missing interval and ranks over any value."""
    ordered = sorted(values, key=lambda v: (v is None, v if v is not None else 0))
    if not ordered:
        return None
    v = ordered[max(0, math.ceil(p * len(ordered)) - 1)]
    return math.inf if v is None else v


DECLARED_VCPUS = 8
D3_KINDS = ("mover", "parity")  # array operations D3 exempts on an ssd_apps window


def contention(samples, arm, ws, we, device, vcpus=DECLARED_VCPUS):
    """§6.3's contention verdict for one candidate window [ws, we): `ok`, `breach` or
    `not_judged` (a series under 98 % coverage). Every 10 s interval is read; a missing one
    counts over the threshold. Uplink drops are a cumulative counter, read as each hour's delta."""
    c = CONTENTION
    if not isinstance(samples, dict) or samples.get("interval_seconds") != c["interval_seconds"]:
        return {"verdict": "not_judged", "reasons": ["contention_samples_missing"]}
    if samples.get("backing_device") != device:  # §6.3: /data latency on the declared device
        return {"verdict": "not_judged", "reasons": ["backing_device_differs"]}
    if samples.get("vcpus") != vcpus:  # item J: the CPU shares are of the declared vCPUs
        return {"verdict": "not_judged", "reasons": ["vcpus_differ"]}
    step = c["interval_seconds"]
    # Every interval that starts inside the window, the last one possibly partial (a window is
    # boundary scrape to boundary scrape, rarely a multiple of 10 s).
    n = math.ceil((we - ws).total_seconds() / step)
    slots = [None] * n
    for s in samples.get("samples") or []:
        t = utc(s.get("utc"))
        if t is None or not ws <= t < we:
            continue
        k = min(n - 1, int((t - ws).total_seconds() // step))
        if slots[k] is None:
            slots[k] = s
    median = (samples.get("rehearsal_rtt_median_ms") or {}).get(arm)

    def series(read):
        vals = [None if s is None else read(s) for s in slots]
        return vals, sum(v is not None for v in vals) / n if n else 0.0

    def num(v):
        return v if number(v) else None
    cpu, cpu_cov = series(lambda s: num(s.get("cpu")))
    steal, steal_cov = series(lambda s: num(s.get("steal")))
    tunnel = [None if s is None else (s.get("tunnels") or {}).get(arm) for s in slots]
    tun_cov = sum(isinstance(t, dict) and isinstance(t.get("lost"), bool) for t in tunnel) / n if n else 0.0
    up, up_cov = series(lambda s: num(s.get("uplink_drops_total")))
    # A counter that goes down was reset: the drops across the reset can't be read.
    readings = [v for v in up if v is not None]
    if any(b < a for a, b in zip(readings, readings[1:])):
        return {"verdict": "not_judged", "reasons": ["uplink_counter_reset"], "intervals": n}
    # Disk: no completed write in an interval is excluded with nothing in flight, over otherwise.
    disk, disk_seen = [], 0
    for s in slots:
        if s is None or not number(s.get("disk_inflight")):
            disk.append(None)
            continue
        disk_seen += 1
        if number(s.get("disk_write_ms")):
            disk.append(s["disk_write_ms"])
        elif s["disk_inflight"] > 0:
            disk.append(None)
    coverage = {"cpu": cpu_cov, "steal": steal_cov, "disk": disk_seen / n if n else 0.0,
                "tunnel": tun_cov, "uplink": up_cov}
    out = {"intervals": n, "coverage": coverage}
    low = sorted(k for k, v in coverage.items() if v < c["coverage_min"])
    if low or not number(median) or median <= 0:
        out.update(verdict="not_judged", reasons=["coverage_below_98pct:" + k for k in low]
                   + ([] if number(median) and median > 0 else ["rehearsal_rtt_median_missing"]))
        return out
    out.update(cpu_p95=nearest_rank(cpu, 0.95), steal_p95=nearest_rank(steal, 0.95),
               disk_write_p99_ms=nearest_rank(disk, 0.99) if disk else 0.0)
    breaches = []
    if out["cpu_p95"] > c["cpu_p95_max"]:
        breaches.append("cpu")
    if out["steal_p95"] > c["steal_p95_max"]:
        breaches.append("steal")
    if out["disk_write_p99_ms"] > c["disk_write_p99_ms_max"]:
        breaches.append("disk")
    # The window's hours: a final partial interval (the boundary scrape's lag) joins hour 23.
    per_hour = 3600 // step
    count = max(1, round(n * step / 3600))
    hours = []
    for h in range(count):
        lo, hi = h * per_hour, n if h == count - 1 else (h + 1) * per_hour
        part = tunnel[lo:hi]
        lost = sum(not (isinstance(t, dict) and t.get("lost") is False) for t in part)
        rtts = [t.get("rtt_ms") if isinstance(t, dict) and t.get("lost") is False and number(t.get("rtt_ms"))
                else None for t in part]
        # The counter from the hour's last reading before it (or its first) to its last.
        drops = [v for v in up[max(0, lo - 1):hi] if v is not None]
        rec = {"hour": h, "loss": lost / len(part), "rtt_p95_ms": nearest_rank(rtts, 0.95),
               "uplink_drops": sum(max(0, b - a) for a, b in zip(drops, drops[1:])) if len(drops) >= 2 else None}
        rec["ok"] = (rec["loss"] <= c["tunnel_loss_max"]
                     and rec["rtt_p95_ms"] <= c["tunnel_rtt_p95_over_rehearsal_max"] * median
                     and rec["uplink_drops"] is not None and rec["uplink_drops"] <= c["uplink_drops_max"])
        hours.append(rec)
    if not all(h["ok"] for h in hours):
        breaches.append("network")
    out["network_hours_breached"] = [h["hour"] for h in hours if not h["ok"]]
    out.update(verdict="breach" if breaches else "ok", reasons=breaches)
    return out


def crawler_tests(report):
    """§6.4's three tests on one valid window (the existing gate's arithmetic: the mean and
    hours net of carry-in; the latency cohort keeps carry-in and charges dropped samples)."""
    g = gate(report)
    return {"mean_net_per_min": g["mean_net_per_min"], "mean_passes": g["mean_passes"],
            "worst_hour_net_per_min": g["worst_hour_net_per_min"], "every_hour_passes": g["every_hour_passes"],
            "p95": g["p95"], "p95_seconds": g["p95_seconds"], "coverage": g["coverage"],
            "passes": g["mean_passes"] and g["every_hour_passes"] and g["p95"] == "pass"}


def memory_verdict(report):
    fit = report.get("memory") if isinstance(report, dict) else None
    meets = (fit or {}).get("meets_bar")
    out = {"verdict": "pass" if meets is True else ("fail" if meets is False else "not_judged")}
    if fit:
        out.update(slope_mib_per_hour=fit.get("slope_mib_per_hour"), ci95_mib_per_hour=fit.get("ci95_mib_per_hour"),
                   refused=fit.get("refused"), window_complete=fit.get("window_complete"))
    else:
        out["refused"] = "no_fit"
    return out


def comparator_problems(comp, decl):
    """§5.2 item 10: the booked, valid, complete, uninterrupted comparator on the expected
    host and network."""
    if comp is None:
        return ["comparator_missing"]
    out = []
    want = decl["comparator"]
    b = comp.get("binding") or {}
    t = comp.get("timeline") or {}
    if b.get("run_id") != want["run_id"]:
        out.append("comparator_not_booked")
    if comp.get("kind") != "comparator":
        out.append("comparator_kind")
    if comp.get("verdict") != "valid":
        out.append("comparator_" + str(comp.get("verdict")))
    if comp.get("rehearsal") or comp.get("window_hours") != 24:
        out.append("comparator_not_a_24h_window")
    if b.get("host_identity") != want.get("host_identity", decl["host_identity"]):
        out.append("comparator_host_identity")
    if b.get("network_identity") != want.get("network_identity", decl["network_identity"]):
        out.append("comparator_network_identity")
    end, ended = utc(t.get("window_end_utc")), utc(b.get("ended_utc"))
    if end is None or ended is None or ended < end:
        out.append("comparator_interrupted")
    return out


def day_entries(decl, attempt, day):
    return {e["arm"]: e for e in decl["arms"] if e["attempt"] == attempt and e["day"] == day}


def layout_problems(attempt, decl, day):
    """§6.3: the day's declared arms and exits are the attempt's layout exactly."""
    entries = day_entries(decl, attempt["id"], day)
    placed = {arm: e["exit"] for arm, e in entries.items()}
    layout = LAYOUTS[attempt["layout"]]
    if layout is None:  # S: the candidate alone, on a different exit each day
        other = day_entries(decl, attempt["id"], 3 - day)
        if len(placed) != 1 or len(other) != 1 or set(placed) != set(other) \
                or next(iter(placed.values())) == next(iter(other.values()))["exit"]:
            return ["layout_S_is_one_arm_on_two_exits"]
        return []
    return [] if placed == layout[day] else ["layout_" + attempt["layout"] + "_not_as_declared"]


def arm_window(report):
    t = (report or {}).get("timeline") or {}
    return utc(t.get("window_start_utc")), utc(t.get("window_end_utc"))


def exit_in_force(archive, exit_label, at):
    """The exit's hash as checked last at or before `at` (the archive's `exit_checks`, a timed
    history: a repair before the start can re-check a server); the archive's `exits` without one."""
    checks = [c for c in (archive.get("exit_checks") or []) if isinstance(c, dict) and utc(c.get("utc")) is not None]
    before = [c for c in checks if utc(c["utc"]) <= at] if at is not None else []
    if before:
        return (before[-1].get("exits") or {}).get(exit_label)
    if checks:
        return (checks[0].get("exits") or {}).get(exit_label)
    return ((archive or {}).get("exits") or {}).get(exit_label)


def egress_problems(archive, arm, exit_label, report):
    """§5.2 item 4: the arm's salted egress hash equals its exit's at every boundary of its window.
    Each reading is judged against the exit in force at its time; inside the window, that's the
    exit checked last before the window's start (X-cr-round1-collect-fixes, item C)."""
    ws, we = arm_window(report)
    if ws is None or we is None:
        return ["egress_window_unknown"]
    exits = (archive or {}).get("exits") or {}
    want = exit_in_force(archive or {}, exit_label, ws)
    if not isinstance(want, str) or not HASH.fullmatch(want) or exits.get(exit_label) != want:
        return ["egress_assignment_missing"]
    readings = ((archive.get("arms") or {}).get(arm) or {}).get("egress") or []
    hours = report.get("window_hours") or 24
    hour = (we - ws) / hours
    out = []
    for k in range(hours + 1):
        b = ws + k * hour
        got = [r.get("hash") for r in readings if (t := utc(r.get("utc"))) is not None
               and b - timedelta(seconds=BOUNDARY_LATE) <= t <= b + timedelta(seconds=BOUNDARY_LATE)]
        if not got:
            out.append("egress_unchecked_at_a_boundary")
        elif any(g != want for g in got):
            out.append("egress_differs_from_declared_exit")
    for r in readings:
        t = utc(r.get("utc"))
        expected = want if t is None or t >= ws else exit_in_force(archive, exit_label, t)
        if r.get("hash") != expected:
            out.append("egress_differs_from_declared_exit")
    return sorted(set(out))


def judge_day(cand, attempt, decl, day, comparator_ok):
    """One candidate's day: `judged` (with its tests) or `not_judged`, with reasons; also
    whether contention calls for a solo confirmation."""
    reports, archive = day["reports"], day["archive"] or {}
    entries = day_entries(decl, attempt["id"], day["day"])
    reasons = list(layout_problems(attempt, decl, day["day"]))
    out = {"attempt": attempt["id"], "day": day["day"], "repeat": day["repeat"], "solo_confirmation_required": False}
    rep = reports.get(cand)
    if rep is None:
        out.update(status="not_judged", reasons=sorted(set(reasons + ["candidate_report_missing"])))
        return out
    out["run_id"] = (rep.get("binding") or {}).get("run_id")
    entry = entries.get(cand)
    # Validity (§6.4): a valid, non-rehearsal 24 h comb window, bound to its declaration.
    if rep.get("kind") != "comb" or rep.get("rehearsal") or rep.get("window_hours") != 24:
        reasons.append("not_a_24h_comb_window")
    if rep.get("verdict") != "valid":
        reasons.append("segment_" + str(rep.get("verdict")))
    b = rep.get("binding") or {}
    place = b.get("placement") or {}
    want = {"round": decl["round"], "attempt": attempt["id"], "day": day["day"], "arm": cand}
    if entry is None or {k: place.get(k) for k in want} != want or place.get("repeat", 0) != day["repeat"]:
        reasons.append("report_not_bound_to_its_declaration")
    if (rep.get("round1") or {}).get("declared") != entry:
        reasons.append("report_judged_against_another_declaration")
    ident = rep.get("identity") or {}
    if ident.get("spec_commit") != decl["spec_commit"]:
        reasons.append("spec_commit_differs")
    if b.get("host_identity") != decl["host_identity"]:
        reasons.append("host_identity_differs")
    if b.get("network_identity") != decl["network_identity"]:
        reasons.append("network_identity_differs")
    if not RUN_ID.fullmatch(str(b.get("run_id"))):
        reasons.append("run_id_missing")
    digest = attempt["candidate_digests"].get(cand)
    if digest_part(ident.get("image_digest")) != digest_part(decl["image_digest"]) \
            or (digest is not None and digest != decl["image_digest"]):
        reasons.append("image_digest_differs")
    if (rep.get("round1") or {}).get("fresh_store") is not True:
        reasons.append("no_fresh_store_evidence")
    # The layout held: every declared arm reported, none beyond them, each on its own exit,
    # active from the candidate's ws through its final boundary, with its leak tests passing.
    ws, we = arm_window(rep)
    if set(reports) != set(entries):
        reasons.append("roster_differs_from_declaration")
    arms = archive.get("arms") or {}
    if not archive:
        reasons.append("day_archive_missing")
    exits = archive.get("exits") or {}
    if len(set(exits.values())) != len(exits):
        reasons.append("exits_share_an_egress_hash")
    for arm, e in sorted(entries.items()):
        info = arms.get(arm) or {}
        if arm != cand and arm not in reports:
            reasons.append("peer_report_missing")
        active_from, active_through = utc(info.get("active_from_utc")), utc(info.get("active_through_utc"))
        if ws is None or we is None or active_from is None or active_through is None \
                or active_from > ws or active_through < we:
            reasons.append("peer_not_active_through_candidate_window" if arm != cand else "activity_evidence_missing")
        leak = info.get("leak_test") or {}
        if leak.get("before") != "pass" or leak.get("after") != "pass":
            reasons.append("leak_test_not_passed")
        if arm in reports:
            reasons += egress_problems(archive, arm, e["exit"], reports[arm])
            pb = reports[arm].get("binding") or {}
            pp = pb.get("placement") or {}
            if arm != cand and ({k: pp.get(k) for k in want} != dict(want, arm=arm)
                                or pp.get("repeat", 0) != day["repeat"] or not RUN_ID.fullmatch(str(pb.get("run_id")))):
                reasons.append("peer_report_not_bound")
    runs = [(r.get("binding") or {}).get("run_id") for r in reports.values()]
    if len(set(runs)) != len(runs):
        reasons.append("duplicate_run_in_day")
    # Events (§6.3's classification): an infrastructure fault invalidates the affected arms'
    # day; a product failure on any arm breaks the layout for its peers.
    # A product failure is terminal only once diagnosed as one; a diagnosis that finds an
    # infrastructure cause reclassifies it (§6.2, §6.3). An unknown class is never ignored.
    out["product_failure"] = False
    for ev in archive.get("events") or []:
        hit, cls, diag = ev.get("arms") or [], ev.get("class"), ev.get("diagnosis")
        if diag not in DIAGNOSES or cls not in ("infrastructure", "product") or not isinstance(hit, list):
            reasons.append("unknown_event")
            continue
        if cls == "product" and diag == "infrastructure":
            cls = "infrastructure"
        if cls == "infrastructure":  # the whole concurrent day is repeated
            reasons.append("infrastructure_fault" if (not hit or cand in hit) else "peer_infrastructure_fault")
        elif cand in hit:
            reasons.append("product_failure" if diag == "product" else "product_failure_under_diagnosis")
            out["product_failure"] = out["product_failure"] or diag == "product"
        else:
            reasons.append("peer_product_failure")
    if not comparator_ok:
        reasons.append("comparator_not_valid")
    # Contention for this window (O4): a breach isn't a valid miss.
    if ws is not None and we is not None:
        out["contention"] = contention(day["contention"], cand, ws, we, decl["backing_device"], decl["vcpus"])
    else:
        out["contention"] = {"verdict": "not_judged", "reasons": ["window_unknown"]}
    if out["contention"]["verdict"] == "not_judged":
        reasons.append("contention_not_judged")
    elif out["contention"]["verdict"] == "breach":
        reasons.append("contention_breach")
        # §6.3: the first class that applies decides. A day with an evidenced infrastructure
        # fault (class 1) or a product failure (class 2) is that, not demonstrated contention,
        # so it calls for no solo confirmation (and a breach in layout S isn't its failure).
        classed = {"infrastructure_fault", "peer_infrastructure_fault", "product_failure",
                   "product_failure_under_diagnosis", "peer_product_failure", "unknown_event"} & set(reasons)
        out["contention_class"] = "infrastructure_or_product" if classed else "demonstrated_contention"
        out["solo_confirmation_required"] = not classed and attempt["layout"] != "S"
        out["solo_confirmation_failed"] = not classed and attempt["layout"] == "S"
    out["tests"] = crawler_tests(rep)
    out["memory"] = memory_verdict(rep)
    out["window"] = {"start_utc": (rep.get("timeline") or {}).get("window_start_utc"),
                     "end_utc": (rep.get("timeline") or {}).get("window_end_utc")}
    out["reasons"] = sorted(set(reasons))
    out["status"] = "not_judged" if reasons else ("pass" if out["tests"]["passes"] else "fail")
    return out


def candidate_days(inputs, decl, attempt, cand):
    """Every manifest day of `attempt` the candidate is declared in: {day: [versions by repeat]}."""
    out = {}
    for d in inputs["days"]:
        if d["attempt"] == attempt["id"] and cand in day_entries(decl, attempt["id"], d["day"]):
            out.setdefault(d["day"], []).append(d)
    return {k: sorted(v, key=lambda d: d["repeat"]) for k, v in sorted(out.items())}


# Problems that keep any day from counting, a valid miss included: the arm can't qualify, or
# the comparator or spacing that every window of the round depends on isn't there.
UNJUDGEABLE = ("not_authorized", "spacing", "retry_budget_exhausted", "earlier_attempt_incomplete",
               "repeats_not_contiguous", "run_id_reused")


def judge_candidate(cand, inputs):
    """§5.2 item 5: one candidate's crawler verdict, its per-day memory verdicts and
    shipping eligibility. Its two days pair only within its latest attempt; a valid miss in
    any attempt, or in a day a repeat followed, still ends the claim (§6.2, §6.4)."""
    decl = inputs["declaration"]
    entries = [e for e in decl["arms"] if e["arm"] == cand]
    out = {"arm": cand, "role": entries[0]["role"] if entries else None, "reasons": []}
    attempts = [a for a in decl["attempts"] if any(e["attempt"] == a["id"] for e in entries)]
    if not attempts:
        out.update(crawler="not_judged", memory="not_judged", shipping_eligible=False, reasons=["not_declared"])
        return out
    attempt = attempts[-1]
    out["attempt"], out["layout"] = attempt["id"], attempt["layout"]
    reasons = []
    if out["role"] != "candidate":
        reasons.append("not_authorized")  # a diagnostic arm is never eligible (§6.2, §6.4)
    comp = comparator_problems(inputs["comparator"], decl)
    out["comparator_problems"] = comp
    reasons += comp
    ok_comp = not comp
    # Every attempt's days must be in the manifest: an earlier attempt's declared days, or the
    # days its recorded end says ran; and each (attempt, day)'s repeats run 0..k.
    for a in attempts:
        listed = candidate_days(inputs, decl, a, cand)
        for k, vs in listed.items():
            if [d["repeat"] for d in vs] != list(range(len(vs))):
                reasons.append("repeats_not_contiguous")
        if a is attempt:
            continue
        end = inputs.get("attempt_ends", {}).get(a["id"])
        need = set(end["days_run"]) if end else {e["day"] for e in entries if e["attempt"] == a["id"]}
        if not need <= set(listed):
            reasons.append("earlier_attempt_incomplete")

    # Earlier attempts: reported, and a valid miss in one still counts.
    earlier = []
    for a in attempts[:-1]:
        for k, versions in candidate_days(inputs, decl, a, cand).items():
            earlier += [dict(judge_day(cand, a, decl, d, ok_comp), layout=a["layout"],
                             digest=a["candidate_digests"].get(cand)) for d in versions]
    out["earlier_attempts"] = [{"attempt": j["attempt"], "day": j["day"], "repeat": j["repeat"],
                                "status": j["status"], "reasons": j["reasons"]} for j in earlier]
    out["attempt_ends"] = {a["id"]: inputs["attempt_ends"][a["id"]] for a in attempts
                           if a["id"] in inputs.get("attempt_ends", {})}
    # This attempt: a repeated day replaces the one it repeats only if that one wasn't judged
    # (§6.3 repeats a day after an evidenced fault); a judged miss stays a miss.
    versions = candidate_days(inputs, decl, attempt, cand)
    replaced, judged, days = [], [], {}
    for k, vs in versions.items():
        if len({d["repeat"] for d in vs}) != len(vs):
            reasons.append("repeat_supplied_twice")
        js = [dict(judge_day(cand, attempt, decl, d, ok_comp), layout=attempt["layout"],
                   digest=attempt["candidate_digests"].get(cand)) for d in vs]
        replaced += js[:-1]
        judged.append(js[-1])
        days[k] = vs[-1]
    if any(j["status"] == "pass" for j in replaced):
        reasons.append("repeat_of_a_judged_day")
    # Only an evidenced infrastructure fault repeats a day (§6.3, event class 1): contention
    # calls for a solo confirmation, a product failure for a diagnosis, never a repeat.
    if any(j["status"] == "not_judged" and not {"infrastructure_fault", "peer_infrastructure_fault"} & set(j["reasons"])
           for j in replaced):
        reasons.append("repeat_without_infrastructure_fault")
    out["replaced_days"] = [{"day": j["day"], "repeat": j["repeat"], "status": j["status"], "reasons": j["reasons"]}
                            for j in replaced]
    declared_days = sorted({e["day"] for e in entries if e["attempt"] == attempt["id"]})
    if declared_days != [1, 2]:
        reasons.append("attempt_does_not_declare_two_days")
    if sorted(days) != [1, 2]:
        reasons.append("day_missing")
    # Binding across the two days: distinct runs, separate windows, two different exits.
    runs = [j.get("run_id") for j in judged]
    if len(runs) == 2 and (runs[0] == runs[1] or None in runs):
        reasons.append("duplicate_run")
    if len(judged) == 2:
        w = [(utc(j["window"]["start_utc"]), utc(j["window"]["end_utc"])) if "window" in j else (None, None)
             for j in judged]
        if any(x is None for pair in w for x in pair) or not w[0][1] <= w[1][0]:
            reasons.append("windows_overlap_or_out_of_order")
        exits = [day_entries(decl, attempt["id"], k)[cand]["exit"] for k in (1, 2)]
        hashes = [((days[k]["archive"] or {}).get("exits") or {}).get(exits[k - 1]) for k in (1, 2)]
        if exits[0] == exits[1] or hashes[0] is None or hashes[0] == hashes[1]:
            reasons.append("repeated_exit")
    # Rule 2.2 spacing against every Lodestar window in the round, from the comparator's
    # timeline.window_end_utc (never result.json's `ended`).
    if inputs["comparator"] is not None:
        timelines = [r.get("timeline") or {} for d in inputs["days"] for r in d["reports"].values()]
        problems = spacing(inputs["comparator"], timelines)
        # §6.3: the round follows its comparator. Every Lodestar process and window starts at
        # or after the comparator's window end + 12 h (a comparator after the round is no use).
        cend = utc((inputs["comparator"].get("timeline") or {}).get("window_end_utc"))
        if cend is None or any(utc(t.get(k)) is None or utc(t.get(k)) < cend + timedelta(seconds=COMPARATOR_CLEAR)
                               for t in timelines for k in ("process_start_utc", "window_start_utc")):
            problems = sorted(set(problems) | {"lodestar_before_comparator_end_plus_12h"})
        out["spacing_problems"] = problems
        if problems:
            reasons.append("spacing")
    # §6.3: at most two repeated days per round, whatever the cause (a solo confirmation's
    # own two days are attempts of their own, repeat 0, so they never count).
    # Run IDs are unique across the whole manifest, the comparator's included.
    ids = [(r.get("binding") or {}).get("run_id") for d in inputs["days"] for r in d["reports"].values()]
    ids += [((inputs["comparator"] or {}).get("binding") or {}).get("run_id")] if inputs["comparator"] else []
    if len(set(ids)) != len(ids):
        reasons.append("run_id_reused")
    # A day listed only as repeat k still implies k repeats (the gap itself is not judged).
    top = {}
    for d in inputs["days"]:
        top[(d["attempt"], d["day"])] = max(top.get((d["attempt"], d["day"]), 0), d["repeat"])
    repeats = sum(top.values())
    out["retry_budget_exhausted"] = repeats > RETRY_BUDGET
    if out["retry_budget_exhausted"]:
        reasons.append("retry_budget_exhausted")
    out["days"] = judged
    every = earlier + replaced + judged
    # History across attempts (§6.3). A breach outside S needs a solo confirmation after it;
    # a failed solo confirmation has no further retry; a product failure on this digest,
    # diagnosed or pending, withholds qualification (a fix makes a new candidate).
    breached = {j["attempt"] for j in every if j.get("solo_confirmation_required")}
    out["solo_confirmation_required"] = bool(breached) and not (attempt["layout"] == "S" and attempt["id"] not in breached)
    if out["solo_confirmation_required"]:
        reasons.append("solo_confirmation_required")
    out["solo_confirmation_failed"] = any(j.get("solo_confirmation_failed") for j in every)
    if out["solo_confirmation_failed"]:
        reasons.append("solo_confirmation_failed")
    digest = attempt["candidate_digests"].get(cand)
    same = [j for j in every if j["digest"] == digest]
    out["product_failure"] = any(j.get("product_failure") for j in same)
    if any("product_failure" in j["reasons"] or "product_failure_under_diagnosis" in j["reasons"] for j in same):
        reasons.append("product_failure_on_this_digest")
    misses = [j for j in every if j["status"] == "fail"]
    out["valid_misses"] = [{"attempt": j["attempt"], "day": j["day"], "repeat": j["repeat"]} for j in misses]
    status = [j["status"] for j in judged]
    if any(r in UNJUDGEABLE for r in reasons) or comp:
        crawler = "not_judged"
    elif misses:  # a valid miss on either day ends the claim (§6.4), whatever is still missing
        crawler = "fail"
    elif reasons:
        crawler = "not_judged"
    else:
        crawler = "pass" if status == ["pass", "pass"] else "not_judged"
    out["reasons"] = sorted(set(reasons))
    out["crawler"] = crawler
    mem = [j["memory"]["verdict"] for j in judged if "memory" in j]
    out["memory"] = "pass" if mem == ["pass", "pass"] and len(days) == 2 else ("fail" if "fail" in mem else "not_judged")
    out["shipping_eligible"] = crawler == "pass" and out["memory"] == "pass"
    return out


def disposition(c):
    """§6.2: whether a candidate's outcome is terminal, and what it is."""
    if c.get("product_failure"):
        return "product_failure"
    if c.get("retry_budget_exhausted") or c.get("solo_confirmation_failed"):
        return "out_of_retries"
    if c["shipping_eligible"]:
        return "shipping_eligible"
    if c["crawler"] == "pass" and c["memory"] == "fail":
        return "crawler_accepted_memory_fails"
    if c["crawler"] == "fail":
        return "valid_miss"
    return None  # incomplete: the retry rule (§6.3)


def select(candidates, order):
    """§6.2's Balanced-first selection: the first choice's disposition must be terminal
    before the next is read; a diagnostic's result only selects, it never ships."""
    trail = []
    for arm in order:
        c = candidates.get(arm)
        if c is None:
            continue
        d = disposition(c)
        trail.append({"arm": arm, "disposition": d or "incomplete", "authorized": c["role"] == "candidate"})
        if c["role"] != "candidate":
            # Not authorized: its result only selects; if the owner then chooses it, it needs
            # a fresh two-day run (§6.2).
            trail[-1]["selects_only"] = True
            continue
        if d is None:
            return {"ships": None, "state": "pending_retry", "waiting_on": arm, "trail": trail}
        if d == "shipping_eligible":
            return {"ships": arm, "state": "ships", "trail": trail}
    return {"ships": None, "state": "round1_report", "trail": trail}


def gate_round(inputs):
    """§5.2 items 5 and 12: every arm's verdict and the selection, from one manifest's inputs."""
    decl = inputs["declaration"]
    arms = sorted({e["arm"] for e in decl["arms"]})
    judged = {arm: judge_candidate(arm, inputs) for arm in arms}
    out = {"schema": 1, "kind": "cr_round1_verdict", "spec": SPEC, "round1_spec": ROUND1_SPEC,
           "plan": "docs/plan/2026-11-27-revised-path-to-100.md",
           "round": decl["round"], "manifest_sha256": inputs["manifest_sha256"], "analyzer_sha256": ANALYZER_SHA256,
           "inputs": dict(sorted(inputs["inputs"].items())), "contention_thresholds": CONTENTION,
           "attempt_ends": dict(sorted(inputs.get("attempt_ends", {}).items())),
           "comparator_bound_after_window": bool(((inputs.get("comparator") or {}).get("binding") or {})
                                                 .get("bound_after_window")),
           "candidates": {a: j for a, j in judged.items() if j["role"] == "candidate"},
           "diagnostics": {a: j for a, j in judged.items() if j["role"] != "candidate"}}
    out["selection"] = select(judged, SELECTION_ORDER)
    return out


# §6.3's drop-C rule (plan lines 430-432), frozen in CR-SPEC-R1: tighter than contention's.
DROP_C = {"cpu_p95_max": 0.70, "steal_p95_max": 0.03, "minutes": 60, "crawling_lookback_minutes": 10}


def crawling_from(rows):
    """The first scrape from which the arm is crawling: sampling (suppression 0) and its store
    grew in the 10 min before. None if it never is."""
    look = timedelta(minutes=DROP_C["crawling_lookback_minutes"])
    seen = []
    for r in rows:
        t, vals = utc(r.get("time")), r.get("values") or {}
        if t is None or not r.get("metrics_ok") or not number(vals.get("torrents")):
            continue
        seen.append((t, vals["torrents"]))
        before = [v for u, v in seen if u <= t - look]
        if vals.get("dht_sample_suppression_seconds") == 0 and before and vals["torrents"] > before[-1]:
            return t
    return None


def drop_c(samples, arms):
    """§6.3's drop-C rule on the rehearsal: the last 60 min, starting only once every arm is
    crawling. C is dropped if host CPU p95 is over 70 % of the declared 8 vCPU or steal p95 over
    3 % (nearest rank over every 10 s slot; a missing slot ranks over). `samples` is the
    rehearsal's host samples (host_samples.py's lines or its assembled JSON); `arms` maps an
    arm to its rehearsal collector's samples.jsonl rows."""
    rows = samples.get("samples") if isinstance(samples, dict) else samples
    if isinstance(samples, dict):
        vcpus = {samples.get("vcpus")}
    else:  # the sampler's header lines: one per start, each naming the vCPUs it saw
        vcpus = {r.get("vcpus") for r in rows or [] if isinstance(r, dict) and r.get("kind") == "header"}
    rows = sorted((r for r in rows or [] if utc(r.get("utc")) is not None), key=lambda r: r["utc"])
    out = {"thresholds": DROP_C, "arms": {}}
    if vcpus != {DECLARED_VCPUS}:  # the shares are of the declared 8 vCPU, or they mean nothing
        return dict(out, decision="not_judged", reasons=["vcpus_differ"])
    if not rows:
        return dict(out, decision="not_judged", reasons=["no_host_samples"])
    end = utc(rows[-1]["utc"]) + timedelta(seconds=CONTENTION["interval_seconds"])
    starts = {}
    for arm, arm_rows in sorted(arms.items()):
        starts[arm] = crawling_from(arm_rows)
        out["arms"][arm] = {"crawling_from_utc": iso(starts[arm]) if starts[arm] else None}
    if not arms or any(v is None for v in starts.values()):
        return dict(out, decision="not_judged", reasons=["an_arm_never_crawled"])
    start = max(end - timedelta(minutes=DROP_C["minutes"]), max(starts.values()))
    minutes = (end - start).total_seconds() / 60
    out.update(interval_start_utc=iso(start), interval_end_utc=iso(end), interval_minutes=round(minutes, 2))
    if minutes < DROP_C["minutes"]:
        return dict(out, decision="not_judged", reasons=["fewer_than_60_crawling_minutes"])
    step = CONTENTION["interval_seconds"]
    n = math.ceil((end - start).total_seconds() / step)
    slots = [None] * n
    for r in rows:
        t = utc(r["utc"])
        if start <= t < end:
            k = int((t - start).total_seconds() // step)
            slots[k] = slots[k] or r
    cpu = [s.get("cpu") if s and number(s.get("cpu")) else None for s in slots]
    steal = [s.get("steal") if s and number(s.get("steal")) else None for s in slots]
    p_cpu, p_steal = nearest_rank(cpu, 0.95), nearest_rank(steal, 0.95)
    over = {"cpu": p_cpu > DROP_C["cpu_p95_max"], "steal": p_steal > DROP_C["steal_p95_max"]}
    # A missing slot ranks over every value, so a p95 that lands on one is over (shown as null).
    out.update(slots=n, missing_slots=sum(s is None for s in slots),
               cpu_p95=None if math.isinf(p_cpu) else p_cpu, steal_p95=None if math.isinf(p_steal) else p_steal,
               decision="drop" if any(over.values()) else "keep", reasons=[k for k, v in over.items() if v])
    return out


def read_jsonl(path):
    out = []
    for line in Path(path).read_text(encoding="utf-8").splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if isinstance(row, dict):
            out.append(row)
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


FINAL_OUTCOMES = ("win", "no_effect", "loss", "inconclusive_switch_stays_off", "no_opportunity")


def checkpoint1(utp, others, comparator, cr0):
    """Rule 6.9's projection, with both models and their intervals."""
    out = {"schema": 1, "spec": SPEC, "kind": "projection_not_measurement"}
    if any(e.get("analysis") == "noise_floor" or e.get("outcome") == "noise_floor" for e in [utp] + list(others)):
        out["error"] = NOISE_FLOOR_REFUSED  # X-fleet-baab
        return out
    if any(e.get("rehearsal") for e in [utp] + list(others)) or (comparator or {}).get("rehearsal"):
        out["error"] = "rehearsal evidence: not projected"
        return out
    if utp.get("treatment") != "utp" or any(e.get("treatment") not in ("adm", "route") for e in others):
        out["error"] = "the --utp report isn't uTP's, or another report isn't an A/B effect"
        return out
    pending = [e.get("treatment") for e in [utp] + list(others) if e.get("outcome") not in FINAL_OUTCOMES]
    if pending:  # rule 6.9 follows the outcomes
        out["error"] = "outcomes not final: " + ", ".join(pending)
        return out
    if comparator is not None:
        spaced = spacing(comparator, [t for e in [utp] + list(others) for t in timelines(e)])
        out["comparator_spacing_problems"] = spaced
        if comparator.get("verdict") != "valid" or spaced:  # rules 2.2, 5.4, D7
            out["comparator_not_used"] = (f"comparator verdict {comparator.get('verdict')}"
                                          if comparator.get("verdict") != "valid" else "rule 2.2 spacing")
            comparator = None
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
    p.add_argument("--winners", help="From stage 2: JSON file naming the earlier winners (r5.1 A2)")
    p.add_argument("--declaration", type=Path, help="Round 1: the round's declaration (judges the arm against it)")
    p = sub.add_parser("effect")
    p.add_argument("--treatment", choices=sorted(collect.TREATMENT_SWITCHES), required=True)
    p.add_argument("--block", nargs=4, action="append", required=True, metavar="REPORT")
    p.add_argument("--winners", help="From stage 2: JSON file naming the earlier winners (r5.1 A2)")
    p.add_argument("--design", action="append", choices=sorted(DESIGNS),
                   help="Each block's recorded order, one per --block, in the same order")
    p.add_argument("--aa", action="store_true", help="An A/A run: report the noise floor (needs --design)")
    p.add_argument("--output", type=Path)
    p = sub.add_parser("route-baseline")
    p.add_argument("reports", nargs="+")
    p = sub.add_parser("gate")
    p.add_argument("comb")
    p.add_argument("--comparator")
    p = sub.add_parser("gate-round")
    p.add_argument("manifest", type=Path, help="Round 1's manifest: every input with its SHA-256")
    p.add_argument("--output", type=Path)
    p = sub.add_parser("drop-c")
    p.add_argument("samples", type=Path, help="The rehearsal's host samples (host-samples.jsonl or its assembled JSON)")
    p.add_argument("--arm", action="append", default=[], metavar="ARM=RUN_DIR",
                   help="Each rehearsal arm's collector run directory (repeat for every arm)")
    p.add_argument("--output", type=Path)
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
            report = analyze_segment(args.root, load_winners(args.winners) if args.winners else None,
                                     check_declaration(read_json(args.declaration)) if args.declaration else None)
            if args.publish:
                publish(report, args.publish)
            write(report, args.output)
        elif args.command == "effect":
            write(analyze_effect(args.treatment, [[load_report(p) for p in block] for block in args.block],
                                 load_winners(args.winners) if args.winners else None, args.design, args.aa),
                  args.output)
        elif args.command == "route-baseline":
            write(route_baseline([load_report(p) for p in args.reports]), None)
        elif args.command == "drop-c":
            text = args.samples.read_text(encoding="utf-8")
            samples = json.loads(text) if args.samples.suffix == ".json" else read_jsonl(args.samples)
            arms = {}
            for item in args.arm:
                arm, _, run_dir = item.partition("=")
                arms[arm] = read_jsonl(Path(run_dir) / "samples.jsonl")
            write(drop_c(samples, arms), args.output)
        elif args.command == "gate-round":
            write(gate_round(load_manifest(args.manifest)), args.output)
        elif args.command == "gate":
            write(gate(refuse_noise_floor(load_report(args.comb)),
                       load_report(args.comparator) if args.comparator else None), None)
        else:
            adm = refuse_noise_floor(load_report(args.adm))
            if adm.get("treatment") != "adm":
                raise ValueError("--adm must be the CR-ADM effect report")
            write(checkpoint1(refuse_noise_floor(load_report(args.utp)),
                              [adm] + [refuse_noise_floor(load_report(p)) for p in args.other],
                              load_report(args.comparator), load_report(args.cr0_report)), None)
    except (OSError, ValueError, KeyError) as error:
        print(f"Cannot analyze evidence: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
