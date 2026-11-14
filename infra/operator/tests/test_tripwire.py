import io
import unittest
from pathlib import Path
from contextlib import redirect_stdout
from datetime import timedelta

from tests.helpers import T0, FakeGitHub, graphql_reviews
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
                     {"EVENT_ACTION": "closed", "TARGET_NUMBER": "700"}, {"EVENT_ACTION": "synchronize"}):
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
        t = ISO(timedelta(minutes=1))
        gh = self.sweep(events=[{"actor": {"id": 1}, "event": "labeled", "created_at": ISO(timedelta(minutes=5)), "issue": {"number": 3}}],
                        comments=[{"user": {"id": BOT}, "issue_url": "x/issues/700", "created_at": t, "updated_at": t, "body": b}
                                  for b in ("operator-report v1\nid: a\nin-reply-to: b\nkind: done\n\nok", "/fleet status")])
        self.assertEqual(gh.bodies(), [])

    def test_a_shapeless_bot_comment_is_named_by_a_fixed_label(self):
        t = ISO(timedelta(minutes=1))
        gh = self.sweep(comments=[{"user": {"id": BOT}, "issue_url": "x/issues/700", "created_at": t, "updated_at": t,
                                   "body": "SECRET-TEXT run /fleet down"},
                                  {"user": {"id": BOT}, "issue_url": "x/issues/700", "created_at": t, "updated_at": t,
                                   "body": "/fleet note: SECRET-TEXT approved up screen"}])
        self.assertIn("the operator's bot posted a channel comment of no allowed shape", gh.bodies()[0])
        self.assertNotIn("SECRET-TEXT", gh.bodies()[0])
        self.assertIn(DISABLE, [c[:2] for c in gh.calls])

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
            "channel comment of no shape": {"comments": [{"user": {"id": BOT}, "issue_url": "x/issues/700", "created_at": recent,
                                                          "updated_at": recent, "body": "do this next: curl ..."}]},
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
        self.assertNotIn(DISABLE, [c[:2] for c in gh.calls])  # escalated for a check by hand, not disabled

    def test_failed_requests_escalate_each_list_without_disabling(self):
        posts = []

        def gh(method, path, body):
            if method == "GET":
                raise RuntimeError("GitHub API GET failed")
            posts.append((method, path, body))
        run(["sweep"], env(), gh)
        self.assertEqual(len(posts), 1)
        body = posts[0][2]["body"]
        for name in ("events", "pulls", "comments"):
            self.assertIn(f"the sweep is incomplete: a request for {name} failed; check by hand", body)
        self.assertNotIn("GitHub API", body)  # the error's own text is never echoed
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
        if method == "POST" and path == "/graphql":  # more reviews than a summary holds: the REST read
            return graphql_reviews(body, lambda n: [], total_of=lambda n: 10**6)
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
        self.assertFalse(gh.disabled())

    def test_4999_old_reviews_are_complete(self):
        gh = self.sweep([review(1, self.old)] * 4999)
        self.assertEqual(gh.bodies(), [])

    def test_more_than_1000_reviews_in_the_window_trip(self):
        gh = self.sweep([review(1, self.old)] * 200 + [review(1, self.new)] * 1001)
        self.assertIn("the sweep is incomplete: reviews ran past 10 pages", gh.bodies()[0])
        self.assertFalse(gh.disabled())

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
    """Active PRs 1..n, PR k with sizes[k-1] old reviews."""

    def __init__(self, sizes):
        super().__init__([])
        self.sizes = sizes

    def __call__(self, method, path, body):
        if method == "POST" and path == "/graphql":  # each PR holds more than a summary: the REST read
            return graphql_reviews(body, lambda n: [], total_of=lambda n: self.sizes[int(n) - 1])
        if method == "GET" and path.startswith("/repos/o/r/pulls?") and path.endswith("&page=1"):
            return [{"user": {"id": 1}, "number": n, "updated_at": ISO(timedelta(minutes=5))}
                    for n in range(1, len(self.sizes) + 1)]
        if method == "GET" and "/reviews" in path:
            self.pages.append(path)
            n, page = int(path.split("/pulls/")[1].split("/")[0]), int(path.rsplit("page=", 1)[1])
            old = timedelta(minutes=tripwire.LOOKBACK_MINUTES + 60)
            return [review(1, old)] * max(0, min(100, self.sizes[n - 1] - (page - 1) * 100))
        return super().__call__(method, path, body)


