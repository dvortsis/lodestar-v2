import io
import unittest
from pathlib import Path
from contextlib import redirect_stdout
from datetime import timedelta

from tests.helpers import T0, FakeGitHub, graphql_reviews, no_runs
import tripwire

BOT, NOW = 555, T0
MONTH = f"created={(T0 - timedelta(days=30)).strftime('%Y-%m-%dT%H:%M:%SZ')}.."  # the in-flight window
ISO = lambda d: (NOW - d).strftime("%Y-%m-%dT%H:%M:%SZ")  # noqa: E731
DISABLE = ("PUT", "/repos/o/r/actions/workflows/operator-wake.yml/disable")


def env(**over):
    e = {"GITHUB_REPOSITORY": "o/r", "CHANNEL_PR": "700", "OPERATOR_BOT_USER_ID": str(BOT), "SENDER_ID": str(BOT),
         "EVENT_NAME": "pull_request_target", "EVENT_ACTION": "edited", "TARGET_NUMBER": "12",
         "GITHUB_RUN_ID": "9", "GITHUB_RUN_ATTEMPT": "1"}
    e.update(over)
    return e


def no_sleep(_seconds):
    raise AssertionError("a test reached a real wait")


def run(argv, e, gh):
    with redirect_stdout(io.StringIO()):
        return tripwire.main(argv, env=e, transport=gh, clock=lambda: NOW, sleep=no_sleep)


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
            return no_runs(path) if method == "GET" else None
        with redirect_stdout(io.StringIO()):
            findings, disable = tripwire.sweep(env(), gh, NOW)
        self.assertTrue(disable)  # the bot's event sat on page 2
        self.assertEqual(sum(p.startswith("/repos/o/r/issues/events") for p in seen), 2)

    def test_a_list_past_the_page_limit_is_a_finding(self):
        recent = ISO(timedelta(minutes=5))
        full = [{"actor": {"id": 1}, "event": "labeled", "created_at": recent, "issue": {"number": 3}}] * 100
        gh = FakeGitHub()
        gh_full = lambda m, p, b: (gh.calls.append((m, p, b, None)) or (full if p.startswith("/repos/o/r/issues/events") else no_runs(p)))  # noqa: E731
        run(["sweep"], env(), gh_full)
        self.assertIn("the sweep is incomplete: events ran past 10 pages", gh.bodies()[0])
        self.assertNotIn(DISABLE, [c[:2] for c in gh.calls])  # escalated for a check by hand, not disabled

    def test_failed_requests_escalate_each_list_and_the_failed_session_check_disables(self):
        posts = []

        def gh(method, path, body):
            if method == "GET":
                raise RuntimeError("GitHub API GET failed")
            posts.append((method, path, body))
        run(["sweep"], env(), gh)
        bodies = [b["body"] for _, _, b in posts if isinstance(b, dict) and "body" in b]
        self.assertEqual(len(bodies), 2)  # the escalation, then the disable's notice
        body = bodies[0]
        for name in ("events", "pulls", "comments"):
            self.assertIn(f"the sweep is incomplete: a request for {name} failed; check by hand", body)
        self.assertNotIn("GitHub API", "".join(bodies))  # the error's own text is never echoed
        self.assertIn(f"{tripwire.SESSION_UNKNOWN}, so an incomplete sweep disables", bodies[1])
        self.assertIn(DISABLE, [(m, p) for m, p, _ in posts])  # the session check fails closed


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
            return no_runs(path)
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
        sweep = tripwire.REVIEW_PAGE_BUDGET + 4 * tripwire.MAX_PAGES + 2
        self.assertLessEqual((wake.PER_HOUR + 1) * sweep, 750)
        # The worst hour: every sweep incomplete at its full page use, then the session check
        # at its request cap. Still inside the token's ~1,000 (decisions file).
        self.assertLessEqual((wake.PER_HOUR + 1) * (sweep + tripwire.SESSION_REQUESTS), 1000)
        # One full pass: two counts, every status list, two counts again, with room for jobs and a retry.
        # Quiet passes: two counts, two counts and the common completed lists, two counts again.
        quiet = 2 + 2 + len(tripwire.DONE_FIRST) + 2
        self.assertGreaterEqual(tripwire.SESSION_REQUESTS, 3 * quiet)

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
        gh = ManyPRs([2000] * 3)  # 63 pages: past 300 s before the 60-page budget
        with redirect_stdout(io.StringIO()):
            tripwire.main(["sweep"], env=env(), transport=gh, clock=lambda: NOW, monotonic=mono, sleep=no_sleep)
        self.assertIn("the sweep is incomplete: it ran past its 5-minute deadline", gh.bodies()[0])
        self.assertIn(tripwire.PENDING, gh.bodies()[0])  # posted before the session check
        # At 7 s a request the check passes its own deadline: that counts as a session.
        self.assertTrue(gh.disabled())
        self.assertIn(f"{tripwire.SESSION_UNKNOWN}, so an incomplete sweep disables", gh.bodies()[1])

    def test_the_deadline_sits_under_the_job_timeout(self):
        import yaml
        root = Path(tripwire.__file__).resolve().parents[2] / ".github" / "workflows"
        for name, job in (("operator-tripwire.yml", "sweep"), ("operator-wake.yml", "sweep")):
            wf = yaml.safe_load((root / name).read_text())
            # Checkout and start-up, then the sweep's deadline with one request past it and the
            # escalation; then the session check's deadline (its retries and gate re-reads
            # inside it) with one request past it and the disable. Each request can take a
            # full timeout. At least 60 s of the job's timeout must be left over.
            t = tripwire.REQUEST_TIMEOUT_SECONDS
            escalated = 60 + tripwire.SWEEP_DEADLINE_SECONDS + t + t
            disabled = escalated + tripwire.SESSION_DEADLINE_SECONDS + t + t
            self.assertGreaterEqual(wf["jobs"][job]["timeout-minutes"] * 60 - disabled, 60, name)
            waits = (tripwire.GATE_READS * tripwire.GATE_WAIT_SECONDS
                     + (tripwire.SESSION_ATTEMPTS - 1) * tripwire.SETTLE_SECONDS
                     + max(tripwire.SETTLE_SECONDS, tripwire.GATE_WAIT_SECONDS))  # a wait just before the deadline
            self.assertLess(waits, tripwire.SESSION_DEADLINE_SECONDS)
        from comments import urllib_transport  # noqa: F401 - the timeout above is comments.py's
        src = (Path(tripwire.__file__).resolve().parents[1] / "fleet" / "comments.py").read_text()
        self.assertIn(f"timeout={tripwire.REQUEST_TIMEOUT_SECONDS})", src)


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
                return no_runs(path)
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

    def test_an_unreadable_summary_is_logged_and_the_rest_read_decides(self):
        for bad in (None, {"errors": [{"message": "x"}]}, {"data": {"repository": {}}},
                    {"data": {"repository": {"p1": {"reviews": {"totalCount": "3", "nodes": []}}}}}):
            with self.subTest(bad=bad):
                gh = self.gh(1)

                def call(method, path, body, bad=bad):
                    return bad if path == "/graphql" else gh(method, path, body)
                out = io.StringIO()
                with redirect_stdout(out):
                    tripwire.main(["sweep"], env=env(), transport=call, clock=lambda: NOW)
                self.assertIn("tripwire: a reviews summary wasn't readable; every PR took the REST read", out.getvalue())
                self.assertEqual(self.bodies(gh), [])  # the REST read was complete: nothing to escalate
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
                return no_runs(path)
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
                return no_runs(path)
            posts.append((method, path, body))
        run(["sweep"], env(), gh)
        body = next(b["body"] for _, _, b in posts if isinstance(b, dict) and "body" in b)
        self.assertIn("the operator's bot reviewed #12", body)
        self.assertNotIn("incomplete", body)  # the REST read was complete
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
                return no_runs(path)
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
        self.assertNotIn("incomplete", body)  # the REST read was complete

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
                return no_runs(path)
            posts.append((method, path, body))
        run(["sweep"], env(), gh)
        self.assertIn(DISABLE, [(m, p) for m, p, _ in posts])

    def test_an_item_that_cant_be_checked_is_a_stop_and_later_items_still_run(self):
        posts = []
        when = ISO(timedelta(minutes=5))

        def gh(method, path, body):
            if method == "GET" and path.startswith("/repos/o/r/issues/comments") and path.endswith("&page=1"):
                return [{"user": {"id": BOT}, "issue_url": "x/issues/700", "created_at": when, "updated_at": when,
                         "body": "x"},
                        {"user": {"id": BOT}, "issue_url": "x/issues/9", "created_at": when, "updated_at": when}]
            if method == "GET":
                return no_runs(path)
            posts.append((method, path, body))

        def boom(_body):
            raise ValueError("shape")
        real, tripwire.thread.channel_shape = tripwire.thread.channel_shape, boom
        try:
            run(["sweep"], env(), gh)
        finally:
            tripwire.thread.channel_shape = real
        body = next(b["body"] for _, _, b in posts if isinstance(b, dict) and "body" in b)
        self.assertIn("the operator's bot commented on #9", body)  # the item after it still ran
        self.assertIn(f"the sweep is incomplete: {tripwire.ITEM_FAILED}; check by hand", body)
        self.assertIn(DISABLE, [(m, p) for m, p, _ in posts])

    def test_an_error_that_escapes_the_sweep_still_disables_on_a_bot_finding(self):
        posts = []

        def gh(method, path, body):
            if method == "GET":
                return no_runs(path)
            posts.append((method, path, body))

        def escapes(env, transport, now, found):
            found["bot"].append("the operator's bot: issue event `closed` on #9")
            raise ValueError("escaped")
        real, tripwire.sweep = tripwire.sweep, escapes
        try:
            with self.assertRaises(SystemExit) as cm:
                run(["sweep"], env(), gh)
        finally:
            tripwire.sweep = real
        self.assertEqual(str(cm.exception), "tripwire: the sweep failed")
        body = next(b["body"] for _, _, b in posts if isinstance(b, dict) and "body" in b)
        self.assertIn("the operator's bot: issue event `closed` on #9", body)
        self.assertIn("the sweep failed (ValueError) before it finished; check by hand", body)
        self.assertNotIn("escaped", body)
        self.assertIn(DISABLE, [(m, p) for m, p, _ in posts])

    def test_an_error_that_escapes_without_a_bot_finding_follows_the_session_rule(self):
        posts = []

        def gh(method, path, body):
            if method == "GET":
                return no_runs(path)  # no wake runs in the lookback
            posts.append((method, path, body))

        def escapes(env, transport, now, found):
            raise ValueError("escaped")
        real, tripwire.sweep = tripwire.sweep, escapes
        try:
            with self.assertRaises(SystemExit):
                run(["sweep"], env(), gh)
        finally:
            tripwire.sweep = real
        self.assertNotIn(DISABLE, [(m, p) for m, p, _ in posts])
        self.assertEqual(sum(isinstance(b, dict) and "body" in b for _, _, b in posts), 1)

