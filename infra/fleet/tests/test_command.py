import unittest

from hypothesis import given, settings, strategies as st

import limits
from command import HEX40, REASONS, Command, Refused, parse

L = limits.load()
P, S = L["presets"], L["steps"]
SHA = "a" * 40


def p(text):
    return parse(text, P, S)


class ParserUnit(unittest.TestCase):
    def test_allow_list(self):
        self.assertEqual(p("/fleet status"), Command("status"))
        self.assertEqual(p("/fleet down\n"), Command("down"))
        self.assertEqual(p("/fleet up screen\r\n"), Command("up", preset="screen"))
        self.assertEqual(p("/fleet run leak-test main"), Command("run", step="leak-test", vm="main"))
        self.assertEqual(p(f"/fleet run start-block worker {SHA}"),
                         Command("run", step="start-block", vm="worker", commit=SHA))

    def test_refusals(self):
        cases = {
            "": "not a /fleet command", "fleet status": "not a /fleet command",
            "/FLEET status": "not a /fleet command", "/fleet": "unknown verb",
            "/fleet extend": "unknown verb", "/fleet  status": "malformed command",
            " /fleet status": "not a /fleet command", "/fleet status ": "malformed command",
            "/fleet status\tx": "malformed command", "/fleet status\n\n": "malformed command",
            "/fleet status\n/fleet down": "malformed command", "/fleet Status": "malformed command",
            "/fleet down now": "unexpected argument", "/fleet up": "unknown preset",
            "/fleet up windows": "unknown preset", "/fleet up screen extra": "unexpected argument",
            "/fleet run leak-test": "malformed command", "/fleet run rm-rf main": "unknown step",
            "/fleet run leak-test soak": "unknown vm", "/fleet run leak-test main " + SHA: "unexpected argument",
            "/fleet run start-block main": "commit argument missing or malformed",
            "/fleet run start-block main " + SHA[:39]: "malformed command",
            "/fleet run start-block main abc": "commit argument missing or malformed",
            "/fleet run start-block main " + SHA.upper(): "malformed command",
            "/fleet run wireguard main": "unknown step", "/fleet up screen;id": "malformed command",
            "/fleet status $(id)": "malformed command", "/fleet stätus": "malformed command",
            "/fleet " + "a" * 300: "malformed command",
        }
        for text, reason in cases.items():
            with self.subTest(text=text):
                with self.assertRaises(Refused) as cm:
                    p(text)
                self.assertEqual(cm.exception.reason, reason)

    def test_wireguard_step_is_never_on_the_list(self):
        self.assertFalse(any("wire" in s or "pia" in s for s in S))


def _strip_eol(text):
    for eol in ("\r\n", "\n"):
        if text.endswith(eol):
            return text[: -len(eol)]
    return text


VALID = st.one_of(
    st.just("/fleet status"), st.just("/fleet down"),
    st.sampled_from(sorted(P)).map(lambda x: f"/fleet up {x}"),
    st.tuples(st.sampled_from(sorted(S)), st.sampled_from(["main", "worker"]),
              st.text("0123456789abcdef", min_size=40, max_size=40)).map(
        lambda t: f"/fleet run {t[0]} {t[1]}" + (f" {t[2]}" if S[t[0]].get("takes_commit") else "")),
)


class ParserProperties(unittest.TestCase):
    @settings(max_examples=2000, deadline=None)
    @given(st.text(max_size=260))
    def test_arbitrary_text_is_refused_or_canonical(self, text):
        try:
            cmd = p(text)
        except Refused as r:
            self.assertIn(r.reason, REASONS)
            return
        self.assertEqual(_strip_eol(text), cmd.canonical())

    @settings(max_examples=1000, deadline=None)
    @given(VALID)
    def test_valid_commands_round_trip(self, text):
        cmd = p(text)
        self.assertEqual(cmd.canonical(), text)
        if cmd.vm is not None:
            self.assertIn(cmd.vm, ("main", "worker"))
        if cmd.commit is not None:
            self.assertTrue(HEX40.match(cmd.commit))

    @settings(max_examples=2000, deadline=None)
    @given(VALID, st.integers(min_value=0, max_value=200), st.characters())
    def test_any_insertion_into_a_valid_command_is_refused_or_still_valid(self, text, at, ch):
        mutated = text[: at % (len(text) + 1)] + ch + text[at % (len(text) + 1):]
        try:
            cmd = p(mutated)
        except Refused:
            return
        # A survivor must itself be an allow-listed command in canonical form.
        self.assertEqual(cmd.canonical(), _strip_eol(mutated))


if __name__ == "__main__":
    unittest.main()
