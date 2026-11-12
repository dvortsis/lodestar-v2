import json
import math
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from datetime import datetime, timezone

from analyze_cr0 import analyze, markdown, parse_prom


def sample(elapsed, count=0, phase="baseline", **changes):
    row = {
        "elapsed_seconds": elapsed, "phase": phase, "process_id": 123,
        "health": 200, "metrics_ok": True, "working_set_bytes": (32 + elapsed / 60) * 2**20,
        "values": {"start_time_seconds": 1000, "dht_peer_admitted_total": count,
                   "dht_peer_active": 3, "dht_peer_queued": 0,
                   "dht_sample_cooling": 200000, "dht_sample_inflight": 0},
    }
    row.update(changes)
    return row


def analyze_with(case, rows):
    case.rows(rows)
    return analyze(case.root)


class AnalyzerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "metrics").mkdir()
        (self.root / "run.json").write_text(json.dumps({
            "commit": "a" * 40, "settings": {"baseline_hours": 24, "intensity": "balanced"},
            "secret": "never-export-this",
        }), encoding="utf-8")

    def run_json(self, **fields):
        (self.root / "run.json").write_text(json.dumps({
            "commit": "a" * 40, "settings": {"baseline_hours": 24, "intensity": "balanced"}, **fields,
        }), encoding="utf-8")

    def rows(self, rows, tail=""):
        (self.root / "samples.jsonl").write_text(
            "".join(json.dumps(r) + "\n" for r in rows) + tail, encoding="utf-8-sig")

    def counter(self, report, phase="baseline", name="dht_peer_admitted_total"):
        return report["phases"][phase]["counters"][name]

    def test_rates_and_memory_are_separate_by_phase(self):
        self.rows([sample(0, 100, "startup"), sample(60, 102, "startup"),
                   sample(120, 150, "warmup"), sample(180, 170, "warmup"),
                   sample(240, 200), sample(300, 230)])
        report = analyze(self.root)
        self.assertEqual(self.counter(report, "startup")["per_hour"], 120)
        self.assertEqual(self.counter(report, "warmup")["delta"], 20)
        self.assertEqual(self.counter(report)["delta"], 30)
        self.assertEqual(report["phases"]["baseline"]["rss_change_mib_per_hour"], 60)
        self.assertEqual(report["baseline_status"], "completion_unknown")
        self.assertNotIn("never-export-this", json.dumps(report))

    def test_peak_working_set_and_private_are_separate_columns(self):
        self.rows([sample(0, 100, private_bytes=20 * 2**20), sample(60, 101, private_bytes=24 * 2**20),
                   sample(120, 102)])
        report = analyze(self.root)
        baseline = report["phases"]["baseline"]
        self.assertEqual(baseline["rss_max_mib"], 34)  # working set, old key kept
        self.assertEqual(baseline["private_max_mib"], 24)
        text = markdown(report)
        self.assertIn("| Peak working set (MiB) | Peak private (MiB) | RSS change (MiB/h) |", text)
        self.assertNotIn("Peak RSS", text)
        self.assertIn("| 34.00 | 24.00 |", text)
        # Rows without private_bytes (older collectors): unknown, not zero.
        self.rows([sample(0, 100), sample(60, 101)])
        self.assertIsNone(analyze(self.root)["phases"]["baseline"]["private_max_mib"])

    def test_missing_baseline_and_metrics_are_unknown(self):
        self.rows([sample(0, phase="startup")])
        report = analyze(self.root)
        self.assertEqual(report["baseline_status"], "not_started")
        self.assertIsNone(self.counter(report)["delta"])
        self.assertIsNone(self.counter(report, "startup")["per_hour"])
        self.assertIn("unknown", markdown(report))

    def test_gaps_failed_metrics_and_restarts_are_not_bridged(self):
        self.rows([sample(0, 0), sample(60, 2), sample(240, 10),
                   sample(300, 12, metrics_ok=False), sample(360, 20),
                   sample(420, 1, process_id=456), sample(480, 3, process_id=456)])
        report = analyze(self.root)
        self.assertEqual(self.counter(report)["delta"], 4)
        self.assertEqual(self.counter(report)["covered_seconds"], 120)
        self.assertEqual(report["issues"]["sampling_gaps"], 1)
        self.assertEqual(report["issues"]["identity_changes"], 1)
        self.assertEqual(report["phases"]["baseline"]["metrics_failed_samples"], 1)
        self.assertIsNone(report["phases"]["baseline"]["rss_change_mib"])

    def test_health_failure_is_counted_without_success_only_rate_bias(self):
        self.rows([sample(0), sample(60, 2, health=0)])
        report = analyze(self.root)
        self.assertEqual(self.counter(report)["per_hour"], 120)
        self.assertEqual(report["phases"]["baseline"]["health_failed_samples"], 1)

    def test_counter_reset_is_excluded_and_recovery_is_usable(self):
        self.rows([sample(0, 100), sample(60, 1), sample(120, 4)])
        report = analyze(self.root)
        self.assertEqual(report["issues"]["counter_discontinuities"], 1)
        self.assertEqual(self.counter(report)["delta"], 3)
        self.assertEqual(self.counter(report)["covered_seconds"], 60)

    def test_missing_counter_has_its_own_coverage(self):
        rows = [sample(0, 0), sample(60, 2), sample(120, 4), sample(180, 7)]
        del rows[1]["values"]["dht_peer_admitted_total"]
        self.rows(rows)
        report = analyze(self.root)
        self.assertEqual(report["phases"]["baseline"]["valid_interval_seconds"], 180)
        self.assertEqual(self.counter(report)["covered_seconds"], 60)
        self.assertEqual(self.counter(report)["delta"], 3)

    def test_label_reset_is_not_hidden_by_other_series_growth(self):
        self.rows([sample(0), sample(60)])
        for i, (tcp, utp) in enumerate([(100, 0), (1, 200)]):
            (self.root / "metrics" / f"{i:05d}.prom").write_text(
                f'lodestar_fetch_success_total{{transport="tcp"}} {tcp}\n'
                f'lodestar_fetch_success_total{{transport="utp"}} {utp}\n', encoding="utf-8")
        report = analyze(self.root)
        self.assertEqual(report["issues"]["counter_discontinuities"], 1)
        self.assertIsNone(self.counter(report, name="fetch_success_total")["delta"])

    def test_prom_counters_include_failures_and_scientific_notation(self):
        self.rows([sample(0), sample(60)])
        for i in range(2):
            (self.root / "metrics" / f"{i:05d}.prom").write_text(
                f'lodestar_fetch_failures_total{{kind="refused"}} {i * 2}e0\n'
                f'lodestar_fetch_failures_total{{kind="timed_out"}} {i}\n'
                'lodestar_unknown_metric{secret="do-not-export"} 123\n', encoding="utf-8")
        report = analyze(self.root)
        self.assertEqual(self.counter(report, name="fetch_failures_total")["per_hour"], 180)
        self.assertNotIn("do-not-export", json.dumps(report))

    def test_capacity_equality_is_not_violation(self):
        self.rows([sample(0), sample(60)])
        report = analyze(self.root)
        bounds = report["phases"]["baseline"]["bounds"]
        self.assertEqual(bounds["sampler_states"]["at_capacity_samples"], 2)
        self.assertEqual(bounds["sampler_states"]["over_capacity_samples"], 0)
        self.assertIsNone(bounds["pool_lookups"]["max"])
        row = sample(120)
        row["values"]["dht_sample_inflight"] = 1
        self.rows([row])
        self.assertEqual(analyze(self.root)["phases"]["baseline"]["bounds"]
                         ["sampler_states"]["over_capacity_samples"], 1)

    def test_partial_tail_is_safe_but_corrupt_middle_is_error(self):
        self.rows([sample(0)], '{"elapsed_seconds":')
        self.assertEqual(analyze(self.root)["issues"]["incomplete_tail_ignored"], 1)
        self.rows([sample(0)], '{"elapsed_seconds":\n{}\n')
        with self.assertRaisesRegex(ValueError, "line 2"):
            analyze(self.root)

    def test_nonmonotonic_and_regressed_phases_are_flagged(self):
        self.rows([sample(60), sample(0), sample(120, phase="startup")])
        report = analyze(self.root)
        self.assertEqual(report["issues"]["nonmonotonic_intervals"], 1)
        self.assertEqual(report["issues"]["phase_regressions"], 1)
        self.assertIsNone(self.counter(report)["delta"])

    def test_terminated_short_run_is_not_accepted(self):
        self.rows([sample(0), sample(60)])
        result = {"reason": "baseline_window_complete", "samples": 2, "stop_mode": "forced_kill"}
        (self.root / "result.json").write_text(json.dumps(result), encoding="utf-8")
        report = analyze(self.root)
        self.assertEqual(report["baseline_status"], "collector_window_complete_review_required")
        self.assertEqual(report["issues"]["baseline_evidence_short"], 1)
        self.assertFalse(report["clean_shutdown"])
        result["reason"] = "operator_stop"
        (self.root / "result.json").write_text(json.dumps(result), encoding="utf-8")
        self.assertEqual(analyze(self.root)["baseline_status"], "interruption_exit_unknown")

    def test_invalid_prom_and_snapshot_mismatch_do_not_poison_json_rates(self):
        self.rows([sample(0), sample(60, 2)])
        (self.root / "metrics/00001.prom").write_text(
            "lodestar_dht_peer_admitted_total 999\n", encoding="utf-8")
        report = analyze(self.root)
        self.assertEqual(report["issues"]["invalid_prom_snapshots"], 1)
        self.assertEqual(self.counter(report)["delta"], 2)
        for raw in ("NaN", "+Inf", "-1"):
            with self.assertRaises(ValueError):
                parse_prom("lodestar_fetch_success_total " + raw)

    def test_cli_cannot_write_inside_evidence_or_overwrite_report(self):
        self.rows([sample(0)])
        script = str(Path(__file__).with_name("analyze_cr0.py"))
        before = (self.root / "samples.jsonl").read_bytes()
        result = subprocess.run([sys.executable, script, str(self.root), "--output",
                                 str(self.root / "samples.jsonl")], capture_output=True)
        self.assertEqual(result.returncode, 2)
        self.assertEqual((self.root / "samples.jsonl").read_bytes(), before)
        with tempfile.TemporaryDirectory() as output_dir:
            output = Path(output_dir) / "report.json"
            output.write_text("keep this", encoding="utf-8")
            result = subprocess.run([sys.executable, script, str(self.root), "--output",
                                     str(output)], capture_output=True)
            self.assertEqual(result.returncode, 2)
            self.assertEqual(output.read_text(encoding="utf-8"), "keep this")

    def test_full_baseline_is_review_required_not_automatic_pass(self):
        self.rows([sample(t, t // 60) for t in range(0, 24 * 3600 + 60, 60)])
        (self.root / "result.json").write_text(json.dumps({
            "reason": "baseline_window_complete", "samples": 1441, "stop_mode": "exited_0",
        }), encoding="utf-8")
        report = analyze(self.root)
        self.assertEqual(report["baseline_status"], "collector_window_complete_review_required")
        self.assertNotIn("baseline_evidence_short", report["issues"])
        self.assertEqual(self.counter(report)["per_hour"], 60)
        self.assertTrue(report["clean_shutdown"])

    def test_missing_and_corrupt_result_never_implies_running(self):
        from datetime import datetime, timezone
        now = datetime(2026, 10, 27, tzinfo=timezone.utc)
        self.rows([sample(0, time="2026-10-26T00:00:00Z")])
        report = analyze(self.root, now=now)
        self.assertEqual(report["run_state"], "completion_unknown")
        self.assertEqual(report["observation_state"], "stale_observations")
        (self.root / "result.json").write_text("{", encoding="utf-8")
        self.assertEqual(analyze(self.root)["issues"]["invalid_result"], 1)
        self.rows([sample(0, time=now.isoformat())])
        self.assertEqual(analyze(self.root, now=now)["observation_state"], "fresh_observations")

    def test_reason_breakdown_is_allowlisted_and_covered(self):
        self.rows([sample(0), sample(60)])
        for i in range(2):
            (self.root / "metrics" / f"{i:05d}.prom").write_text(
                f'lodestar_fetch_lost_total{{reason="evicted"}} {i * 3}\n'
                f'lodestar_fetch_lost_total{{reason="secret-name"}} {i}\n', encoding="utf-8")
        report = analyze(self.root)
        self.assertEqual(report["phases"]["baseline"]["breakdowns"]["fetch_lost_total"]["evicted"],
                         {"delta": 3, "covered_seconds": 60})
        self.assertNotIn("secret-name", json.dumps(report))

    def test_report_is_byte_identical_whatever_the_hash_seed(self):
        # r5 archives this report: set order (the hash seed) must not reorder it.
        self.rows([sample(0), sample(60), sample(120)])
        for i in range(3):
            (self.root / "metrics" / f"{i:05d}.prom").write_text(
                f'lodestar_fetch_failures_total{{kind="timed_out"}} {i * 2}\n'
                f'lodestar_fetch_failures_total{{kind="refused"}} {i}\n'
                f'lodestar_fetch_lost_total{{reason="evicted"}} {i * 3}\n'
                f'lodestar_fetch_lost_total{{reason="gave_up"}} {i}\n'
                f'lodestar_dht_peer_rejection_reasons_total{{reason="capacity"}} {i * 5}\n'
                f'lodestar_fetch_evicted_service_total{{stage="lookup"}} {i * 7}\n'
                f'lodestar_fetch_attempts_total {i * 11}\n', encoding="utf-8")
        script = str(Path(__file__).with_name("analyze_cr0.py"))
        outputs = set()
        for seed in ("0", "1", "2", "3", "42", "12345"):
            for fmt in ("markdown", "json"):
                result = subprocess.run([sys.executable, "-I", script, str(self.root), "--format", fmt],
                                        capture_output=True, text=True, env={"PYTHONHASHSEED": seed})
                self.assertEqual(result.returncode, 0, result.stderr)
                outputs.add((fmt, result.stdout))
        self.assertEqual(len(outputs), 2, "the report changed with PYTHONHASHSEED")
        text = next(out for fmt, out in outputs if fmt == "markdown")
        names = [line.removeprefix("Reason breakdown: ") for line in text.splitlines()
                 if line.startswith("Reason breakdown: ")]
        self.assertEqual(names, sorted(names))
        self.assertEqual(len(names), 4)

    def test_short_endpoint_disk_and_sequence(self):
        self.rows([sample(0, sequence=0), sample(60, sequence=1)])
        (self.root / "disk.jsonl").write_text(json.dumps({
            "volumes": [{"volume": "private-path", "free_bytes": 123}],
            "redirected_log_bytes": 42, "read_failures": 0}) + "\n{", encoding="utf-8")
        report = analyze(self.root)
        self.assertEqual(report["disk"]["minimum_free_bytes"], 123)
        self.assertEqual(report["issues"]["invalid_disk_observations"], 1)
        self.assertNotIn("private-path", json.dumps(report))
        self.rows([sample(0, sequence=2)])
        with self.assertRaisesRegex(ValueError, "sequence"):
            analyze(self.root)

    def test_unconfirmed_shutdown_does_not_claim_stopped(self):
        self.rows([sample(0)])
        for confirmed in (False, True):
            (self.root / "result.json").write_text(json.dumps({
                "schema_version": 2, "reason": "operator_stop", "samples": 1,
                "stop_mode": "unknown", "shutdown": {"exit_confirmed": confirmed}
            }), encoding="utf-8")
            self.assertEqual(analyze(self.root)["run_state"],
                             "stopped" if confirmed else "completion_unknown")

    def test_positive_suppression_in_baseline_requires_review(self):
        row = sample(0)
        row["values"]["dht_sample_suppression_seconds"] = 10
        self.rows([row])
        self.assertEqual(analyze(self.root)["issues"]["baseline_during_suppression"], 1)

    def test_invalid_result_versions_remain_unknown(self):
        self.rows([sample(0)])
        for version in (None, "2", True, 0, 3, {}):
            with self.subTest(version=version):
                (self.root / "result.json").write_text(json.dumps({
                    "schema_version": version, "reason": "baseline_window_complete"
                }), encoding="utf-8")
                report = analyze(self.root)
                self.assertEqual(report["run_state"], "completion_unknown")
                self.assertEqual(report["issues"]["invalid_result"], 1)

    def test_core_coverage_does_not_substitute_other_metrics_or_extra_hours(self):
        self.rows([sample(0), sample(60), sample(300)])
        report = analyze(self.root)
        quality = report["collection_quality"]
        self.assertEqual(quality["core_coverage"]["working_set_bytes"], 0.2)
        self.assertEqual(quality["core_coverage"]["writer_stored_total"], 0)
        self.assertFalse(quality["core_coverage_sufficient"])
        self.assertFalse(quality["gap_limit_met"])
        self.assertFalse(quality["full_duration_observed"])

    def test_hourly_disk_history_keeps_failures_without_volume_paths(self):
        self.rows([sample(0)])
        (self.root / "disk.jsonl").write_text("\n".join(json.dumps(row) for row in [
            {"elapsed_seconds": 0, "volumes": [{"volume": "private", "free_bytes": 100}], "redirected_log_bytes": 2},
            {"elapsed_seconds": 60, "volumes": [], "read_failures": 1},
            {"elapsed_seconds": 3600, "volumes": [{"volume": "private", "free_bytes": 50}], "redirected_log_bytes": 4},
        ]), encoding="utf-8")
        report = analyze(self.root)
        self.assertEqual(report["disk"]["hourly"]["0"]["read_failure_observations"], 1)
        self.assertEqual(report["disk"]["hourly"]["1"]["minimum_free_bytes"], 50)
        self.assertNotIn('"volume"', json.dumps(report))

    def test_clean_v2_shutdown_requires_confirmation_signal_and_exit_code(self):
        self.rows([sample(0)])
        result = {"schema_version": 2, "reason": "operator_stop", "samples": 1,
                  "stop_mode": "exited_0", "shutdown": {
                      "exit_confirmed": True, "signal_delivered": True,
                      "exit_code": 0, "errors": []}}
        for field, bad in (("exit_confirmed", False), ("signal_delivered", False),
                           ("exit_code", 1), ("errors", ["graceful_wait_failed"])):
            changed = {**result, "shutdown": {**result["shutdown"], field: bad}}
            (self.root / "result.json").write_text(json.dumps(changed), encoding="utf-8")
            self.assertFalse(analyze(self.root)["clean_shutdown"])
        (self.root / "result.json").write_text(json.dumps(result), encoding="utf-8")
        self.assertTrue(analyze(self.root)["clean_shutdown"])


    def run_settings(self, **settings):
        (self.root / "run.json").write_text(json.dumps({
            "commit": "a" * 40, "settings": {"baseline_hours": 24, **settings}}), encoding="utf-8")

    def outstanding(self, report):
        return report["phases"]["baseline"]["bounds"]["outstanding_rpcs"]

    def test_outstanding_rpc_limit_follows_the_preset(self):
        rows = [sample(0), sample(60)]
        for row in rows:
            row["values"]["dht_outstanding"] = 2000
        self.rows(rows)
        for preset, limit, over in (("low", 256, 2), ("balanced", 1024, 2),
                                    ("aggressive", 4096, 0), ("Aggressive", 4096, 0)):
            self.run_settings(intensity=preset)
            report = analyze(self.root)
            self.assertEqual(report["intensity"], preset.lower())
            self.assertEqual(self.outstanding(report)["limit"], limit)
            self.assertEqual(self.outstanding(report)["over_capacity_samples"], over)
            self.assertNotIn("intensity_assumed_balanced", report["issues"])
        # The flag wins over run.json.
        self.assertEqual(self.outstanding(analyze(self.root, intensity="low"))["limit"], 256)
        self.run_settings()
        report = analyze(self.root)
        self.assertEqual(self.outstanding(report)["limit"], 1024)
        self.assertEqual(report["issues"]["intensity_assumed_balanced"], 1)
        self.run_settings(intensity="turbo")
        with self.assertRaisesRegex(ValueError, "preset"):
            analyze(self.root)
        script = str(Path(__file__).with_name("analyze_cr0.py"))
        result = subprocess.run([sys.executable, script, str(self.root), "--format", "json",
                                 "--intensity", "aggressive"], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["intensity"], "aggressive")

    def kind_run(self, hours, **run_fields):
        """A run of `hours` of baseline whose run.json carries run_fields."""
        step = 600
        self.rows([sample(t, t // 60, process_id=123) for t in range(0, int(hours * 3600) + step, step)])
        run = {"commit": "a" * 40, **run_fields}
        (self.root / "run.json").write_text(json.dumps(run), encoding="utf-8")
        report = analyze(self.root, max_gap=step)
        return report, markdown(report)

    LINUX = {"python": "3.12.0"}

    def test_main_24h_run_is_gate_evidence(self):
        report, md = self.kind_run(24, **self.LINUX, settings={
            "leg": "main", "rehearsal": False, "baseline_hours": 24, "intensity": "balanced"})
        self.assertEqual(report["run"]["leg"], "main")
        self.assertIs(report["run"]["rehearsal"], False)
        self.assertEqual(report["run"]["baseline_required_hours"], 24)
        self.assertEqual(report["run"]["intensity"], "balanced")
        self.assertEqual(report["run"]["collector"], "linux")
        self.assertIs(report["run"]["gate_evidence"], True)
        self.assertEqual(report["run"]["gate_evidence_reasons"], [])
        self.assertIn("- Leg: main\n", md)
        self.assertIn("- Rehearsal: false\n", md)
        self.assertIn("- Gate evidence: yes\n", md)
        self.assertLess(md.index("## Run"), md.index("Commit:"))

    def test_gluetun_2h_leg_is_gate_evidence_labelled_as_the_leg(self):
        report, md = self.kind_run(2, **self.LINUX, settings={
            "leg": "gluetun", "rehearsal": False, "baseline_hours": 2, "intensity": "low"})
        self.assertEqual(report["run"]["leg"], "gluetun")
        self.assertIs(report["run"]["gate_evidence"], True)
        self.assertIn("- Leg: gluetun (2 h leg)\n", md)
        self.assertIn("- Baseline hours configured: 2\n", md)

    def test_rehearsal_is_never_gate_evidence(self):
        report, md = self.kind_run(24, **self.LINUX, settings={
            "leg": "main", "rehearsal": True, "baseline_hours": 24, "intensity": "balanced"})
        self.assertIs(report["run"]["gate_evidence"], False)
        self.assertEqual(report["run"]["gate_evidence_reasons"], ["rehearsal"])
        self.assertIn("- Gate evidence: no: rehearsal\n", md)

    def test_windows_run_without_settings_fields_shows_absent_not_guessed(self):
        report, md = self.kind_run(1, os="Microsoft Windows NT 10.0", common_sha256="b" * 64, settings={})
        run = report["run"]
        self.assertEqual(run["collector"], "windows")
        for field in ("leg", "rehearsal", "baseline_required_hours", "intensity"):
            self.assertIsNone(run[field])
        self.assertIs(run["gate_evidence"], False)
        self.assertEqual(run["gate_evidence_reasons"], [
            "rehearsal_not_recorded", "leg_not_recorded", "intensity_not_recorded"])
        for line in ("Leg", "Rehearsal", "Baseline hours configured", "Intensity"):
            self.assertIn(f"- {line}: not recorded\n", md)
        self.assertIn("- Collector: windows\n", md)
        # No settings at all must still work.
        report, _ = self.kind_run(1, os="Microsoft Windows NT 10.0")
        self.assertIsNone(report["run"]["intensity"])
        # The --intensity override picks the cap but is not a recorded intensity.
        report = analyze(self.root, intensity="aggressive")
        self.assertIsNone(report["run"]["intensity"])
        self.assertIn("intensity_not_recorded", report["run"]["gate_evidence_reasons"])

    def test_windows_run_with_the_real_settings_shape(self):
        # cr0-soak.ps1 writes intensity and baseline_hours but no leg or rehearsal.
        report, md = self.kind_run(24, os="Microsoft Windows NT 10.0", common_sha256="b" * 64,
                                   settings={"intensity": "balanced", "baseline_hours": 24, "warmup_minutes": 30})
        run = report["run"]
        self.assertEqual((run["collector"], run["intensity"], run["baseline_required_hours"]),
                         ("windows", "balanced", 24))
        self.assertIsNone(run["leg"])
        self.assertIs(run["gate_evidence"], False)
        self.assertEqual(run["gate_evidence_reasons"], ["rehearsal_not_recorded", "leg_not_recorded"])
        self.assertEqual(list(report)[0], "run")

    def test_gluetun_rehearsal_is_not_gate_evidence(self):
        report, _ = self.kind_run(2, **self.LINUX, settings={
            "leg": "gluetun", "rehearsal": True, "baseline_hours": 2, "intensity": "low"})
        self.assertEqual(report["run"]["gate_evidence_reasons"], ["rehearsal"])

    def test_run_too_short_for_its_leg_is_not_gate_evidence(self):
        for leg, hours in (("main", 23), ("gluetun", 1)):
            with self.subTest(leg=leg):
                report, md = self.kind_run(hours, **self.LINUX, settings={
                    "leg": leg, "rehearsal": False, "baseline_hours": hours, "intensity": "balanced"})
                self.assertIs(report["run"]["gate_evidence"], False)
                self.assertEqual(report["run"]["gate_evidence_reasons"], ["coverage_below_required"])
                self.assertIn("- Gate evidence: no: coverage_below_required\n", md)

    def test_deeply_nested_json_is_malformed_evidence(self):
        deep = "[" * 200000 + "]" * 200000
        self.rows([sample(0)])
        (self.root / "disk.jsonl").write_text(deep + "\n", encoding="utf-8")
        (self.root / "result.json").write_text(deep, encoding="utf-8")
        report = analyze(self.root)
        self.assertEqual(report["issues"]["invalid_disk_observations"], 1)
        self.assertEqual(report["issues"]["invalid_result"], 1)
        self.rows([sample(0)], deep + "\n")
        with self.assertRaisesRegex(ValueError, "Malformed samples.jsonl line 2"):
            analyze(self.root)
        (self.root / "run.json").write_text(deep, encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "Malformed run.json"):
            analyze(self.root)
        script = str(Path(__file__).with_name("analyze_cr0.py"))
        result = subprocess.run([sys.executable, script, str(self.root)],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn("Cannot analyze evidence", result.stderr)
        self.assertNotIn("Traceback", result.stderr)

    def memory_rows(self, slope_mib_per_hour, noise_mib=0.0, hours=24.5, step=60, **changes):
        # Baseline only, one sample a minute; a deterministic zig-zag stands in for noise.
        rows = []
        for i in range(int(hours * 3600 // step) + 1):
            t = i * step
            mib = 600 + slope_mib_per_hour * t / 3600 + (noise_mib if i % 2 else -noise_mib)
            rows.append(sample(t, i, private_bytes=mib * 2**20, **changes))
        return rows

    def test_memory_fit_flat_series_meets_the_bar(self):
        self.rows(self.memory_rows(0.0, noise_mib=1.0))
        fit = analyze(self.root)["memory_fit"]
        self.assertTrue(fit["window_complete"])
        self.assertEqual(fit["samples"], 721)
        self.assertAlmostEqual(fit["slope_mib_per_hour"], 0.0, places=2)
        self.assertLess(fit["ci95_mib_per_hour"][1], 2.0)
        self.assertTrue(fit["meets_bar"])
        self.assertAlmostEqual(fit["peak_mib"], 601.0, places=3)

    def test_memory_fit_growth_misses_the_bar(self):
        self.rows(self.memory_rows(3.0, noise_mib=1.0))
        report = analyze(self.root)
        fit = report["memory_fit"]
        self.assertAlmostEqual(fit["slope_mib_per_hour"], 3.0, places=2)
        self.assertFalse(fit["meets_bar"])
        self.assertIn("misses the bar", markdown(report))

    def test_memory_fit_uses_only_hours_12_to_24(self):
        # Steep warm-up growth before hour 12 is outside the gate.
        rows = self.memory_rows(0.0, noise_mib=0.5)
        for row in rows:
            if row["elapsed_seconds"] < 12 * 3600:
                row["private_bytes"] -= (12 * 3600 - row["elapsed_seconds"]) / 3600 * 10 * 2**20
        self.rows(rows)
        fit = analyze(self.root)["memory_fit"]
        self.assertAlmostEqual(fit["slope_mib_per_hour"], 0.0, places=2)
        self.assertTrue(fit["meets_bar"])

    def test_memory_fit_short_run_is_not_judged(self):
        self.rows(self.memory_rows(0.0, hours=20))
        fit = analyze(self.root)["memory_fit"]
        self.assertFalse(fit["window_complete"])
        self.assertIsNone(fit["meets_bar"])
        self.assertIsNotNone(fit["slope_mib_per_hour"])

    def test_memory_fit_gap_inside_the_window_is_not_judged(self):
        rows = [r for r in self.memory_rows(0.0, noise_mib=1.0)
                if not 12.5 * 3600 < r["elapsed_seconds"] < 23.9 * 3600]
        self.rows(rows)
        fit = analyze(self.root)["memory_fit"]
        self.assertGreaterEqual(fit["samples"], 30)
        self.assertFalse(fit["window_complete"])
        self.assertIsNone(fit["meets_bar"])
        self.assertGreater(fit["max_gap_seconds"], 180)

    def test_memory_fit_refuses_a_restart_in_the_window(self):
        rows = self.memory_rows(0.0)
        for row in rows[900:]:
            row["process_id"] = 456
        self.rows(rows)
        fit = analyze(self.root)["memory_fit"]
        self.assertEqual(fit["refused"], "identity_change")
        self.assertIsNone(fit["meets_bar"])

    def test_memory_fit_ignores_a_failed_metrics_read(self):
        rows = self.memory_rows(0.0, noise_mib=1.0)
        rows[1000]["metrics_ok"] = False
        rows[1000]["values"] = {}
        self.rows(rows)
        fit = analyze(self.root)["memory_fit"]
        self.assertIsNone(fit["refused"])
        self.assertTrue(fit["meets_bar"])

    def context_rows(self, per_1000, per_hour, stored=True, hours=24.5, step=60):
        # Stored torrents grow ~669/h with a slow wobble, so hours and stored differ.
        rows = []
        for i in range(int(hours * 3600 // step) + 1):
            t, h = i * step, i * step / 3600
            total = 669 * h + 400 * math.sin(i / 37)  # monotone: 400/37 < 11 per step
            row = sample(t, i, private_bytes=(600 + per_hour * h + per_1000 * total / 1000) * 2**20)
            if stored:
                row["values"]["writer_stored_total"] = total + 5000
            rows.append(row)
        return rows

    def test_memory_context_tracking_stored_torrents(self):
        self.rows(self.context_rows(per_1000=2.8, per_hour=0.0))
        ctx = analyze(self.root)["memory_context"]
        self.assertTrue(ctx["available"])
        self.assertAlmostEqual(ctx["mib_per_1000_stored"], 2.8, places=2)
        self.assertAlmostEqual(ctx["stored_per_hour"], 669, delta=150)
        self.assertAlmostEqual(ctx["explained_slope_mib_per_hour"], 2.8 * ctx["stored_per_hour"] / 1000, places=3)
        self.assertAlmostEqual(ctx["time_only_slope_mib_per_hour"], 0.0, places=1)
        self.assertAlmostEqual(ctx["stored_coefficient_mib_per_1000"], 2.8, places=2)
        text = markdown(analyze(self.root))
        self.assertIn("## Memory context (informational, not part of the gate)", text)
        self.assertIn("A hint, not a test", text)

    def test_memory_context_growth_with_time_alone(self):
        self.rows(self.context_rows(per_1000=0.0, per_hour=3.0))
        ctx = analyze(self.root)["memory_context"]
        self.assertAlmostEqual(ctx["time_only_slope_mib_per_hour"], 3.0, places=1)
        self.assertGreater(ctx["time_only_ci95_mib_per_hour"][0], 2.0)
        self.assertAlmostEqual(ctx["stored_coefficient_mib_per_1000"], 0.0, places=1)

    def test_memory_context_skipped_without_stored_data(self):
        self.rows(self.context_rows(per_1000=0.0, per_hour=1.0, stored=False))
        report = analyze(self.root)
        ctx = report["memory_context"]
        self.assertFalse(ctx["available"])
        self.assertEqual(ctx["reason"], "no_stored_torrent_data")
        self.assertIsNone(ctx["mib_per_1000_stored"])
        self.assertIn("Skipped: no_stored_torrent_data", markdown(report))

    def test_memory_context_skip_paths(self):
        base = self.context_rows(per_1000=2.8, per_hour=0.0)
        mid = len(base) // 2
        for row in base[mid:]:
            row["process_identity"] = "b"
        for row in base[:mid]:
            row["process_identity"] = "a"
        self.rows(base)
        self.assertEqual(analyze(self.root)["memory_context"]["reason"], "identity_change")
        reset = self.context_rows(per_1000=2.8, per_hour=0.0)
        for row in reset[len(reset) // 2:]:
            row["values"]["writer_stored_total"] -= 15000
        self.rows(reset)
        self.assertEqual(analyze(self.root)["memory_context"]["reason"], "counter_reset")
        self.rows(self.context_rows(2.8, 0.0, hours=12.4))
        self.assertEqual(analyze(self.root)["memory_context"]["reason"], "too_few_samples")
        self.rows([sample(0, 0, phase="startup")])
        self.assertEqual(analyze(self.root)["memory_context"]["reason"], "no_baseline")

    def test_memory_context_collinear_regressors_skip_the_two_variable_fit(self):
        rows = self.memory_rows(1.0)
        for row in rows:
            row["values"]["writer_stored_total"] = 700 * row["elapsed_seconds"] / 3600
        self.rows(rows)
        ctx = analyze(self.root)["memory_context"]
        self.assertTrue(ctx["available"])
        self.assertEqual(ctx["two_variable_reason"], "regressors_collinear")
        self.assertIsNone(ctx["time_only_slope_mib_per_hour"])
        self.assertIn("Two-variable fit not shown: regressors_collinear", markdown(analyze(self.root)))

    def test_snapshots_without_the_search_metrics_still_analyze(self):
        # An r3-style run: no commit counter and no gauge in any snapshot.
        rows = self.context_rows(per_1000=2.8, per_hour=0.0)
        for i in range(len(rows)):
            rows[i]["metrics_ok"] = True
            (self.root / "metrics" / f"{i:05d}.prom").write_text("lodestar_torrents 5\n")
        self.rows(rows)
        report = analyze(self.root)
        ctx = report["memory_context"]
        self.assertTrue(ctx["available"])  # the stored-torrent view is unchanged
        self.assertIsNone(ctx["mib_per_1000_commits"])
        self.assertEqual(ctx["commits_reason"], "absent")
        self.assertEqual(report["search"], {"commits": None, "mmap_cache_entries": None})
        text = markdown(report)
        self.assertIn("search commits: not shown (absent)", text)
        self.assertIn("Search mmap-cache entries: absent", text)

    def test_memory_fit_over_an_incomplete_window_is_provisional(self):
        self.rows(self.memory_rows(3.0, noise_mib=1.0, hours=20))
        report = analyze(self.root)
        fit = report["memory_fit"]
        self.assertTrue(fit["provisional"])
        self.assertIsNone(fit["meets_bar"])
        text = markdown(report)
        gate = text[text.index("## Memory gate"):text.index("## Memory context")]
        self.assertIn("Bar: upper 95% bound <= 2 MiB/h: provisional (window incomplete)", gate)
        for wording in ("meets the bar", "misses the bar", "not judged"):
            self.assertNotIn(wording, gate)

    def test_complete_window_is_not_provisional(self):
        self.rows(self.memory_rows(3.0, noise_mib=1.0))
        self.assertNotIn("provisional", analyze(self.root)["memory_fit"])

    def test_memory_gate_is_unchanged_by_the_context_view(self):
        # Pinned from the analyzer before the context view existed.
        plain = analyze_with(self, self.memory_rows(3.0, noise_mib=1.0))
        with_stored = self.memory_rows(3.0, noise_mib=1.0)
        for i, row in enumerate(with_stored):
            row["values"]["writer_stored_total"] = 700 * i
        self.rows(with_stored)
        stored_report = analyze(self.root)
        for report in (plain, stored_report):
            fit = report["memory_fit"]
            self.assertEqual(
                {k: v for k, v in fit.items() if k not in ("ci95_mib_per_hour", "max_gap_seconds", "standard_error")},
                {"bar_upper_ci_mib_per_hour": 2.0, "from_hour": 12.0, "meets_bar": False, "peak_mib": 674.45,
                 "refused": None, "samples": 721, "series": "private_bytes", "slope_mib_per_hour": 3.0,
                 "to_hour": 24.0, "window_complete": True})
            self.assertEqual(fit["ci95_mib_per_hour"], [2.9789283650019294, 3.0210716349980706])
            text = markdown(report)
            gate = text[text.index("## Memory gate"):text.index("## Memory context")]
            self.assertEqual(gate, (
                "## Memory gate\n\n"
                "- Series: `private_bytes`, baseline hours 12-24 (721 samples, window complete: true)\n"
                "- OLS slope: 3.00 MiB/h, 95% CI [2.98, 3.02] MiB/h\n"
                "- Bar: upper 95% bound <= 2 MiB/h: misses the bar\n"
                "- Peak private memory: 674.45 MiB\n\n"))

    def attribution_rows(self, hours=24.5, step=60, sqlite=True, segments=True, slope=0.0, noise=0.0):
        """Per minute: anon grows 1 MiB/h, mimalloc 0.25 MiB/h, SQLite flat at 40 MiB."""
        rows = self.memory_rows(slope, noise_mib=noise, hours=hours, step=step)
        for i, row in enumerate(rows):
            h = row["elapsed_seconds"] / 3600
            prom = ["# HELP lodestar_mem_rss_anon_bytes Resident anonymous memory.",
                    "# TYPE lodestar_mem_rss_anon_bytes gauge",
                    f"lodestar_mem_rss_anon_bytes {int((400 + 1.0 * h) * 2**20)}",
                    f"lodestar_mem_rss_file_bytes {int(900 * 2**20)}",
                    f"lodestar_mem_anon_huge_bytes {int(212 * 2**20)}",
                    f"lodestar_mem_mimalloc_committed_bytes {int((150 + 0.25 * h) * 2**20)}",
                    f"lodestar_mem_mimalloc_peak_committed_bytes {int(160 * 2**20)}",
                    "lodestar_mem_rss_anon_bytes 1"]  # a duplicate: the first value is kept
            if sqlite:
                prom += [f"lodestar_mem_sqlite_bytes {40 * 2**20}",
                         f"lodestar_mem_sqlite_highwater_bytes {48 * 2**20}"]
            if segments:
                prom += [f'lodestar_search_segments{{index="main"}} {10 + i % 3}',
                         'lodestar_search_segments{index="files"} 4',
                         'lodestar_search_segments{index="other"} 99']
            (self.root / "metrics" / f"{i:05d}.prom").write_text("\n".join(prom) + "\n")
        return rows

    def test_memory_attribution_end_to_end(self):
        # A glibc build: C's malloc is the C library's, so SQLite is outside mimalloc.
        self.run_json(build_target="x86_64-unknown-linux-gnu")
        self.rows(self.attribution_rows())
        report = analyze(self.root)
        attr = report["memory_attribution"]
        self.assertIsNone(attr["refused"])
        anon = attr["series"]["mem_rss_anon_bytes"]
        self.assertEqual(anon["status"], "ok")
        self.assertEqual(anon["samples"], 721)
        self.assertAlmostEqual(anon["first"], 412.0, places=2)
        self.assertAlmostEqual(anon["last"], 424.0, places=2)
        self.assertAlmostEqual(anon["slope_per_hour"], 1.0, places=3)
        self.assertAlmostEqual(attr["series"]["mem_mimalloc_committed_bytes"]["slope_per_hour"], 0.25, places=3)
        self.assertAlmostEqual(attr["series"]["mem_anon_huge_bytes"]["last"], 212.0)
        self.assertAlmostEqual(attr["series"]["mem_sqlite_bytes"]["slope_per_hour"], 0.0, places=6)
        # Other heap = anon - mimalloc - sqlite: 400 - 150 - 40 = 210 MiB at hour 0, +0.75 MiB/h.
        other = attr["other_heap"]
        self.assertTrue(attr["other_heap_excludes_sqlite"])
        self.assertEqual((attr["build_target"], attr["c_allocator"], attr["c_allocator_source"]),
                         ("x86_64-unknown-linux-gnu", "libc", "build_target"))
        self.assertAlmostEqual(other["first"], 210 + 0.75 * 12, places=2)
        self.assertAlmostEqual(other["slope_per_hour"], 0.75, places=3)
        self.assertEqual(attr["segments"]["main"]["max"], 12)
        self.assertAlmostEqual(attr["segments"]["files"]["slope_per_hour"], 0.0)
        self.assertNotIn("other", attr["segments"])
        text = markdown(report)
        self.assertIn("Memory attribution (informational; baseline hours 12-24):", text)
        self.assertIn("| RssAnon (MiB) | 412.00 | 424.00 | 424.00 | 1.00 | 721 |", text)
        self.assertIn("| Other heap (zstd, other C code): RssAnon - mimalloc - SQLite (MiB) | 219.00 | 228.00 |", text)
        self.assertIn("| Segments, main (count) |", text)
        self.assertIn("C allocator: the C library's malloc, from the build target "
                      "(build target `x86_64-unknown-linux-gnu`).", text)

    def test_musl_builds_do_not_subtract_sqlite_twice(self):
        # X-mimalloc-override: in musl builds SQLite is already in mimalloc's bytes.
        for target in ("x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"):
            self.run_json(build_target=target)
            self.rows(self.attribution_rows())
            report = analyze(self.root)
            attr = report["memory_attribution"]
            self.assertEqual((attr["build_target"], attr["c_allocator"], attr["c_allocator_source"]),
                             (target, "mimalloc", "build_target"))
            self.assertFalse(attr["other_heap_excludes_sqlite"])
            # anon - mimalloc only: 400 - 150 = 250 MiB at hour 0, +0.75 MiB/h; SQLite's
            # 40 MiB is not taken off again.
            self.assertAlmostEqual(attr["other_heap"]["first"], 250 + 0.75 * 12, places=2)
            self.assertAlmostEqual(attr["other_heap"]["slope_per_hour"], 0.75, places=3)
            self.assertAlmostEqual(attr["series"]["mem_sqlite_bytes"]["last"], 40.0)  # still reported
            text = markdown(report)
            self.assertIn("| Other heap (outside mimalloc, which holds SQLite and zstd here): "
                          "RssAnon - mimalloc (MiB) | 259.00 | 268.00 |", text)
            self.assertIn(f"C allocator: mimalloc (its `override`), from the build target (build target `{target}`).",
                          text)
            self.assertNotIn("RssAnon - mimalloc - SQLite", text)

    def test_unknown_build_target_subtracts_nothing_more_and_says_so(self):
        # Older collectors don't record the target; a malformed one counts as absent.
        for fields in ({}, {"build_target": "x86_64 linux musl"}, {"build_target": 7},
                       {"build_target": "a" * 200}):
            self.run_json(**fields)
            self.rows(self.attribution_rows())
            report = analyze(self.root)
            attr = report["memory_attribution"]
            self.assertEqual((attr["build_target"], attr["c_allocator"], attr["c_allocator_source"]),
                             (None, None, None), fields)
            self.assertFalse(attr["other_heap_excludes_sqlite"])
            self.assertAlmostEqual(attr["other_heap"]["first"], 250 + 0.75 * 12, places=2)
            text = markdown(report)
            self.assertIn("| Other heap (C allocator unknown: may include SQLite): RssAnon - mimalloc (MiB) |", text)
            self.assertIn("C allocator: unknown (build target not recorded).", text)
            self.assertNotIn("x86_64 linux musl", json.dumps(report))

    def test_the_operator_can_state_the_c_allocator(self):
        # A musl build from before the override: the target alone can't tell.
        self.run_json(build_target="x86_64-unknown-linux-musl")
        self.rows(self.attribution_rows())
        attr = analyze(self.root, allocator="libc")["memory_attribution"]
        self.assertEqual((attr["build_target"], attr["c_allocator"], attr["c_allocator_source"]),
                         ("x86_64-unknown-linux-musl", "libc", "operator"))
        self.assertTrue(attr["other_heap_excludes_sqlite"])
        self.assertAlmostEqual(attr["other_heap"]["first"], 210 + 0.75 * 12, places=2)
        self.run_json()
        attr = analyze(self.root, allocator="mimalloc")["memory_attribution"]
        self.assertEqual((attr["c_allocator"], attr["c_allocator_source"]), ("mimalloc", "operator"))
        self.assertFalse(attr["other_heap_excludes_sqlite"])
        with self.assertRaisesRegex(ValueError, "Unknown C allocator"):
            analyze(self.root, allocator="glibc")
        script = str(Path(__file__).with_name("analyze_cr0.py"))
        result = subprocess.run([sys.executable, "-I", script, str(self.root), "--c-allocator", "libc",
                                 "--format", "json"], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["memory_attribution"]["c_allocator_source"], "operator")
        result = subprocess.run([sys.executable, "-I", script, str(self.root), "--c-allocator", "glibc"],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)

    def test_memory_gate_is_unchanged_by_the_c_allocator(self):
        # The gate (private_bytes over hours 12-24) never reads the build target or the
        # allocator. Pinned from the analyzer before X-mimalloc-docs: the same numbers as
        # test_memory_gate_is_unchanged_by_the_context_view, on rows carrying every gauge.
        reports = []
        for fields, allocator in (({}, None), ({"build_target": "x86_64-unknown-linux-musl"}, None),
                                  ({"build_target": "x86_64-unknown-linux-gnu"}, None),
                                  ({"build_target": "x86_64-pc-windows-msvc"}, None),
                                  ({"build_target": "x86_64-unknown-linux-musl"}, "libc"),
                                  ({}, "mimalloc")):
            self.run_json(**fields)
            self.rows(self.attribution_rows(slope=3.0, noise=1.0))
            report = analyze(self.root, now=datetime(2026, 11, 5, tzinfo=timezone.utc), allocator=allocator)
            fit = report["memory_fit"]
            self.assertEqual(
                {k: v for k, v in fit.items() if k not in ("ci95_mib_per_hour", "max_gap_seconds", "standard_error")},
                {"bar_upper_ci_mib_per_hour": 2.0, "from_hour": 12.0, "meets_bar": False, "peak_mib": 674.45,
                 "refused": None, "samples": 721, "series": "private_bytes", "slope_mib_per_hour": 3.0,
                 "to_hour": 24.0, "window_complete": True})
            self.assertEqual(fit["ci95_mib_per_hour"], [2.9789283650019294, 3.0210716349980706])
            text = markdown(report)
            self.assertEqual(text[text.index("## Memory gate"):text.index("## Memory context")], (
                "## Memory gate\n\n"
                "- Series: `private_bytes`, baseline hours 12-24 (721 samples, window complete: true)\n"
                "- OLS slope: 3.00 MiB/h, 95% CI [2.98, 3.02] MiB/h\n"
                "- Bar: upper 95% bound <= 2 MiB/h: misses the bar\n"
                "- Peak private memory: 674.45 MiB\n\n"))
            reports.append(report)
        # Nothing but the attribution section differs between them.
        rest = [{k: v for k, v in r.items() if k != "memory_attribution"} for r in reports]
        self.assertTrue(all(r == rest[0] for r in rest))

    def test_memory_attribution_without_sqlite_says_so(self):
        self.run_json(build_target="x86_64-pc-windows-msvc")
        self.rows(self.attribution_rows(sqlite=False, segments=False))
        report = analyze(self.root)
        attr = report["memory_attribution"]
        self.assertEqual(attr["series"]["mem_sqlite_bytes"], {"status": "absent"})
        self.assertFalse(attr["other_heap_excludes_sqlite"])
        self.assertAlmostEqual(attr["other_heap"]["slope_per_hour"], 0.75, places=3)
        self.assertEqual(attr["segments"]["main"], {"status": "absent"})
        text = markdown(report)
        self.assertIn("| Other heap (includes SQLite, zstd, C code): RssAnon - mimalloc (MiB) | 259.00 |", text)
        self.assertIn("| SQLite | absent | | | | |", text)

    def test_other_heap_subtracts_sqlite_from_every_point_or_none(self):
        # SQLite appears part-way through the window (an upgrade mid-run, say).
        rows = self.attribution_rows(sqlite=False)
        for i in range(len(rows) * 3 // 4, len(rows)):
            (self.root / "metrics" / f"{i:05d}.prom").write_text(
                (self.root / "metrics" / f"{i:05d}.prom").read_text()
                + f"lodestar_mem_sqlite_bytes {40 * 2**20}\n")
        self.run_json(build_target="x86_64-unknown-linux-gnu")
        self.rows(rows)
        attr = analyze(self.root)["memory_attribution"]
        self.assertFalse(attr["other_heap_excludes_sqlite"])
        # No step: the slope is anon - mimalloc's alone, 0.75 MiB/h.
        self.assertAlmostEqual(attr["other_heap"]["slope_per_hour"], 0.75, places=3)
        self.assertIn("read it with the AnonHugePages row", markdown(analyze(self.root)))

    def test_the_attribution_table_stays_one_table(self):
        # Every series, other-heap and segment row sits in one unbroken block of
        # table lines; the approximation note comes after it.
        self.rows(self.attribution_rows())
        lines = markdown(analyze(self.root)).splitlines()
        start = lines.index("| Series | First | Last | Max | Slope per hour | Samples |")
        end = start
        while end < len(lines) and lines[end].startswith("|"):
            end += 1
        table = lines[start:end]
        self.assertTrue(any(l.startswith("| Segments, main (count) |") for l in table), table)
        self.assertTrue(any(l.startswith("| Segments, files (count) |") for l in table), table)
        self.assertTrue(any(l.startswith("| Other heap") for l in table), table)
        self.assertLess(end + 1, len(lines), "the note is missing after the table")
        self.assertEqual(lines[end], "")
        self.assertTrue(lines[end + 1].startswith("Other heap is an approximation"), lines[end + 1])

    def test_a_restart_seen_only_in_a_failed_read_still_refuses(self):
        # The gate counts every baseline row for the process; so must this section.
        # A failed read has no values (as in test_memory_fit_ignores_a_failed_metrics_read).

        def failed(change):
            rows = self.attribution_rows()
            for row in rows[len(rows) * 3 // 4::50]:
                row.update(metrics_ok=False, values={}, **change)
            for row in rows:
                row.setdefault("process_identity", "p1")
            self.rows(rows)
            report = analyze(self.root)
            return report["memory_fit"]["refused"], report["memory_attribution"]["refused"]
        # Another process, seen only in failed reads.
        self.assertEqual(failed({"process_id": 456}), ("identity_change", "identity_change"))
        # The same process id, another identity, seen only in failed reads.
        self.assertEqual(failed({"process_identity": "other"}), ("identity_change", "identity_change"))
        # Failed reads of the same process lack a start time: no restart.
        self.assertEqual(failed({}), (None, None))

    def test_snapshots_without_the_memory_metrics_report_them_absent(self):
        # An r4-style run: snapshots carry other series, none of X-mem-gauges'.
        rows = self.memory_rows(0.0)
        for i in range(len(rows)):
            (self.root / "metrics" / f"{i:05d}.prom").write_text("lodestar_torrents 5\n")
        self.rows(rows)
        report = analyze(self.root)
        attr = report["memory_attribution"]
        self.assertIsNone(attr["refused"])
        self.assertTrue(all(g == {"status": "absent"} for g in attr["series"].values()))
        self.assertEqual(attr["other_heap"], {"status": "absent"})
        self.assertIsNone(attr["other_heap_excludes_sqlite"])
        self.assertEqual(attr["segments"], {"main": {"status": "absent"}, "files": {"status": "absent"}})
        self.assertTrue(report["memory_fit"]["window_complete"])  # the gate is untouched
        self.assertIn("| RssAnon | absent | | | | |", markdown(report))
        # No snapshots at all (an older collector) reads the same way.
        for path in (self.root / "metrics").iterdir():
            path.unlink()
        self.rows(self.memory_rows(0.0))
        attr = analyze(self.root)["memory_attribution"]
        self.assertEqual(attr["series"]["mem_rss_anon_bytes"], {"status": "absent"})

    def test_memory_attribution_short_runs_and_restarts(self):
        # Present, but the run stopped before hour 12.
        self.rows(self.attribution_rows(hours=2))
        attr = analyze(self.root)["memory_attribution"]
        self.assertEqual(attr["series"]["mem_rss_anon_bytes"], {"status": "no_window_samples"})
        # A restart inside the window refuses the slopes, as the gate does.
        rows = self.attribution_rows()
        for row in rows[len(rows) * 3 // 4:]:
            row["process_id"] = 456
        self.rows(rows)
        report = analyze(self.root)
        attr = report["memory_attribution"]
        self.assertEqual(attr["refused"], "identity_change")
        self.assertEqual((attr["series"], attr["segments"]), ({}, {}))  # no misleading statuses
        self.assertIn("Not shown: identity_change.", markdown(report))
        # A missing process_identity on some rows isn't a restart, as in the gate.
        rows = self.attribution_rows()
        for row in rows[::2]:
            row["process_identity"] = "p1"
        self.rows(rows)
        self.assertIsNone(analyze(self.root)["memory_attribution"]["refused"])
        self.rows([sample(0, phase="startup")])
        self.assertEqual(analyze(self.root)["memory_attribution"]["refused"], "no_baseline")

    # ---- Windows 1.0 checks (M8-WIN-1) ----
    def svc_rows(self, pid_at=lambda i: 700, start=1000):
        return [sample(i * 60, i, service_pid=pid_at(i), service_start_ticks=pid_at(i) * 10, image_sha256="b" * 64,
                       values={"start_time_seconds": start, "dht_peer_admitted_total": i,
                               "dht_peer_active": 3, "dht_peer_queued": 0,
                               "dht_sample_cooling": 200000, "dht_sample_inflight": 0})
                for i in range(5)]

    def life(self, **parts):
        (self.root / "lifecycle.json").write_text(json.dumps({"schema_version": 1, **parts}))

    GOOD_UP = {"from_version": "0.5.1", "to_version": "1.0.0", "service_running": True,
               "service_restarted": True, "image_matches_installed": True,
               "from_image_sha256": "a" * 64, "to_image_sha256": "b" * 64,
               "data_present": True, "torrents_before": 500, "torrents_after": 520}
    GOOD_UN = {"service_present": False, "program_files_present": False,
               "data_present": True, "data_policy": "kept"}

    def done(self, **changes):
        (self.root / "result.json").write_text(json.dumps(
            {"schema_version": 2, "reason": "baseline_window_complete", "samples": 5,
             "service_detached": True, "finalization_complete": True,
             "shutdown": {"exit_confirmed": False}, **changes}))

    def checks(self, rows=None, finished=True, bound="b" * 64, **parts):
        self.run_json(service_image_sha256=bound, settings={"baseline_hours": 24, "service_mode": True})
        self.rows(rows if rows is not None else self.svc_rows())
        if finished:
            self.done()
        if parts:
            self.life(**parts)
        return analyze(self.root)["windows_checks"]

    def test_memory_fit_constants_are_unchanged(self):
        from analyze_cr0 import MEMORY_FIT
        self.assertEqual((MEMORY_FIT["field"], MEMORY_FIT["from_hour"], MEMORY_FIT["to_hour"],
                          MEMORY_FIT["bar_mib_per_hour"], MEMORY_FIT["min_samples"]),
                         ("private_bytes", 12.0, 24.0, 2.0, 30))

    def test_old_evidence_reports_absent_not_failed(self):
        self.rows([sample(0, 1), sample(60, 2)])
        checks = analyze(self.root)["windows_checks"]
        self.assertEqual({c["status"] for c in checks.values()}, {"absent"})
        self.assertIn("absent", markdown(analyze(self.root)))

    def test_service_restart_pass_and_fail(self):
        self.assertEqual(self.checks()["service_restart"]["status"], "pass")
        pid = self.checks(self.svc_rows(lambda i: 700 if i < 3 else 701))
        self.assertEqual(pid["service_restart"]["status"], "fail")
        start = self.checks(self.svc_rows(start=1000)[:3] + self.svc_rows(start=2000)[3:])
        self.assertEqual(start["service_restart"]["status"], "fail")

    def test_unfinished_or_aborted_service_run_fails_restart_check(self):
        # A restart or reboot ends the loop before another sample, so one PID proves nothing.
        self.assertEqual(self.checks(finished=False)["service_restart"]["status"], "fail")
        for reason in ("process_exited_1", "collector_error"):
            self.rows(self.svc_rows())
            self.done(reason=reason)
            self.assertEqual(analyze(self.root)["windows_checks"]["service_restart"]["status"], "fail", reason)

    def test_sample_without_service_identity_fails_restart_check(self):
        rows = self.svc_rows()
        del rows[2]["service_pid"]
        self.assertEqual(self.checks(rows)["service_restart"]["status"], "fail")

    def test_build_binding(self):
        self.assertEqual(self.checks()["build_binding"]["status"], "pass")
        rows = self.svc_rows()
        rows[3]["image_sha256"] = "c" * 64  # binary swapped mid-run
        self.assertEqual(self.checks(rows)["build_binding"]["status"], "fail")
        self.assertEqual(self.checks(bound="d" * 64)["build_binding"]["status"], "fail")
        rows = self.svc_rows()
        del rows[1]["image_sha256"]
        self.assertEqual(self.checks(rows)["build_binding"]["status"], "fail")

    def test_versions_are_information_not_a_check(self):
        # An RC prints the base version, so equal versions with a new binary still pass.
        up = {**self.GOOD_UP, "from_version": "1.0.0", "to_version": "1.0.0"}
        self.assertEqual(self.checks(upgrade=up)["upgrade"]["status"], "pass")

    def test_service_restart_needs_a_complete_sample_set(self):
        self.assertEqual(self.checks([])["service_restart"]["status"], "fail")  # empty samples
        self.rows(self.svc_rows())
        self.done(samples=1440)  # truncated file
        self.assertEqual(analyze(self.root)["windows_checks"]["service_restart"]["status"], "fail")
        self.done(finalization_complete=False)
        self.assertEqual(analyze(self.root)["windows_checks"]["service_restart"]["status"], "fail")

    def test_service_mode_without_any_hash_fails_binding(self):
        rows = [{k: v for k, v in r.items() if k != "image_sha256"} for r in self.svc_rows()]
        self.assertEqual(self.checks(rows, bound=None)["build_binding"]["status"], "fail")

    def test_lifecycle_file_cannot_hide_check_lines_with_a_key(self):
        checks = self.checks(invalid=True, upgrade=self.GOOD_UP, uninstall=self.GOOD_UN)
        self.assertEqual((checks["upgrade"]["status"], checks["uninstall"]["status"]), ("pass", "pass"))

    def test_upgrade_must_land_on_the_bound_binary(self):
        same = {**self.GOOD_UP, "from_image_sha256": "b" * 64}
        other = {**self.GOOD_UP, "to_image_sha256": "c" * 64}
        for up in (same, other):
            self.assertEqual(self.checks(upgrade=up)["upgrade"]["status"], "fail")

    def test_upgrade_pass_and_fail(self):
        self.assertEqual(self.checks(upgrade=self.GOOD_UP)["upgrade"]["status"], "pass")
        for change in ({"service_running": False}, {"service_restarted": False}, {"image_matches_installed": False}, {"data_present": False}, {"torrents_after": 10},
                       {"torrents_before": 0}, {"torrents_after": None}):
            self.assertEqual(self.checks(upgrade={**self.GOOD_UP, **change})["upgrade"]["status"],
                             "fail", change)

    def test_uninstall_pass_and_fail(self):
        self.assertEqual(self.checks(uninstall=self.GOOD_UN)["uninstall"]["status"], "pass")
        removed = {**self.GOOD_UN, "data_present": False, "data_policy": "removed"}
        self.assertEqual(self.checks(uninstall=removed)["uninstall"]["status"], "pass")
        for change in ({"service_present": True}, {"program_files_present": True},
                       {"data_present": False}, {"data_policy": "removed"}, {"data_policy": "maybe"}):
            self.assertEqual(self.checks(uninstall={**self.GOOD_UN, **change})["uninstall"]["status"],
                             "fail", change)

    def test_invalid_lifecycle_file_fails_both_steps(self):
        self.rows(self.svc_rows())
        (self.root / "lifecycle.json").write_text("{not json")
        checks = analyze(self.root)["windows_checks"]
        self.assertEqual((checks["upgrade"]["status"], checks["uninstall"]["status"]), ("fail", "fail"))

    def test_detached_service_result_is_not_a_failed_shutdown(self):
        self.rows(self.svc_rows())
        self.done()
        report = analyze(self.root)
        self.assertIsNone(report["clean_shutdown"])
        self.assertEqual(report["run_state"], "detached_service_running")


if __name__ == "__main__":
    unittest.main()
