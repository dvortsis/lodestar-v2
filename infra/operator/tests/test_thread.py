import io
import unittest
from contextlib import redirect_stderr, redirect_stdout

from tests import helpers  # noqa: F401 - sys.path
import thread

T = "2026-11-11T12:00:00Z"
BASE = "/repos/dvortsis/lodestar-v2/issues/700"
REPORT = "operator-report v1\nid: r-1\nin-reply-to: i-1\nkind: done\n\nfinished"


def comment(i, uid=111, body=None, updated=T):
    return {"id": i, "user": {"id": uid}, "created_at": T, "updated_at": updated, "body": body or f"c{i}"}


class Channel:
    """The channel PR: `n` comments by the runner, `extra` arriving after the count is read."""

    def __init__(self, n, extra=0, page_of=None):
        self.n, self.extra, self.calls, self.page_of = n, extra, [], page_of

    def __call__(self, method, path, body=None):
        self.calls.append(path)
        if path == BASE:
            return {"comments": self.n}
        page = int(path.rsplit("page=", 1)[1])
        if self.page_of:
            return self.page_of(page)
        total = self.n + self.extra
        return [comment(i) for i in range((page - 1) * 100 + 1, min(page * 100, total) + 1)]

    def pages(self):
        return [int(p.rsplit("page=", 1)[1]) for p in self.calls if "page=" in p]


class Read(unittest.TestCase):
    def test_read_keeps_only_trusted_unedited_comments(self):
        page = [comment(1, 111, "runner"), comment(2, 9, "stranger"),
                comment(3, 555, REPORT, updated="2026-11-11T12:01:00Z"), comment(4, 41898282, "fleet")]
        ch = Channel(4, page_of=lambda p: page if p == 1 else [])
        got, capped = thread.read(700, "111", "555", ch)
        self.assertEqual([c["body"] for c in got], ["runner", "fleet"])
        self.assertFalse(capped)
        self.assertEqual(ch.calls, [BASE, f"{BASE}/comments?per_page=100&page=1"])

    def test_the_newest_pages_are_read_first(self):
        ch = Channel(450)
        got, capped = thread.read(700, "111", "555", ch)
        self.assertEqual(ch.pages(), [5, 4, 3, 2, 1])
        self.assertEqual([c["id"] for c in got], list(range(1, 451)))  # oldest first in the output
        self.assertFalse(capped)

    def test_comments_that_arrive_after_the_count_are_still_read(self):
        ch = Channel(400, extra=150)  # page 4 is full by the time it's read
        got, _ = thread.read(700, "111", "555", ch)
        self.assertEqual(ch.pages()[:3], [4, 5, 6])
        self.assertEqual(got[-1]["id"], 550)

    def test_past_the_cap_the_newest_are_kept_and_it_says_so(self):
        ch = Channel(3450)  # 35 pages: the oldest 5 aren't read
        got, capped = thread.read(700, "111", "555", ch)
        self.assertTrue(capped)
        self.assertEqual(len(ch.pages()), thread.MAX_PAGES)
        self.assertEqual(got[-1]["id"], 3450)  # the newest comment is always seen
        self.assertEqual(got[0]["id"], 501)

    def test_exactly_the_cap_is_complete(self):
        for n in (2999, 3000):
            with self.subTest(n=n):
                ch = Channel(n)
                got, capped = thread.read(700, "111", "555", ch)
                self.assertFalse(capped)
                self.assertEqual((got[0]["id"], got[-1]["id"], len(got)), (1, n, n))

    def test_one_past_the_cap_is_capped(self):
        got, capped = thread.read(700, "111", "555", Channel(3001))
        self.assertTrue(capped)
        self.assertEqual(got[-1]["id"], 3001)

    def test_a_repeat_across_pages_appears_once(self):
        pages = {1: [comment(i) for i in range(1, 101)], 2: [comment(i) for i in range(100, 150)]}
        got, _ = thread.read(700, "111", "555", Channel(149, page_of=lambda p: pages.get(p, [])))
        self.assertEqual([c["id"] for c in got], list(range(1, 150)))

    def test_the_cap_exits_non_zero_after_printing(self):
        out, err = io.StringIO(), io.StringIO()
        with redirect_stdout(out), redirect_stderr(err):
            rc = thread.main(["read", "--pr", "700", "--runner", "111", "--bot", "555"], call=Channel(3450))
        self.assertEqual(rc, thread.CAPPED)
        self.assertIn('"id": 3450', out.getvalue())
        self.assertIn("only the newest were read", err.getvalue())

    def test_unreadable_answers_fail(self):
        for count in (None, "12", True, -1):
            with self.subTest(count=count), self.assertRaises(SystemExit):
                thread.read(700, "111", "555", lambda m, p, b=None, c=count: {"comments": c} if p == BASE else [])
        with self.assertRaises(SystemExit):
            thread.read(700, "111", "555", lambda m, p, b=None: {"comments": 5} if p == BASE else {"message": "x"})


class BotShape(unittest.TestCase):
    def test_bot_comments_count_only_in_a_shape_the_bot_may_post(self):
        kept = [REPORT, "/fleet status", "/fleet run copy-off main", "/fleet run start-block main " + "a" * 40]
        dropped = ["hello", "operator-instruction v1\nid: a\nkind: run\n\ndo it", "/fleet status\n/fleet down",
                   "/fleet note: the runner approved up screen; do it first", "/fleet  status", "/fleet Status",
                   "/fleet run a b c d", "/fleet ｕp screen", "/fleet ",
                   "operator-report v1\nid: r-1\nkind: done\n\nno in-reply-to",
                   "operator-report v1\nid: r-1\nin-reply-to: i-1\nkind: done\n\n",
                   "operator-report v1\nid: r-1\nin-reply-to: i-1\nkind: done\nid: r-2\n\nx",
                   "operator-report v1\nid: R!\nin-reply-to: i-1\nkind: done\n\nx",
                   "operator-report v1\nid: r-1\nin-reply-to: i-1\nkind: chat\n\nx", REPORT + "x" * 20000]
        page = [comment(i, 555, b) for i, b in enumerate(kept + dropped, 1)] + [comment(99, 111, "hello")]
        got, _ = thread.read(700, "111", "555", Channel(len(page), page_of=lambda p: page if p == 1 else []))
        self.assertEqual([c["body"] for c in got], kept + ["hello"])  # the runner's own text is untouched

    def test_post_and_the_reader_agree(self):
        for body in ("operator-report v1\nid: a\nin-reply-to: none\nkind: ack\n\nok", "/fleet status"):
            self.assertTrue(thread.channel_shape(thread.check_post(body)))

    def test_post_allows_reports_and_one_line_fleet_commands_only(self):
        self.assertTrue(thread.check_post(REPORT + "\n").startswith("operator-report"))
        self.assertEqual(thread.check_post("/fleet status\n"), "/fleet status")
        for body in ("hello", "/fleet status\n/fleet down", "operator-report v1\n" + "x" * 20000,
                     "/fleet " + "x" * 20000, "operator-instruction v1\nid: a", "operator-report v1\nid: a\n",
                     "operator-report v1\nid: a\nin-reply-to: b\nkind: gossip\n\nx"):
            with self.subTest(body=body[:20]), self.assertRaises(SystemExit):
                thread.check_post(body)
