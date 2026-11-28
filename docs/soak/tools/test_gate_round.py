"""Round 1 (X-cr-round1-gate): `segment --declaration` and `gate-round`.

docs/plan/2026-11-27-revised-path-to-100.md §5.2 (items 2, 4-6, 9-12), §6.1-§6.4 and §8;
the formats are in docs/decisions/2026-11-27-X-cr-round1-gate.md. Every verdict row of
§5.2's fixture table has a test here with its expected verdict (the collect slice owns the
recording, CR-0w first start and leak-test rows).

Segments are written as the collector writes them, by test_analyze_cr's Sim, so the
declaration and bundle checks run through real files. Round archives come from
fixtures/round1/make_archive.py, changed per test.
"""

import hashlib
import json
import math
import sys
import tempfile
import unittest
from datetime import timedelta
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE / "linux"))
sys.path.insert(0, str(HERE / "fixtures" / "round1"))

import analyze_cr  # noqa: E402
import analyze_cr0  # noqa: E402
import collect  # noqa: E402
import make_archive as mk  # noqa: E402
import test_analyze_cr as cr  # noqa: E402
import test_analyze_cr_col6 as col6  # noqa: E402

L = collect.LEDGER
GOLDEN = HERE / "fixtures" / "round1" / "l3_pass_verdict.json"


# ----------------------------------------------------------------- segments
class Round1Sim(col6.Stage3Sim):
    """One round-1 arm: a comb segment whose binary renders every §6.1 series."""

    def __init__(self, arm="B", preset="balanced", switches=None, **kw):
        switches = dict(mk.ALL_ON if switches is None else switches)
        on = [s for s, v in switches.items() if v and s != "hint_first"]
        kw.setdefault("hours", 4)
        kw.setdefault("hour_s", 1200)
        super().__init__(treatment="hint", on=switches["hint_first"], winners=on, kind="comb", **kw)
        self.switches.update(switches)
        self.utp_in = 50.0 if switches["utp"] else 0.0
        self.grants = 10.0 if switches["coordinated_admission"] else 0.0
        self.route_skipped = 0.0
        self.split_full = switches["sampler_cooldown_split"]
        if not self.split_full:
            self.spill = lambda e: 0
        self.arm, self.preset = arm, preset
        self.api = {"intensity": preset, "paused": False}
        self.env_preset = "unset" if preset == "balanced" else preset
        self.effective = dict(self.switches)
        self.qps = 100.0               # queries per second (Balanced allows 200)
        self.outstanding = lambda e: 500
        self.bundle = {"dht_hints_captured_total": 2.0, "fetch_stream_batches_total": 3.0,
                       "dht_target_prefix_queries_total": 4.0}
        self.first_failure = 7
        self.soft_rate = 0.5
        self.cap = None  # the slots gauge; None: the preset's (or 32 with the switch off)
        self.anon = lambda e: 200 << 20
        self.drop_series = set()
        self.early_share, self.render_early = 0.1, True  # lookups ended on enough values
        self.edits.append((lambda e, i: i == 0, lambda row, lines: row["values"].update(torrents=0)))
        self.edits.append((lambda e, i: True, lambda row, lines: row.update(private_bytes=self.anon(row["elapsed_seconds"]))))

    def snapshot(self, e, row):
        lines = super().snapshot(e, row)
        on = self.switches
        out = []
        add = lambda name, value, labels="": out.append(f"lodestar_{name}{labels} {value}")
        ended = math.floor(e * 2)
        values = math.floor(ended * 0.3) if on["lookup_end"] else 0
        early = math.floor(ended * self.early_share) if on["lookup_end"] else 0  # CR-LOOKUP-END's own end
        ends = (("values", values), ("no_values", ended - values - early))
        if self.render_early:
            ends += (("enough_values", early),)
        for reason, n in ends:
            add(L["ended"], n, f'{{reason="{reason}"}}')
            add(L["residence"], f"{n * 15.0:.3f}", f'{{reason="{reason}"}}')
        add(L["queries"], math.floor(ended * 40), f'{{purpose="{collect.LEDGER_PEER_PURPOSE}"}}')
        add("dht_queries_out_total", math.floor(e * self.qps))
        slots = analyze_cr.PRESET_SLOTS[self.preset] if on["preset_slots"] else 32
        add(L["cap"], slots if self.cap is None else self.cap)
        add(L["occupied"], min(slots, 50))
        add("dht_outstanding", self.outstanding(e))
        for name, per_s in self.bundle.items():
            key = {"responder_hints": "dht_hints_captured_total", "streaming_peers": "fetch_stream_batches_total",
                   "target_prefixes": "dht_target_prefix_queries_total"}
            switch = next(s for s, n in key.items() if n == name)
            value = math.floor(e * per_s) if on[switch] else 0
            add(name, value, '{kind="sample"}' if name == "dht_target_prefix_queries_total" else "")
        for reason in ("interval", "first_failure"):
            v = (self.first_failure if on["sample_backoff_escalate"] else 0) if reason == "first_failure" else 100
            add("dht_sample_cooling_reason", v, f'{{reason="{reason}"}}')
        add(collect.SOFT["soft"], math.floor(e * self.soft_rate) if on["soft_timeout"] else 0)
        add(collect.SOFT["late"], 0)
        out = [x for x in out if x.split("{")[0].split(" ")[0][len("lodestar_"):] not in self.drop_series]
        lines = [x for x in lines if x.split("{")[0].split(" ")[0][len("lodestar_"):] not in self.drop_series]
        return lines + out

    def write(self, root, extra_seconds=120):
        self.config_extra = {"effective_switches": self.effective}
        root = super().write(root, extra_seconds)
        run = json.loads((root / "run.json").read_text())
        run.update(run_id=f"r1-a1-d1-{self.arm}", host_identity=mk.HOST, network_identity=mk.NETWORK,
                   round1={"round": 1, "attempt": "r1-a1", "day": 1, "arm": self.arm})
        run["image_digest"] = getattr(self, "digest", mk.DIGEST)
        (root / "run.json").write_text(json.dumps(run))
        return root


def declaration():
    """The L3 fixture's declaration, checked."""
    return analyze_cr.check_declaration(mk.archive("L3")["declaration"])


class SegmentBase(cr.Base):
    def judge(self, sim, decl=None):
        self.n += 1
        return analyze_cr.analyze_segment(sim.write(self.tmp / f"seg{self.n}"), None, decl or declaration())

    def check(self, report, name):
        return [c for c in report["checks"] if c["check"] == name]


class DeclarationTests(SegmentBase):
    def test_an_all_on_balanced_arm_matching_its_declaration_is_valid(self):
        report = self.judge(Round1Sim())
        self.assertValid(report)
        self.assertEqual(report["round1"]["declared"]["arm"], "B")
        self.assertTrue(report["round1"]["fresh_store"])
        self.assertEqual(report["binding"]["run_id"], "r1-a1-d1-B")
        self.assertFalse(self.check(report, "balanced_preset"))  # replaced in round 1
        self.assertTrue(self.check(report, "declared_preset")[0]["ok"])

    def test_an_aggressive_arm_is_judged_against_its_declaration_not_balanced(self):
        report = self.judge(Round1Sim(arm="A", preset="aggressive"))
        self.assertValid(report)
        self.assertEqual(report["bundle"]["caps"]["outstanding_cap"], 4096)
        self.assertEqual(report["bundle"]["preset_slots"]["capacity"], 256)

    def test_the_all_off_control_is_valid(self):
        report = self.judge(Round1Sim(arm="C", switches=mk.ALL_OFF))
        self.assertValid(report)

    def test_a_preset_other_than_declared_voids(self):
        sim = Round1Sim(arm="A", preset="balanced")  # A is declared Aggressive
        report = self.judge(sim)
        self.assertEqual(report["verdict"], "void")
        self.assertIn("declared_preset", report["verdict_reasons"])

    def test_preset_changed_mid_run_voids(self):
        sim = Round1Sim()
        sim.edits.append((lambda e, i: e > 2400, lambda row, lines: row.get("api") and
                          row["api"]["settings"].update(intensity="aggressive")))
        report = self.judge(sim)
        self.assertEqual(report["verdict"], "void")
        self.assertIn("preset_stayed_declared", report["verdict_reasons"])

    def test_a_preset_change_annotation_voids(self):
        sim = Round1Sim()
        sim.imports = dict(sim.imports, intensity_changes=1)
        self.assertIn("no_preset_change", self.judge(sim)["verdict_reasons"])

    def test_an_effective_switch_other_than_declared_voids(self):
        sim = Round1Sim()
        sim.effective = dict(sim.effective, utp=False)
        report = self.judge(sim)
        self.assertEqual(report["verdict"], "void")
        self.assertEqual(self.check(report, "declared_switches")[0]["detail"], {"differ": ["utp"]})

    def test_a_switch_seen_changed_at_a_boundary_voids(self):
        sim = Round1Sim()
        sim.edits.append((lambda e, i: e > 2400, lambda row, lines: row.get("api") and
                          row["api"].update(switches=dict(mk.ALL_ON, utp=False))))
        self.assertIn("switches_stayed_declared", self.judge(sim)["verdict_reasons"])

    def test_effective_switches_missing_is_missing_evidence(self):
        sim = Round1Sim()
        sim.effective = None
        report = self.judge(sim)
        self.assertEqual(report["verdict"], "invalid")
        self.assertIn("effective_switches_recorded", report["verdict_reasons"])

    def test_another_image_digest_voids(self):
        sim = Round1Sim()
        sim.digest = "sha256:" + "9" * 64  # the arm ran another image than the round's
        self.assertIn("declared_image_digest", self.judge(sim)["verdict_reasons"])

    def test_safe_restart_off_voids(self):
        sim = Round1Sim()
        sim.effective = dict(sim.effective, safe_restart=False)
        self.assertIn("safe_restart_on", self.judge(sim)["verdict_reasons"])

    def test_an_arm_the_declaration_doesnt_name_is_invalid(self):
        sim = Round1Sim(arm="D")
        report = self.judge(sim)
        self.assertEqual(report["verdict"], "invalid")
        self.assertIn("declaration_entry_found", report["verdict_reasons"])

    def test_a_store_not_empty_at_start_is_invalid(self):
        sim = Round1Sim()
        sim.edits.append((lambda e, i: i == 0, lambda row, lines: row["values"].update(torrents=12)))
        self.assertIn("fresh_store_at_start", self.judge(sim)["verdict_reasons"])

    def test_declaration_is_only_for_a_comb_arm(self):
        sim = cr.Sim()  # an A/B segment
        with self.assertRaises(ValueError):
            analyze_cr.analyze_segment(sim.write(self.tmp / "ab"), None, declaration())

    def test_without_a_declaration_a_comb_segment_keeps_balanced_preset(self):
        sim = Round1Sim(arm="A", preset="aggressive")
        report = analyze_cr.analyze_segment(sim.write(self.tmp / "hist"))
        self.assertIn("balanced_preset", report["verdict_reasons"])  # §8 item 8: historical behaviour


class DeclarationSchemaTests(unittest.TestCase):
    def decl(self):
        return mk.archive("L3")["declaration"]

    def refused(self, decl):
        with self.assertRaises(ValueError):
            analyze_cr.check_declaration(decl)

    def test_the_fixture_declaration_passes(self):
        analyze_cr.check_declaration(self.decl())

    def test_eleven_switches_is_refused(self):
        d = self.decl()
        del d["arms"][0]["switches"]["utp"]
        self.refused(d)

    def test_safe_restart_is_not_one_of_the_twelve(self):
        d = self.decl()
        d["arms"][0]["switches"]["safe_restart"] = True
        self.refused(d)

    def test_an_unknown_layout_or_role_or_preset_is_refused(self):
        for path, value in ((("attempts", 0, "layout"), "L4"), (("arms", 0, "role"), "maybe"),
                            (("arms", 0, "preset"), "middle"), (("arms", 0, "exit"), "W")):
            d = self.decl()
            target = d
            for k in path[:-1]:
                target = target[k]
            target[path[-1]] = value
            self.refused(d)

    def test_a_raw_address_for_an_identity_is_refused(self):
        d = self.decl()
        d["host_identity"] = "192.0.2.7"
        self.refused(d)

    def test_the_selection_order_is_the_owners(self):
        d = self.decl()
        d["selection_order"] = ["A", "B"]
        self.refused(d)

    def test_a_backing_device_is_required(self):
        d = self.decl()
        del d["backing_device"]
        self.refused(d)

    def test_every_candidate_digest_is_recorded(self):
        d = self.decl()
        del d["attempts"][0]["candidate_digests"]["A"]
        self.refused(d)

    def two_attempts(self, second):
        d = self.decl()
        later = mk.archive(second, attempt="r1-a2")["declaration"]
        d["attempts"] += later["attempts"]
        d["arms"] += later["arms"]
        return d

    def test_a_layout_change_must_drop_an_arm(self):
        self.refused(self.two_attempts("L3"))  # L3 again: a loop
        analyze_cr.check_declaration(self.two_attempts("L2"))
        analyze_cr.check_declaration(self.two_attempts("L2b"))
        analyze_cr.check_declaration(self.two_attempts("S"))  # a solo confirmation of B

    def test_a_solo_confirmation_after_another_solo_is_allowed_but_not_growth(self):
        d = self.two_attempts("S")
        grow = mk.archive("L2", attempt="r1-a3")["declaration"]
        d["attempts"] += grow["attempts"]
        d["arms"] += grow["arms"]
        analyze_cr.check_declaration(d)  # L3 → S(B) → L2 still shrinks the chain L3 → L2
        again = mk.archive("L2", attempt="r1-a4")["declaration"]
        d["attempts"] += again["attempts"]
        d["arms"] += again["arms"]
        self.refused(d)  # L2 → L2 doesn't drop an arm

    def test_section_6_1s_arm_table_is_enforced(self):
        for arm, change in (("B", {"switches": dict(mk.ALL_OFF)}), ("B", {"preset": "aggressive"}),
                            ("A", {"preset": "balanced"}), ("C", {"role": "candidate"}),
                            ("C", {"switches": dict(mk.ALL_ON)}), ("B", {"role": "diagnostic"}),
                            ("B", {"switches": dict(mk.ALL_ON, route_backoff=True)})):
            d = self.decl()
            for e in d["arms"]:
                if e["arm"] == arm:
                    e.update(change)
            self.refused(d)
        d = self.decl()
        for e in d["arms"]:
            if e["arm"] == "A":
                e["role"] = "diagnostic"
        analyze_cr.check_declaration(d)  # A may be either, as declared

    def test_an_arm_outside_the_table_is_refused(self):
        d = self.decl()
        d["arms"][0].update(arm="D", role="diagnostic", preset="balanced", switches=dict(mk.ALL_OFF))
        self.refused(d)

    def test_other_contention_thresholds_are_refused(self):
        d = self.decl()
        d["contention"] = dict(analyze_cr.CONTENTION, cpu_p95_max=0.9)
        self.refused(d)

    def test_an_arm_with_two_roles_is_refused(self):
        d = self.decl()
        d["arms"][0]["role"] = "diagnostic"  # B on day 1 only
        self.refused(d)