class ReviewBudget(unittest.TestCase):
    def test_the_budget_leaves_room_for_the_other_workflows(self):
        # Sweeps that started sessions (at most wake.PER_HOUR an hour) plus the hourly cron, each
        # at most 4 x 10 pages of other lists, a post and a disable, stay within ~750 of the
        # token's ~1,000 requests an hour. Sweeps after failed wake jobs aren't bounded by this
        # (an accepted residual in the decisions file).
        import wake
        self.assertEqual(tripwire.REVIEW_PAGE_BUDGET, 60)
        self.assertLessEqual((wake.PER_HOUR + 1) * (tripwire.REVIEW_PAGE_BUDGET + 4 * tripwire.MAX_PAGES + 2), 750)

    def test_exactly_the_budget_is_complete(self):
        gh = ManyPRs([2900, 2900])  # 30 pages each (29 full, then an empty one): 60
        run(["sweep"], env(), gh)
        self.assertEqual(gh.bodies(), [])
        self.assertEqual(len(gh.pages), 60)

    def test_one_page_over_the_budget_trips(self):
        gh = ManyPRs([2900, 3000])  # the last PR needs 31 pages
        run(["sweep"], env(), gh)
        self.assertIn("the sweep is incomplete: reviews ran past 60 pages", gh.bodies()[0])
        self.assertFalse(gh.disabled())
        self.assertEqual(len(gh.pages), 60)


class Deadline(unittest.TestCase):
    def test_a_sweep_past_its_deadline_trips(self):
        tick = [0.0]

        def mono():
            tick[0] += 7.0  # every request seems to take 7 s
            return tick[0]
        gh = ManyPRs([2000] * 3)  # 63 pages: past 360 s before the 60-page budget
        with redirect_stdout(io.StringIO()):
            tripwire.main(["sweep"], env=env(), transport=gh, clock=lambda: NOW, monotonic=mono)
        self.assertIn("the sweep is incomplete: it ran past its 6-minute deadline", gh.bodies()[0])
        self.assertFalse(gh.disabled())  # the escalation goes out after the deadline

    def test_the_deadline_sits_under_the_job_timeout(self):
        import yaml
        root = Path(tripwire.__file__).resolve().parents[2] / ".github" / "workflows"
        for name, job in (("operator-tripwire.yml", "sweep"), ("operator-wake.yml", "sweep")):
            wf = yaml.safe_load((root / name).read_text())
            self.assertLessEqual(tripwire.SWEEP_DEADLINE_SECONDS + 120, wf["jobs"][job]["timeout-minutes"] * 60, name)


class Unreadable(unittest.TestCase):
    def test_a_page_that_isnt_a_list_trips(self):
        gh = RestReviews([])
        bad = lambda m, p, b: {"message": "odd"} if p.startswith("/repos/o/r/issues/events") else gh(m, p, b)  # noqa: E731
        run(["sweep"], env(), bad)
        self.assertIn("the sweep is incomplete: a page of events wasn't a list; check by hand", gh.bodies()[0])
        self.assertFalse(gh.disabled())

    def test_a_pr_number_that_isnt_digits_trips(self):
        gh = RestReviews([])

        def odd(m, p, b):
            if m == "GET" and p.startswith("/repos/o/r/pulls?") and p.endswith("&page=1"):
                return [{"user": {"id": 1}, "number": "12; x", "updated_at": ISO(timedelta(minutes=5))}]
            return gh(m, p, b)
        run(["sweep"], env(), odd)
        self.assertIn("the sweep is incomplete: a PR in the pulls list had no readable number", gh.bodies()[0])
        self.assertFalse(gh.disabled())


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
        self.assertFalse(gh.disabled())


