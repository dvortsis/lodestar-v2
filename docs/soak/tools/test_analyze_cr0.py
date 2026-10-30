import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

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


if __name__ == "__main__":
    unittest.main()
