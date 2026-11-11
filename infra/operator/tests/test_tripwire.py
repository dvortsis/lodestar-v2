import io
import unittest
from contextlib import redirect_stdout
from datetime import timedelta

from tests.helpers import T0, FakeGitHub
import tripwire

BOT, NOW = 555, T0
ISO = lambda d: (NOW - d).strftime("%Y-%m-%dT%H:%M:%SZ")  # noqa: E731
DISABLE = ("PUT", "/repos/o/r/actions/workflows/operator-wake.yml/disable")


def env(**over):
    e = {"GITHUB_REPOSITORY": "o/r", "CHANNEL_PR": "700", "OPERATOR_BOT_USER_ID": str(BOT), "SENDER_ID": str(BOT),
         "EVENT_NAME": "pull_request_target", "EVENT_ACTION": "edited", "TARGET_NUMBER": "12",
         "GITHUB_RUN_ID": "9", "GITHUB_RUN_ATTEMPT": "1"}
    e.update(over)
    return e


def run(argv, e, gh):
    with redirect_stdout(io.StringIO()):
        return tripwire.main(argv, env=e, transport=gh, clock=lambda: NOW)


class Trip(unittest.TestCase):
    def test_bot_actions_trip_and_disable(self):
        for over in ({}, {"EVENT_ACTION": "labeled"}, {"EVENT_NAME": "issue_comment", "EVENT_ACTION": "edited", "TARGET_NUMBER": "700"},
                     {"EVENT_NAME": "issue_comment", "EVENT_ACTION": "deleted"},
                     {"EVENT_NAME": "issue_comment", "EVENT_ACTION": "created", "TARGET_NUMBER": "12"},
                     {"EVENT_ACTION": "closed", "TARGET_NUMBER": "700"}):
            with self.subTest(**over):
                gh = FakeGitHub()
                run(["trip"], env(**over), gh)
                self.assertIn(DISABLE, [c[:2] for c in gh.calls])
                self.assertIn("operator-report v1", gh.bodies()[0])
                self.assertIn("kind: escalation", gh.bodies()[0])

    def test_allowed_or_other_senders_do_nothing(self):
        for over in ({"EVENT_NAME": "issue_comment", "EVENT_ACTION": "created", "TARGET_NUMBER": "700"},
                     {"SENDER_ID": "111"}, {"OPERATOR_BOT_USER_ID": ""}, {"SENDER_ID": ""}):
            with self.subTest(**over):
                gh = FakeGitHub()
                run(["trip"], env(**over), gh)
                self.assertEqual(gh.calls, [])

    def test_no_repository_means_no_call(self):
        gh = FakeGitHub()
        with self.assertRaises(SystemExit):
            run(["trip"], env(GITHUB_REPOSITORY=""), gh)
        self.assertEqual(gh.calls, [])

    def test_labels_never_echo_event_text(self):
        gh = FakeGitHub()
        run(["trip"], env(EVENT_ACTION="x; rm -rf /"), gh)
        self.assertNotIn("rm -rf", gh.bodies()[0])