class BundleEvidenceTests(SegmentBase):
    def test_every_row_reads_its_series(self):
        report = self.judge(Round1Sim())
        bundle = report["bundle"]
        for switch in analyze_cr.BUNDLE_COUNTERS:
            self.assertGreater(bundle[switch]["value"], 0, switch)
        self.assertTrue(report["bundle"]["lookup_end"]["ended_on_values"] > 0)
        self.assertEqual(report["mechanism"]["utp"]["on"], True)

    def test_an_on_switch_whose_counter_is_absent_voids(self):
        for switch, (_, name, _) in analyze_cr.BUNDLE_COUNTERS.items():
            sim = Round1Sim()
            sim.drop_series = {name}
            report = self.judge(sim)
            self.assertEqual(report["verdict"], "void", switch)
            self.assertIn(f"{switch}_evidence", report["verdict_reasons"])

    def test_a_gauge_row_absent_at_one_scrape_voids(self):
        sim = Round1Sim()
        drop = lambda e, i: 2000 < e < 2070
        sim.edits.append((drop, lambda row, lines: lines.__setitem__(slice(None), [
            x for x in lines if not x.startswith('lodestar_dht_sample_cooling_reason{reason="first_failure"}')])))
        report = self.judge(sim)
        self.assertIn("sample_backoff_escalate_evidence", report["verdict_reasons"])

    def test_an_on_switch_with_a_measured_zero_voids(self):
        sim = Round1Sim()
        sim.bundle["dht_hints_captured_total"] = 0.0
        report = self.judge(sim)
        self.assertEqual(report["verdict_reasons"], ["responder_hints_evidence"])
        sim = Round1Sim()
        sim.first_failure = 0
        self.assertEqual(self.judge(sim)["verdict_reasons"], ["sample_backoff_escalate_evidence"])

    def test_lookup_end_runs_the_lookup_cost_pair_whatever_the_run_kind(self):
        sim = Round1Sim()
        sim.drop_series = {L["ended"]}
        report = self.judge(sim)
        self.assertIn("lookups_end_on_values", report["verdict_reasons"])

    def test_preset_slots_capacity_must_equal_the_declared_count(self):
        sim = Round1Sim()
        sim.cap = 64
        report = self.judge(sim)
        self.assertIn("preset_slots_capacity", report["verdict_reasons"])

    def test_hint_first_checks_run_in_a_comb_arm(self):
        sim = Round1Sim()
        sim.marks = 0
        self.assertIn("hint_first_marks_hashes", self.judge(sim)["verdict_reasons"])

    def test_sampler_split_series_absent_voids_and_a_table_never_full_is_reported(self):
        sim = Round1Sim()
        sim.render_split = False
        self.assertIn("sampler_split_series_present", self.judge(sim)["verdict_reasons"])
        sim = Round1Sim()
        sim.table = lambda e: 10
        sim.spill = lambda e: 0
        report = self.judge(sim)
        self.assertValid(report)
        self.assertEqual(report["sampler_split"]["spill_check"], "not_exercised")

    def test_soft_timeout_on_with_a_count_of_zero_is_a_valid_result(self):
        sim = Round1Sim()
        sim.soft_rate = 0
        report = self.judge(sim)
        self.assertValid(report)
        soft = report["soft_timeout"]
        self.assertNotIn("soft", soft["absent"])
        self.assertEqual(soft["stalled"]["soft"], 0)  # the fixture's sampler is full throughout

    def test_soft_timeout_counter_absent_voids(self):
        sim = Round1Sim()
        sim.drop_series = {collect.SOFT["soft"]}
        self.assertIn("soft_timeout_counter_rendered", self.judge(sim)["verdict_reasons"])

    def test_an_interval_over_the_preset_query_rate_voids(self):
        sim = Round1Sim()
        sim.qps = 260.0  # over Balanced's 200 × 1.05, with its slack
        report = self.judge(sim)
        self.assertEqual(report["verdict"], "void")
        self.assertIn("query_rate_within_declared_preset", report["verdict_reasons"])

    def test_the_query_rate_is_checked_in_the_control_too(self):
        sim = Round1Sim(arm="C", switches=mk.ALL_OFF)
        sim.qps = 260.0
        self.assertIn("query_rate_within_declared_preset", self.judge(sim)["verdict_reasons"])

    def test_a_scrape_over_the_outstanding_cap_voids(self):
        sim = Round1Sim()
        sim.outstanding = lambda e: 1025 if 2000 < e < 2100 else 500
        report = self.judge(sim)
        self.assertEqual(report["verdict_reasons"], ["outstanding_within_declared_preset"])
        sim = Round1Sim(arm="A", preset="aggressive")
        sim.outstanding = lambda e: 1025
        self.assertValid(self.judge(sim))  # within Aggressive's 4096

    def test_route_backoff_off_keeps_rule_5_3_idle_check(self):
        report = self.judge(Round1Sim())
        names = {c["check"] for c in report["checks"]}
        self.assertIn("route_backoff_idle_when_off", names)
        self.assertNotIn("no_opportunity", names)


class MemoryWindowTests(SegmentBase):
    def test_the_fit_is_over_ws_plus_half_to_the_end(self):
        sim = Round1Sim()
        # Rising 10 MiB per real hour, but only after ws + 2 h: the fit sees only that.
        sim.anon = lambda e: (200 << 20) + int(max(0.0, e - (605 + 2400)) / 3600 * (10 << 20))
        report = self.judge(sim)
        fit = report["memory"]
        self.assertEqual(fit["clock"], "ws")
        self.assertAlmostEqual(fit["from_hour"], 2 * 1200 / 3600)
        self.assertAlmostEqual(fit["slope_mib_per_hour"], 10, delta=0.5)
        self.assertIs(fit["meets_bar"], False)

    def test_flat_memory_meets_the_bar(self):
        self.assertIs(self.judge(Round1Sim())["memory"]["meets_bar"], True)

    def test_missing_anon_is_not_judged(self):
        sim = Round1Sim()
        sim.anon = lambda e: None if e > 3500 else 200 << 20
        fit = self.judge(sim)["memory"]
        self.assertIsNone(fit["meets_bar"])
        self.assertEqual(fit["refused"], "anon_missing")

    def test_an_incomplete_fit_is_not_judged(self):
        fit = analyze_cr0.measured_window_fit([(t, 1 << 20) for t in range(43200, 60000, 60)], 0.0)
        self.assertFalse(fit["window_complete"])
        self.assertIsNone(fit["meets_bar"])

    def test_a_complete_flat_day_meets_the_bar(self):
        fit = analyze_cr0.measured_window_fit([(t, 300 << 20) for t in range(0, 86401, 60)], 0.0)
        self.assertTrue(fit["window_complete"])
        self.assertIs(fit["meets_bar"], True)
        self.assertEqual((fit["from_hour"], fit["to_hour"]), (12.0, 24.0))

    def test_the_historical_fit_is_unchanged(self):
        rows = [{"phase": "baseline", "elapsed_seconds": float(t), "process_id": 1, "process_identity": "p",
                 "private_bytes": (100 << 20) + t * 10, "metrics_ok": False} for t in range(0, 86401, 60)]
        fit = analyze_cr0.memory_fit(rows)
        self.assertEqual(fit["series"], "private_bytes")
        self.assertTrue(fit["window_complete"])


def edge_scrapes(le600, total_count, dropped):
    """Two scrapes whose histogram deltas hold `le600` samples within 600 s of `total_count`, and `dropped`."""
    def snap(scale):
        s = {"discovery_to_searchable_seconds_count": {(): total_count * scale},
             "discovery_to_searchable_dropped_total": {(): dropped * scale},
             "discovery_to_searchable_seconds_bucket": {}}
        for e in collect.LATENCY_EDGES:
            v = total_count if e == "+Inf" or (e != "+Inf" and float(e) > 600) else (le600 if float(e) >= 600 else 0)
            s["discovery_to_searchable_seconds_bucket"][(("le", e),)] = v * scale
        return s
    return {"snap": snap(0)}, {"snap": snap(1)}


class LatencyEdgeTests(unittest.TestCase):
    """§6.4 and CR-SPEC §3: the latency cohort keeps carry-in samples and charges dropped
    samples above 600 s. A hand calculation at the 95 % edge."""

    def test_exactly_95_percent_within_600s_passes(self):
        # 10,000 samples + 100 dropped = 10,100. 95 % of that is 9,595: 9,595 within 600 s passes.
        a, b = edge_scrapes(9595, 10000, 100)
        lat = analyze_cr.latency(a, b)
        self.assertAlmostEqual(lat["coverage"], 10000 / 10100)
        self.assertTrue(lat["passes_p95_600"])

    def test_one_sample_below_the_edge_fails(self):
        a, b = edge_scrapes(9594, 10000, 100)  # 9,594 < 9,595: the dropped are charged above 600 s
        self.assertFalse(analyze_cr.latency(a, b)["passes_p95_600"])

    def test_carry_in_samples_are_not_filtered(self):
        # Carry-in samples are in the histogram's count: removing them would raise the share.
        # The cohort is the whole histogram delta, so the result depends on it alone.
        a, b = edge_scrapes(9595, 10000, 100)
        self.assertEqual(analyze_cr.latency(a, b)["count_delta"], 10000)