class IncompleteRule(unittest.TestCase):
    """The owner's decision (2026-11-14): an incomplete sweep disables only when
    a wake session ran inside the lookback; a bot finding always disables. The
    escalation is posted before the session check, and the disable follows it."""

    NOISE = {"actor": {"id": 1}, "event": "labeled", "created_at": ISO(timedelta(minutes=5)), "issue": {"number": 3}}
    BOT = {"actor": {"id": BOT}, "event": "closed", "created_at": ISO(timedelta(minutes=5)), "issue": {"number": 9}}

    def sweep(self, actions, bot=False, complete=False, **over):
        posts, seen = [], []
        events = ([self.BOT] if bot else []) + ([] if complete else [self.NOISE] * (100 - bot))

        def gh(method, path, body):
            seen.append(path)
            if method == "GET" and "/actions/" in path:
                return actions(path)
            if method == "GET" and path.startswith("/repos/o/r/issues/events"):
                return events  # every page full: incomplete, unless complete
            if method == "GET":
                return []
            posts.append((method, path, body))
        run(["sweep"], env(**over), gh)
        bodies = [b["body"] for _, _, b in posts if isinstance(b, dict) and "body" in b]
        order = [("post" if isinstance(b, dict) else p) for _, p, b in posts]
        return bodies, DISABLE in [(m, p) for m, p, _ in posts], seen, order

    @staticmethod
    def failing(path):
        raise RuntimeError("GitHub API failed")

    def test_no_session_in_the_lookback_escalates_only(self):
        for actions in (Actions([]), Actions([skipped_run(i) for i in range(1, 300)])):
            with self.subTest(runs=len(actions.runs)):
                bodies, disabled, seen, _ = self.sweep(actions)
                self.assertFalse(disabled)
                self.assertEqual(len(bodies), 1)
                self.assertIn("the sweep is incomplete: events ran past 10 pages", bodies[0])
                self.assertIn(tripwire.PENDING, bodies[0])

    def test_a_session_in_the_lookback_disables_after_the_escalation(self):
        for conclusion in ("success", "failure", "cancelled", None):  # None: still running
            with self.subTest(conclusion=conclusion):
                actions = Actions([wake_run(1, conclusion)], {"1": wake_jobs(conclusion)})
                bodies, disabled, _, order = self.sweep(actions)
                self.assertTrue(disabled)
                self.assertEqual(order[0], "post")  # the escalation before the disable
                self.assertIn(DISABLE[1], order)
                self.assertIn(f"{tripwire.SESSION_RAN}, so an incomplete sweep disables", bodies[1])

    def test_a_failed_session_check_disables(self):
        bad = (self.failing, lambda p: None, lambda p: {"total_count": 1, "workflow_runs": [{"id": "x"}]},
               lambda p: {"total_count": "1", "workflow_runs": []})
        for actions in bad:
            with self.subTest(actions=actions):
                bodies, disabled, _, _ = self.sweep(actions)
                self.assertTrue(disabled)
                self.assertIn(f"{tripwire.SESSION_UNKNOWN}, so an incomplete sweep disables", bodies[1])
                self.assertNotIn("GitHub API", "".join(bodies))

    def test_the_end_of_wake_sweep_counts_as_inside_the_lookback(self):
        ref = "o/r/.github/workflows/operator-wake.yml@refs/heads/main"
        bodies, disabled, seen, _ = self.sweep(self.failing, GITHUB_WORKFLOW_REF=ref)
        self.assertTrue(disabled)
        self.assertIn(f"{tripwire.END_OF_WAKE}, so an incomplete sweep disables", bodies[0])
        self.assertFalse(any("/actions/workflows/operator-wake.yml/runs" in p for p in seen))
        tripwire_ref = "o/r/.github/workflows/operator-tripwire.yml@refs/heads/main"
        self.assertFalse(self.sweep(Actions([]), GITHUB_WORKFLOW_REF=tripwire_ref)[1])
        for near in ("x/o/r/.github/workflows/operator-wake.yml@refs/heads/main",
                     "o/r/.github/workflows/operator-wake.yml@refs/heads/main2",
                     "o/r/.github/workflows/operator-wake.yml@refs/heads/other"):
            with self.subTest(ref=near):  # the whole value, not a part of it
                self.assertFalse(self.sweep(Actions([]), GITHUB_WORKFLOW_REF=near)[1])

    def test_a_bot_finding_disables_in_every_case(self):
        for actions in (Actions([]), Actions([wake_run(1)], {"1": wake_jobs()}), self.failing):
            with self.subTest(actions=actions):
                bodies, disabled, seen, _ = self.sweep(actions, bot=True)
                self.assertTrue(disabled)
                self.assertIn("the operator's bot: issue event `closed` on #9", bodies[0])
                self.assertFalse(any("/actions/workflows/" in p and "/disable" not in p for p in seen))

    def test_a_complete_sweep_skips_the_session_check(self):
        bodies, disabled, seen, _ = self.sweep(self.failing, complete=True)
        self.assertEqual(bodies, [])
        self.assertFalse(disabled)
        self.assertFalse(any("/actions/workflows/operator-wake.yml/runs" in p for p in seen))