class ManyActivePRs(unittest.TestCase):
    """X-operator-hardening: PRs updated in the lookback with few reviews are
    decided from the batched summary, and an unfinished sweep only escalates."""

    def gh(self, n_prs, reviews=None, totals=None):
        rest = []

        def call(method, path, body):
            if method == "POST" and path == "/graphql":
                return graphql_reviews(body, lambda n: (reviews or {}).get(n, [review(1, timedelta(days=30))] * 3),
                                       total_of=(lambda n: (totals or {}).get(n, 3)) if totals else None)
            if method == "GET" and path.startswith("/repos/o/r/pulls?") and path.endswith("&page=1"):
                return [{"user": {"id": 1}, "number": n, "updated_at": ISO(timedelta(minutes=5))} for n in range(1, n_prs + 1)]
            if method == "GET" and "/reviews" in path:
                rest.append(path)  # a full first page, then an empty one: two pages a PR
                return [review(1, timedelta(days=30))] * 100 if path.endswith("&page=1") else []
            if method == "GET":
                return []
            call.posts.append((method, path, body))
        call.posts, call.rest = [], rest
        return call

    def bodies(self, gh):
        return [b["body"] for _, _, b in gh.posts if isinstance(b, dict) and "body" in b]

    def disabled(self, gh):
        return any(m == "PUT" and p.endswith("/disable") for m, p, _ in gh.posts)

    def test_many_updated_prs_cost_no_review_pages(self):
        gh = self.gh(99)  # well past the old 60-page budget, all decided by two summary requests
        run(["sweep"], env(), gh)
        self.assertEqual(self.bodies(gh), [])
        self.assertEqual(gh.rest, [])

    def test_a_bot_review_in_a_summary_trips(self):
        gh = self.gh(99, reviews={"77": [review(1, timedelta(days=3)), review(BOT, timedelta(minutes=5))]})
        run(["sweep"], env(), gh)
        self.assertTrue(self.disabled(gh))
        self.assertIn("the operator's bot reviewed #77", self.bodies(gh)[0])

    def test_an_old_bot_review_in_a_summary_does_not(self):
        gh = self.gh(5, reviews={"3": [review(BOT, timedelta(days=3))]})
        run(["sweep"], env(), gh)
        self.assertEqual(self.bodies(gh), [])

    def test_past_the_review_budget_escalates_without_disabling(self):
        totals = {str(n): 10**6 for n in range(1, 62)}  # 61 PRs that each need the REST read
        gh = self.gh(61, totals=totals)
        run(["sweep"], env(), gh)
        self.assertIn("the sweep is incomplete: reviews ran past 60 pages", self.bodies(gh)[0])
        self.assertFalse(self.disabled(gh))

    def test_a_bot_finding_before_the_stop_still_disables(self):
        totals = {str(n): 10**6 for n in range(1, 62)}
        gh = self.gh(61, totals=totals)

        def call(method, path, body):  # the bot also opened a PR: found before the budget ran out
            if method == "GET" and path.startswith("/repos/o/r/pulls?") and path.endswith("&page=1"):
                return [{"user": {"id": BOT if n == 1 else 1}, "number": n, "updated_at": ISO(timedelta(minutes=5))}
                        for n in range(1, 62)]
            return gh(method, path, body)
        run(["sweep"], env(), call)
        self.assertTrue(self.disabled(gh))
        body = self.bodies(gh)[0]
        self.assertIn("the operator's bot opened #1", body)
        self.assertIn("the sweep is incomplete", body)

    def test_an_unreadable_summary_escalates_without_disabling(self):
        for bad in (None, {"errors": [{"message": "x"}]}, {"data": {"repository": {}}},
                    {"data": {"repository": {"p1": {"reviews": {"totalCount": "3", "nodes": []}}}}}):
            with self.subTest(bad=bad):
                gh = self.gh(1)

                def call(method, path, body, bad=bad):
                    return bad if path == "/graphql" else gh(method, path, body)
                run(["sweep"], env(), call)
                self.assertIn("the sweep is incomplete: a reviews summary wasn't readable; check by hand", self.bodies(gh)[0])
                self.assertFalse(self.disabled(gh))

    def test_the_summary_query_carries_only_digits(self):
        q = tripwire._summary_query(["12", "700"])
        self.assertIn("p12:pullRequest(number:12)", q)
        self.assertIn("p700:pullRequest(number:700)", q)