class Sweep(unittest.TestCase):
    def sweep(self, **lists):
        prefixes = {"events": "/repos/o/r/issues/events", "pulls": "/repos/o/r/pulls?", "reviews": "/repos/o/r/pulls/12/reviews",
                    "review_comments": "/repos/o/r/pulls/comments", "comments": "/repos/o/r/issues/comments"}
        gh = FakeGitHub(lists={prefixes[k]: v for k, v in lists.items()})
        run(["sweep"], env(), gh)
        return gh

    def test_clean(self):
        gh = self.sweep(events=[{"actor": {"id": 1}, "event": "labeled", "created_at": ISO(timedelta(minutes=5)), "issue": {"number": 3}}],
                        comments=[{"user": {"id": BOT}, "issue_url": "x/issues/700", "created_at": ISO(timedelta(minutes=1)),
                                   "updated_at": ISO(timedelta(minutes=1))}])
        self.assertEqual(gh.bodies(), [])

    def test_bot_findings_disable(self):
        recent = ISO(timedelta(minutes=5))
        cases = {
            "issue event": {"events": [{"actor": {"id": BOT}, "event": "renamed", "created_at": recent, "issue": {"number": 3}}]},
            "opened PR": {"pulls": [{"user": {"id": BOT}, "number": 12, "updated_at": recent}]},
            "review": {"pulls": [{"user": {"id": 1}, "number": 12, "updated_at": recent}],
                       "reviews": [{"user": {"id": BOT}, "submitted_at": recent}]},
            "review comment": {"review_comments": [{"user": {"id": BOT}, "pull_request_url": "x/pulls/12",
                                                    "created_at": recent, "updated_at": recent}]},
            "comment elsewhere": {"comments": [{"user": {"id": BOT}, "issue_url": "x/issues/12", "created_at": recent,
                                                "updated_at": recent}]},
            "edited own comment": {"comments": [{"user": {"id": BOT}, "issue_url": "x/issues/700", "created_at": ISO(timedelta(minutes=9)),
                                                 "updated_at": recent}]},
        }
        for name, lists in cases.items():
            with self.subTest(name):
                gh = self.sweep(**lists)
                self.assertIn(DISABLE, [c[:2] for c in gh.calls])
                self.assertEqual(len(gh.bodies()), 1)

    def test_unattributed_findings_escalate_without_disabling(self):
        recent = ISO(timedelta(minutes=5))
        for name, lists in {
            "base changed": {"events": [{"actor": {"id": 1}, "event": "base_ref_changed", "created_at": recent, "issue": {"number": 3}}]},
            "review comment edited": {"review_comments": [{"user": {"id": 1}, "pull_request_url": "x/pulls/4",
                                                           "created_at": ISO(timedelta(minutes=30)), "updated_at": recent}]},
        }.items():
            with self.subTest(name):
                gh = self.sweep(**lists)
                self.assertNotIn(DISABLE, [c[:2] for c in gh.calls])
                self.assertEqual(len(gh.bodies()), 1)

    def test_old_activity_is_outside_the_lookback(self):
        old = ISO(timedelta(minutes=tripwire.LOOKBACK_MINUTES + 5))
        gh = self.sweep(events=[{"actor": {"id": BOT}, "event": "renamed", "created_at": old, "issue": {"number": 3}}],
                        pulls=[{"user": {"id": BOT}, "number": 12, "updated_at": old}])
        self.assertEqual(gh.bodies(), [])


class Paging(unittest.TestCase):
    def test_pages_until_the_lookback_then_stops(self):
        recent, old = ISO(timedelta(minutes=5)), ISO(timedelta(minutes=tripwire.LOOKBACK_MINUTES + 5))
        page1 = [{"actor": {"id": 1}, "event": "labeled", "created_at": recent, "issue": {"number": 3}}] * 100
        page2 = [{"actor": {"id": BOT}, "event": "renamed", "created_at": recent, "issue": {"number": 4}}] + \
                [{"actor": {"id": 1}, "event": "labeled", "created_at": old, "issue": {"number": 3}}] * 99
        seen = []

        def gh(method, path, body):
            seen.append(path)
            if path.startswith("/repos/o/r/issues/events"):
                return page1 if path.endswith("&page=1") else page2 if path.endswith("&page=2") else []
            return [] if method == "GET" else None
        with redirect_stdout(io.StringIO()):
            findings, disable = tripwire.sweep(env(), gh, NOW)
        self.assertTrue(disable)  # the bot's event sat on page 2
        self.assertEqual(sum(p.startswith("/repos/o/r/issues/events") for p in seen), 2)

    def test_a_list_past_the_page_limit_is_a_finding(self):
        recent = ISO(timedelta(minutes=5))
        full = [{"actor": {"id": 1}, "event": "labeled", "created_at": recent, "issue": {"number": 3}}] * 100
        gh = FakeGitHub()
        gh_full = lambda m, p, b: (gh.calls.append((m, p, b, None)) or (full if p.startswith("/repos/o/r/issues/events") else []))  # noqa: E731
        run(["sweep"], env(), gh_full)
        self.assertIn("the sweep is incomplete: events ran past 10 pages", gh.bodies()[0])
        self.assertIn(DISABLE, [c[:2] for c in gh.calls])  # fail closed: an unfinished sweep is a trip

    def test_an_api_error_escalates_then_fails_the_job(self):
        posts = []

        def gh(method, path, body):
            if method == "GET":
                raise RuntimeError("GitHub API GET failed")
            posts.append((method, path, body))
        with self.assertRaises(SystemExit) as cm:
            run(["sweep"], env(), gh)
        self.assertEqual(str(cm.exception), "tripwire: the sweep failed")
        self.assertEqual(len(posts), 1)
        self.assertIn("the sweep failed (RuntimeError)", posts[0][2]["body"])
        self.assertNotIn("disable", posts[0][1])


