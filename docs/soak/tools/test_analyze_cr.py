"""Fixtures that pass and that fail for each CR-SPEC rule analyze_cr.py judges.

Segments are written as the Linux collector writes them (run.json, samples.jsonl,
metrics/NNNNN.prom, result.json, activity.jsonl) from a small simulation, so
every rule is exercised through the same files a real run produces.
"""

import json
import math
import sys
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE / "linux"))

import analyze_cr  # noqa: E402
import collect  # noqa: E402

T0 = datetime(2026, 11, 3, 6, 0, tzinfo=timezone.utc)  # the collector's elapsed 0
START = T0.timestamp() - 2  # the process started 2 s before the collector
DIGEST = "sha256:" + "a" * 64
COMMIT = "b" * 40
EDGES = collect.LATENCY_EDGES


class Sim:
    """A Lodestar segment: rates per minute, a latency per sample, and fault hooks."""

    def __init__(self, kind="ab", treatment="utp", switches=None, hours=3, hour_s=600, warm_min=10,
                 rehearsal=True, sample=60, rate=120.0, latency=25, suppressed_until=0, reason="resumed"):
        self.kind, self.treatment, self.hours, self.hour_s = kind, treatment, hours, hour_s
        self.warm_min, self.rehearsal, self.sample, self.rate, self.latency = warm_min, rehearsal, sample, rate, latency
        self.suppressed_until, self.reason = suppressed_until, reason
        self.switches = {name: False for name in collect.SWITCH_ENV}
        self.switches["safe_restart"] = True
        self.switches.update(switches or {})
        self.dropped_share = 0.0
        self.count_share = 1.0
        self.utp_in = 50.0 if self.switches["utp"] else 0.0
        self.grants = 10.0 if self.switches["coordinated_admission"] else 0.0
        self.permit_leak = None  # (from_e, to_e): an unreturned permit
        self.route_skipped = 3.0 if self.switches["route_backoff"] else 0.0
        self.probe_down = 0.0
        self.import_rendered = False
        self.edits = []  # (predicate(e), fn(row, lines)) applied per scrape
        self.drop = lambda e: False
        self.drift_from = None
        self.ip = {}
        self.api = {"intensity": "balanced", "paused": False}
        self.imports = {"import_started": 0, "import_finished": 0, "intensity_changes": 0, "paused": 0}
        self.config_extra = {}
        self.checklist = {"auto_update": {"state": "excluded", "next_run_utc": "2026-11-10T03:00:00Z"},
                          "backup": {"state": "excluded", "next_run_utc": "2026-11-09T03:00:00Z"}}
        self.witness = {"configured": True, "host_mounted": True, "outside_data_dir": True}
        self.ntp = {"source": "chronyc", "synchronized": True, "offset_seconds": 0.002}
        self.activity = []
        self.result = {"reason": "baseline_window_complete", "stop_mode": "exited_0",
                       "shutdown": {"exit_code": 0, "signal_delivered": True, "errors": [], "exit_confirmed": True}}
        self.first_off = False  # the DHT's pre-report default at the first scrape

    # -- cumulative counters at elapsed e
    def searchable(self, e):
        open_e = max(self.suppressed_until, 0)
        return max(0.0, (e - open_e) / 60 * self.rate) if e > open_e else 0.0

    def snapshot(self, e, row):
        s = math.floor(self.searchable(e))
        dropped = math.floor(s * self.dropped_share)
        count = math.floor((s - dropped) * self.count_share)
        lines = []
        add = lambda name, value, labels="": lines.append(f"lodestar_{name}{labels} {int(value)}")
        add("searchable_new_total", s, '{origin="network"}')
        add("searchable_new_total", 0, '{origin="recovery"}')
        add("metadata_new_verified_total", s + 3, '{origin="network"}')
        add("metadata_new_verified_total", 0, '{origin="recovery"}')
        if self.import_rendered:
            add("metadata_new_verified_total", 1, '{origin="import"}')
        for edge in EDGES:
            hit = edge == "+Inf" or float(edge) >= self.latency
            add("discovery_to_searchable_seconds_bucket", count if hit else 0, f'{{le="{edge}"}}')
        add("discovery_to_searchable_seconds_count", count)
        add("discovery_to_searchable_seconds_sum", count * self.latency)
        add("discovery_to_searchable_dropped_total", dropped)
        for reason in ("already_stored", "rediscovered", "filtered"):
            add("metadata_excluded_total", math.floor(e / 30), f'{{reason="{reason}"}}')
        add("fetch_failures_total", math.floor(e / 20), '{kind="timed_out"}')
        add("fetch_lost_total", math.floor(e / 40), '{reason="no_peers"}')
        add("fetch_attempts_total", math.floor(e * 2), '{transport="tcp"}')
        add("fetch_attempts_total", math.floor(e / 60 * self.utp_in), '{transport="utp"}')
        add("fetch_success_total", math.floor(e), '{transport="tcp"}')
        add("fetch_success_total", 0, '{transport="utp"}')
        for fam in ("ipv4", "ipv6"):
            add("fetch_family_attempts_total", math.floor(e * 2) if fam == "ipv4" else 0, f'{{family="{fam}"}}')
            add("fetch_family_success_total", math.floor(e) if fam == "ipv4" else 0, f'{{family="{fam}"}}')
            add("fetch_no_route_total", 0, f'{{family="{fam}"}}')
            add("fetch_route_probe_down_total", math.floor(e / 60 * self.probe_down) if fam == "ipv6" else 0,
                f'{{family="{fam}"}}')
            add("fetch_route_skipped_total", math.floor(e / 60 * self.route_skipped) if fam == "ipv6" else 0,
                f'{{family="{fam}"}}')
        add("dht_utp_in_total", math.floor(e / 60 * self.utp_in))
        if self.switches["utp"]:
            add("utp_retransmits_total", math.floor(e / 10))
            add("utp_connection_timeouts_total", math.floor(e / 100))
        grants = math.floor(e / 60 * self.grants)
        leak = 1 if self.permit_leak and self.permit_leak[0] <= e < self.permit_leak[1] else 0
        add("fetch_admission_grants_total", grants, '{kind="arrival",class="background"}')
        add("dht_peer_permit_returns_total", grants - leak, '{boundary="completed"}')
        add("fetch_admission_permit_returns_total", 0, '{boundary="finder_full"}')
        for name in ("fetch_admission_waits_total", "fetch_admission_wait_cancelled_total",
                     "fetch_admission_wakeups_total", "fetch_admission_refusals_total",
                     "fetch_admission_holds_total"):
            add(name, 0)
        add("fetch_admission_waiters", 0)
        add("fetch_admission_permits_in_use", 0)
        suppression = max(0, self.suppressed_until - e)
        reason = "off" if (self.first_off and row == 0) else self.reason
        add("dht_sample_suppression_seconds", 0 if (self.first_off and row == 0) else suppression)
        for r in ("off", "first_start", "resumed"):
            add("dht_sample_suppression_reason", int(r == reason), f'{{reason="{r}"}}')
        for name in ("dht_peer_active", "dht_peer_queued", "fetch_lookups"):
            add(name, 2)
        for name in analyze_cr.CORE:
            if name not in ("fetch_attempts_total", "fetch_success_total"):
                add(name, math.floor(e))
        for name in ("dht_timeouts_total", "dht_throttled_total", "dht_sample_deferred_total",
                     "dht_peer_admitted_total", "dht_peer_rejected_total"):
            if not any(l.startswith(f"lodestar_{name} ") for l in lines):
                add(name, math.floor(e / 3))
        add("dht_peer_rejection_reasons_total", 0, '{reason="capacity"}')
        add("start_time_seconds", START)
        return lines

    def boundaries(self, times):
        """The collector's own T_open and boundary scrapes for these sample times."""
        open_e = None
        for i, e in enumerate(times):
            if self.kind == "comparator":
                break
            if not (self.first_off and i == 0) and e >= self.suppressed_until:
                open_e = e
                break
        if self.kind == "comparator":
            ws = (START - T0.timestamp()) + self.warm_min * 60
        elif open_e is None:
            return {}
        else:
            ws = open_e + self.warm_min * 60
        out = {}
        k = 0
        for i, e in enumerate(times):
            passed = []
            while k <= self.hours and e >= ws + k * self.hour_s:
                passed.append(k)
                k += 1
            if passed:
                out[i] = passed
        return out

    def write(self, root, extra_seconds=120):
        root = Path(root)
        (root / "metrics").mkdir(parents=True)
        total = self.suppressed_until + self.warm_min * 60 + self.hours * self.hour_s + extra_seconds
        times = [5.0 + self.sample * i for i in range(int(total // self.sample) + 1)]
        times = [e for e in times if not self.drop(e)]
        marks = self.boundaries(times)
        rows = []
        for i, e in enumerate(times):
            utc = T0 + timedelta(seconds=e + (6.5 if self.drift_from is not None and e >= self.drift_from else 0))
            row = {"schema_version": 2, "sequence": i, "time": utc.isoformat().replace("+00:00", "Z"),
                   "elapsed_seconds": e, "phase": "baseline", "process_id": 1,
                   "process_identity": "c:2026-11-03T06:00:00Z", "metrics_ok": True,
                   "private_bytes": 200 << 20, "working_set_bytes": 300 << 20, "cpu_seconds": e / 10,
                   "net": {"rx_bytes": int(e * 1000), "tx_bytes": int(e * 900), "rx_packets": int(e * 10),
                           "tx_packets": int(e * 9), "sockets_used": 40},
                   "values": {"start_time_seconds": START, "dht_sample_suppression_seconds": 0}}
            if self.kind == "comparator":
                row["values"] = {"comparator_torrents": int(max(0, e - 5) * 2)}
            lines = self.snapshot(e, i) if self.kind != "comparator" else []
            if self.kind != "comparator":
                row["values"]["dht_sample_suppression_seconds"] = float(
                    [l for l in lines if l.startswith("lodestar_dht_sample_suppression_seconds ")][0].split()[1])
            if i == 0:
                row["public_ip"] = "reference"
            if i in marks:
                row["boundary"] = marks[i]
                row["public_ip"] = self.ip.get(marks[i][-1], "unchanged")
                if self.kind != "comparator":
                    row["api"] = {"settings": dict(self.api, restart_pending=False),
                                  "imports": dict(self.imports, from_unix=int(START) - 60,
                                                  to_unix=int(utc.timestamp()) + 60, truncated=False)}
            for when, fn in self.edits:
                if when(e, i):
                    fn(row, lines)
            if row.get("metrics_ok") and self.kind != "comparator":
                (root / "metrics" / f"{i:05d}.prom").write_text("\n".join(lines) + "\n")
            rows.append(row)
        (root / "samples.jsonl").write_text("".join(json.dumps(r) + "\n" for r in rows))
        settings = {"mode": "docker", "run_kind": self.kind, "treatment": self.treatment if self.kind == "ab" else None,
                    "rehearsal": self.rehearsal, "warmup_minutes": self.warm_min, "baseline_hours": self.hours,
                    "hour_seconds": self.hour_s, "sample_seconds": self.sample, "spec_commit": COMMIT}
        run = {"schema_version": 2, "commit": COMMIT, "image_digest": DIGEST, "utc_offset_seconds": -25200,
               "settings": settings, "network": {"mode": "bridge"}, "checklist": self.checklist,
               "clock": {"monotonic_origin": 1000.0, "boot_id": "boot", "ntp_start": self.ntp}}
        if self.kind == "comparator":
            run["process_started_utc"] = datetime.fromtimestamp(START, timezone.utc).isoformat().replace("+00:00", "Z")
            run["comparator"] = {"query": collect.COMPARATOR_QUERY}
        else:
            run["config"] = {"switches": dict(self.switches), "env_preset": "unset", "witness": self.witness,
                             "settings_api": dict(self.api, intensity_source="default"), **self.config_extra}
        (root / "run.json").write_text(json.dumps(run))
        if self.result is not None:
            (root / "result.json").write_text(json.dumps(dict(self.result, samples=len(rows))))
        if self.activity:
            (root / "activity.jsonl").write_text("".join(json.dumps(a) + "\n" for a in self.activity))
        return root


class Base(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.tmp = Path(self.temp.name)
        self.n = 0

    def run_sim(self, sim):
        self.n += 1
        return analyze_cr.analyze_segment(sim.write(self.tmp / f"seg{self.n}"))

    def failed(self, report):
        return {c["check"] for c in report["checks"] if not c["ok"]}

    def assertValid(self, report):
        self.assertEqual(report["verdict"], "valid", self.failed(report))


class WindowTests(Base):
    def test_a_clean_resumed_segment_is_valid(self):
        report = self.run_sim(Sim())
        self.assertValid(report)
        num = report["numerator"]
        self.assertEqual(len(num["hourly"]), 3)
        # Rule 3.2: the first interval (a_0 = 0) is all carry-in; after it le=30 holds a 25 s latency.
        self.assertEqual(num["window"]["carry_in_bound"], 120)
        self.assertAlmostEqual(num["window"]["mean_net_per_min"], (num["window"]["gross"] - 120) / 30)
        self.assertAlmostEqual(num["window"]["mean_gross_per_min"], 120)
        t = report["timeline"]
        self.assertEqual(t["t_open_elapsed"], 5.0)  # D6: the first scrape of a resumed start
        self.assertEqual(t["ws_elapsed"], 605.0)  # rule 1.1: T_open + warmup
        self.assertEqual(t["t_s_elapsed"], 605.0)
        self.assertEqual(t["host_local_start_hour"], 22)  # 05:59:58 UTC at -7 h

    def test_real_parameters_six_hours_after_a_sixty_minute_warmup(self):
        report = self.run_sim(Sim(hours=6, hour_s=3600, warm_min=60, rehearsal=False))
        self.assertValid(report)
        self.assertEqual(report["window_hours"], 6)
        self.assertEqual(report["timeline"]["t_s_elapsed"] - report["timeline"]["t_open_elapsed"], 3600)
        minutes = [h["minutes"] for h in report["numerator"]["hourly"]]
        self.assertTrue(all(m == 60 for m in minutes), minutes)  # consecutive 60-min hours, shared scrapes

    def test_hours_share_boundary_scrapes_and_window_mean_is_not_an_hourly_average(self):
        sim = Sim()
        sim.rate = 60.0
        report = self.run_sim(sim)
        hourly, window = report["numerator"]["hourly"], report["numerator"]["window"]
        self.assertEqual(sum(h["gross"] for h in hourly), window["gross"])
        self.assertAlmostEqual(sum(h["minutes"] for h in hourly), window["minutes"])

    def test_pre_report_default_is_not_t_open(self):
        sim = Sim(suppressed_until=0)
        sim.first_off = True  # reason off, 0 s, one second after start
        report = self.run_sim(sim)
        self.assertValid(report)
        self.assertEqual(report["timeline"]["t_open_elapsed"], 65.0)

    def test_first_start_opens_after_suppression(self):
        sim = Sim(suppressed_until=1800, reason="first_start")
        report = self.run_sim(sim)
        self.assertValid(report)
        self.assertGreaterEqual(report["timeline"]["t_open_elapsed"], 1800)
        self.assertEqual(report["startup"]["suppression_reason_at_start"], "first_start")
        self.assertLess(report["startup"]["suppressed_gross_per_min"], 1)

    def test_no_t_open_within_seven_hours_is_invalid(self):
        sim = Sim(suppressed_until=7 * 3600 + 120, reason="first_start", hours=1)
        report = self.run_sim(sim)
        self.assertEqual(report["verdict"], "invalid")
        self.assertIn("t_open_within_7h", self.failed(report))

    def test_null_stop_reason_is_not_a_crash(self):
        sim = Sim()
        sim.result = dict(sim.result, reason=None)
        self.assertValid(self.run_sim(sim))

    def test_window_open_shows_no_rates(self):
        sim = Sim()
        sim.result = None
        report = self.run_sim(sim)
        self.assertEqual(report["state"], "window_open")
        self.assertNotIn("numerator", report)
        self.assertNotIn("latency", report)


class CarryInTests(Base):
    def test_carry_in_bound_follows_the_bucket_edge_rule(self):
        # Every sample records 4000 s: a commit can predate t_s until a_i - 2 reaches 7200.
        sim = Sim(hours=3, hour_s=3600, warm_min=10, latency=4000)
        report = self.run_sim(sim)
        hourly = report["numerator"]["hourly"]
        self.assertEqual(hourly[0]["net"], 0)
        self.assertEqual(hourly[1]["net"], 0)
        self.assertGreater(hourly[2]["net"], 0)  # a_i >= 7202 s: le=7200 holds them
        self.assertAlmostEqual(hourly[0]["carry_in_share"], 1.0)

    def test_short_latency_carries_only_the_first_seconds(self):
        sim = Sim(latency=1)
        report = self.run_sim(sim)
        hour0 = report["numerator"]["hourly"][0]
        # a_0 = 0 (no edge: all of it), a_1 = 60 (e = 30: none of it).
        self.assertGreater(hour0["carry_in_bound"], 0)
        self.assertLess(hour0["carry_in_bound"], 130)

    def test_bound_unit(self):
        snap = lambda count, b30, b60, dropped: {"snap": {
            "discovery_to_searchable_seconds_count": {(): count},
            "discovery_to_searchable_seconds_bucket": {(("le", "30"),): b30, (("le", "60"),): b60},
            "discovery_to_searchable_dropped_total": {(): dropped}}}
        a, b = snap(0, 0, 0, 0), snap(10, 4, 7, 2)
        self.assertEqual(analyze_cr.carry_in_bound(a, b, 2), 12)     # a_i < 3: no edge
        self.assertEqual(analyze_cr.carry_in_bound(a, b, 32), 8)     # e = 30
        self.assertEqual(analyze_cr.carry_in_bound(a, b, 61), 8)     # 61 - 2 = 59: still e = 30
        self.assertEqual(analyze_cr.carry_in_bound(a, b, 62), 5)     # e = 60
        self.assertEqual(analyze_cr.carry_in_bound(a, b, 7200), 10)  # dropped stop counting at 2 h


class LatencyTests(Base):
    def test_p95_pass_and_reported_edge(self):
        report = self.run_sim(Sim(latency=25))
        lat = report["latency"]
        self.assertTrue(lat["measured"])
        self.assertEqual(lat["p95_seconds"], 30)
        self.assertTrue(lat["passes_p95_600"])

    def test_slow_latency_fails_the_test(self):
        report = self.run_sim(Sim(latency=900))
        self.assertEqual(report["latency"]["p95_seconds"], 1800)
        self.assertFalse(report["latency"]["passes_p95_600"])

    def test_coverage_below_98_percent_is_not_measured(self):
        sim = Sim()
        sim.dropped_share = 0.03
        report = self.run_sim(sim)
        self.assertLess(report["latency"]["coverage"], 0.98)
        self.assertFalse(report["latency"]["measured"])
        self.assertIsNone(report["latency"]["p95_seconds"])
        self.assertFalse(report["latency"]["passes_p95_600"])

    def test_dropped_samples_count_as_slow(self):
        sim = Sim()
        sim.dropped_share = 0.015  # coverage 98.5 %: measured, but dropped push le=600 under 95 % only if > 5 %
        report = self.run_sim(sim)
        self.assertTrue(report["latency"]["measured"])
        self.assertTrue(report["latency"]["passes_p95_600"])
        lat = analyze_cr.latency(*[{"snap": {
            "discovery_to_searchable_seconds_bucket": {(("le", e),): v for e in EDGES},
            "discovery_to_searchable_seconds_count": {(): v},
            "discovery_to_searchable_dropped_total": {(): d}}} for v, d in ((0, 0), (94, 6))])
        self.assertFalse(lat["measured"])  # 94 % coverage
        self.assertFalse(lat["passes_p95_600"])

    def test_reconciliation_gap_is_missing_evidence(self):
        sim = Sim()
        sim.count_share = 0.95
        report = self.run_sim(sim)
        self.assertEqual(report["verdict"], "invalid")
        self.assertIn("latency_reconciles_with_numerator", self.failed(report))


class EvidenceTests(Base):
    def test_scrape_gap_over_180s(self):
        sim = Sim()
        sim.drop = lambda e: 1000 < e < 1200
        report = self.run_sim(sim)
        self.assertIn("scrape_gaps_within_180s", self.failed(report))
        self.assertEqual(report["verdict"], "invalid")

    def test_late_boundary_scrape(self):
        sim = Sim()
        sim.drop = lambda e: 1200 <= e < 1320  # boundary 1 is at 1205
        report = self.run_sim(sim)
        self.assertIn("boundary_scrapes_on_time", self.failed(report))

    def test_failed_scrape_in_window(self):
        sim = Sim()
        sim.edits.append((lambda e, i: i == 20, lambda row, lines: row.update(metrics_ok=False)))
        self.assertIn("no_failed_scrape_in_window", self.failed(self.run_sim(sim)))

    def test_missing_required_series(self):
        sim = Sim()
        sim.edits.append((lambda e, i: i == 20, lambda row, lines: lines.__setitem__(
            slice(None), [l for l in lines if not l.startswith("lodestar_dht_utp_in_total")])))
        self.assertIn("required_series_present", self.failed(self.run_sim(sim)))

    def test_counter_reset(self):
        sim = Sim()
        def reset(row, lines):
            lines[:] = [("lodestar_dht_peer_admitted_total 1" if l.startswith("lodestar_dht_peer_admitted_total ") else l)
                        for l in lines]
        sim.edits.append((lambda e, i: i == 25, reset))
        self.assertIn("no_counter_reset", self.failed(self.run_sim(sim)))

    def test_identity_change(self):
        sim = Sim()
        sim.edits.append((lambda e, i: i > 25, lambda row, lines: row.update(process_identity="c:later")))
        self.assertIn("one_process_identity", self.failed(self.run_sim(sim)))

    def test_ntp_not_synchronized(self):
        sim = Sim()
        sim.ntp = {"source": "timedatectl", "synchronized": True, "offset_seconds": None}
        self.assertIn("ntp_synchronized", self.failed(self.run_sim(sim)))

    def test_utc_drift_over_5s(self):
        sim = Sim()
        sim.drift_from = 1500
        self.assertIn("utc_elapsed_drift_within_5s", self.failed(self.run_sim(sim)))

    def test_missing_configuration_identity(self):
        sim = Sim()
        sim.api = {"intensity": None, "paused": False}
        self.assertIn("configuration_identity_recorded", self.failed(self.run_sim(sim)))

    def test_snapshot_cap(self):
        sim = Sim()
        sim.edits.append((lambda e, i: i == 3, lambda row, lines: row.update(series_capped=True)))
        self.assertIn("snapshots_not_capped", self.failed(self.run_sim(sim)))


class HostTests(Base):
    def test_activity_in_window_invalidates(self):
        sim = Sim()
        sim.activity = [{"elapsed_seconds": 900, "category": "array_operation", "same_boot": True,
                         "collector_finished": False}]
        report = self.run_sim(sim)
        self.assertIn("no_host_activity", self.failed(report))

    def test_activity_outside_the_list_or_after_the_end_is_a_note(self):
        sim = Sim()
        sim.activity = [{"elapsed_seconds": 900, "category": "other", "same_boot": True, "collector_finished": False},
                        {"elapsed_seconds": 9000, "category": "power_event", "same_boot": True,
                         "collector_finished": True}]
        report = self.run_sim(sim)
        self.assertValid(report)
        self.assertEqual([n["category"] for n in report["activity_notes"]], ["other", "power_event"])

    def test_public_ip_change(self):
        sim = Sim()
        sim.ip = {2: "changed"}
        self.assertIn("public_ip_unchanged", self.failed(self.run_sim(sim)))

    def test_public_ip_unchecked_at_a_boundary(self):
        sim = Sim()
        sim.ip = {1: "failed"}
        self.assertIn("public_ip_checked_each_boundary", self.failed(self.run_sim(sim)))

    def test_checklist_and_witness(self):
        sim = Sim()
        sim.checklist = {"auto_update": {"state": "excluded"}}
        sim.witness = {"configured": True, "host_mounted": False, "outside_data_dir": True}
        failed = self.failed(self.run_sim(sim))
        self.assertIn("host_checklist_complete", failed)
        self.assertIn("witness_host_mounted_outside_data_dir", failed)

    def test_paused_crawler_is_an_interruption(self):
        sim = Sim()
        sim.api = {"intensity": "balanced", "paused": True}
        self.assertIn("crawler_never_paused", self.failed(self.run_sim(sim)))

    def test_ab_needs_safe_restart_and_other_switches_off(self):
        failed = self.failed(self.run_sim(Sim(switches={"safe_restart": False, "responder_hints": True})))
        self.assertIn("safe_restart_on", failed)
        self.assertIn("other_switches_off", failed)


class MechanismTests(Base):
    def verdict(self, sim):
        report = self.run_sim(sim)
        return report["verdict"], self.failed(report), report

    def test_imports_void_the_segment(self):
        sim = Sim()
        sim.imports["import_finished"] = 1
        verdict, failed, _ = self.verdict(sim)
        self.assertEqual(verdict, "void")
        self.assertIn("no_import_job", failed)
        sim = Sim()
        sim.import_rendered = True
        self.assertIn("import_origin_not_rendered", self.verdict(sim)[1])

    def test_preset_must_be_balanced(self):
        sim = Sim()
        sim.api = {"intensity": "aggressive", "paused": False}
        self.assertIn("balanced_preset", self.verdict(sim)[1])

    def test_utp_on_and_off(self):
        self.assertEqual(self.verdict(Sim(switches={"utp": True}))[0], "valid")
        sim = Sim(switches={"utp": True})
        sim.utp_in = 0.0
        self.assertIn("utp_in_use", self.verdict(sim)[1])
        sim = Sim()
        sim.utp_in = 5.0
        self.assertIn("utp_idle_when_off", self.verdict(sim)[1])

    def test_admission_on_and_off(self):
        on = dict(coordinated_admission=True)
        verdict, _, report = self.verdict(Sim(treatment="adm", switches=on))
        self.assertEqual(verdict, "valid")
        self.assertEqual(report["mechanism"]["coordinated_admission"]["longest_nonzero_permit_gap_seconds"], 0)
        sim = Sim(treatment="adm", switches=on)
        sim.grants = 0.0
        self.assertIn("admission_granted", self.verdict(sim)[1])
        sim = Sim(treatment="adm")
        sim.grants = 4.0
        self.assertIn("admission_idle_when_off", self.verdict(sim)[1])

    def test_permit_identity_gap_of_sixty_minutes_voids(self):
        on = dict(coordinated_admission=True)
        sim = Sim(treatment="adm", switches=on, hours=2, hour_s=3600)
        sim.permit_leak = (700, 700 + 3540)  # back to 0 within the hour
        verdict, _, report = self.verdict(sim)
        self.assertEqual(verdict, "valid")
        self.assertLess(report["mechanism"]["coordinated_admission"]["longest_nonzero_permit_gap_seconds"], 3600)
        sim = Sim(treatment="adm", switches=on, hours=2, hour_s=3600)
        sim.permit_leak = (700, 700 + 3700)
        self.assertIn("permit_identity_returns_to_zero_within_60min", self.verdict(sim)[1])

    def test_route_backoff_no_opportunity_and_off(self):
        sim = Sim(treatment="route", switches={"route_backoff": True})
        sim.route_skipped = 0.0
        verdict, failed, _ = self.verdict(sim)
        self.assertEqual((verdict, failed), ("void", {"no_opportunity"}))
        sim = Sim(treatment="route")
        sim.probe_down = 2.0
        self.assertIn("route_backoff_idle_when_off", self.verdict(sim)[1])


class ComparatorTests(Base):
    def test_comparator_valid_and_its_checks(self):
        sim = Sim(kind="comparator")
        report = self.run_sim(sim)
        self.assertValid(report)
        self.assertAlmostEqual(report["numerator"]["window"]["mean_per_min"], 120)
        sim = Sim(kind="comparator")
        sim.edits.append((lambda e, i: i == 0, lambda row, lines: row["values"].update(comparator_torrents=7)))
        self.assertIn("store_empty_at_start", self.failed(self.run_sim(sim)))
        sim = Sim(kind="comparator")
        sim.edits.append((lambda e, i: e > 1100, lambda row, lines: row["values"].update(comparator_torrents=2200)))
        self.assertIn("count_rises_every_hour", self.failed(self.run_sim(sim)))

    def test_spacing(self):
        comp = {"timeline": {"process_start_utc": "2026-11-05T00:00:00Z", "window_end_utc": "2026-11-06T01:00:00Z"}}
        near = {"process_start_utc": "2026-11-04T10:00:00Z", "window_start_utc": "2026-11-04T11:00:00Z",
                "window_end_utc": "2026-11-04T17:00:00Z"}
        after = {"process_start_utc": "2026-11-06T02:00:00Z", "window_start_utc": "2026-11-06T03:00:00Z",
                 "window_end_utc": "2026-11-06T09:00:00Z"}
        clear = {"process_start_utc": "2026-11-06T13:30:00Z", "window_start_utc": "2026-11-06T14:30:00Z",
                 "window_end_utc": "2026-11-06T20:30:00Z"}
        self.assertEqual(analyze_cr.spacing(comp, [near]), ["lodestar_ran_within_12h_before_or_during_comparator"])
        self.assertEqual(analyze_cr.spacing(comp, [after]), ["lodestar_window_within_12h_after_comparator"])
        self.assertEqual(analyze_cr.spacing(comp, [clear]), [])


class PublicationTests(Base):
    def test_published_bundle_is_aggregates_only(self):
        report = self.run_sim(Sim())
        out = self.tmp / "pub"
        analyze_cr.publish(report, out)
        self.assertEqual({p.name for p in out.iterdir()}, {"segment.json", "segment.md"})
        text = (out / "segment.json").read_text()
        self.assertLessEqual(len(json.loads(text)["numerator"]["hourly"]), 3)  # hourly, never per scrape
        self.assertNotIn("\"sequence\"", text)

    def test_publication_refuses_addresses_and_infohashes(self):
        for bad in ({"note": "peer 203.0.113.9"}, {"node": "2001:db8::1"}, {"name": "0123456789abcdef" * 2 + "01234567"}):
            with self.assertRaises(ValueError):
                analyze_cr.publish(dict(bad), self.tmp / f"x{self.n}")
            self.n += 1
        self.assertEqual(analyze_cr.publication_problems({"source_commit": COMMIT, "t": "2026-11-03T06:00:00Z"}), [])


def seg(arm, net, start, minutes=360, verdict="valid", reasons=(), digest=DIGEST, clean=True, switch="utp",
        gap=300, p95=30, others=None):
    """A segment report as `effect` reads it."""
    switches = {name: False for name in collect.SWITCH_ENV}
    switches.update(safe_restart=True, **(others or {}))
    switches[switch] = arm == "B"
    begin = T0 + timedelta(hours=start)
    end = begin + timedelta(minutes=minutes)
    stamp = lambda t: t.isoformat().replace("+00:00", "Z")
    return {"kind": "ab", "treatment": {"utp": "utp", "coordinated_admission": "adm", "route_backoff": "route"}[switch],
            "verdict": verdict, "verdict_reasons": list(reasons), "clean_stop": clean,
            "identity": {"switches": switches, "image_digest": digest},
            "numerator": {"window": {"mean_net_per_min": net, "mean_gross_per_min": net * 1.1,
                                     "stored_per_min": net * 1.2, "minutes": minutes},
                          "worst_hour_net_per_min": net * 0.9},
            "fetch": {"success_per_attempt": 0.3}, "latency": {"measured": True, "p95_seconds": p95},
            "guardrails": {"dht_timeouts_total": net * 10},
            "timeline": {"process_start_utc": stamp(begin - timedelta(minutes=60)), "window_start_utc": stamp(begin),
                         "window_end_utc": stamp(end), "last_scrape_utc": stamp(end),
                         "host_local_start_hour": begin.hour}}


def block(order, means, start=0.0, step=7.0 + 5 / 60, **kw):
    """Four segments 7 h apart; arm switches 5 minutes apart."""
    return [seg(arm, m, start + i * step, **kw) for i, (arm, m) in enumerate(zip(order, means))]


class EffectTests(unittest.TestCase):
    def effect(self, blocks, treatment="utp"):
        return analyze_cr.analyze_effect(treatment, blocks)

    def test_win_with_three_degrees_of_freedom(self):
        out = self.effect([block("ABBA", [100, 112, 111, 101]), block("BAAB", [110, 99, 100, 111], start=12)])
        self.assertEqual(out["fit"]["df"], 3)
        self.assertAlmostEqual(out["fit"]["t_quantile"], 3.182, places=3)
        self.assertEqual(out["outcome"], "win")
        self.assertAlmostEqual(out["fit"]["effect"], 0.11, delta=0.01)

    def test_four_blocks_have_seven_degrees_of_freedom(self):
        blocks = [block("ABBA", [100, 101, 100, 100]), block("BAAB", [100, 100, 101, 100], start=12),
                  block("ABBA", [100, 100, 100, 101], start=48), block("BAAB", [101, 100, 100, 100], start=60)]
        out = self.effect(blocks)
        self.assertEqual(out["fit"]["df"], 7)
        self.assertEqual(out["outcome"], "no_effect")

    def test_no_effect_band_is_ln_095_to_ln_105(self):
        fit = lambda lo, hi: {"ci": [lo, hi], "effect": 0.0}
        self.assertEqual(analyze_cr.outcome(fit(math.log(0.951), math.log(1.04)), 2), "no_effect")
        self.assertEqual(analyze_cr.outcome(fit(math.log(0.95), math.log(1.05)), 2), "no_effect")
        self.assertEqual(analyze_cr.outcome(fit(math.log(0.949), math.log(1.04)), 2),
                         "inconclusive_run_blocks_3_and_4")
        self.assertEqual(analyze_cr.outcome(fit(math.log(0.949), math.log(1.04)), 4),
                         "inconclusive_switch_stays_off")

    def test_control_segments_survive_a_loss(self):
        out = self.effect([block("ABBA", [100, 0, 110, 100]), block("BAAB", [110, 99, 100, 111], start=12)])
        self.assertEqual(out["outcome"], "loss")
        self.assertEqual(len(out["control_segments"]), 4)
        self.assertIn("base", analyze_cr.checkpoint1(out, [], None, None))

    def test_loss_and_inconclusive(self):
        out = self.effect([block("ABBA", [100, 80, 81, 100]), block("BAAB", [79, 101, 100, 80], start=12)])
        self.assertEqual(out["outcome"], "loss")
        out = self.effect([block("ABBA", [100, 130, 80, 100]), block("BAAB", [70, 101, 140, 120], start=12)])
        self.assertEqual(out["outcome"], "inconclusive_run_blocks_3_and_4")

    def test_a_linear_trend_cancels(self):
        # A 2 %/h decline on top of a +10 % effect: ABBA cancels it.
        means = lambda order, start: [100 * (1.1 if a == "B" else 1) * math.exp(-0.02 * (start + i * 7.083))
                                      for i, a in enumerate(order)]
        out = self.effect([block("ABBA", means("ABBA", 0)), block("BAAB", means("BAAB", 12), start=12)])
        self.assertAlmostEqual(out["fit"]["tau"], math.log(1.1), places=6)

    def test_block_problems(self):
        bad = block("ABAB", [100, 110, 100, 110])
        self.assertIn("order_not_abba_or_baab", analyze_cr.judge_block(bad, "utp")["problems"])
        slow = block("ABBA", [100, 110, 110, 100], step=6 + 15 / 60)
        self.assertIn("arm_switch_over_10_minutes", analyze_cr.judge_block(slow, "utp")["problems"])
        unclean = block("ABBA", [100, 110, 110, 100], clean=False)
        self.assertIn("arm_switch_not_a_clean_stop", analyze_cr.judge_block(unclean, "utp")["problems"])
        digests = block("ABBA", [100, 110, 110, 100])
        digests[2]["identity"]["image_digest"] = "sha256:" + "c" * 64
        self.assertIn("image_digest_differs", analyze_cr.judge_block(digests, "utp")["problems"])
        dead = block("ABBA", [0, 110, 110, 100])
        self.assertIn("control_did_not_crawl", analyze_cr.judge_block(dead, "utp")["problems"])
        invalid = block("ABBA", [100, 110, 110, 100])
        invalid[1]["verdict"] = "invalid"
        self.assertIn("segment_invalid", analyze_cr.judge_block(invalid, "utp")["problems"])
        mixed = block("ABBA", [100, 110, 110, 100])
        mixed[3]["identity"]["switches"]["responder_hints"] = True
        self.assertIn("switch_set_differs", analyze_cr.judge_block(mixed, "utp")["problems"])

    def test_zero_treatment_mean_is_a_loss_without_fit(self):
        out = self.effect([block("ABBA", [100, 0, 110, 100]), block("BAAB", [110, 99, 100, 111], start=12)])
        self.assertEqual(out["outcome"], "loss")
        self.assertNotIn("fit", out)

    def test_blocks_must_come_in_pairs(self):
        out = self.effect([block("ABBA", [100, 110, 110, 100])])
        self.assertEqual(out["outcome"], "not_decidable")
        out = self.effect([block("ABBA", [100, 110, 110, 100]), block("ABBA", [100, 110, 110, 100], start=12)])
        self.assertEqual(out["outcome"], "not_decidable")

    def test_route_no_opportunity(self):
        b = lambda order, start: [seg(a, 100, start + i * 7.083, switch="route_backoff",
                                      verdict="void" if a == "B" else "valid",
                                      reasons=["no_opportunity"] if a == "B" else ())
                                  for i, a in enumerate(order)]
        out = self.effect([b("ABBA", 0), b("BAAB", 12)], treatment="route")
        self.assertEqual(out["outcome"], "no_opportunity")

    def test_route_baseline(self):
        valid = {"verdict": "valid", "kind": "ab", "identity": {"switches": {"utp": False, "route_backoff": False}},
                 "mechanism": {"no_route_ipv6": 0}}
        self.assertEqual(analyze_cr.route_baseline([valid, valid])["decision"], "recorded_skip")
        seen = dict(valid, mechanism={"no_route_ipv6": 4})
        self.assertEqual(analyze_cr.route_baseline([valid, seen])["decision"], "run")
        utp_on = dict(seen, identity={"switches": {"utp": True, "route_backoff": False}})
        self.assertEqual(analyze_cr.route_baseline([valid, utp_on])["no_route_ipv6_sum"], 0)


class GateTests(unittest.TestCase):
    def comb(self, mean=120.0, hours=None, measured=True, passes=True, verdict="valid", rehearsal=False):
        hours = hours or [mean] * 24
        return {"kind": "comb", "rehearsal": rehearsal, "window_hours": 24, "verdict": verdict,
                "numerator": {"window": {"mean_net_per_min": mean}, "hourly": [{"net_per_min": h} for h in hours]},
                "latency": {"measured": measured, "passes_p95_600": passes, "p95_seconds": 300, "coverage": 0.99}}

    def test_gate(self):
        self.assertEqual(analyze_cr.gate(self.comb())["gate"], "pass")
        self.assertEqual(analyze_cr.gate(self.comb(mean=99.9))["gate"], "fail")
        self.assertEqual(analyze_cr.gate(self.comb(hours=[120] * 23 + [69.9]))["gate"], "fail")
        out = analyze_cr.gate(self.comb(measured=False))
        self.assertEqual((out["gate"], out["p95"]), ("fail", "not_measured"))
        self.assertEqual(analyze_cr.gate(self.comb(passes=False))["gate"], "fail")
        self.assertEqual(analyze_cr.gate(self.comb(verdict="invalid"))["gate"], "not_judged")
        self.assertEqual(analyze_cr.gate(self.comb(rehearsal=True))["gate"], "not_judged")


class CheckpointTests(unittest.TestCase):
    def test_projection_models(self):
        fit = lambda g, se, df: {"tau": math.log(g), "se": se, "df": df, "ci": [math.log(g) - 3 * se, math.log(g) + 3 * se]}
        utp = {"treatment": "utp", "outcome": "win", "fit": fit(1.2, 0.05, 3),
               "control_segments": [{"net": n, "p95": 120, "p95_measured": True} for n in (80, 84, 76, 80)],
               "treatment_segments": [{"net": 96, "p95": 300, "p95_measured": True}], "blocks": []}
        adm = {"treatment": "adm", "outcome": "win", "fit": fit(1.1, 0.04, 7),
               "treatment_segments": [{"net": 90, "p95": 600, "p95_measured": True}], "blocks": []}
        comp = {"verdict": "valid", "numerator": {"window": {"mean_per_min": 150.0}, "worst_hour_per_min": 120.0},
                "timeline": {"process_start_utc": "2026-10-01T00:00:00Z", "window_end_utc": "2026-10-02T01:00:00Z"}}
        cr0 = {"phases": {"baseline": {"hourly": {"0": {"writer_stored_total": {"delta": 3600, "covered_seconds": 3600}},
                                                  "1": {"writer_stored_total": {"delta": 2700, "covered_seconds": 3600}}}}}}
        out = analyze_cr.checkpoint1(utp, [adm], comp, cr0)
        r0 = 80.0
        self.assertAlmostEqual(out["base"]["R0"], r0)
        se = math.sqrt(sum((x - 80) ** 2 for x in (80, 84, 76, 80)) / 3 / 4)
        self.assertAlmostEqual(out["base"]["var_ln_R0"], (se / r0) ** 2)
        self.assertAlmostEqual(out["multiplicative"]["point"], 80 * 1.2 * 1.1)
        self.assertAlmostEqual(out["additive"]["point"], 80 * (1 + 0.2 + 0.1))
        self.assertAlmostEqual(out["projection"], 80 * 1.3)  # the lower of the two
        self.assertAlmostEqual(out["t_quantile"], analyze_cr.t_quantile(0.975, 3))  # the largest quantile
        ratio = min(120 / 150, 2700 / ((3600 + 2700) / 2))
        self.assertAlmostEqual(out["worst_hour"]["projection"], 104 * ratio)
        self.assertEqual(out["p95_seconds"], 600)
        self.assertTrue(out["on_track"])  # 104/min, worst hour 83.2/min, p95 600 s
        self.assertTrue(out["range_below_100"])  # on track, but the runner must say the range dips below 100
        self.assertAlmostEqual(out["fraction_of_comparator"], 104 / 150)

    def test_rehearsal_and_invalid_inputs_are_not_projected(self):
        utp = {"treatment": "utp", "outcome": "no_effect", "rehearsal": True,
               "control_segments": [{"net": 100, "p95": 30, "p95_measured": True}] * 4}
        self.assertIn("error", analyze_cr.checkpoint1(utp, [], None, None))
        utp["rehearsal"] = False
        comp = {"verdict": "void", "numerator": {"window": {"mean_per_min": 150.0}, "worst_hour_per_min": 120.0}}
        out = analyze_cr.checkpoint1(utp, [], comp, None)
        self.assertIn("comparator_not_used", out)
        self.assertIsNone(out["comparator_mean_per_min"])

    def test_no_winner_projects_r0(self):
        utp = {"treatment": "utp", "outcome": "no_effect", "fit": {"tau": 0.0, "se": 0.02, "df": 3},
               "control_segments": [{"net": n, "p95": 120, "p95_measured": n != 0} for n in (110, 112, 108, 110)]}
        out = analyze_cr.checkpoint1(utp, [], None, None)
        self.assertAlmostEqual(out["projection"], 110)
        self.assertEqual(out["p95_seconds"], 120)
        self.assertFalse(out["on_track"])  # no worst-hour ratios, so not on track


if __name__ == "__main__":
    unittest.main()
