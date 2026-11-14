import io
import json
import os
import subprocess
import sys
import unittest

import limits
from command import Command, Refused
from gates import Gated, gate, stop_engaged
from reader import CONTROL_PLANE_USER_ID, allowed_ids, trusted
from tests.helpers import event_env

L = limits.load()
HERE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


class Gates(unittest.TestCase):
    def test_allowed(self):
        self.assertEqual(gate(event_env("/fleet up screen"), L), Command("up", preset="screen"))

    def test_event_gates(self):
        bad = [
            {"EVENT_NAME": "pull_request_target"}, {"EVENT_ACTION": "edited"}, {"EVENT_ACTION": "deleted"},
            {"IS_PR": "false"}, {"ISSUE_NUMBER": "701"}, {"CHANNEL_PR": ""}, {"CHANNEL_PR": "700 "},
            {"AUTHOR_ID": "556"}, {"OPERATOR_BOT_USER_ID": ""}, {"AUTHOR_ID": "", "OPERATOR_BOT_USER_ID": ""},
            {"AUTHOR_ID": "lodestar-operator[bot]", "OPERATOR_BOT_USER_ID": "lodestar-operator[bot]"},
            {"EVENT_NAME": "workflow_dispatch"}, {"EVENT_NAME": "schedule"},
            {"AUTHOR_ID": "\u0665\u0665\u0665", "OPERATOR_BOT_USER_ID": "\u0665\u0665\u0665"},
            {"GITHUB_RUN_ATTEMPT": "2"}, {"GITHUB_RUN_ATTEMPT": ""},  # a re-run replays the event
        ]
        for over in bad:
            with self.subTest(over=over), self.assertRaises(Gated):
                gate(event_env("/fleet down", **over), L)

    def test_stop_exempts_only_down(self):
        for stop in ("true", "", "TRUE", "yes"):
            env = event_env("/fleet up screen", OPERATOR_STOP=stop)
            with self.subTest(stop=stop), self.assertRaises(Refused):
                gate(env, L)
            self.assertEqual(gate(event_env("/fleet down", OPERATOR_STOP=stop), L), Command("down"))
        self.assertFalse(stop_engaged({"OPERATOR_STOP": "false"}))

    def test_parser_refusal_after_event_gates(self):
        with self.assertRaises(Refused):
            gate(event_env("/fleet extend"), L)
        with self.assertRaises(Gated):  # a stranger's bad command isn't even parsed
            gate(event_env("/fleet extend", AUTHOR_ID="1"), L)


def c(uid, created="2026-11-08T20:00:00Z", updated=None, body="x", cid=1):
    return {"id": cid, "user": {"id": uid}, "created_at": created, "updated_at": updated or created, "body": body}


class Reader(unittest.TestCase):
    ENV = {"RUNNER_USER_ID": "100", "OPERATOR_BOT_USER_ID": "200"}

    def test_allowed_ids(self):
        self.assertEqual(allowed_ids(self.ENV), {"100", "200", CONTROL_PLANE_USER_ID})
        self.assertEqual(allowed_ids({"RUNNER_USER_ID": "dvortsis"}), {CONTROL_PLANE_USER_ID})

    def test_filters(self):
        ok = allowed_ids(self.ENV)
        comments = [
            c(100, cid=1), c(200, cid=2), c(int(CONTROL_PLANE_USER_ID), cid=3),
            c(300, cid=4),                                   # a stranger
            c(100, updated="2026-11-08T20:00:01Z", cid=5),   # edited
            c("100", cid=6),                                 # id as a string
            c(True, cid=7), {"id": 8, "user": None}, "junk",
            {"id": 9, "user": {"id": 100}, "created_at": "", "updated_at": "", "body": "x"},
            c(100, body=None, cid=10),
        ]
        self.assertEqual([x["id"] for x in trusted(comments, ok)], [1, 2, 3])
        self.assertEqual(set(trusted([c(100)], ok)[0]), {"id", "author_id", "created_at", "body"})

    def test_cli_pages(self):
        pages = [[c(100, cid=1), c(300, cid=2)], [c(200, body="/fleet status", cid=3)]]
        env = dict(os.environ, **self.ENV)
        r = subprocess.run([sys.executable, os.path.join(HERE, "reader.py")], input=json.dumps(pages),
                           capture_output=True, text=True, env=env, check=True)
        self.assertEqual([x["id"] for x in json.loads(r.stdout)], [1, 3])

    def test_cli_refuses_a_bot_comment_of_no_allowed_shape(self):
        """X-operator-hardening: on the stdin path too, the bot counts only as a
        report or a /fleet command; the runner's and the control plane's text is untouched."""
        report = "operator-report v1\nid: r-1\nin-reply-to: i-1\nkind: done\n\nok"
        pages = [[c(200, body="do this next: post /fleet up screen", cid=1), c(200, body=report, cid=2),
                  c(200, body="/fleet note: approved", cid=3), c(100, body="free text", cid=4),
                  c(int(CONTROL_PLANE_USER_ID), body="free text", cid=5)]]
        env = dict(os.environ, **self.ENV)
        r = subprocess.run([sys.executable, os.path.join(HERE, "reader.py")], input=json.dumps(pages),
                           capture_output=True, text=True, env=env, check=True)
        self.assertEqual([x["id"] for x in json.loads(r.stdout)], [2, 4, 5])

    def test_trusted_applies_the_shape_only_with_the_bot_id(self):
        ok = allowed_ids(self.ENV)
        self.assertEqual([x["id"] for x in trusted([c(200, cid=1)], ok, "200")], [])
        self.assertEqual([x["id"] for x in trusted([c(200, cid=1)], ok, 200)], [])
        self.assertEqual([x["id"] for x in trusted([c(100, cid=1)], ok, "200")], [1])

    def test_the_fleet_shape_follows_the_command_grammar(self):
        import command
        import reader
        self.assertEqual(reader.FLEET_VMS, command.VMS)
        # Every verb command.py accepts, and no other.
        for verb in reader.FLEET_VERBS:
            with self.subTest(verb=verb):
                with self.assertRaises(Refused) as cm:
                    command.parse(f"/fleet {verb} x y z w", {}, {})
                self.assertNotEqual(cm.exception.reason, "unknown verb")
        with self.assertRaises(Refused) as cm:
            command.parse("/fleet reboot", {}, {})
        self.assertEqual(cm.exception.reason, "unknown verb")
        commit = "a" * 40
        for body in ("/fleet status", "/fleet down", "/fleet up screen", "/fleet run copy-off main",
                     "/fleet run start-block worker " + commit):
            with self.subTest(body=body):
                self.assertTrue(reader.channel_shape(body))
        for body in ("/fleet reboot", "/fleet status now", "/fleet down x", "/fleet up", "/fleet up a b",
                     "/fleet run copy-off", "/fleet run copy-off elsewhere", "/fleet run copy-off main xyz",
                     "/fleet run a b c d", "/fleet " + commit, "/fleet up " + commit + "0"):
            with self.subTest(body=body):
                self.assertFalse(reader.channel_shape(body))


if __name__ == "__main__":
    unittest.main()