class Disable(unittest.TestCase):
    def test_a_failed_disable_fails_the_job(self):
        def gh(method, path, body):
            if method == "PUT":
                raise RuntimeError("403")
            return None
        with self.assertRaises(SystemExit) as cm:
            run(["trip"], env(), gh)
        self.assertEqual(cm.exception.code, 1)


class Unconfigured(unittest.TestCase):
    def test_an_unset_bot_id_escalates_then_fails_the_job(self):
        gh = FakeGitHub()
        with self.assertRaises(SystemExit) as cm:
            run(["sweep"], env(OPERATOR_BOT_USER_ID=""), gh)
        self.assertEqual(str(cm.exception), "tripwire: OPERATOR_BOT_USER_ID isn't set")
        self.assertEqual(len(gh.bodies()), 1)
        self.assertIn("OPERATOR_BOT_USER_ID isn't set", gh.bodies()[0])
        self.assertNotIn(DISABLE, [c[:2] for c in gh.calls])
        self.assertFalse([c for c in gh.calls if c[0] == "GET"])  # nothing listed without the ID


def review(uid, delta):
    return {"user": {"id": uid}, "submitted_at": None if delta is None else ISO(delta)}


class RestReviews:
    """A fake for the paged REST reviews list (creation order), plus one active PR."""

    def __init__(self, reviews):
        self.reviews, self.pages, self.posts = reviews, [], []

    def __call__(self, method, path, body):
        if method == "GET" and path.startswith("/repos/o/r/pulls/12/reviews"):
            page = int(path.rsplit("page=", 1)[1])
            self.pages.append(page)
            return self.reviews[(page - 1) * 100:page * 100]
        if method == "GET" and path.startswith("/repos/o/r/pulls?") and path.endswith("&page=1"):
            return [{"user": {"id": 1}, "number": 12, "updated_at": ISO(timedelta(minutes=5))}]
        if method == "GET":
            return []
        self.posts.append((method, path, body))
        return None

    def bodies(self):
        return [b["body"] for _, _, b in self.posts if isinstance(b, dict) and "body" in b]

    def disabled(self):
        return any(m == "PUT" and p.endswith("/disable") for m, p, _ in self.posts)


class ReviewsLookback(unittest.TestCase):
    old = timedelta(minutes=tripwire.LOOKBACK_MINUTES + 60)
    new = timedelta(minutes=5)

    def sweep(self, reviews):
        gh = RestReviews(reviews)
        run(["sweep"], env(), gh)
        return gh

    def test_a_recent_bot_review_trips(self):
        gh = self.sweep([review(1, self.old)] * 30 + [review(BOT, self.new)])
        self.assertTrue(gh.disabled())
        self.assertIn("the operator's bot reviewed #12", gh.bodies()[0])

    def test_the_window_edge_is_inside(self):
        edge = timedelta(minutes=tripwire.LOOKBACK_MINUTES)
        self.assertTrue(self.sweep([review(BOT, edge)]).disabled())  # exactly at the start counts
        self.assertTrue(self.sweep([review(BOT, edge - timedelta(seconds=30))]).disabled())
        self.assertFalse(self.sweep([review(BOT, edge + timedelta(seconds=1))]).disabled())

    def test_an_old_bot_review_is_outside_the_lookback(self):
        gh = self.sweep([review(BOT, self.old)] + [review(1, self.new)] * 3)
        self.assertEqual(gh.bodies(), [])

    def test_a_thousand_old_reviews_on_an_updated_pr_dont_trip(self):
        gh = self.sweep([review(1, self.old)] * 1500 + [review(1, self.new)] * 20)
        self.assertEqual(gh.bodies(), [])
        self.assertFalse(gh.disabled())
        self.assertEqual(gh.pages, list(range(1, 17)))  # every page read, none skipped

    def test_a_review_created_early_and_submitted_late_trips(self):
        # Creation order: the bot's pending review sits first, its submission comes later (B1).
        gh = self.sweep([review(BOT, self.new)] + [review(1, self.old)] * 300)
        self.assertTrue(gh.disabled())

    def test_a_bot_review_on_an_earlier_page_trips(self):
        gh = self.sweep([review(BOT, self.new)] + [review(1, self.new)] * 150)
        self.assertTrue(gh.disabled())
        self.assertEqual(gh.pages, [1, 2])

    def test_a_pr_with_5000_reviews_is_a_trip(self):
        self.assertEqual(tripwire.REVIEW_MAX_PAGES * 100, 5000)  # the number the decisions file names
        gh = self.sweep([review(1, self.old)] * 5000)
        self.assertIn("the sweep is incomplete: reviews ran past 50 pages", gh.bodies()[0])
        self.assertTrue(gh.disabled())

    def test_4999_old_reviews_are_complete(self):
        gh = self.sweep([review(1, self.old)] * 4999)
        self.assertEqual(gh.bodies(), [])

    def test_more_than_1000_reviews_in_the_window_trip(self):
        gh = self.sweep([review(1, self.old)] * 200 + [review(1, self.new)] * 1001)
        self.assertIn("the sweep is incomplete: reviews ran past 10 pages", gh.bodies()[0])
        self.assertTrue(gh.disabled())

    def test_1000_reviews_in_the_window_are_complete(self):
        gh = self.sweep([review(1, self.old)] * 200 + [review(1, self.new)] * 1000)
        self.assertEqual(gh.bodies(), [])

    def test_a_bot_review_behind_recent_reviews_trips(self):
        gh = self.sweep([review(1, self.new)] * 150 + [review(BOT, self.new)] + [review(1, self.new)] * 20)
        self.assertTrue(gh.disabled())
        self.assertIn("the operator's bot reviewed #12", gh.bodies()[0])

    def test_undated_or_unzoned_reviews_count_as_recent(self):
        for r in (review(BOT, None), {"user": {"id": BOT}, "submitted_at": "2026-11-11T11:00:00"}):
            with self.subTest(r=r):
                gh = self.sweep([review(1, self.old), r])
                self.assertTrue(gh.disabled())


