import unittest

from hypothesis import given, settings, strategies as st

from tests import helpers  # noqa: F401 - sys.path
import message
from message import INSTRUCTION, REPORT, Refused, parse, report


def inst(*header, body="do it"):
    return "\n".join([INSTRUCTION, *header, "", body])


class Instruction(unittest.TestCase):
    def test_valid(self):
        m = parse(inst("id: run-7", "in-reply-to: q-1", "kind: run", "mode: triage", body=" look \n"))
        self.assertEqual((m.id, m.in_reply_to, m.kind, m.mode, m.body), ("run-7", "q-1", "run", "triage", "look"))
        self.assertEqual(parse(inst("kind: query", "id: a")).mode, "routine")
        self.assertEqual(parse(inst("id: a", "kind: query").replace("\n", "\r\n")).id, "a")

    def test_refusals(self):
        cases = {
            "x": "not an operator instruction",
            "operator-instruction v2\nid: a\nkind: run\n\nb": "not an operator instruction",
            " " + inst("id: a", "kind: run"): "not an operator instruction",
            inst("id:a", "kind: run"): "malformed header",
            inst("id: a b", "kind: run"): "malformed header",
            inst("id: a", "kind: run", "model: x"): "unknown field",
            inst("id: a", "id: b", "kind: run"): "duplicate field",
            inst("kind: run"): "missing field",
            inst("id: a"): "missing field",
            inst("id: A", "kind: run"): "malformed id",
            inst("id: -a", "kind: run"): "malformed id",
            inst("id: " + "a" * 41, "kind: run"): "malformed id",
            inst("id: a", "in-reply-to: B", "kind: run"): "malformed id",
            inst("id: a", "kind: escalation"): "unknown kind",
            inst("id: a", "kind: run", "mode: max"): "unknown mode",
            inst("id: a", "kind: run", body=" \n "): "empty body",
            INSTRUCTION + "\nid: a\nkind: run": "empty body",
            inst("id: a", "kind: run", body="x" * message.MAX_LEN): "too long",
        }
        for text, reason in cases.items():
            with self.subTest(text=text[:50]), self.assertRaises(Refused) as cm:
                parse(text)
            self.assertEqual(cm.exception.reason, reason)

    @settings(max_examples=400, deadline=None)
    @given(st.text(max_size=300))
    def test_any_text_parses_or_refuses_with_a_fixed_reason(self, text):
        for t in (text, INSTRUCTION + "\n" + text):
            try:
                m = parse(t)
            except Refused as e:
                self.assertIn(e.reason, message.REASONS)
            else:
                self.assertTrue(message.ID.match(m.id) and m.body and m.kind in ("run", "query"))


class Report(unittest.TestCase):
    def test_round_trip(self):
        m = parse(report("bridge-42-1", "ping-1", "escalation", "it stopped"), REPORT)
        self.assertEqual((m.id, m.in_reply_to, m.kind, m.body), ("bridge-42-1", "ping-1", "escalation", "it stopped"))

    def test_report_needs_in_reply_to_and_report_kinds(self):
        with self.assertRaises(Refused):
            parse(f"{REPORT}\nid: a\nkind: done\n\nx", REPORT)
        with self.assertRaises(Refused):
            report("a", "b", "run", "x")
        with self.assertRaises(Refused):  # an instruction header isn't a report
            parse(inst("id: a", "kind: run"), REPORT)