# ----------------------------------------------------------------- gate-round
class RoundBase(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.n = 0

    def gate(self, a):
        self.n += 1
        return analyze_cr.gate_round(analyze_cr.load_manifest(mk.write(self.root / f"r{self.n}", a)))

    def cand(self, v, arm="B"):
        return v["candidates"].get(arm) or v["diagnostics"][arm]

    def day(self, a, day, attempt="r1-a1"):
        return next(d for d in a["days"] if d["day"] == day and d["attempt"] == attempt)

    def assertNotJudged(self, v, reason, arm="B"):
        c = self.cand(v, arm)
        self.assertEqual(c["crawler"], "not_judged")
        found = set(c["reasons"]) | {r for d in c["days"] for r in d["reasons"]}
        self.assertIn(reason, found)


class LayoutTests(RoundBase):
    def test_three_arm_layout_both_days_valid_and_passing(self):
        v = self.gate(mk.archive("L3"))
        self.assertEqual(self.cand(v)["crawler"], "pass")
        self.assertTrue(self.cand(v)["shipping_eligible"])
        self.assertEqual(v["selection"]["ships"], "B")

    def test_two_arm_layout_a_and_b_swapped_across_two_exits(self):
        v = self.gate(mk.archive("L2"))
        self.assertEqual(self.cand(v)["crawler"], "pass")
        self.assertEqual(self.cand(v, "A")["crawler"], "pass")

    def test_l2b_without_a(self):
        v = self.gate(mk.archive("L2b"))
        self.assertEqual(self.cand(v)["crawler"], "pass")
        self.assertNotIn("A", v["candidates"])

    def test_solo_confirmation_one_arm_two_days_on_two_exits(self):
        a = mk.archive("L3")
        lose_b_probes(self.day(a, 1))  # the breach that calls for it (SoloBoundTests)
        v = self.gate(add_attempt(a, "S", "r1-a2"))
        self.assertEqual(self.cand(v)["layout"], "S")
        self.assertEqual(self.cand(v)["crawler"], "pass", self.cand(v)["reasons"])

    def test_solo_on_one_exit_twice_is_not_judged(self):
        a = mk.archive("S")
        a["declaration"]["arms"][1]["exit"] = "X"
        self.assertNotJudged(self.gate(a), "layout_S_is_one_arm_on_two_exits")

    def test_exits_other_than_the_layout_are_not_judged(self):
        a = mk.archive("L3")
        for e in a["declaration"]["arms"]:
            if e["day"] == 2 and e["arm"] in ("B", "C"):
                e["exit"] = {"B": "X", "C": "Y"}[e["arm"]]
        self.assertNotJudged(self.gate(a), "layout_L3_not_as_declared")

    def test_two_days_of_one_candidate_on_one_exit_is_not_judged(self):
        a = mk.archive("L3")
        self.day(a, 2)["archive"]["exits"]["Y"] = mk.EXITS["X"]  # day 2's Y is day 1's X
        for r in self.day(a, 2)["archive"]["arms"]["B"]["egress"]:
            r["hash"] = mk.EXITS["X"]
        self.assertNotJudged(self.gate(a), "repeated_exit")

    def test_two_arms_on_one_exit_is_not_judged(self):
        a = mk.archive("L3")
        d = self.day(a, 1)["archive"]
        d["exits"]["Y"] = d["exits"]["X"]
        for r in d["arms"]["A"]["egress"]:
            r["hash"] = d["exits"]["X"]
        self.assertNotJudged(self.gate(a), "exits_share_an_egress_hash")

    def test_an_egress_reading_off_its_exit_is_not_judged(self):
        a = mk.archive("L3")
        self.day(a, 1)["archive"]["arms"]["C"]["egress"][5]["hash"] = mk.h("elsewhere")
        self.assertNotJudged(self.gate(a), "egress_differs_from_declared_exit")

    def test_a_boundary_without_an_egress_reading_is_not_judged(self):
        a = mk.archive("L3")
        del self.day(a, 2)["archive"]["arms"]["B"]["egress"][10]
        self.assertNotJudged(self.gate(a), "egress_unchecked_at_a_boundary")

    def test_a_declared_concurrent_day_with_one_peer_report_missing(self):
        a = mk.archive("L3")
        del self.day(a, 1)["reports"]["C"]
        self.assertNotJudged(self.gate(a), "peer_report_missing")

    def test_an_arm_beyond_the_declaration_is_not_judged(self):
        a = mk.archive("L2")
        extra = json.loads(json.dumps(self.day(a, 1)["reports"]["A"]))
        self.day(a, 1)["reports"]["C"] = extra
        self.assertNotJudged(self.gate(a), "roster_differs_from_declaration")

    def test_staggered_starts_and_finishes_every_peer_active_through_the_overhang(self):
        a = mk.archive("L3")
        starts = {arm: r["timeline"]["window_start_utc"] for arm, r in self.day(a, 1)["reports"].items()}
        self.assertEqual(len(set(starts.values())), 3)  # the fixture's arms start minutes apart
        v = self.gate(a)
        self.assertEqual(self.cand(v)["crawler"], "pass")
        self.assertEqual(self.cand(v, "A")["crawler"], "pass")

    def test_a_peer_that_exits_during_the_overhang_before_a_later_candidates_final_boundary(self):
        a = mk.archive("L3")
        d = self.day(a, 1)
        b_end = analyze_cr.parse_utc(d["reports"]["B"]["timeline"]["window_end_utc"])
        # B stops 1 min after its own final boundary; A's window ends 3 min after B's.
        d["archive"]["arms"]["B"]["active_through_utc"] = mk.iso(b_end + timedelta(minutes=1))
        v = self.gate(a)
        self.assertNotJudged(v, "peer_not_active_through_candidate_window", "A")
        self.assertEqual(self.cand(v)["crawler"], "pass")  # B's own window was covered by every peer

    def test_a_failed_leak_test_is_not_judged(self):
        a = mk.archive("L3")
        self.day(a, 1)["archive"]["arms"]["C"]["leak_test"]["after"] = "fail"
        self.assertNotJudged(self.gate(a), "leak_test_not_passed")

    def test_missing_day_archive_is_not_judged(self):
        a = mk.archive("L3")
        self.day(a, 2)["archive"] = None
        self.assertNotJudged(self.gate(a), "day_archive_missing")


class ValidityTests(RoundBase):
    def test_a_failed_second_day_fails(self):
        a = mk.archive("L3")
        self.day(a, 2)["reports"]["B"]["numerator"]["hourly"][7]["net_per_min"] = 65.0
        v = self.gate(a)
        self.assertEqual(self.cand(v)["crawler"], "fail")
        self.assertEqual(v["selection"]["trail"][0]["disposition"], "valid_miss")

    def test_the_same_days_report_supplied_twice(self):
        a = mk.archive("S")
        day2 = self.day(a, 2)
        day2["reports"]["B"] = json.loads(json.dumps(self.day(a, 1)["reports"]["B"]))
        self.assertNotJudged(self.gate(a), "duplicate_run")

    def test_every_rate_passes_but_the_segment_is_void(self):
        a = mk.archive("L3")
        r = self.day(a, 1)["reports"]["B"]
        r["verdict"], r["verdict_reasons"] = "void", ["declared_preset"]
        self.assertNotJudged(self.gate(a), "segment_void")

    def test_an_invalid_report_is_not_judged(self):
        a = mk.archive("L3")
        self.day(a, 1)["reports"]["B"]["verdict"] = "invalid"
        self.assertNotJudged(self.gate(a), "segment_invalid")

    def test_a_rehearsal_report_is_not_judged(self):
        a = mk.archive("L3")
        self.day(a, 1)["reports"]["B"]["rehearsal"] = True
        self.assertNotJudged(self.gate(a), "not_a_24h_comb_window")

    def test_a_report_bound_to_another_day_is_not_judged(self):
        a = mk.archive("L3")
        self.day(a, 1)["reports"]["B"]["binding"]["placement"]["day"] = 2
        self.assertNotJudged(self.gate(a), "report_not_bound_to_its_declaration")

    def test_another_spec_commit_or_host_is_not_judged(self):
        a = mk.archive("L3")
        self.day(a, 1)["reports"]["B"]["identity"]["spec_commit"] = "f" * 40
        self.assertNotJudged(self.gate(a), "spec_commit_differs")
        a = mk.archive("L3")
        self.day(a, 2)["reports"]["B"]["binding"]["host_identity"] = mk.h("other-host")
        self.assertNotJudged(self.gate(a), "host_identity_differs")

    def test_no_fresh_store_evidence_is_not_judged(self):
        a = mk.archive("L3")
        self.day(a, 1)["reports"]["B"]["round1"]["fresh_store"] = False
        self.assertNotJudged(self.gate(a), "no_fresh_store_evidence")

    def test_overlapping_windows_are_not_judged(self):
        a = mk.archive("S")
        r2 = self.day(a, 2)["reports"]["B"]
        r2["timeline"]["window_start_utc"] = self.day(a, 1)["reports"]["B"]["timeline"]["window_start_utc"]
        self.assertNotJudged(self.gate(a), "windows_overlap_or_out_of_order")

    def test_a_missing_day_is_not_judged(self):
        a = mk.archive("L3")
        a["days"] = [d for d in a["days"] if d["day"] == 1]
        self.assertNotJudged(self.gate(a), "day_missing")

    def test_a_diagnostic_a_whose_measurements_all_pass_is_ineligible(self):
        a = mk.archive("L3")
        for e in a["declaration"]["arms"]:
            if e["arm"] == "A":
                e["role"] = "diagnostic"
        for d in a["days"]:
            d["reports"]["A"]["round1"]["declared"]["role"] = "diagnostic"
        v = self.gate(a)
        self.assertNotIn("A", v["candidates"])
        self.assertNotJudged(v, "not_authorized", "A")
        self.assertFalse(self.cand(v, "A")["shipping_eligible"])
        self.assertTrue(all(d["status"] == "pass" for d in self.cand(v, "A")["days"]))

    def test_a_product_failure_on_a_peer_breaks_the_layout(self):
        a = mk.archive("L3")
        self.day(a, 1)["archive"]["events"] = [{"class": "product", "arms": ["C"]}]
        v = self.gate(a)
        self.assertNotJudged(v, "peer_product_failure")
        self.assertIsNone(v["selection"]["ships"])
        self.assertEqual(v["selection"]["state"], "pending_retry")

    def test_a_product_failure_under_diagnosis_is_not_terminal(self):
        a = mk.archive("L3")
        self.day(a, 1)["archive"]["events"] = [{"class": "product", "arms": ["B"], "diagnosis": "pending"}]
        v = self.gate(a)
        self.assertNotJudged(v, "product_failure_under_diagnosis")
        self.assertEqual(v["selection"]["state"], "pending_retry")
        self.assertEqual(v["selection"]["waiting_on"], "B")  # A isn't read until B's diagnosis

    def test_a_diagnosis_that_finds_infrastructure_reclassifies_it(self):
        a = mk.archive("L3")
        self.day(a, 1)["archive"]["events"] = [{"class": "product", "arms": ["B"], "diagnosis": "infrastructure"}]
        self.assertNotJudged(self.gate(a), "infrastructure_fault")

    def test_an_unknown_event_class_is_not_ignored(self):
        a = mk.archive("L3")
        self.day(a, 1)["archive"]["events"] = [{"class": "weather", "arms": ["C"]}]
        self.assertNotJudged(self.gate(a), "unknown_event")

    def test_an_infrastructure_fault_on_a_peer_takes_the_whole_day(self):
        a = mk.archive("L3")
        self.day(a, 1)["archive"]["events"] = [{"class": "infrastructure", "arms": ["C"]}]
        self.assertNotJudged(self.gate(a), "peer_infrastructure_fault")

    def test_a_peer_report_from_another_arm_is_not_bound(self):
        a = mk.archive("L3")
        d = self.day(a, 1)
        fake = json.loads(json.dumps(d["reports"]["C"]))
        fake["binding"]["placement"]["arm"] = "A"
        d["reports"]["C"] = fake
        self.assertNotJudged(self.gate(a), "peer_report_not_bound")

    def test_a_valid_miss_is_not_undone_by_a_repeat(self):
        a = mk.archive("L3")
        miss = self.day(a, 1)
        miss["reports"]["B"]["numerator"]["hourly"][7]["net_per_min"] = 65.0
        again = json.loads(json.dumps(miss))
        again["repeat"] = 1
        again["reports"]["B"]["numerator"]["hourly"][7]["net_per_min"] = 120.0
        for r in again["reports"].values():
            r["binding"]["placement"]["repeat"] = 1
            r["binding"]["run_id"] += "-repeat1"
        a["days"].append(again)
        v = self.gate(a)
        self.assertEqual(self.cand(v)["crawler"], "fail")
        self.assertEqual([d["status"] for d in self.cand(v)["replaced_days"]], ["fail"])
        self.assertEqual(v["selection"]["trail"][0]["disposition"], "valid_miss")

    def repeat_day1(self, a, setup):
        setup(self.day(a, 1))
        again = json.loads(json.dumps(self.day(a, 1)))
        again["repeat"] = 1
        again["archive"]["events"] = []
        again["contention"] = mk.host_samples(again["reports"])
        for r in again["reports"].values():
            r["verdict"] = "valid"
            r["binding"]["placement"]["repeat"] = 1
            r["binding"]["run_id"] += "-repeat1"
        a["days"].append(again)
        return self.gate(a)

    def test_a_contention_breach_is_not_repeated(self):
        def breach(d):
            for s in d["contention"]["samples"]:
                s["cpu"] = 0.9
        v = self.repeat_day1(mk.archive("L3"), breach)
        self.assertNotJudged(v, "repeat_of_a_non_repeatable_day")
        self.assertTrue(self.cand(v)["solo_confirmation_required"])

    def test_a_product_failure_under_diagnosis_is_not_repeated(self):
        v = self.repeat_day1(mk.archive("L3"), lambda d: d["archive"].update(
            events=[{"class": "product", "arms": ["B"], "diagnosis": "pending"}]))
        self.assertNotJudged(v, "repeat_of_a_non_repeatable_day")
        self.assertIsNone(v["selection"]["ships"])

    def test_a_void_day_is_repeated(self):
        # The runner's ruling on CR-SPEC-R1's review: §6.3's retry rule repeats a void day.
        v = self.repeat_day1(mk.archive("L3"), lambda d: d["reports"]["B"].update(verdict="void"))
        self.assertEqual(self.cand(v)["crawler"], "pass", self.cand(v)["reasons"])
        self.assertEqual([d["reasons"] for d in self.cand(v)["replaced_days"]], [["segment_void"]])

    def test_a_repeat_of_a_passing_day_is_not_judged(self):
        a = mk.archive("L3")
        again = json.loads(json.dumps(self.day(a, 1)))
        again["repeat"] = 1
        for r in again["reports"].values():
            r["binding"]["placement"]["repeat"] = 1
            r["binding"]["run_id"] += "-repeat1"
        a["days"].append(again)
        self.assertNotJudged(self.gate(a), "repeat_of_a_judged_day")

    def test_a_valid_miss_in_an_earlier_attempt_still_ends_the_claim(self):
        a = mk.archive("L3")
        self.day(a, 2)["reports"]["B"]["numerator"]["hourly"][3]["net_per_min"] = 60.0
        later = mk.archive("L2", attempt="r1-a2")  # C dropped: a fresh attempt
        a["declaration"]["attempts"] += later["declaration"]["attempts"]
        a["declaration"]["arms"] += later["declaration"]["arms"]
        a["days"] += later["days"]
        v = self.gate(a)
        self.assertEqual(self.cand(v)["crawler"], "fail")
        self.assertEqual(self.cand(v)["valid_misses"], [{"attempt": "r1-a1", "day": 2, "repeat": 0}])

    def test_a_valid_miss_ends_the_claim_while_day_two_is_still_missing(self):
        a = mk.archive("L3")
        self.day(a, 1)["reports"]["B"]["numerator"]["window"]["mean_net_per_min"] = 90.0
        a["days"] = [d for d in a["days"] if d["day"] == 1]
        v = self.gate(a)
        self.assertEqual(self.cand(v)["crawler"], "fail")
        self.assertEqual(v["selection"]["trail"][0]["disposition"], "valid_miss")

    def test_repeats_in_a_solo_attempt_count_against_the_budget(self):
        a = mk.archive("S")
        for k in (1, 2, 3):
            again = json.loads(json.dumps(self.day(a, 1)))
            again["repeat"] = k
            again["archive"]["events"] = [] if k == 3 else [{"class": "infrastructure", "arms": []}]
            for r in again["reports"].values():
                r["binding"]["placement"]["repeat"] = k
                r["binding"]["run_id"] += f"-repeat{k}"
            a["days"].append(again)
        self.day(a, 1)["archive"]["events"] = [{"class": "infrastructure", "arms": []}]
        self.assertNotJudged(self.gate(a), "retry_budget_exhausted")

    def test_a_candidates_product_failure_is_terminal_and_a_is_read(self):
        a = mk.archive("L3")
        self.day(a, 1)["archive"]["events"] = [{"class": "product", "arms": ["B"], "diagnosis": "product"}]
        v = self.gate(a)
        self.assertEqual(v["selection"]["trail"][0]["disposition"], "product_failure")
        # A continues in a fresh attempt (its day 1 lost its peer B): not terminal yet.
        self.assertEqual((v["selection"]["state"], v["selection"]["waiting_on"]), ("pending_retry", "A"))

    def test_an_infrastructure_fault_invalidates_the_day(self):
        a = mk.archive("L3")
        self.day(a, 2)["archive"]["events"] = [{"class": "infrastructure", "arms": []}]
        self.assertNotJudged(self.gate(a), "infrastructure_fault")

    def test_a_repeated_day_replaces_the_one_it_repeats(self):
        a = mk.archive("L3")
        bad = self.day(a, 1)
        bad["archive"]["events"] = [{"class": "infrastructure", "arms": []}]
        again = json.loads(json.dumps(bad))
        again.update(repeat=1)
        again["archive"]["events"] = []
        for r in again["reports"].values():
            r["binding"]["placement"]["repeat"] = 1
            r["binding"]["run_id"] += "-repeat1"
        a["days"].append(again)
        self.assertEqual(self.cand(self.gate(a))["crawler"], "pass")

    def test_a_third_repeated_day_stops_the_round(self):
        a = mk.archive("L3")
        self.day(a, 1)["archive"]["events"] = [{"class": "infrastructure", "arms": []}]
        for k in (1, 2, 3):
            again = json.loads(json.dumps(self.day(a, 1)))
            again.update(repeat=k)
            for r in again["reports"].values():
                r["binding"]["placement"]["repeat"] = k
                r["binding"]["run_id"] += f"-repeat{k}"
            a["days"].append(again)
        v = self.gate(a)
        self.assertNotJudged(v, "retry_budget_exhausted")
        self.assertEqual(v["selection"]["trail"][0]["disposition"], "out_of_retries")


def add_attempt(a, layout, attempt, fresh=None, later=None):
    """Append a fresh attempt (from make_archive, or `later`) to archive `a`, with distinct
    run IDs and, by default, days after the existing ones."""
    later = later or mk.archive(layout, attempt=attempt)
    a["declaration"]["attempts"] += later["declaration"]["attempts"]
    a["declaration"]["arms"] += later["declaration"]["arms"]
    shift = timedelta(days=3 * (len(a["declaration"]["attempts"]) - 1))
    for d in later["days"]:
        for r in d["reports"].values():
            for k in ("process_start_utc", "window_start_utc", "window_end_utc", "last_scrape_utc"):
                r["timeline"][k] = mk.iso(analyze_cr.parse_utc(r["timeline"][k]) + shift)
            r["binding"]["ended_utc"] = mk.iso(analyze_cr.parse_utc(r["binding"]["ended_utc"]) + shift)
        for arm in d["archive"]["arms"].values():
            for k in ("active_from_utc", "active_through_utc"):
                arm[k] = mk.iso(analyze_cr.parse_utc(arm[k]) + shift)
            for e in arm["egress"]:
                e["utc"] = mk.iso(analyze_cr.parse_utc(e["utc"]) + shift)
        for smp in d["contention"]["samples"]:
            smp["utc"] = mk.iso(analyze_cr.parse_utc(smp["utc"]) + shift)
    a["days"] += later["days"]
    return a


def lose_b_probes(day):
    """2 % of B's tunnel probes lost through its window: a network breach on B alone."""
    for k, smp in enumerate(day["contention"]["samples"]):
        if k % 50 == 0:
            smp["tunnels"]["B"] = {"rtt_ms": None, "lost": True}


class HistoryTests(RoundBase):
    """Runner review of #1147: history across attempts, and a manifest complete against the
    declaration."""

    def test_a_breach_then_a_fresh_non_solo_attempt_still_needs_solo_confirmation(self):
        a = mk.archive("L3")
        lose_b_probes(self.day(a, 1))
        add_attempt(a, "L2", "r1-a2")
        v = self.gate(a)
        self.assertNotJudged(v, "solo_confirmation_required")

    def test_a_breach_then_a_passing_solo_confirmation_passes(self):
        a = mk.archive("L3")
        lose_b_probes(self.day(a, 1))
        add_attempt(a, "S", "r1-a2")
        self.assertEqual(self.cand(self.gate(a))["crawler"], "pass")

    def test_a_product_failure_on_the_same_digest_withholds_qualification(self):
        for diagnosis in ("product", "pending"):
            a = mk.archive("L3")
            self.day(a, 1)["archive"]["events"] = [{"class": "product", "arms": ["B"], "diagnosis": diagnosis}]
            add_attempt(a, "L2", "r1-a2")
            v = self.gate(a)
            self.assertNotJudged(v, "product_failure_on_this_digest")
            if diagnosis == "pending":  # withheld: B isn't terminal, so A isn't read
                self.assertEqual(v["selection"]["state"], "pending_retry")
            else:  # diagnosed: B's claim ends, and A (clean in its own fresh attempt) is read
                self.assertEqual(v["selection"]["trail"][0]["disposition"], "product_failure")
                self.assertNotEqual(v["selection"]["ships"], "B")

    def test_another_digest_for_an_earlier_attempt_is_refused(self):
        # Re-review of 970a6556: a wrong digest declared for a1 once wiped out its valid miss
        # (or its product failure), so B shipped from a2. One image for the round (§6.3).
        for variant in ("miss", "product"):
            a = mk.archive("L3")
            if variant == "miss":
                self.day(a, 2)["reports"]["B"]["numerator"]["hourly"][3]["net_per_min"] = 60.0
            else:
                self.day(a, 1)["archive"]["events"] = [{"class": "product", "arms": ["B"], "diagnosis": "product"}]
            a["declaration"]["attempts"][0]["candidate_digests"]["B"] = "sha256:" + "0" * 64
            add_attempt(a, "L2", "r1-a2")
            with self.assertRaises(ValueError):
                self.gate(a)

    def test_an_entry_with_another_image_digest_is_refused(self):
        a = mk.archive("L3")
        a["declaration"]["arms"][0]["image_digest"] = "sha256:" + "0" * 64
        with self.assertRaises(ValueError):
            self.gate(a)

    def ended_short(self, a, days_run):
        path = mk.write(self.root / f"ended{self.n}", a)
        self.n += 1
        man = json.loads(path.read_text())
        man["attempt_ends"] = {"r1-a1": {"reason": "x", "days_run": days_run}}
        path.write_text(json.dumps(man))
        return path

    def test_an_attempt_that_never_started_is_echoed_in_the_verdict(self):
        a = mk.archive("L3")
        add_attempt(a, "L2", "r1-a2")
        a["days"] = [d for d in a["days"] if d["attempt"] != "r1-a1"]
        v = analyze_cr.gate_round(analyze_cr.load_manifest(self.ended_short(a, [])))
        self.assertEqual(v["attempt_ends"], {"r1-a1": {"reason": "x", "days_run": []}})
        self.assertEqual(self.cand(v)["attempt_ends"], v["attempt_ends"])
        self.assertEqual(self.cand(v)["crawler"], "pass")  # accepted: the echo makes it auditable

    def test_an_ended_attempts_listed_days_keep_their_history(self):
        a = mk.archive("L3")
        self.day(a, 1)["reports"]["B"]["numerator"]["hourly"][3]["net_per_min"] = 60.0
        add_attempt(a, "L2", "r1-a2")
        a["days"] = [d for d in a["days"] if not (d["attempt"] == "r1-a1" and d["day"] == 2)]
        v = analyze_cr.gate_round(analyze_cr.load_manifest(self.ended_short(a, [1])))
        self.assertEqual(self.cand(v)["crawler"], "fail")  # a1 day 1's valid miss still counts

    def test_attempt_ends_days_run_must_be_a_plain_prefix(self):
        a = mk.archive("L3")
        add_attempt(a, "L2", "r1-a2")
        for bad in ([[1]], [True], [2], [1, 1], [2, 1]):
            with self.assertRaises(ValueError):
                analyze_cr.load_manifest(self.ended_short(a, bad))
        with self.assertRaises(ValueError):  # never started, but its days are listed
            analyze_cr.load_manifest(self.ended_short(a, []))
        b = mk.archive("L3")
        add_attempt(b, "L2", "r1-a2")
        b["days"] = [d for d in b["days"] if not (d["attempt"] == "r1-a1" and d["day"] == 1)]
        with self.assertRaises(ValueError):  # it says day 1 ran, but day 1 isn't listed
            analyze_cr.load_manifest(self.ended_short(b, [1]))

    def test_a_product_failure_outranks_a_valid_miss_in_the_disposition(self):
        a = mk.archive("L3")
        self.day(a, 1)["archive"]["events"] = [{"class": "product", "arms": ["B"], "diagnosis": "product"}]
        self.day(a, 2)["reports"]["B"]["numerator"]["hourly"][3]["net_per_min"] = 60.0
        v = self.gate(a)
        self.assertEqual(self.cand(v)["crawler"], "fail")
        self.assertEqual(v["selection"]["trail"][0]["disposition"], "product_failure")

    def test_a_failed_solo_confirmation_has_no_further_retry_and_a_is_read(self):
        a = mk.archive("L3")
        lose_b_probes(self.day(a, 1))
        add_attempt(a, "S", "r1-a2")
        lose_b_probes(self.day(a, 2, "r1-a2"))  # the solo confirmation breaches too
        add_attempt(a, "S", "r1-a3")
        v = self.gate(a)
        self.assertNotJudged(v, "solo_confirmation_failed")
        self.assertEqual(v["selection"]["trail"][0]["disposition"], "out_of_retries")
        self.assertEqual(v["selection"]["ships"], "A")  # A's own windows were clean

    def test_l3_then_l3_after_an_infrastructure_day_is_refused(self):
        a = mk.archive("L3")
        self.day(a, 1)["archive"]["events"] = [{"class": "infrastructure", "arms": []}]
        with self.assertRaises(ValueError):
            self.gate(add_attempt(a, "L3", "r1-a2"))

    def test_omitting_an_earlier_attempts_days_is_not_judged(self):
        a = mk.archive("L3")
        self.day(a, 2)["reports"]["B"]["numerator"]["hourly"][3]["net_per_min"] = 60.0  # a1: a valid miss
        add_attempt(a, "L2", "r1-a2")
        a["days"] = [d for d in a["days"] if d["attempt"] != "r1-a1"]
        self.assertNotJudged(self.gate(a), "earlier_attempt_incomplete")

    def test_an_earlier_attempt_with_a_recorded_end_needs_only_the_days_it_ran(self):
        a = mk.archive("L3")
        add_attempt(a, "L2", "r1-a2")
        a["days"] = [d for d in a["days"] if not (d["attempt"] == "r1-a1" and d["day"] == 2)]
        path = mk.write(self.root / "ended", a)
        man = json.loads(path.read_text())
        man["attempt_ends"] = {"r1-a1": {"reason": "C dropped after day 1", "days_run": [1]}}
        path.write_text(json.dumps(man))
        v = analyze_cr.gate_round(analyze_cr.load_manifest(path))
        self.assertEqual(self.cand(v)["crawler"], "pass")

    def test_a_repeat_without_its_repeat_0_is_not_judged(self):
        a = mk.archive("L3")
        d1 = self.day(a, 1)
        d1["repeat"] = 1
        for r in d1["reports"].values():
            r["binding"]["placement"]["repeat"] = 1
        self.assertNotJudged(self.gate(a), "repeats_not_contiguous")

    def test_only_repeat_3_listed_is_not_judged(self):
        a = mk.archive("L3")
        d1 = self.day(a, 1)
        d1["repeat"] = 3
        for r in d1["reports"].values():
            r["binding"]["placement"]["repeat"] = 3
        v = self.gate(a)
        self.assertNotJudged(v, "repeats_not_contiguous")
        self.assertNotJudged(v, "retry_budget_exhausted")

    def test_an_undeclared_attempt_in_the_manifest_is_an_error(self):
        a = mk.archive("L3")
        self.day(a, 2)["attempt"] = "r1-a9"
        with self.assertRaises(ValueError):
            self.gate(a)

    def test_a_run_id_reused_across_attempts_is_not_judged(self):
        a = mk.archive("L3")
        add_attempt(a, "L2", "r1-a2")
        self.day(a, 1, "r1-a2")["reports"]["B"]["binding"]["run_id"] = "r1-a1-d1-B"
        self.assertNotJudged(self.gate(a), "run_id_reused")


class DropCTests(unittest.TestCase):
    """Item H: `analyze_cr.py drop-c` on the rehearsal (plan lines 430-432)."""
    T0 = analyze_cr.parse_utc("2026-11-28T20:00:00Z")

    def host(self, minutes=120, cpu=lambda k: 0.5, steal=lambda k: 0.01, skip=(), vcpus=8):
        header = [{"kind": "header", "started_utc": mk.iso(self.T0), "device": mk.DEVICE, "tunnels": [],
                   "vcpus": vcpus}]
        return header + [{"utc": mk.iso(self.T0 + timedelta(seconds=10 * k)), "cpu": cpu(k), "steal": steal(k)}
                         for k in range(minutes * 6) if k not in skip]

    def test_samples_on_other_vcpus_or_without_a_header_are_refused(self):
        arms = {a: self.arm() for a in "BAC"}
        for samples in (self.host(vcpus=16), self.host()[1:], {"vcpus": 4, "samples": self.host()[1:]}):
            with self.subTest(samples=type(samples).__name__):
                out = analyze_cr.drop_c(samples, arms)
                self.assertEqual((out["decision"], out["reasons"]), ("not_judged", ["vcpus_differ"]))
        self.assertEqual(analyze_cr.drop_c({"vcpus": 8, "samples": self.host()[1:]}, arms)["decision"], "keep")

    def arm(self, crawl_after_min=5, minutes=120):
        rows = []
        for m in range(minutes + 1):
            t = self.T0 + timedelta(minutes=m)
            stored = max(0, m - crawl_after_min) * 3
            rows.append({"time": mk.iso(t), "metrics_ok": True,
                         "values": {"torrents": stored, "dht_sample_suppression_seconds": 0}})
        return rows

    def test_a_quiet_rehearsal_keeps_c(self):
        out = analyze_cr.drop_c(self.host(), {a: self.arm() for a in "BAC"})
        self.assertEqual((out["decision"], out["interval_minutes"]), ("keep", 60.0))
        self.assertEqual(out["slots"], 360)

    def test_cpu_or_steal_over_drops_c(self):
        hot = analyze_cr.drop_c(self.host(cpu=lambda k: 0.72), {a: self.arm() for a in "BAC"})
        self.assertEqual((hot["decision"], hot["reasons"]), ("drop", ["cpu"]))
        steal = analyze_cr.drop_c(self.host(steal=lambda k: 0.04 if k % 10 == 0 else 0.0), {a: self.arm() for a in "BAC"})
        self.assertEqual(steal["decision"], "drop")

    def test_missing_slots_rank_over(self):
        gaps = set(range(360, 720, 15))  # 24 of the last hour's 360 slots missing: 6.7 %
        out = analyze_cr.drop_c(self.host(skip=gaps), {a: self.arm() for a in "BAC"})
        self.assertEqual((out["decision"], out["cpu_p95"]), ("drop", None))

    def test_the_interval_starts_only_once_every_arm_crawls(self):
        out = analyze_cr.drop_c(self.host(), {"B": self.arm(), "C": self.arm(crawl_after_min=90)})
        self.assertEqual(out["decision"], "not_judged")
        self.assertIn("fewer_than_60_crawling_minutes", out["reasons"])
        out = analyze_cr.drop_c(self.host(), {"B": self.arm(), "C": self.arm(crawl_after_min=200)})
        self.assertEqual(out["reasons"], ["an_arm_never_crawled"])

    def test_the_command_prints_its_numbers(self):
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            (d / "host-samples.jsonl").write_text("".join(json.dumps(r) + "\n" for r in self.host()))
            argv = ["drop-c", str(d / "host-samples.jsonl"), "--output", str(d / "out.json")]
            for a in "BAC":
                (d / a).mkdir()
                (d / a / "samples.jsonl").write_text("".join(json.dumps(r) + "\n" for r in self.arm()))
                argv += ["--arm", f"{a}={d / a}"]
            self.assertEqual(analyze_cr.main(argv), 0)
            self.assertEqual(json.loads((d / "out.json").read_text())["decision"], "keep")


class D3ExemptionTests(SegmentBase):
    """Item B (plan §8 item 6; owner §2.85 D3; runner, 2026-11-27): a mover run or parity check
    spares the window only when none of the pools it touches holds the arm's /data device."""
    POOLS = {"ssd_apps": [mk.DEVICE], "cache": ["sdc"], "array": ["md1", "md2"]}

    def decl(self, pools):
        d = mk.archive("L3")["declaration"]
        if pools is not None:
            d["pools"] = pools
        return analyze_cr.check_declaration(d)

    def segment_with(self, kind, touched, pools=POOLS, checked=True):
        sim = Round1Sim()
        entry = {"category": "array_operation", "elapsed_seconds": 4000.0, "same_boot": True,
                 "collector_finished": False, "text": ""}
        if kind:
            entry.update(kind=kind, pools=touched)
        sim.activity = [entry]
        if checked:
            return self.judge(sim, self.decl(pools))
        return self.judge(sim, dict(mk.archive("L3")["declaration"], pools=pools))  # past check_declaration

    def test_the_declared_device_must_sit_in_exactly_one_pool(self):
        # Runner, 2026-11-27: the map lists the VM's kernel names; a host-side map
        # (vdb in no pool), a device in two pools or a partition name is refused.
        for pools in ({"ssd_apps": ["sdz"], "array": ["md1"]},
                      {"ssd_apps": [mk.DEVICE], "cache": [mk.DEVICE], "array": ["md1"]},
                      {"ssd_apps": [mk.DEVICE + "p1"], "array": ["md1"]}):
            with self.subTest(pools=pools), self.assertRaises(ValueError):
                self.decl(pools)
        self.decl(self.POOLS)  # exactly one: accepted
        self.decl({})  # no map: accepted, and nothing is exempt

    def test_a_device_in_no_pool_is_never_exempt(self):
        for pools in ({"ssd_apps": ["sdz"], "array": ["md1"]},
                      {"ssd_apps": [mk.DEVICE + "p1"], "array": ["md1"]}):
            with self.subTest(pools=pools):
                report = self.segment_with("mover", ["ssd_apps", "array"], pools, checked=False)
                self.assertFalse(self.check(report, "no_host_activity")[0]["ok"])
                self.assertEqual(report["activity_exempt_d3"], [])

    def test_mover_and_parity_off_the_arms_pool_spare_the_window(self):
        for kind, touched in (("mover", ["cache", "array"]), ("parity", ["array"])):
            report = self.segment_with(kind, touched)
            self.assertValid(report)
            self.assertEqual(report["activity_exempt_d3"][0]["pools"], touched)

    def test_everything_else_still_counts(self):
        for kind, touched, pools in (("mover", ["ssd_apps", "array"], self.POOLS),  # touches the arm's own pool
                                     ("other", ["array"], self.POOLS),
                                     ("mover", ["array", "unmapped"], self.POOLS),
                                     ("mover", ["array"], None),
                                     (None, None, self.POOLS)):
            with self.subTest(kind=kind, touched=touched, pools=pools is not None):
                report = self.segment_with(kind, touched, pools)
                self.assertFalse(self.check(report, "no_host_activity")[0]["ok"])

    def test_the_note_command_records_kind_and_pools(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "run.json").write_text(json.dumps({"clock": {"monotonic_origin": 0.0, "boot_id": "b"},
                                                       "round1": {"round": 1}}))
            boot = root / "boot_id"
            boot.write_text("b\n")
            rc = collect.note_command(["--root", d, "--category", "array_operation", "--kind", "mover",
                                       "--pool", "cache", "--pool", "array"], clock=lambda: 10.0, boot_id_path=boot)
            self.assertEqual(rc, 0)
            entry = json.loads((root / "activity.jsonl").read_text())
            self.assertEqual((entry["kind"], entry["pools"]), ("mover", ["array", "cache"]))
            for argv in (["--kind", "mover"], []):  # a round-1 run needs both
                with self.assertRaises(SystemExit):
                    collect.note_command(["--root", d, "--category", "array_operation", *argv], boot_id_path=boot)


class RoundSpecAndVcpuTests(RoundBase):
    def test_reports_and_the_verdict_name_round_1s_spec(self):
        v = self.gate(mk.archive("L3"))
        self.assertEqual(v["round1_spec"], "docs/decisions/2026-11-27-CR-SPEC-round1.md")
        self.assertEqual(v["spec"], analyze_cr.SPEC)  # old runs keep theirs
        with tempfile.TemporaryDirectory() as d:
            report = analyze_cr.analyze_segment(Round1Sim().write(Path(d) / "s"), None, declaration())
        self.assertEqual(report["round1"]["round1_spec"], analyze_cr.ROUND1_SPEC)

    def test_host_samples_on_other_vcpus_are_not_judged(self):
        a = mk.archive("L3")
        self.day(a, 1)["contention"]["vcpus"] = 16
        v = self.gate(a)
        self.assertIn("vcpus_differ", self.cand(v)["days"][0]["contention"]["reasons"])
        self.assertNotJudged(v, "contention_not_judged")

    def test_the_declaration_names_8_vcpus(self):
        decl = mk.archive("L3")["declaration"]
        for bad in (None, 16):
            d = dict(decl, vcpus=bad)
            with self.assertRaises(ValueError):
                analyze_cr.check_declaration(d)


class L2cTests(RoundBase):
    """Item G: B fails in L3; A (candidate) and C (diagnostic) go on in a fresh attempt."""

    def test_after_l3_the_remaining_arms_pass_in_l2c(self):
        a = mk.archive("L3")
        add_attempt(a, "L2c", "r1-a2")
        v = self.gate(a)
        self.assertEqual(self.cand(v, "A")["layout"], "L2c")
        self.assertEqual(self.cand(v, "A")["crawler"], "pass", self.cand(v, "A")["reasons"])

    def test_l3_then_a_solo_confirmation_then_l2c(self):
        # Runner review (G): a solo confirmation of B sits outside the chain, so L2c still
        # follows L3; A and C go on in it.
        a = mk.archive("L3")
        add_attempt(a, "S", "r1-a2")
        add_attempt(a, "L2c", "r1-a3")
        analyze_cr.check_declaration(a["declaration"])
        v = self.gate(a)
        self.assertEqual(self.cand(v, "A")["layout"], "L2c")
        self.assertEqual(self.cand(v, "A")["crawler"], "pass", self.cand(v, "A")["reasons"])

    def test_l2c_follows_only_l3(self):
        a = mk.archive("L2c")
        with self.assertRaises(ValueError):
            analyze_cr.check_declaration(a["declaration"])
        a = mk.archive("L2")
        add_attempt(a, "L2c", "r1-a2")
        with self.assertRaises(ValueError):
            analyze_cr.check_declaration(a["declaration"])


class DigestFormTests(SegmentBase):
    """Item K: the declaration names the image as name@sha256:<hex>; reports (whose digest the
    collector records without the repository) are compared on the part after the last `@`."""

    def test_the_declaration_needs_the_full_form(self):
        a = mk.archive("L3")["declaration"]
        bad = json.loads(json.dumps(a).replace(mk.DIGEST, "sha256:" + "d" * 64))
        with self.assertRaises(ValueError):
            analyze_cr.check_declaration(bad)
        analyze_cr.check_declaration(a)

    def test_the_collectors_bare_digest_matches_and_another_digest_part_fails(self):
        sim = Round1Sim()
        sim.digest = "sha256:" + "d" * 64  # as the collector records it
        self.assertValid(self.judge(sim))
        sim = Round1Sim()
        sim.digest = "lodestar@sha256:" + "e" * 64  # same name, another image
        report = self.judge(sim)
        self.assertEqual(report["verdict"], "void")
        self.assertFalse(self.check(report, "declared_image_digest")[0]["ok"])


class GateDigestTests(RoundBase):
    """Item K, at the gate: a report is judged on its digest part, never on a prefix of it or
    on its repository name."""

    def digest(self, value):
        a = mk.archive("L3")
        for d in a["days"]:
            d["reports"]["B"]["identity"]["image_digest"] = value
        return self.cand(self.gate(a))

    def test_another_image_fails(self):
        for value in ("sha256:" + "e" * 64, "sha256:" + "d" * 63 + "e", "sha256:" + "d" * 32):
            with self.subTest(value=value):
                c = self.digest(value)
                self.assertIn("image_digest_differs", c["days"][0]["reasons"])
                self.assertNotEqual(c["crawler"], "pass")

    def test_the_same_digest_under_another_repository_name_passes(self):
        for value in ("sha256:" + "d" * 64, "ghcr.io/someone/lodestar@sha256:" + "d" * 64):
            with self.subTest(value=value):
                self.assertEqual(self.digest(value)["crawler"], "pass")


class ExitRecheckTests(RoundBase):
    """Item C: a repair before the start re-checks the exits; readings are judged against the
    exit in force at their time, so the pre-repair reading is history, not a fault."""

    def repaired(self, in_window_old=False):
        a = mk.archive("L3")
        day = self.day(a, 1)
        arc = day["archive"]
        start = analyze_cr.parse_utc(day["reports"]["B"]["timeline"]["process_start_utc"])
        old = dict(arc["exits"], X=mk.h("exit-x-before-repair"))
        arc["exit_checks"] = [{"utc": mk.iso(start - timedelta(minutes=30)), "exits": old},
                              {"utc": mk.iso(start - timedelta(minutes=10)), "exits": dict(arc["exits"])}]
        arc["arms"]["B"]["egress"].insert(0, {"utc": mk.iso(start - timedelta(minutes=30)), "hash": old["X"]})
        if in_window_old:
            ws = analyze_cr.parse_utc(day["reports"]["B"]["timeline"]["window_start_utc"])
            arc["arms"]["B"]["egress"].append({"utc": mk.iso(ws + timedelta(hours=3, minutes=10)), "hash": old["X"]})
        return a

    def test_a_repair_then_a_recheck_keeps_the_day_judged(self):
        v = self.gate(self.repaired())
        self.assertEqual(self.cand(v)["days"][0]["status"], "pass", self.cand(v)["days"][0]["reasons"])

    def test_a_recheck_inside_the_window_that_moves_the_exit_is_a_fault(self):
        # Readings match the exit in force at ws, but a later check (and so the day's final
        # exits) names another server for X: the arm's exit changed under it.
        a = mk.archive("L3")
        day = self.day(a, 1)
        arc = day["archive"]
        ws = analyze_cr.parse_utc(day["reports"]["B"]["timeline"]["window_start_utc"])
        start = analyze_cr.parse_utc(day["reports"]["B"]["timeline"]["process_start_utc"])
        moved = dict(arc["exits"], X=mk.h("exit-x-after-ws"))
        arc["exit_checks"] = [{"utc": mk.iso(start - timedelta(minutes=10)), "exits": dict(arc["exits"])},
                              {"utc": mk.iso(ws + timedelta(hours=2)), "exits": moved}]
        arc["exits"] = moved
        day_out = self.cand(self.gate(a))["days"][0]
        self.assertIn("egress_assignment_missing", day_out["reasons"])
        self.assertNotEqual(day_out["status"], "pass")

    def test_the_old_exit_inside_the_window_is_still_a_fault(self):
        v = self.gate(self.repaired(in_window_old=True))
        self.assertIn("egress_differs_from_declared_exit", self.cand(v)["days"][0]["reasons"])


class EnoughValuesTests(SegmentBase):
    """Plan §6.1: lookup_end's row reads lookups ended on enough values (CR-LOOKUP-END's own
    end, `enough_values`), absent or zero voiding; the ledger counts it as an end on values."""

    def test_the_row_reads_enough_values(self):
        report = self.judge(Round1Sim())
        self.assertValid(report)
        check = self.check(report, "lookups_end_on_enough_values")[0]
        self.assertTrue(check["ok"])
        self.assertGreater(report["bundle"]["lookup_end"]["ended_on_enough_values"], 0)

    def test_absent_or_zero_voids(self):
        sim = Round1Sim()
        sim.render_early = False
        report = self.judge(sim)
        self.assertEqual(report["verdict"], "void")
        self.assertEqual(self.check(report, "lookups_end_on_enough_values")[0]["detail"]["note"], "absent")
        sim = Round1Sim()
        sim.early_share = 0.0
        report = self.judge(sim)
        self.assertEqual(self.check(report, "lookups_end_on_enough_values")[0]["detail"]["note"], "a measured zero")

    def test_the_collector_keeps_the_series(self):
        self.assertIn("enough_values", collect.LEDGER_END_REASONS)
        snap = collect.sanitize_snapshot(f'lodestar_{L["ended"]}{{reason="enough_values"}} 7\n')
        self.assertIn('reason="enough_values"} 7', snap)

    def test_old_runs_without_the_series_keep_their_numbers(self):
        a = {"snap": {L["ended"]: {(("reason", "values"),): 10.0, (("reason", "no_values"),): 5.0}}}
        b = {"snap": {L["ended"]: {(("reason", "values"),): 30.0, (("reason", "no_values"),): 9.0}}}
        self.assertEqual(analyze_cr.ledger(a, b)["ended_on_values"], 20.0)
        b["snap"][L["ended"]][(("reason", "enough_values"),)] = 4.0
        a["snap"][L["ended"]][(("reason", "enough_values"),)] = 1.0
        self.assertEqual(analyze_cr.ledger(a, b)["ended_on_values"], 23.0)


class ComparatorBindingTests(RoundBase):
    """Runner, 2026-11-27 (§2.19): a comparator run by a collector without round-1 fields
    (cr-ab-comp-2's) is bound after its window by the coordinator's `bind-comparator`, and the
    manifest carries the binding. The report here is the analyzer's own, of a comparator run
    written as the pre-round-1 collector writes it (no run_id)."""

    @classmethod
    def setUpClass(cls):
        cls.tmp_cls = tempfile.TemporaryDirectory()
        sim = cr.Sim(kind="comparator", hours=24, hour_s=3600, rehearsal=False, warm_min=60)
        cls.old = analyze_cr.analyze_segment(sim.write(Path(cls.tmp_cls.name) / "comp"))

    @classmethod
    def tearDownClass(cls):
        cls.tmp_cls.cleanup()

    def bound(self, **over):
        end = self.old["timeline"]["window_end_utc"]
        b = {"schema": 1, "kind": "cr_round1_comparator_binding", "run_id": "comp-1", "host_identity": mk.HOST,
             "network_identity": dict(mk.NETWORK), "ended_utc": mk.iso(analyze_cr.parse_utc(end) + timedelta(seconds=40)),
             "bound_utc": "2026-11-28T18:30:00Z", "bound_after_window": True}
        b.update(over)
        return b

    def gate_bound(self, binding):
        a = mk.archive("L3")
        a["comparator"] = json.loads(json.dumps(self.old))
        self.n += 1
        path = mk.write(self.root / f"b{self.n}", a)
        data = (json.dumps(binding) + "\n").encode()
        (path.parent / "comparator-binding.json").write_bytes(data)
        man = json.loads(path.read_text())
        man["comparator_binding"] = {"path": "comparator-binding.json", "sha256": hashlib.sha256(data).hexdigest()}
        path.write_text(json.dumps(man))
        return analyze_cr.gate_round(analyze_cr.load_manifest(path))

    def test_the_old_report_has_no_binding_of_its_own(self):
        self.assertEqual(self.old["verdict"], "valid", [c for c in self.old["checks"] if not c["ok"]])
        self.assertNotIn("binding", self.old)
        a = mk.archive("L3")
        a["comparator"] = json.loads(json.dumps(self.old))
        v = self.gate(a)  # unbound: every candidate not judged
        self.assertNotJudged(v, "comparator_not_booked")

    def test_bound_after_its_window_it_passes(self):
        v = self.gate_bound(self.bound())
        self.assertEqual(self.cand(v)["crawler"], "pass", self.cand(v)["reasons"])
        self.assertTrue(v["comparator_bound_after_window"])

    def test_a_host_or_network_mismatch_at_bind_time_fails(self):
        self.assertNotJudged(self.gate_bound(self.bound(host_identity=mk.h("another-host"))), "comparator_host_identity")
        net = dict(mk.NETWORK, uplink_hash=mk.h("another-uplink"))
        self.assertNotJudged(self.gate_bound(self.bound(network_identity=net)), "comparator_network_identity")

    def test_an_end_before_the_window_end_is_interrupted(self):
        end = analyze_cr.parse_utc(self.old["timeline"]["window_end_utc"])
        self.assertNotJudged(self.gate_bound(self.bound(ended_utc=mk.iso(end - timedelta(minutes=5)))),
                             "comparator_interrupted")

    def test_a_malformed_binding_is_an_error(self):
        with self.assertRaises(ValueError):
            self.gate_bound(self.bound(bound_after_window=False))


class ComparatorTests(RoundBase):
    def test_missing_comparator(self):
        a = mk.archive("L3")
        a["comparator"] = None
        self.assertNotJudged(self.gate(a), "comparator_missing")

    def test_comparator_spacing_violation(self):
        a = mk.archive("L3")
        # Day 1's processes start 12 h after the comparator's window end, its windows 13 h 01 min
        # after; moving the comparator 90 min later puts every day-1 window inside the 12 h.
        t = a["comparator"]["timeline"]
        for k in ("window_end_utc", "last_scrape_utc"):
            t[k] = mk.iso(analyze_cr.parse_utc(t[k]) + timedelta(minutes=90))
        a["comparator"]["binding"]["ended_utc"] = mk.iso(analyze_cr.parse_utc(t["window_end_utc"]) + timedelta(seconds=40))
        v = self.gate(a)
        self.assertNotJudged(v, "spacing")
        self.assertIn("lodestar_window_within_12h_after_comparator", self.cand(v)["spacing_problems"])
        self.assertIn("lodestar_before_comparator_end_plus_12h", self.cand(v)["spacing_problems"])

    def shift_comparator(self, a, delta):
        c = a["comparator"]
        for k in ("process_start_utc", "window_start_utc", "window_end_utc", "last_scrape_utc"):
            c["timeline"][k] = mk.iso(analyze_cr.parse_utc(c["timeline"][k]) + delta)
        c["binding"]["ended_utc"] = mk.iso(analyze_cr.parse_utc(c["binding"]["ended_utc"]) + delta)

    def test_a_comparator_after_the_round_is_not_judged(self):
        a = mk.archive("L3")
        self.shift_comparator(a, timedelta(days=10))
        v = self.gate(a)
        self.assertNotJudged(v, "spacing")
        self.assertIn("lodestar_before_comparator_end_plus_12h", self.cand(v)["spacing_problems"])

    def test_a_process_one_second_inside_the_12h_is_not_judged(self):
        a = mk.archive("L3")
        self.shift_comparator(a, timedelta(seconds=1))  # B's day-1 process starts at end + 12 h - 1 s
        self.assertIn("lodestar_before_comparator_end_plus_12h", self.cand(self.gate(a))["spacing_problems"])
        self.assertEqual(self.cand(self.gate(mk.archive("L3")))["spacing_problems"], [])  # exactly 12 h passes

    def test_spacing_reads_the_window_end_never_ended(self):
        a = mk.archive("L3")
        # A late `ended` (finalization after the window) doesn't move spacing.
        a["comparator"]["binding"]["ended_utc"] = mk.iso(mk.COMP_END + timedelta(hours=11, minutes=30))
        self.assertEqual(self.cand(self.gate(a))["crawler"], "pass")

    def test_a_complete_comparator_report_that_is_invalid(self):
        a = mk.archive("L3")
        a["comparator"]["verdict"] = "void"  # a stalled hour: count_rises_every_hour
        self.assertNotJudged(self.gate(a), "comparator_not_valid")
        self.assertIn("comparator_void", self.cand(self.gate(a))["comparator_problems"])

    def test_an_interrupted_comparator(self):
        a = mk.archive("L3")
        a["comparator"]["binding"]["ended_utc"] = mk.iso(mk.COMP_END - timedelta(hours=2))
        self.assertIn("comparator_interrupted", self.cand(self.gate(a))["comparator_problems"])

    def test_an_unbooked_comparator(self):
        a = mk.archive("L3")
        a["comparator"]["binding"]["run_id"] = "comp-2"
        self.assertIn("comparator_not_booked", self.cand(self.gate(a))["comparator_problems"])

    def test_a_comparator_on_another_network(self):
        a = mk.archive("L3")
        a["comparator"]["binding"]["network_identity"]["uplink_hash"] = mk.h("other-uplink")
        self.assertIn("comparator_network_identity", self.cand(self.gate(a))["comparator_problems"])


class ContentionTests(RoundBase):
    def set(self, a, day, fn, start=None, end=None):
        for s in self.day(a, day)["contention"]["samples"]:
            t = analyze_cr.parse_utc(s["utc"])
            if (start is None or t >= start) and (end is None or t < end):
                fn(s)

    def ws(self, a, day, arm="B"):
        return analyze_cr.parse_utc(self.day(a, day)["reports"][arm]["timeline"]["window_start_utc"])

    def test_a_cpu_breach_on_one_day_needs_solo_confirmation(self):
        a = mk.archive("L3")
        self.set(a, 1, lambda s: s.update(cpu=0.85))
        v = self.gate(a)
        self.assertNotJudged(v, "contention_breach")
        self.assertTrue(self.cand(v)["solo_confirmation_required"])
        self.assertEqual(v["selection"]["state"], "pending_retry")

    def test_a_breach_on_an_infrastructure_or_product_day_is_classed_first(self):
        """§6.3: the first class that applies decides; class 1 and 2 get no solo confirmation."""
        for event, cls in (({"class": "infrastructure", "arms": [], "diagnosis": None}, "infrastructure_fault"),
                           ({"class": "product", "arms": ["B"], "diagnosis": "pending"},
                            "product_failure_under_diagnosis")):
            with self.subTest(cls):
                a = mk.archive("L3")
                self.set(a, 1, lambda s: s.update(cpu=0.85))
                self.day(a, 1)["archive"]["events"].append(dict(event))
                v = self.gate(a)
                day1 = self.cand(v)["days"][0]
                self.assertIn("contention_breach", day1["reasons"])
                self.assertIn(cls, day1["reasons"])
                self.assertFalse(day1["solo_confirmation_required"])
                self.assertEqual(day1["contention_class"], "infrastructure_or_product")

    def test_a_network_contention_breach_on_a_b_day(self):
        a = mk.archive("L3")
        ws = self.ws(a, 2)
        # 2 % of probes lost in hour 5 of B's day-2 window.
        n = [0]

        def lose(s):
            n[0] += 1
            if n[0] % 50 == 0:
                s["tunnels"]["B"] = {"rtt_ms": None, "lost": True}
        self.set(a, 2, lose, ws + timedelta(hours=5), ws + timedelta(hours=6))
        v = self.gate(a)
        day2 = self.cand(v)["days"][1]
        self.assertEqual(day2["status"], "not_judged")
        self.assertEqual(day2["contention"]["network_hours_breached"], [5])
        self.assertTrue(day2["solo_confirmation_required"])
        self.assertEqual(self.cand(v, "A")["crawler"], "pass")  # A's own tunnel was clean

    def test_uplink_drops_breach(self):
        a = mk.archive("L3")
        ws = self.ws(a, 1)
        self.set(a, 1, lambda s: s.update(uplink_drops_total=3), ws + timedelta(hours=9, minutes=30))
        day1 = self.cand(self.gate(a))["days"][0]
        self.assertEqual(day1["contention"]["network_hours_breached"], [9])

    def test_an_uplink_counter_reset_is_not_judged(self):
        for before, after in ((1000, 5), (100, 0)):
            a = mk.archive("L3")
            ws = self.ws(a, 1)
            self.set(a, 1, lambda s: s.update(uplink_drops_total=before), None, ws + timedelta(hours=4))
            self.set(a, 1, lambda s: s.update(uplink_drops_total=after), ws + timedelta(hours=4))
            day1 = self.cand(self.gate(a))["days"][0]
            self.assertEqual(day1["contention"]["reasons"], ["uplink_counter_reset"])

    def test_uplink_drops_are_summed_step_by_step(self):
        a = mk.archive("L3")
        ws = self.ws(a, 1)
        self.set(a, 1, lambda s: s.update(uplink_drops_total=2), ws + timedelta(hours=6, minutes=10),
                 ws + timedelta(hours=6, minutes=20))
        self.set(a, 1, lambda s: s.update(uplink_drops_total=2), ws + timedelta(hours=6, minutes=20))
        day1 = self.cand(self.gate(a))["days"][0]
        self.assertEqual(day1["contention"]["network_hours_breached"], [6])

    def test_a_missing_tunnel_probe_counts_as_lost(self):
        a = mk.archive("L3")
        ws = self.ws(a, 1)
        n = [0]

        def drop_probe(s):
            n[0] += 1
            if n[0] % 50 == 0:  # 2 % of hour 8's probes missing, the rest present
                del s["tunnels"]["B"]
        self.set(a, 1, drop_probe, ws + timedelta(hours=8), ws + timedelta(hours=9))
        day1 = self.cand(self.gate(a))["days"][0]
        self.assertEqual(day1["contention"]["network_hours_breached"], [8])

    def test_rtt_over_twice_the_rehearsal_median(self):
        a = mk.archive("L3")
        ws = self.ws(a, 1)
        self.set(a, 1, lambda s: s["tunnels"]["B"].update(rtt_ms=61.0), ws + timedelta(hours=3),
                 ws + timedelta(hours=4))
        self.assertIn("network", self.cand(self.gate(a))["days"][0]["contention"]["reasons"])

    def test_a_series_below_98_percent_coverage_is_not_judged(self):
        a = mk.archive("L3")
        ws = self.ws(a, 1)
        self.set(a, 1, lambda s: s.update(steal=None), ws + timedelta(hours=2), ws + timedelta(hours=2, minutes=30))
        day1 = self.cand(self.gate(a))["days"][0]
        self.assertEqual(day1["contention"]["verdict"], "not_judged")
        self.assertIn("coverage_below_98pct:steal", day1["contention"]["reasons"])
        self.assertFalse(day1["solo_confirmation_required"])

    def test_missing_intervals_count_over_the_threshold(self):
        a = mk.archive("L3")
        ws = self.ws(a, 1)
        samples = self.day(a, 1)["contention"]["samples"]
        # 1.5 % of the window's intervals missing: covered, but they rank over 80 % CPU
        # where 3.6 % of intervals already read 0.81, so together they pass p95.
        cut = ws + timedelta(hours=1)
        drop = {mk.iso(cut + timedelta(seconds=10 * k)) for k in range(130)}
        self.day(a, 1)["contention"]["samples"] = [s for s in samples if s["utc"] not in drop]
        self.set(a, 1, lambda s: s.update(cpu=0.81), ws + timedelta(hours=3), ws + timedelta(hours=3, minutes=52))
        day1 = self.cand(self.gate(a))["days"][0]
        self.assertEqual(day1["contention"]["verdict"], "breach")
        self.assertIn("cpu", day1["contention"]["reasons"])

    def test_disk_intervals_without_writes(self):
        a = mk.archive("L3")
        ws = self.ws(a, 1)
        self.set(a, 1, lambda s: s.update(disk_write_ms=None, disk_inflight=0), ws, ws + timedelta(hours=6))
        self.assertEqual(self.cand(self.gate(a))["crawler"], "pass")  # nothing in flight: excluded
        a = mk.archive("L3")
        self.set(a, 1, lambda s: s.update(disk_write_ms=None, disk_inflight=3), ws, ws + timedelta(hours=6))
        self.assertIn("disk", self.cand(self.gate(a))["days"][0]["contention"]["reasons"])

    def test_a_breach_in_a_solo_confirmation_fails_the_confirmation(self):
        a = mk.archive("S")
        self.set(a, 2, lambda s: s.update(steal=0.2))
        c = self.cand(self.gate(a))
        self.assertTrue(c["solo_confirmation_failed"])
        self.assertFalse(c["solo_confirmation_required"])
        self.assertEqual(self.gate(a)["selection"]["trail"][0]["disposition"], "out_of_retries")

    def test_a_window_that_is_not_a_multiple_of_ten_seconds(self):
        a = mk.archive("L3")
        r = self.day(a, 1)["reports"]["B"]["timeline"]
        r["window_end_utc"] = mk.iso(analyze_cr.parse_utc(r["window_end_utc"]) + timedelta(seconds=5))
        for s in self.day(a, 1)["contention"]["samples"]:
            s["utc"] = mk.iso(analyze_cr.parse_utc(s["utc"]) + timedelta(seconds=3))
        day1 = self.cand(self.gate(a))["days"][0]
        self.assertEqual(day1["contention"]["verdict"], "ok")
        self.assertEqual(day1["contention"]["intervals"], 8641)

    def test_samples_from_another_device_are_not_judged(self):
        a = mk.archive("L3")
        self.day(a, 1)["contention"]["backing_device"] = "sda"
        self.assertNotJudged(self.gate(a), "contention_not_judged")

    def test_missing_host_samples_are_not_judged(self):
        a = mk.archive("L3")
        self.day(a, 1)["contention"] = None
        self.assertNotJudged(self.gate(a), "contention_not_judged")


class CohortTests(RoundBase):
    """§6.4's cohorts through gate-round, by hand: the mean and hours are net of the carry-in
    bound; the latency cohort keeps carry-in samples and charges dropped ones above 600 s."""

    def with_edge(self, le600, gross_mean, carry_mean):
        a = mk.archive("L3")  # B's day 1 (a lone S attempt is no longer allowed: SoloBoundTests)
        r = self.day(a, 1)["reports"]["B"]
        lat = analyze_cr.latency(*edge_scrapes(le600, 10000, 100))
        r["latency"] = lat
        net = gross_mean - carry_mean
        r["numerator"]["window"] = {"mean_gross_per_min": gross_mean, "mean_net_per_min": net,
                                    "carry_in_bound": carry_mean * 1440}
        return a

    def test_at_the_edge_with_carry_in_it_passes(self):
        # 9,595 of 10,000 samples + 100 dropped within 600 s: 9,595 / 10,100 = 0.95 exactly.
        v = self.gate(self.with_edge(9595, 112.0, 8.0))
        day1 = self.cand(v)["days"][0]
        self.assertEqual(day1["tests"]["p95"], "pass")
        self.assertEqual(day1["tests"]["mean_net_per_min"], 104.0)
        self.assertEqual(self.cand(v)["crawler"], "pass")

    def test_one_sample_under_the_edge_fails(self):
        day1 = self.cand(self.gate(self.with_edge(9594, 112.0, 8.0)))["days"][0]
        self.assertEqual(day1["tests"]["p95"], "fail")
        self.assertEqual(day1["status"], "fail")

    def test_the_mean_is_net_of_carry_in(self):
        # Gross 105/min passes; net of an 8/min carry-in bound it's 97/min, which fails.
        day1 = self.cand(self.gate(self.with_edge(9595, 105.0, 8.0)))["days"][0]
        self.assertFalse(day1["tests"]["mean_passes"])
        self.assertEqual(day1["status"], "fail")


class SelectionTests(RoundBase):
    def test_b_passes_on_rate_but_fails_memory_an_authorized_a_passes_both(self):
        a = mk.archive("L3")
        r = self.day(a, 2)["reports"]["B"]
        r["memory"].update(meets_bar=False, slope_mib_per_hour=2.4, ci95_mib_per_hour=[2.1, 2.7])
        v = self.gate(a)
        self.assertEqual(self.cand(v)["crawler"], "pass")
        self.assertEqual(self.cand(v)["memory"], "fail")
        self.assertEqual(v["selection"]["ships"], "A")
        self.assertEqual(v["selection"]["trail"][0]["disposition"], "crawler_accepted_memory_fails")

    def test_both_eligible_b_ships(self):
        self.assertEqual(self.gate(mk.archive("L2"))["selection"]["ships"], "B")

    def test_an_incomplete_memory_fit_is_memory_not_judged(self):
        a = mk.archive("L3")
        self.day(a, 1)["reports"]["B"]["memory"].update(meets_bar=None, window_complete=False)
        c = self.cand(self.gate(a))
        self.assertEqual(c["crawler"], "pass")
        self.assertEqual(c["memory"], "not_judged")
        self.assertFalse(c["shipping_eligible"])

    def test_missing_anon_is_memory_not_judged(self):
        a = mk.archive("L3")
        del self.day(a, 2)["reports"]["B"]["memory"]
        v = self.gate(a)
        self.assertEqual(self.cand(v)["memory"], "not_judged")
        self.assertEqual(self.cand(v)["days"][1]["memory"]["refused"], "no_fit")
        self.assertEqual(v["selection"]["state"], "pending_retry")  # B isn't terminal: A isn't read

    def test_b_incomplete_means_a_is_not_read(self):
        a = mk.archive("L3")
        self.day(a, 1)["reports"]["B"]["verdict"] = "invalid"
        v = self.gate(a)
        self.assertEqual(v["selection"], {"ships": None, "state": "pending_retry", "waiting_on": "B",
                                          "trail": [{"arm": "B", "disposition": "incomplete", "authorized": True}]})

    def test_neither_ships_gives_the_round_1_report(self):
        a = mk.archive("L3")
        for arm in ("A", "B"):
            self.day(a, 1)["reports"][arm]["numerator"]["window"]["mean_net_per_min"] = 95.0
        v = self.gate(a)
        self.assertEqual(v["selection"]["state"], "round1_report")


class ManifestTests(RoundBase):
    def test_a_repeat_that_is_not_an_integer_is_an_error(self):
        path = mk.write(self.root / "m", mk.archive("L3"))
        man = json.loads(path.read_text())
        man["days"][0]["repeat"] = "1"
        path.write_text(json.dumps(man))
        with self.assertRaises(ValueError):
            analyze_cr.load_manifest(path)

    def test_a_hash_mismatch_is_an_error(self):
        path = mk.write(self.root / "m", mk.archive("L3"))
        (path.parent / "r1-a1" / "day1" / "B.json").write_text("{}")
        with self.assertRaises(ValueError):
            analyze_cr.load_manifest(path)

    def test_a_path_outside_the_archive_is_an_error(self):
        path = mk.write(self.root / "m", mk.archive("L3"))
        man = json.loads(path.read_text())
        man["comparator"]["path"] = "../elsewhere.json"
        path.write_text(json.dumps(man))
        with self.assertRaises(ValueError):
            analyze_cr.load_manifest(path)

    def test_every_input_hash_is_in_the_verdict(self):
        v = self.gate(mk.archive("L3"))
        self.assertEqual(len(v["inputs"]), 1 + 1 + 2 * (3 + 2 + 3))  # + each arm's gluetun log

    def test_the_analyzer_hash_is_in_the_verdict_and_the_segment_report(self):
        self.assertEqual(self.gate(mk.archive("L3"))["analyzer_sha256"], analyze_cr.ANALYZER_SHA256)
        report = analyze_cr.analyze_segment(Round1Sim().write(self.root / "seg-h"), None, declaration())
        self.assertEqual(report["round1"]["analyzer_sha256"], analyze_cr.ANALYZER_SHA256)

    def test_the_same_archive_gives_an_identical_result(self):
        path = mk.write(self.root / "m", mk.archive("L3"))
        out = []
        for k in range(2):
            target = self.root / f"out{k}.json"
            self.assertEqual(analyze_cr.main(["gate-round", str(path), "--output", str(target)]), 0)
            out.append(target.read_bytes())
        self.assertEqual(out[0], out[1])

    def test_the_committed_golden_verdict(self):
        """fixtures/round1/l3_pass_verdict.json is gate-round's output on make_archive.py's L3
        archive: a change in either shows here."""
        v = self.gate(mk.archive("L3"))
        v.pop("manifest_sha256")  # the manifest names temporary paths' contents only
        self.assertEqual(v.pop("analyzer_sha256"), analyze_cr.ANALYZER_SHA256)  # changes with every edit
        self.assertEqual(json.loads(json.dumps(v)), json.loads(GOLDEN.read_text()))

    def test_a_real_segment_report_is_read(self):
        """A report `segment --declaration` wrote, in place of a synthetic one: gate-round
        reads its binding and tests (it's a rehearsal, so only that keeps it from a verdict)."""
        tmp = self.root / "seg"
        real = analyze_cr.analyze_segment(Round1Sim().write(tmp), None, declaration())
        a = mk.archive("L3")
        self.day(a, 1)["reports"]["B"] = real
        c = self.cand(self.gate(a))
        day1 = c["days"][0]
        self.assertEqual(day1["run_id"], "r1-a1-d1-B")
        self.assertIn("not_a_24h_comb_window", day1["reasons"])
        self.assertNotIn("report_not_bound_to_its_declaration", day1["reasons"])
        self.assertNotIn("no_fresh_store_evidence", day1["reasons"])
        self.assertTrue(day1["tests"]["mean_passes"])


# ----------------------------------------------------------------- X-cr-round1-spec-fixes
def repeat_of(day, k, clean=True):
    """A repeat k of `day`: the same layout and exits from fresh stores, its own run IDs;
    by default clean (no events, fresh host samples, valid reports)."""
    again = json.loads(json.dumps(day))
    again["repeat"] = k
    if clean:
        again["archive"]["events"] = []
        again["contention"] = mk.host_samples(again["reports"])
    for r in again["reports"].values():
        if clean:
            r["verdict"] = "valid"
        r["binding"]["placement"]["repeat"] = k
        r["binding"]["run_id"] += f"-repeat{k}"
    return again


def thin_coverage(day, arm="B"):
    """The first 30 min of the arm's window without a steal reading: coverage under 98 %."""
    ws = analyze_cr.parse_utc(day["reports"][arm]["timeline"]["window_start_utc"])
    for smp in day["contention"]["samples"]:
        if ws <= analyze_cr.parse_utc(smp["utc"]) < ws + timedelta(minutes=30):
            smp["steal"] = None


def solo_of(arm, attempt):
    """make_archive's S layout for `arm` instead of B: one arm on X, then Y."""
    later = mk.archive("S", attempt=attempt)
    decl = later["declaration"]
    decl["arms"] = [mk.entry(attempt, e["day"], arm, e["exit"]) for e in decl["arms"]]
    decl["attempts"][0]["candidate_digests"] = {arm: mk.DIGEST}
    for d in later["days"]:
        e = next(x for x in decl["arms"] if x["day"] == d["day"])
        reports = {arm: mk.report(e, f"{attempt}-d{d['day']}-{arm}", mk.DAY_START[d["day"]])}
        d.update(reports=reports, archive=mk.day_archive(attempt, d["day"], reports, {arm: e["exit"]}),
                 contention=mk.host_samples(reports), gluetun_logs={arm: mk.gluetun_log(attempt, d["day"], arm)})
    return later


class SpecFixBase(RoundBase):
    def gate_ends(self, a, ends):
        """gate-round on `a` with the manifest's attempt_ends set to `ends`."""
        self.n += 1
        path = mk.write(self.root / f"e{self.n}", a)
        man = json.loads(path.read_text())
        man["attempt_ends"] = ends
        path.write_text(json.dumps(man))
        return analyze_cr.gate_round(analyze_cr.load_manifest(path))


class SoloBoundTests(SpecFixBase):
    """Item 2: an S attempt only as a solo confirmation (O4) or for the only arm left, at most
    one per candidate per round (CR-SPEC-R1 §5)."""

    def test_a_lone_solo_attempt_is_not_permitted(self):
        v = self.gate(mk.archive("S"))
        self.assertNotJudged(v, "solo_not_permitted")
        self.assertEqual(self.cand(v)["solo_problems"], ["solo_without_a_preceding_attempt"])

    def test_the_reviewers_case_l3_void_then_s_void_then_s_never_passes(self):
        a = mk.archive("L3")
        self.day(a, 1)["reports"]["B"]["verdict"] = "void"
        add_attempt(a, "S", "r1-a2")
        self.day(a, 1, "r1-a2")["reports"]["B"]["verdict"] = "void"
        add_attempt(a, "S", "r1-a3")
        v = self.gate(a)
        self.assertNotJudged(v, "solo_not_permitted")
        self.assertEqual(self.cand(v)["solo_problems"], ["more_than_one_solo_attempt"])
        self.assertNotEqual(v["selection"]["ships"], "B")
        self.assertEqual(v["selection"]["state"], "pending_retry")  # B isn't terminal, so A waits

    def test_a_void_day_then_a_solo_attempt_is_not_permitted(self):
        a = mk.archive("L3")
        self.day(a, 1)["reports"]["B"]["verdict"] = "void"
        v = self.gate(add_attempt(a, "S", "r1-a2"))
        self.assertNotJudged(v, "solo_not_permitted")
        self.assertEqual(self.cand(v)["solo_problems"], ["solo_not_after_a_drop"])

    def test_a_passing_l3_then_a_solo_attempt_is_not_permitted(self):
        v = self.gate(add_attempt(mk.archive("L3"), "S", "r1-a2"))
        self.assertNotJudged(v, "solo_not_permitted")

    def test_a_host_wide_breach_gives_each_candidate_its_own_solo(self):
        # Self-review: "the attempt just before" is the candidate's own. A CPU breach hits B
        # and A alike; B's solo fails, and A's (after B's) is still its one S.
        a = mk.archive("L2")
        for smp in self.day(a, 1)["contention"]["samples"]:
            smp["cpu"] = 0.9
        add_attempt(a, "S", "r1-a2")
        for smp in self.day(a, 1, "r1-a2")["contention"]["samples"]:
            smp["cpu"] = 0.9
        v = self.gate(add_attempt(a, "S", "r1-a3", later=solo_of("A", "r1-a3")))
        self.assertEqual(v["selection"]["trail"][0]["disposition"], "out_of_retries")
        self.assertNotIn("solo_not_permitted", self.cand(v, "A")["reasons"])
        self.assertEqual(self.cand(v, "A")["crawler"], "pass", self.cand(v, "A")["reasons"])
        self.assertEqual(v["selection"]["ships"], "A")

    def test_a_valid_miss_still_stands_beside_a_solo_not_permitted(self):
        a = mk.archive("L3")
        self.day(a, 2)["reports"]["B"]["numerator"]["hourly"][3]["net_per_min"] = 60.0
        v = self.gate(add_attempt(a, "S", "r1-a2"))
        self.assertIn("solo_not_permitted", self.cand(v)["reasons"])
        self.assertEqual(self.cand(v)["crawler"], "fail")

    def product_drop(self, event=True):
        """L2: A fails on day 1 (class 2), the attempt ends; B goes on alone in S."""
        a = mk.archive("L2")
        if event:
            self.day(a, 1)["archive"]["events"] = [{"class": "product", "arms": ["A"], "diagnosis": "pending"}]
        a["days"] = [d for d in a["days"] if d["day"] == 1]
        return self.gate_ends(add_attempt(a, "S", "r1-a2"),
                              {"r1-a1": {"reason": "product_failure:A", "days_run": [1]}})

    def test_the_only_arm_left_after_a_product_failure_goes_solo(self):
        v = self.product_drop()
        self.assertEqual(self.cand(v)["layout"], "S")
        self.assertEqual(self.cand(v)["crawler"], "pass", self.cand(v)["reasons"])

    def test_a_product_drop_the_archive_doesnt_show_is_refused(self):
        self.assertNotJudged(self.product_drop(event=False), "solo_not_permitted")

    def rehearsal_drop(self, reason, days_run=()):
        a = mk.archive("L3")
        a["days"] = [d for d in a["days"] if d["day"] in days_run]
        return self.gate_ends(add_attempt(a, "S", "r1-a2"), {"r1-a1": {"reason": reason, "days_run": list(days_run)}})

    def test_both_others_dropped_at_the_rehearsal(self):
        v = self.rehearsal_drop("rehearsal_drop:A,rehearsal_drop:C")
        self.assertEqual(self.cand(v)["crawler"], "pass", self.cand(v)["reasons"])

    def test_one_other_arm_left_is_no_solo(self):
        self.assertNotJudged(self.rehearsal_drop("rehearsal_drop:C"), "solo_not_permitted")

    def test_a_rehearsal_drop_after_a_day_ran_is_no_drop(self):
        self.assertNotJudged(self.rehearsal_drop("rehearsal_drop:A,rehearsal_drop:C", days_run=(1,)),
                             "solo_not_permitted")

    def test_free_text_is_no_drop(self):
        self.assertNotJudged(self.rehearsal_drop("A and C dropped"), "solo_not_permitted")


class RepeatRuleTests(SpecFixBase):
    """Item 3: a repeat replaces any not_judged day but contention, a product failure and a
    failed solo confirmation, within the budget of 2."""

    def test_a_coverage_miss_repeated_then_passing(self):
        a = mk.archive("L3")
        thin_coverage(self.day(a, 1))
        a["days"].append(repeat_of(self.day(a, 1), 1))
        v = self.gate(a)
        self.assertEqual(self.cand(v)["crawler"], "pass", self.cand(v)["reasons"])
        self.assertEqual([d["reasons"] for d in self.cand(v)["replaced_days"]], [["contention_not_judged"]])

    def test_an_uplink_counter_reset_repeated_then_passing(self):
        a = mk.archive("L3")
        smp = self.day(a, 1)["contention"]["samples"]
        for s_ in smp[len(smp) // 2:]:
            s_["uplink_drops_total"] = -1
        a["days"].append(repeat_of(self.day(a, 1), 1))
        self.assertEqual(self.cand(self.gate(a))["crawler"], "pass")

    def test_a_repeat_of_a_product_failure_day_is_refused_its_own_or_a_peers(self):
        for arm, diagnosis in (("B", "product"), ("A", "pending"), ("C", "product")):
            a = mk.archive("L3")
            self.day(a, 1)["archive"]["events"] = [{"class": "product", "arms": [arm], "diagnosis": diagnosis}]
            a["days"].append(repeat_of(self.day(a, 1), 1))
            self.assertNotJudged(self.gate(a), "repeat_of_a_non_repeatable_day")

    def test_a_repeat_of_an_unclassed_event_day_is_refused(self):
        a = mk.archive("L3")
        self.day(a, 1)["archive"]["events"] = [{"class": "unclassified", "arms": ["B"], "cause": "x"}]
        a["days"].append(repeat_of(self.day(a, 1), 1))
        self.assertNotJudged(self.gate(a), "repeat_of_a_non_repeatable_day")

    def test_a_repeat_of_a_failed_solo_day_is_refused_and_stays_terminal(self):
        a = mk.archive("L3")
        lose_b_probes(self.day(a, 1))
        add_attempt(a, "S", "r1-a2")
        lose_b_probes(self.day(a, 1, "r1-a2"))  # the solo confirmation breaches
        a["days"].append(repeat_of(self.day(a, 1, "r1-a2"), 1))
        v = self.gate(a)
        self.assertNotJudged(v, "repeat_of_a_non_repeatable_day")
        self.assertTrue(self.cand(v)["solo_confirmation_failed"])
        self.assertEqual(v["selection"]["trail"][0]["disposition"], "out_of_retries")

    def test_a_third_repeat_exhausts_the_budget(self):
        a = mk.archive("L3")
        thin_coverage(self.day(a, 1))
        for k in (1, 2):
            again = repeat_of(self.day(a, 1), k, clean=False)  # the miss recurs
            a["days"].append(again)
        a["days"].append(repeat_of(self.day(a, 1), 3))
        v = self.gate(a)
        self.assertNotJudged(v, "retry_budget_exhausted")
        self.assertEqual(v["selection"]["trail"][0]["disposition"], "out_of_retries")

    def test_two_repeats_are_within_the_budget(self):
        a = mk.archive("L3")
        thin_coverage(self.day(a, 1))
        a["days"].append(repeat_of(self.day(a, 1), 1, clean=False))
        a["days"].append(repeat_of(self.day(a, 1), 2))
        self.assertEqual(self.cand(self.gate(a))["crawler"], "pass")


class HandshakeEvidenceTests(SpecFixBase):
    """Self-review: a day needs every declared arm's gluetun log listed, and the archive's own."""

    def test_a_missing_or_other_log_is_not_judged(self):
        for arm in ("B", "C"):
            for broken in ("missing", "other"):
                a = mk.archive("L3")
                d = self.day(a, 2)
                if broken == "missing":
                    del d["gluetun_logs"][arm]
                else:
                    d["gluetun_logs"][arm] += "a later line the archive didn't hash\n"
                self.assertNotJudged(self.gate(a), "handshake_evidence_missing")


class DeclarationChecksPresentTests(SpecFixBase):
    """Item 5: every declaration check present and ok in every arm's report of the day."""

    def test_a_report_without_its_declaration_checks_is_not_judged(self):
        a = mk.archive("L3")
        self.day(a, 1)["reports"]["B"]["checks"] = []  # the entry wasn't found: none was emitted
        self.assertNotJudged(self.gate(a), "declaration_checks_missing")

    def test_each_named_check_is_required(self):
        for name in analyze_cr.DECLARATION_CHECKS:
            a = mk.archive("L3")
            r = self.day(a, 2)["reports"]["B"]
            r["checks"] = [c for c in r["checks"] if c["check"] != name]
            self.assertNotJudged(self.gate(a), "declaration_checks_missing")

    def test_a_peers_missing_or_failed_check_takes_the_day(self):
        for broken in ("missing", "failed"):
            a = mk.archive("L3")
            r = self.day(a, 1)["reports"]["C"]
            if broken == "missing":
                r["checks"] = r["checks"][1:]
            else:
                r["checks"][-1]["ok"] = False
            self.assertNotJudged(self.gate(a), "declaration_checks_missing")

    def test_a_real_segment_report_carries_every_check(self):
        report = analyze_cr.analyze_segment(Round1Sim().write(self.root / "seg-dc"), None, declaration())
        got = {c["check"]: c["ok"] for c in report["checks"]}
        self.assertEqual({n: got.get(n) for n in analyze_cr.DECLARATION_CHECKS},
                         {n: True for n in analyze_cr.DECLARATION_CHECKS})


class ProbeTargetTests(SpecFixBase):
    """Item 4: a day whose archive doesn't show every arm's probe target checked as its
    WireGuard gateway is not judged."""

    def test_an_arm_without_the_gateway_check_is_not_judged(self):
        for arm in ("B", "C"):
            for value in (None, False):
                a = mk.archive("L3")
                info = self.day(a, 1)["archive"]["arms"][arm]
                if value is None:
                    del info["tunnel_target_is_gateway"]
                else:
                    info["tunnel_target_is_gateway"] = value
                self.assertNotJudged(self.gate(a), "probe_target_unchecked")

    def test_gluetun_logs_are_checked_by_hash(self):
        a = mk.archive("L3")
        path = mk.write(self.root / "logs", a)
        log = self.root / "logs" / "r1-a1" / "day1" / "B" / "gluetun.log"
        log.parent.mkdir(parents=True, exist_ok=True)
        log.write_bytes(b"2026-11-28T10:00:00Z INFO [wireguard] handshake [ipv4]\n")
        man = json.loads(path.read_text())
        ref = {"path": "r1-a1/day1/B/gluetun.log", "sha256": hashlib.sha256(log.read_bytes()).hexdigest()}
        man["days"][0]["gluetun_logs"] = {"B": ref}
        path.write_text(json.dumps(man))
        v = analyze_cr.gate_round(analyze_cr.load_manifest(path))
        self.assertIn("r1-a1/day1/B/gluetun.log", v["inputs"])
        log.write_bytes(b"edited\n")
        with self.assertRaises(ValueError):
            analyze_cr.load_manifest(path)


if __name__ == "__main__":
    unittest.main()