class ManyPRs(RestReviews):
    """Many active PRs, each with a full list of old reviews."""

    def __init__(self, prs, per_pr):
        super().__init__([])
        self.prs, self.per_pr = prs, per_pr

    def __call__(self, method, path, body):
        if method == "GET" and path.startswith("/repos/o/r/pulls?") and path.endswith("&page=1"):
            return [{"user": {"id": 1}, "number": n, "updated_at": ISO(timedelta(minutes=5))} for n in range(1, self.prs + 1)]
        if method == "GET" and "/reviews" in path:
            self.pages.append(path)
            page = int(path.rsplit("page=", 1)[1])
            old = timedelta(minutes=tripwire.LOOKBACK_MINUTES + 60)
            return [review(1, old)] * max(0, min(100, self.per_pr - (page - 1) * 100))
        return super().__call__(method, path, body)


class ReviewBudget(unittest.TestCase):
    def test_many_prs_of_old_reviews_spend_the_budget_and_trip(self):
        gh = ManyPRs(prs=10, per_pr=4000)  # 41 pages each, 410 in all, past the 200-page budget
        run(["sweep"], env(), gh)
        self.assertIn("the sweep is incomplete: reviews ran past 200 pages", gh.bodies()[0])
        self.assertTrue(gh.disabled())
        self.assertLessEqual(len(gh.pages), tripwire.REVIEW_PAGE_BUDGET)

    def test_within_the_budget_is_complete(self):
        gh = ManyPRs(prs=4, per_pr=4000)  # 164 pages
        run(["sweep"], env(), gh)
        self.assertEqual(gh.bodies(), [])
        self.assertEqual(len(gh.pages), 164)


class FullEvents(RestReviews):
    """Every issue-events page full, its last item with no zone."""

    def __call__(self, method, path, body):
        if method == "GET" and path.startswith("/repos/o/r/issues/events"):
            return [{"actor": {"id": 1}, "event": "labeled", "created_at": "2026-11-11T11:00:00", "issue": {"number": 3}}] * 100
        return super().__call__(method, path, body)


class UnzonedLists(unittest.TestCase):
    def test_an_unzoned_time_keeps_a_list_paging_and_fails_closed(self):
        # A full page whose last item has no zone can't stop the list early.
        gh = FullEvents([])
        run(["sweep"], env(), gh)
        self.assertIn("the sweep is incomplete: events ran past 10 pages", gh.bodies()[0])
        self.assertTrue(gh.disabled())