def skipped_run(i, created=timedelta(hours=1)):
    return {"id": i, "status": "completed", "conclusion": "skipped", "created_at": ISO(created),
            "updated_at": ISO(created)}


def wake_run(i, conclusion="success", created=timedelta(minutes=30), updated=timedelta(minutes=4)):
    status = "in_progress" if conclusion is None else "completed"
    return {"id": i, "status": status, "conclusion": conclusion, "created_at": ISO(created), "updated_at": ISO(updated)}


def wake_jobs(conclusion="success", completed=timedelta(minutes=5), status=None):
    done = None if conclusion is None or completed is None else ISO(completed)
    return [{"name": "gate", "status": "completed", "conclusion": "success", "completed_at": ISO(timedelta(hours=9))},
            {"name": "wake", "status": status or ("in_progress" if conclusion is None else "completed"),
             "conclusion": conclusion, "completed_at": done}]


class Actions:
    """The Actions API's run and job lists for operator-wake.yml, as GitHub
    answers them: `created=A..B` and `status=` applied on the server (a status
    filter matches a run's status, or its conclusion), paged, at most 1,000 runs
    a list, newest first. `ignore` lists statuses whose filter it ignores;
    `cap` caps every total_count at 1,000; `before_call` runs before each answer
    (a run changing state while the check reads)."""

    STATES = ("queued", "in_progress", "pending", "requested", "waiting", "completed")

    def __init__(self, runs, jobs=None, ignore=(), cap=False, before_call=None):
        self.runs, self.jobs, self.ignore, self.paths = runs, jobs or {}, ignore, []
        self.cap, self.before_call = cap, before_call

    def __call__(self, path):
        self.paths.append(path)
        if self.before_call:
            self.before_call(self)
        from urllib.parse import parse_qs, unquote, urlsplit
        q = {k: v[0] for k, v in parse_qs(urlsplit(path).query).items()}
        page, per = int(q.get("page", 1)), int(q.get("per_page", 30))
        if "/actions/runs/" in path:
            js = self.jobs.get(path.split("/actions/runs/")[1].split("/")[0], [])
            return {"total_count": len(js), "jobs": js[(page - 1) * per:page * per]}
        lo, hi = unquote(q["created"]).split("..")
        rs = [r for r in self.runs if lo <= r["created_at"] <= hi]
        st = q.get("status")
        if st and st not in self.ignore:
            rs = [r for r in rs if (r["status"] if st in self.STATES else r.get("conclusion")) == st]
        rs = sorted(rs, key=lambda r: r["created_at"], reverse=True)
        total = min(len(rs), 1000) if self.cap else len(rs)
        return {"total_count": total, "workflow_runs": rs[:1000][(page - 1) * per:page * per]}


