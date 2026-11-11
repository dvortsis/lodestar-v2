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
                       "reviews": [{"user": {"id": BOT}}]},
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