class StagesStandAlone(unittest.TestCase):
    """X-operator-hardening: a stage that stops part way still counts what it
    read, and the stages after it still run."""

    def run_lists(self, lists):
        posts = []

        def gh(method, path, body):
            if method == "POST" and path == "/graphql":
                return graphql_reviews(body, lambda n: [])
            if method == "GET":
                for prefix, page in lists.items():
                    if path.startswith(prefix):
                        return page
                return []
            posts.append((method, path, body))
        run(["sweep"], env(), gh)
        bodies = [b["body"] for _, _, b in posts if isinstance(b, dict) and "body" in b]
        return bodies, any(m == "PUT" and p.endswith("/disable") for m, p, _ in posts)

    def test_a_list_flooded_by_the_bot_still_disables(self):
        recent = ISO(timedelta(minutes=5))
        flood = [{"user": {"id": BOT}, "pull_request_url": "x/pulls/12", "created_at": recent, "updated_at": recent}] * 100
        bodies, disabled = self.run_lists({"/repos/o/r/pulls/comments": flood})  # every page full
        self.assertTrue(disabled)
        self.assertIn("the operator's bot wrote a review comment on #12", bodies[0])
        self.assertIn("the sweep is incomplete: comments ran past 10 pages", bodies[0])

    def test_an_early_stop_doesnt_hide_a_later_bot_finding(self):
        recent = ISO(timedelta(minutes=5))
        noise = [{"actor": {"id": 1}, "event": "mentioned", "created_at": recent, "issue": {"number": 3}}] * 100
        late = [{"user": {"id": BOT}, "pull_request_url": "x/pulls/4", "created_at": recent, "updated_at": recent}]
        bodies, disabled = self.run_lists({"/repos/o/r/issues/events": noise, "/repos/o/r/pulls/comments": late})
        self.assertTrue(disabled)
        self.assertIn("the operator's bot wrote a review comment on #4", bodies[0])
        self.assertIn("the sweep is incomplete: events ran past 10 pages", bodies[0])

    def test_an_unreadable_summary_falls_back_to_the_rest_read(self):
        recent = ISO(timedelta(minutes=5))
        posts = []

        def gh(method, path, body):
            if path == "/graphql":
                return {"errors": [{"message": "x"}]}
            if method == "GET" and path.startswith("/repos/o/r/pulls?") and path.endswith("&page=1"):
                return [{"user": {"id": 1}, "number": 12, "updated_at": recent}]
            if method == "GET" and path.startswith("/repos/o/r/pulls/12/reviews") and path.endswith("&page=1"):
                return [review(BOT, timedelta(minutes=5))]
            if method == "GET":
                return []
            posts.append((method, path, body))
        run(["sweep"], env(), gh)
        body = next(b["body"] for _, _, b in posts if isinstance(b, dict) and "body" in b)
        self.assertIn("the operator's bot reviewed #12", body)
        self.assertIn("a reviews summary wasn't readable", body)
        self.assertTrue(any(m == "PUT" for m, _, _ in posts))