class SessionCheck(unittest.TestCase):
    """_wake_session: which operator-wake.yml runs count as a session in the lookback.
    The run and job fields follow GitHub's Actions API. Real operator-wake.yml runs on
    this repository show the shapes the check relies on: a run whose jobs were all
    skipped reports conclusion "skipped", and a run whose wake job waits for the
    cloud-operator approval reports status "waiting", its wake job status "waiting"
    with no completed_at (and a started_at that is only its creation time)."""

    def check(self, actions, mono=None, sleep=None):
        def gh(method, path, body):
            return actions(path)
        return tripwire._wake_session(env(), gh, NOW, mono or (lambda: 0.0), sleep or (lambda s: None))

    # A wake job that would count but for the skipped gate, so the gate rule is what decides.
    STRANGER_JOBS = [{"name": "gate", "status": "completed", "conclusion": "skipped"},
                     {"name": "refuse", "status": "completed", "conclusion": "skipped"},
                     {"name": "wake", "status": "queued", "conclusion": None, "completed_at": None}]

    def test_a_strangers_run_in_flight_never_counts(self):
        # Any comment starts an operator-wake run; for a moment it's in flight.
        for status in ("queued", "in_progress"):
            with self.subTest(status=status):
                r = dict(wake_run(1, None), status=status)
                self.assertIsNone(self.check(Actions([r], {"1": self.STRANGER_JOBS})))

    def test_a_strangers_run_in_flight_on_every_call_doesnt_disable(self):
        n = [100]

        def arrive(actions):  # a new comment's run, in flight, on every call (created after the check began)
            n[0] += 1
            actions.runs.append(dict(wake_run(n[0], None), created_at=ISO(timedelta(seconds=-n[0]))))
            actions.jobs[str(n[0])] = self.STRANGER_JOBS
        posts = []

        def gh(method, path, body):
            if method == "GET" and "/actions/" in path:
                return actions(path)
            noise = {"actor": {"id": 1}, "event": "labeled", "created_at": ISO(timedelta(minutes=5)), "issue": {"number": 3}}
            if method == "GET":
                return [noise] * 100 if path.startswith("/repos/o/r/issues/events") else []
            posts.append((method, path, body))
        in_flight = dict(wake_run(99, None), created_at=ISO(timedelta(seconds=20)))  # in flight throughout
        actions = Actions([skipped_run(i) for i in range(1, 50)] + [in_flight], {"99": self.STRANGER_JOBS},
                          before_call=arrive)
        run(["sweep"], env(), gh)
        self.assertNotIn(DISABLE, [(m, p) for m, p, _ in posts])

    def test_a_run_that_settles_during_the_check_is_read_again(self):
        flip = {"n": 0}

        def settle(actions):  # in flight for the first few calls, then skipped
            flip["n"] += 1
            if flip["n"] == 4:
                actions.runs[0].update(status="completed", conclusion="skipped")
        r = dict(wake_run(1, None), created_at=ISO(timedelta(minutes=1)))
        self.assertIsNone(self.check(Actions([r], {"1": self.STRANGER_JOBS}, before_call=settle)))

    def test_skipped_runs_arriving_during_the_check_change_nothing(self):
        def churn(actions):  # a new comment's run on every call: created after the window's end
            n = len(actions.paths)
            actions.runs.append(dict(skipped_run(1000 + n), created_at=ISO(timedelta(seconds=-n))))
        slept = []
        self.assertIsNone(self.check(Actions([skipped_run(1)], before_call=churn), sleep=slept.append))
        self.assertEqual(slept, [])  # nothing to settle

    def test_the_runners_run_in_flight_waits_for_its_gate(self):
        pending = [{"name": "gate", "status": "in_progress", "conclusion": None}]
        reads = {"n": 0}

        def gate_finishes(actions):
            if any("/actions/runs/1/jobs" in p for p in actions.paths):
                reads["n"] += 1
                if reads["n"] == 2:
                    actions.jobs["1"] = wake_jobs(None, status="in_progress")
        actions = Actions([dict(wake_run(1, None), created_at=ISO(timedelta(minutes=1)))], {"1": pending},
                          before_call=gate_finishes)
        slept = []
        self.assertEqual(self.check(actions, sleep=slept.append), tripwire.SESSION_RAN)
        self.assertTrue(slept)
        # A gate that never finishes inside the reads counts as a session too.
        stuck = Actions([dict(wake_run(1, None), created_at=ISO(timedelta(minutes=1)))], {"1": pending})
        self.assertEqual(self.check(stuck), tripwire.SESSION_UNKNOWN)

    def test_a_refused_instruction_doesnt_count(self):
        refused = [{"name": "gate", "status": "completed", "conclusion": "success"},
                   {"name": "wake", "status": "completed", "conclusion": "skipped"},
                   {"name": "refuse", "status": "completed", "conclusion": "success"}]
        self.assertIsNone(self.check(Actions([wake_run(1)], {"1": refused})))

    def test_capped_totals_are_counted_by_splitting_the_window(self):
        # 2,500 skipped runs over 30 days with every total_count capped at 1,000.
        runs = [skipped_run(i, timedelta(minutes=17 * i)) for i in range(1, 2501)]
        self.assertIsNone(self.check(Actions(runs, cap=True)))
        runs.append(wake_run(9999))
        self.assertEqual(self.check(Actions(runs, {"9999": wake_jobs()}, cap=True)), tripwire.SESSION_RAN)

    def test_thousands_of_skipped_runs_and_one_session(self):
        runs = [skipped_run(i, timedelta(minutes=i)) for i in range(1, 5001)]  # past GitHub's 1,000 a list
        runs.append(wake_run(9999))
        actions = Actions(runs, {"9999": wake_jobs()})
        self.assertEqual(self.check(actions), tripwire.SESSION_RAN)
        self.assertFalse(any("status=skipped&per_page=100" in p for p in actions.paths))  # never listed

    def test_thousands_of_skipped_runs_and_no_session(self):
        actions = Actions([skipped_run(i, timedelta(minutes=i)) for i in range(1, 5001)])
        self.assertIsNone(self.check(actions))
        self.assertLessEqual(len(actions.paths), 19)
        self.assertFalse(any("/actions/runs/" in p for p in actions.paths))

    def test_the_windows(self):
        # Runs in flight: 30 days. Completed runs: from COMPLETED_EXTRA_MINUTES before the lookback.
        actions = Actions([wake_run(1, None)], {"1": self.STRANGER_JOBS})
        self.check(actions)
        end = NOW.strftime('%Y-%m-%dT%H:%M:%SZ')
        month = (NOW - timedelta(days=30)).strftime("%Y-%m-%dT%H:%M:%SZ")
        done = (NOW - timedelta(minutes=tripwire.LOOKBACK_MINUTES + tripwire.COMPLETED_EXTRA_MINUTES)
                ).strftime("%Y-%m-%dT%H:%M:%SZ")
        lists = [p for p in actions.paths if "/actions/workflows/" in p]
        completed = [p for p in lists if any(f"&status={c}&" in p for c in tripwire.DONE_FIRST + ("skipped",))
                     or (f"created={done}" in p)]
        self.assertTrue(completed)
        self.assertTrue(all(f"created={done}..{end}&" in p for p in completed))
        self.assertTrue(all(f"created={month}..{end}&" in p for p in lists if p not in completed))
        self.assertTrue(any("&status=in_progress&per_page=100" in p for p in lists))

    def test_a_late_approval_still_counts(self):
        # Still in flight: read however long it waited (up to 30 days).
        actions = Actions([dict(wake_run(1, None), created_at=ISO(timedelta(days=9)))], {"1": wake_jobs(None)})
        self.assertEqual(self.check(actions), tripwire.SESSION_RAN)
        # Completed: read when created inside COMPLETED_EXTRA_MINUTES before the lookback.
        actions = Actions([wake_run(1, created=timedelta(hours=4))], {"1": wake_jobs()})
        self.assertEqual(self.check(actions), tripwire.SESSION_RAN)
        # Completed and older than the window: the wake refuses to start a session for so old a
        # run (tripwire.wake_run_too_old, test_wake), so none can be there to miss.
        self.assertIsNone(self.check(Actions([wake_run(1, created=timedelta(days=9))], {"1": wake_jobs()})))

    def test_a_job_waiting_for_approval_doesnt_count_yet(self):
        run_ = {"id": 1, "status": "waiting", "conclusion": None, "created_at": ISO(timedelta(hours=4)),
                "updated_at": ISO(timedelta(hours=4))}
        actions = Actions([run_], {"1": wake_jobs(None, completed=None, status="waiting")})
        self.assertIsNone(self.check(actions))
        self.assertTrue(any("/actions/runs/1/jobs" in p for p in actions.paths))  # waiting runs' jobs are read

    @staticmethod
    def rerun_waiting(earlier_done):
        """A run re-run after a session: attempt 1's gate and wake completed,
        attempt 2's gate passed and its wake waits for approval."""
        run_ = {"id": 1, "status": "waiting", "conclusion": None, "created_at": ISO(timedelta(hours=6)),
                "updated_at": ISO(timedelta(minutes=2))}
        jobs = [{"name": "gate", "run_attempt": 1, "status": "completed", "conclusion": "success",
                 "completed_at": ISO(timedelta(hours=6))},
                {"name": "wake", "run_attempt": 1, "status": "completed", "conclusion": "success",
                 "completed_at": ISO(earlier_done)},
                {"name": "gate", "run_attempt": 2, "status": "completed", "conclusion": "success",
                 "completed_at": ISO(timedelta(minutes=2))},
                {"name": "wake", "run_attempt": 2, "status": "waiting", "conclusion": None, "completed_at": None}]
        return Actions([run_], {"1": jobs})

    def test_a_rerun_waiting_for_approval_still_shows_the_earlier_session(self):
        self.assertEqual(self.check(self.rerun_waiting(timedelta(minutes=20))), tripwire.SESSION_RAN)
        # Its earlier session ended before the lookback, and the waiting wake job isn't one yet.
        old = timedelta(minutes=tripwire.LOOKBACK_MINUTES + 5)
        self.assertIsNone(self.check(self.rerun_waiting(old)))

    def test_the_first_session_found_ends_the_check(self):
        actions = Actions([dict(wake_run(1, None), status="in_progress")], {"1": wake_jobs(None)})
        self.assertEqual(self.check(actions), tripwire.SESSION_RAN)
        lists = [p for p in actions.paths if "/actions/workflows/" in p and "&per_page=100&" in p]
        self.assertEqual(len(lists), 1)  # in_progress is the first list; nothing after the match is read
        self.assertTrue(all("&event=issue_comment" in p for p in actions.paths if "/actions/workflows/" in p))

    def test_no_run_in_flight_costs_no_in_flight_list(self):
        actions = Actions([skipped_run(i) for i in range(1, 50)])
        self.assertIsNone(self.check(actions))
        self.assertEqual(len(actions.paths), 9)  # counts, the common completed lists, counts again

    def test_a_run_settling_between_attempts_is_read_again_after_a_wait(self):
        # A comment's run is in flight at the first count, then skipped before it's listed.
        r = dict(wake_run(1, None), created_at=ISO(timedelta(seconds=5)))
        actions = Actions([r], {"1": self.STRANGER_JOBS})
        plain = actions.__call__
        slept = []

        def answer(path):
            out = plain(path)
            if len(actions.paths) == 2:  # right after the first count
                r.update(status="completed", conclusion="skipped")
            return out
        self.assertIsNone(self.check(answer, sleep=slept.append))
        self.assertEqual(slept, [tripwire.SETTLE_SECONDS])  # one wait, then the counts agree

    def test_a_mismatch_then_a_match_after_the_settle_wait(self):
        # Listed in flight, then skipped before the last count: the counts disagree once.
        r = dict(wake_run(1, None), created_at=ISO(timedelta(seconds=5)))
        done = wake_run(2, created=timedelta(minutes=20), updated=timedelta(minutes=10))  # the runner's, refused
        refused = [{"name": "gate", "status": "completed", "conclusion": "success"},
                   {"name": "wake", "status": "completed", "conclusion": "skipped"}]
        actions = Actions([r, done], {"1": self.STRANGER_JOBS, "2": refused})
        plain = actions.__call__

        def answer(path):
            out = plain(path)
            if "/actions/runs/1/jobs" in path:
                r.update(status="completed", conclusion="skipped")
            return out
        slept = []
        self.assertIsNone(self.check(answer, sleep=slept.append))
        self.assertEqual(slept, [tripwire.SETTLE_SECONDS])
        self.assertGreater(sum("&status=in_progress&per_page=100" in p for p in actions.paths), 0)
        # Two attempts, but the completed run's jobs were read once.
        self.assertEqual(sum("/actions/runs/2/jobs" in p for p in actions.paths), 1)

    def test_a_final_mismatch_of_others_runs_is_no_session(self):
        # Every listed run in flight has a gate that finished without success; the counts keep
        # disagreeing because each attempt's last count no longer sees them.
        r = dict(wake_run(1, None), created_at=ISO(timedelta(seconds=5)))
        actions = Actions([r], {"1": self.STRANGER_JOBS})
        plain = actions.__call__
        state = {"n": 0}

        def answer(path):
            if "&status=completed&per_page=1&" in path and MONTH in path:  # the in-flight counts: flip each time
                state["n"] += 1
                r.update(status="in_progress" if state["n"] % 2 else "completed",
                         conclusion=None if state["n"] % 2 else "skipped")
            return plain(path)
        slept = []
        self.assertIsNone(self.check(answer, sleep=slept.append))
        self.assertEqual(slept, [tripwire.SETTLE_SECONDS] * (tripwire.SESSION_ATTEMPTS - 1))

    def test_any_other_final_mismatch_stays_unknown(self):
        # A run in flight in a status no list covers: the latest count leaves it unlisted.
        # (The request cap is lifted so the settle rule, not the cap, decides.)
        odd = dict(wake_run(1, None), status="something-new")
        cap, tripwire.SESSION_REQUESTS = tripwire.SESSION_REQUESTS, 1000
        try:
            self.assertEqual(self.check(Actions([odd], {"1": self.STRANGER_JOBS})), tripwire.SESSION_UNKNOWN)
        finally:
            tripwire.SESSION_REQUESTS = cap
        # The runner's run settling: its gate succeeded, so it isn't someone else's.
        r = dict(wake_run(2, None), created_at=ISO(timedelta(seconds=5)))
        jobs = [{"name": "gate", "status": "completed", "conclusion": "success"},
                {"name": "wake", "status": "completed", "conclusion": "skipped"}]
        actions = Actions([r], {"2": jobs})
        plain = actions.__call__
        state = {"n": 0}

        def answer(path):
            if "&status=completed&per_page=1&" in path and MONTH in path:
                state["n"] += 1
                r.update(status="in_progress" if state["n"] % 2 else "completed",
                         conclusion=None if state["n"] % 2 else "success")
            return plain(path)
        self.assertEqual(self.check(answer), tripwire.SESSION_UNKNOWN)

    def test_a_run_listed_in_a_state_it_has_left_is_read_again(self):
        # Listed as in progress, but the copy returned already says completed long ago: read
        # as it says, the runner's session in flight would be missed. It's read again.
        r = dict(wake_run(1, None), created_at=ISO(timedelta(minutes=1)))
        stale = {"left": True}
        actions = Actions([r], {"1": wake_jobs(None, status="in_progress")})

        def answer(path):
            out = actions(path)
            if stale["left"] and "&status=in_progress&per_page=100" in path and out["workflow_runs"]:
                stale["left"] = False
                old = ISO(timedelta(minutes=tripwire.LOOKBACK_MINUTES + 5))
                out = dict(out, workflow_runs=[dict(out["workflow_runs"][0], status="completed",
                                                    conclusion="success", updated_at=old)])
            return out
        self.assertEqual(self.check(answer), tripwire.SESSION_RAN)
        self.assertFalse(stale["left"])

    def test_a_real_total_over_a_thousand_isnt_split(self):
        actions = Actions([skipped_run(i, timedelta(minutes=i)) for i in range(1, 5001)])
        self.assertIsNone(self.check(actions))
        self.assertLessEqual(len(actions.paths), 19)

    def test_a_capped_total_is_split_so_an_unlisted_state_still_shows(self):
        runs = [skipped_run(i, timedelta(minutes=17 * i)) for i in range(1, 2501)]
        runs.append(dict(skipped_run(9999, timedelta(minutes=50)), conclusion="something-new"))
        self.assertEqual(self.check(Actions(runs, cap=True)), tripwire.SESSION_UNKNOWN)

    def runner_runs(self, n):
        """n of the runner's channel comments in the lookback: gate passed, wake skipped."""
        runs = [wake_run(i, created=timedelta(minutes=2 * i), updated=timedelta(minutes=2 * i)) for i in range(1, n + 1)]
        jobs = [{"name": "gate", "status": "completed", "conclusion": "success"},
                {"name": "wake", "status": "completed", "conclusion": "skipped"}]
        return Actions(runs, {str(i): jobs for i in range(1, n + 1)})

    def test_thirty_runner_comments_in_the_lookback_fit_the_request_budget(self):
        actions = self.runner_runs(30)
        self.assertIsNone(self.check(actions))
        self.assertLessEqual(len(actions.paths), tripwire.SESSION_REQUESTS)

    def test_the_check_stops_at_its_request_cap(self):
        actions = self.runner_runs(tripwire.SESSION_REQUESTS)
        self.assertEqual(self.check(actions), tripwire.SESSION_UNKNOWN)
        self.assertLessEqual(len(actions.paths), tripwire.SESSION_REQUESTS)

    def test_old_successes_in_the_window_cost_no_jobs_reads(self):
        # 900 completed runs inside the 72-hour window, all updated before the lookback.
        old = timedelta(minutes=tripwire.LOOKBACK_MINUTES + 5)
        runs = [wake_run(i, created=timedelta(hours=3) + timedelta(minutes=4 * i), updated=old) for i in range(1, 901)]
        actions = Actions(runs, {str(i): wake_jobs() for i in range(1, 901)})
        self.assertIsNone(self.check(actions))
        self.assertFalse(any("/actions/runs/" in p for p in actions.paths))
        self.assertLessEqual(len(actions.paths), 9 + 8)  # nine pages of successes past the first

    def test_the_request_budget_at_three_days_of_runs_with_a_settle_retry(self):
        # 300 completed runs in the 72-hour window, 12 of the runner's comments in the lookback,
        # and a comment's run in flight that settles, costing one retry.
        old = timedelta(minutes=tripwire.LOOKBACK_MINUTES + 5)
        runs = [wake_run(i, created=timedelta(hours=3) + timedelta(minutes=13 * i), updated=old) for i in range(1, 301)]
        mine = [{"name": "gate", "status": "completed", "conclusion": "success"},
                {"name": "wake", "status": "completed", "conclusion": "skipped"}]
        jobs = {}
        for i in range(1000, 1012):
            runs.append(wake_run(i, created=timedelta(minutes=i - 990), updated=timedelta(minutes=i - 991)))
            jobs[str(i)] = mine
        r = dict(wake_run(1, None), id=5000, created_at=ISO(timedelta(seconds=5)))
        runs.append(r)
        jobs["5000"] = self.STRANGER_JOBS
        actions = Actions(runs, jobs)
        plain = actions.__call__

        def answer(path):
            out = plain(path)
            if "/actions/runs/5000/jobs" in path:
                r.update(status="completed", conclusion="skipped")
            return out
        slept = []
        self.assertIsNone(self.check(answer, sleep=slept.append))
        self.assertEqual(slept, [tripwire.SETTLE_SECONDS])  # one retry
        self.assertLessEqual(len(actions.paths), tripwire.SESSION_REQUESTS)

    def test_a_completed_run_missing_at_first_is_found_on_the_next_attempt(self):
        actions = Actions([wake_run(1)], {"1": wake_jobs()})
        plain = actions.__call__
        state = {"hidden": True}

        def answer(path):
            out = plain(path)
            if state["hidden"] and "&status=success&per_page=100" in path:
                state["hidden"] = False
                return {"total_count": 0, "workflow_runs": []}  # an index that hasn't caught up yet
            return out
        slept = []
        self.assertEqual(self.check(answer, sleep=slept.append), tripwire.SESSION_RAN)
        self.assertEqual(slept, [tripwire.SETTLE_SECONDS])

    def test_a_count_below_zero_is_never_settled_on(self):
        actions = Actions([skipped_run(1)])
        plain = actions.__call__

        def answer(path):
            out = plain(path)
            if "&status=completed&per_page=1&" in path:
                return dict(out, total_count=out["total_count"] + 1)  # more completed than all runs
            return out
        self.assertEqual(self.check(answer), tripwire.SESSION_UNKNOWN)

    def test_a_queued_or_running_wake_job_counts(self):
        for status in ("queued", "in_progress"):
            with self.subTest(status=status):
                r = dict(wake_run(1, None), status=status)
                self.assertEqual(self.check(Actions([r], {"1": wake_jobs(None, status=status)})), tripwire.SESSION_RAN)

    def test_a_wake_job_that_completed_before_the_lookback_doesnt_count(self):
        old = timedelta(minutes=tripwire.LOOKBACK_MINUTES + 1)
        self.assertIsNone(self.check(Actions([wake_run(1, updated=timedelta(minutes=1))], {"1": wake_jobs(completed=old)})))

    def test_a_run_completed_before_the_lookback_costs_no_jobs_request(self):
        old = timedelta(minutes=tripwire.LOOKBACK_MINUTES + 1)
        actions = Actions([wake_run(1, created=timedelta(hours=5), updated=old)], {"1": wake_jobs()})
        self.assertIsNone(self.check(actions))
        self.assertFalse(any("/actions/runs/" in p for p in actions.paths))

    def test_counts_that_dont_add_up_count_as_a_session(self):
        # A run in flight in a status no list covers (as if GitHub added one).
        odd = dict(wake_run(1, None), status="something-new")
        self.assertEqual(self.check(Actions([odd])), tripwire.SESSION_UNKNOWN)
        # A status filter the API ignores: the list returns runs in other states.
        runs = [dict(wake_run(1, None)), wake_run(2, updated=timedelta(hours=4))]
        self.assertEqual(self.check(Actions(runs, {"1": self.STRANGER_JOBS}, ignore=("in_progress",))),
                         tripwire.SESSION_UNKNOWN)
        # A conclusion filter the API ignores: the success list returns a skipped run too.
        runs = [wake_run(1, updated=timedelta(hours=4)), skipped_run(2)]
        self.assertEqual(self.check(Actions(runs, ignore=("success",))), tripwire.SESSION_UNKNOWN)

    def test_completed_runs_by_conclusion(self):
        # A rare conclusion is listed once the counts say a completed run is still unread.
        r = dict(wake_run(1), conclusion="timed_out")
        actions = Actions([r], {"1": wake_jobs()})
        self.assertEqual(self.check(actions), tripwire.SESSION_RAN)
        self.assertTrue(any("&status=timed_out&per_page=100" in p for p in actions.paths))
        # An unfamiliar one no list covers: the counts don't add up, so it fails closed.
        self.assertEqual(self.check(Actions([dict(wake_run(1), conclusion="something-new")], {"1": wake_jobs()})),
                         tripwire.SESSION_UNKNOWN)
        # The rare lists aren't read while the counts add up.
        quiet = Actions([wake_run(1, updated=timedelta(hours=4))])
        self.assertIsNone(self.check(quiet))
        self.assertFalse(any("&status=timed_out" in p for p in quiet.paths))

    def test_any_number_of_skipped_runs_costs_nothing(self):
        runs = [skipped_run(i, timedelta(seconds=5 * i)) for i in range(1, 2501)]  # inside the completed window
        actions = Actions(runs)
        self.assertIsNone(self.check(actions))
        self.assertLessEqual(len(actions.paths), 9)

    def test_more_runs_in_one_list_than_it_holds_counts_as_a_session(self):
        old = timedelta(minutes=tripwire.LOOKBACK_MINUTES + 5)
        runs = [wake_run(i, created=timedelta(seconds=10 * i), updated=old) for i in range(1, 1102)]
        self.assertEqual(self.check(Actions(runs)), tripwire.SESSION_UNKNOWN)

    def test_the_check_stops_at_its_own_deadline(self):
        tick = [0.0]

        def mono():
            tick[0] += 10.0  # every request seems to take 10 s
            return tick[0]
        actions = Actions([skipped_run(1)])
        self.assertEqual(self.check(actions, mono), tripwire.SESSION_UNKNOWN)
        self.assertLessEqual(len(actions.paths), tripwire.SESSION_DEADLINE_SECONDS // 10)


class SilentFailure(unittest.TestCase):
    """A failed escalation post with nothing disabled fails the job: never a
    green, silent run."""

    NOISE = {"actor": {"id": 1}, "event": "labeled", "created_at": ISO(timedelta(minutes=5)), "issue": {"number": 3}}

    def sweep(self, events, actions):
        calls = []

        def gh(method, path, body):
            calls.append((method, path))
            if method == "POST":
                raise RuntimeError("post failed")
            if method == "GET" and "/actions/" in path:
                return actions(path)
            if method == "GET":
                return events if path.startswith("/repos/o/r/issues/events") else []
        return gh, calls

    def test_an_incomplete_sweep_with_no_session_and_no_post_fails_the_job(self):
        gh, calls = self.sweep([self.NOISE] * 100, Actions([]))
        with self.assertRaises(SystemExit) as cm:
            run(["sweep"], env(), gh)
        self.assertIn("couldn't be posted", str(cm.exception))
        self.assertNotIn(DISABLE, calls)

    def test_a_finding_with_no_post_fails_the_job(self):
        edited = [{"actor": {"id": 1}, "event": "base_ref_changed", "created_at": ISO(timedelta(minutes=5)),
                   "issue": {"number": 3}}]
        gh, calls = self.sweep(edited, Actions([]))
        with self.assertRaises(SystemExit):
            run(["sweep"], env(), gh)

    def test_a_disable_without_a_post_still_passes(self):
        bot = [{"actor": {"id": BOT}, "event": "closed", "created_at": ISO(timedelta(minutes=5)), "issue": {"number": 9}}]
        gh, calls = self.sweep(bot, Actions([]))
        self.assertEqual(run(["sweep"], env(), gh), 0)
        self.assertIn(DISABLE, calls)
