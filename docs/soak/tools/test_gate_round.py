"""Round 1 (X-cr-round1-gate): `segment --declaration` and `gate-round`.

docs/plan/2026-11-27-revised-path-to-100.md §5.2 (items 2, 4-6, 9-12), §6.1-§6.4 and §8;
the formats are in docs/decisions/2026-11-27-X-cr-round1-gate.md. Every verdict row of
§5.2's fixture table has a test here with its expected verdict (the collect slice owns the
recording, CR-0w first start and leak-test rows).

Segments are written as the collector writes them, by test_analyze_cr's Sim, so the
declaration and bundle checks run through real files. Round archives come from
fixtures/round1/make_archive.py, changed per test.
"""

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
        self.edits.append((lambda e, i: i == 0, lambda row, lines: row["values"].update(torrents=0)))
        self.edits.append((lambda e, i: True, lambda row, lines: row.update(private_bytes=self.anon(row["elapsed_seconds"]))))

    def snapshot(self, e, row):
        lines = super().snapshot(e, row)
        on = self.switches
        out = []
        add = lambda name, value, labels="": out.append(f"lodestar_{name}{labels} {value}")
        ended = math.floor(e * 2)
        values = math.floor(ended * 0.3) if on["lookup_end"] else 0
        for reason, n in (("values", values), ("no_values", ended - values)):
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
        run["image_digest"] = mk.DIGEST
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
        decl = mk.archive("L3")["declaration"]
        for e in decl["arms"]:
            e["image_digest"] = "sha256:" + "9" * 64
        report = self.judge(Round1Sim(), analyze_cr.check_declaration(decl))
        self.assertIn("declared_image_digest", report["verdict_reasons"])

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


class LatencyEdgeTests(unittest.TestCase):
    """§6.4 and CR-SPEC §3: the latency cohort keeps carry-in samples and charges dropped
    samples above 600 s. A hand calculation at the 95 % edge."""

    def scrapes(self, le600, total_count, dropped):
        def snap(scale):
            s = {"discovery_to_searchable_seconds_count": {(): total_count * scale},
                 "discovery_to_searchable_dropped_total": {(): dropped * scale},
                 "discovery_to_searchable_seconds_bucket": {}}
            for e in collect.LATENCY_EDGES:
                v = total_count if e == "+Inf" or (e != "+Inf" and float(e) > 600) else (le600 if float(e) >= 600 else 0)
                s["discovery_to_searchable_seconds_bucket"][(("le", e),)] = v * scale
            return s
        return {"snap": snap(0)}, {"snap": snap(1)}

    def test_exactly_95_percent_within_600s_passes(self):
        # 10,000 samples + 100 dropped = 10,100. 95 % of that is 9,595: 9,595 within 600 s passes.
        a, b = self.scrapes(9595, 10000, 100)
        lat = analyze_cr.latency(a, b)
        self.assertAlmostEqual(lat["coverage"], 10000 / 10100)
        self.assertTrue(lat["passes_p95_600"])

    def test_one_sample_below_the_edge_fails(self):
        a, b = self.scrapes(9594, 10000, 100)  # 9,594 < 9,595: the dropped are charged above 600 s
        self.assertFalse(analyze_cr.latency(a, b)["passes_p95_600"])

    def test_carry_in_samples_are_not_filtered(self):
        # Carry-in samples are in the histogram's count: removing them would raise the share.
        # The cohort is the whole histogram delta, so the result depends on it alone.
        a, b = self.scrapes(9595, 10000, 100)
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
        v = self.gate(mk.archive("S"))
        self.assertEqual(self.cand(v)["crawler"], "pass")

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

    def test_a_candidates_product_failure_is_terminal_and_a_is_read(self):
        a = mk.archive("L3")
        self.day(a, 1)["archive"]["events"] = [{"class": "product", "arms": ["B"]}]
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


class ComparatorTests(RoundBase):
    def test_missing_comparator(self):
        a = mk.archive("L3")
        a["comparator"] = None
        self.assertNotJudged(self.gate(a), "comparator_missing")

    def test_comparator_spacing_violation(self):
        a = mk.archive("L3")
        # Day 1's processes start 11 h after the comparator's window end, its windows 12 h 01 min
        # after; moving the comparator 30 min later puts every day-1 window inside the 12 h.
        t = a["comparator"]["timeline"]
        for k in ("window_end_utc", "last_scrape_utc"):
            t[k] = mk.iso(analyze_cr.parse_utc(t[k]) + timedelta(minutes=30))
        a["comparator"]["binding"]["ended_utc"] = mk.iso(analyze_cr.parse_utc(t["window_end_utc"]) + timedelta(seconds=40))
        v = self.gate(a)
        self.assertNotJudged(v, "spacing")
        self.assertIn("lodestar_window_within_12h_after_comparator", self.cand(v)["spacing_problems"])

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

    def test_missing_host_samples_are_not_judged(self):
        a = mk.archive("L3")
        self.day(a, 1)["contention"] = None
        self.assertNotJudged(self.gate(a), "contention_not_judged")


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
        self.assertEqual(len(v["inputs"]), 1 + 1 + 2 * (3 + 2))

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


if __name__ == "__main__":
    unittest.main()