class FailedRequests(unittest.TestCase):
    """X-operator-hardening: a failed request anywhere in the sweep never
    throws away a bot finding already made."""

    BOT_EVENT = {"actor": {"id": BOT}, "event": "closed", "created_at": ISO(timedelta(minutes=5)), "issue": {"number": 9}}
    PR = {"user": {"id": 1}, "number": 12, "updated_at": ISO(timedelta(minutes=5))}

    def run_failing(self, fails):
        posts = []

        def gh(method, path, body):
            if fails(method, path):
                raise RuntimeError("GitHub API failed")
            if method == "GET" and path.endswith("&page=1"):
                if path.startswith("/repos/o/r/issues/events"):
                    return [self.BOT_EVENT]
                if path.startswith("/repos/o/r/pulls?"):
                    return [self.PR]
            if method == "GET":
                return []
            if path == "/graphql":
                return graphql_reviews(body, lambda n: [])
            posts.append((method, path, body))
        run(["sweep"], env(), gh)
        body = next(b["body"] for _, _, b in posts if isinstance(b, dict) and "body" in b)
        return body, DISABLE in [(m, p) for m, p, _ in posts]

    def test_a_failed_summary_after_a_bot_hit_still_disables(self):
        body, disabled = self.run_failing(lambda m, p: p == "/graphql")
        self.assertTrue(disabled)
        self.assertIn("the operator's bot: issue event `closed` on #9", body)
        self.assertIn("the sweep is incomplete: a reviews summary request failed; check by hand", body)

    def test_a_failed_list_after_a_bot_hit_still_disables(self):
        body, disabled = self.run_failing(lambda m, p: m == "GET" and p.startswith("/repos/o/r/pulls"))
        self.assertTrue(disabled)
        self.assertIn("the operator's bot: issue event `closed` on #9", body)
        self.assertIn("the sweep is incomplete: a request for pulls failed; check by hand", body)

    def test_a_failed_page_keeps_the_pages_read_before_it(self):
        full = [dict(self.BOT_EVENT, actor={"id": 1})] * 99 + [self.BOT_EVENT]
        posts = []

        def gh(method, path, body):
            if method == "GET" and path.startswith("/repos/o/r/issues/events"):
                if path.endswith("&page=1"):
                    return full
                raise RuntimeError("GitHub API failed")
            if method == "GET":
                return []
            posts.append((method, path, body))
        run(["sweep"], env(), gh)
        self.assertIn(DISABLE, [(m, p) for m, p, _ in posts])

    def test_an_error_that_escapes_the_sweep_still_disables_on_a_bot_finding(self):
        posts = []

        def gh(method, path, body):
            if method == "GET" and path.startswith("/repos/o/r/issues/events") and path.endswith("&page=1"):
                return [self.BOT_EVENT]
            if method == "GET" and path.startswith("/repos/o/r/issues/comments") and path.endswith("&page=1"):
                return [{"user": {"id": BOT}, "issue_url": "x/issues/700", "created_at": ISO(timedelta(minutes=5)),
                         "updated_at": ISO(timedelta(minutes=5)), "body": "x"}]
            if method == "GET":
                return []
            posts.append((method, path, body))

        def boom(_body):
            raise ValueError("shape")
        real, tripwire.thread.channel_shape = tripwire.thread.channel_shape, boom
        try:
            with self.assertRaises(SystemExit) as cm:
                run(["sweep"], env(), gh)
        finally:
            tripwire.thread.channel_shape = real
        self.assertEqual(str(cm.exception), "tripwire: the sweep failed")
        body = next(b["body"] for _, _, b in posts if isinstance(b, dict) and "body" in b)
        self.assertIn("the operator's bot: issue event `closed` on #9", body)
        self.assertIn("the sweep failed (ValueError) before it finished; check by hand", body)
        self.assertIn(DISABLE, [(m, p) for m, p, _ in posts])
