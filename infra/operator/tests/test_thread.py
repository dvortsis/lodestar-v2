import unittest

from tests import helpers  # noqa: F401 - sys.path
import thread


class Thread(unittest.TestCase):
    def test_read_keeps_only_trusted_unedited_comments(self):
        t = "2026-11-11T12:00:00Z"
        page = [{"id": 1, "user": {"id": 111}, "created_at": t, "updated_at": t, "body": "runner"},
                {"id": 2, "user": {"id": 9}, "created_at": t, "updated_at": t, "body": "stranger"},
                {"id": 3, "user": {"id": 555}, "created_at": t, "updated_at": "2026-11-11T12:01:00Z", "body": "edited"},
                {"id": 4, "user": {"id": 41898282}, "created_at": t, "updated_at": t, "body": "fleet"}]
        calls = []
        got = thread.read(700, "111", "555", lambda m, p, b=None: calls.append(p) or page)
        self.assertEqual([c["body"] for c in got], ["runner", "fleet"])
        self.assertEqual(calls, ["/repos/dvortsis/lodestar-v2/issues/700/comments?per_page=100&page=1"])

    def test_post_allows_reports_and_one_line_fleet_commands_only(self):
        self.assertTrue(thread.check_post("operator-report v1\nid: a\n").startswith("operator-report"))
        self.assertEqual(thread.check_post("/fleet status\n"), "/fleet status")
        for body in ("hello", "/fleet status\n/fleet down", "operator-report v1\n" + "x" * 20000,
                     "/fleet " + "x" * 20000, "operator-instruction v1\nid: a"):
            with self.subTest(body=body[:20]), self.assertRaises(SystemExit):
                thread.check_post(body)
