import io
import json
import secrets
import unittest
from contextlib import redirect_stderr, redirect_stdout
from datetime import timedelta

from tests.helpers import INSTRUCTION, T0, Clock, FakeAnthropic, FakeGitHub, Mono, event_env, rsa_key, session
import wake
from lock import MemoryStore
from oplock import CLAUDE_LOCK_KEY, LOCK_KEY

IDS = {"agent_id": "agent_A", "agent_version": 3, "environment_id": "env_E", "vault_id": "vlt_V",
       "credential_id": "cred_C"}


def run_gate(env):
    out = io.StringIO()
    import os
    import tempfile
    fd, path = tempfile.mkstemp()
    os.close(fd)
    with redirect_stdout(out):
        rc = wake.main(["gate"], env=dict(env, GITHUB_OUTPUT=path))
    with open(path) as f:
        got = dict(line.strip().split("=", 1) for line in f if "=" in line)
    os.unlink(path)
    return rc, got, out.getvalue()


class Gate(unittest.TestCase):
    def test_allowed(self):
        self.assertEqual(run_gate(event_env())[1], {"allowed": "true", "reason": ""})

    def test_silently_ignored(self):
        cases = {"stranger": {"AUTHOR_ID": "9"}, "the bot": {"AUTHOR_ID": "555"},
                 "bot as runner": {"AUTHOR_ID": "555", "RUNNER_USER_ID": "555"},
                 "control plane as runner": {"AUTHOR_ID": "41898282", "RUNNER_USER_ID": "41898282"},
                 "edited": {"EVENT_ACTION": "edited"}, "other PR": {"ISSUE_NUMBER": "701"},
                 "an issue": {"IS_PR": "false"}, "re-run": {"GITHUB_RUN_ATTEMPT": "2"},
                 "unset runner": {"RUNNER_USER_ID": ""}, "unset channel": {"CHANNEL_PR": ""},
                 "other event": {"EVENT_NAME": "pull_request_target"}}
        for name, over in cases.items():
            with self.subTest(name):
                self.assertEqual(run_gate(event_env(**over))[1], {"allowed": "false", "reason": ""})

    def test_refused_with_a_fixed_reason(self):
        for over, reason in (({"OPERATOR_STOP": "true"}, "OPERATOR_STOP is set"),
                             ({"OPERATOR_STOP": ""}, "OPERATOR_STOP is set"),
                             ({"COMMENT_BODY": "operator-instruction v1\nid: a\n\nhi"}, "missing field"),
                             ({"COMMENT_BODY": "/fleet up screen"}, "not an operator instruction")):
            with self.subTest(reason):
                self.assertEqual(run_gate(event_env(**over))[1], {"allowed": "false", "reason": reason})

    def test_refuse_posts_only_fixed_reasons(self):
        gh = FakeGitHub()
        wake.main(["refuse"], env=event_env(REFUSAL_REASON="missing field"), deps={"channel_transport": gh})
        wake.main(["refuse"], env=event_env(REFUSAL_REASON="rm -rf /"), deps={"channel_transport": gh})
        wake.main(["refuse"], env=event_env(REFUSAL_REASON="not an operator instruction"), deps={"channel_transport": gh})
        self.assertEqual(gh.bodies(), ["`operator-wake.yml`: instruction refused (missing field)"])


class Ledger(unittest.TestCase):
    def test_fine(self):
        self.assertIsNone(wake.ledger_refusal([session("19700")], T0, "routine", "ping-1"))

    def test_refusals(self):
        recent = lambda m: session("0", T0 - timedelta(minutes=m))  # noqa: E731
        cases = [
            ([session("19701")], "routine", "the $200 total would be passed"),
            ([session("18501")], "triage", "the $200 total would be passed"),
            ([recent(5)] * 6, "routine", "the rate limit (6 wakes an hour) is reached"),
            ([recent(70)] * 60, "routine", "the rate limit (60 wakes a day) is reached"),
            ([session(instruction_id="ping-1")], "routine", "this instruction id was already used"),
            ([session("1.5")], "routine", "the spend ledger is unreadable"),
            ([{**session(), "usage": {}}], "routine", "the spend ledger is unreadable"),
            ([{**session(), "usage": {"list_cost": {"amount": "5", "currency": "EUR"}}}], "routine",
             "the spend ledger is unreadable"),
            ([{**session(), "created_at": "yesterday"}], "routine", "the spend ledger is unreadable"),
        ]
        for sessions, mode, reason in cases:
            with self.subTest(reason=reason, mode=mode):
                self.assertEqual(wake.ledger_refusal(sessions, T0, mode, "ping-1"), reason)

    def test_a_ledger_without_the_last_session_refuses(self):
        # If archived sessions were missing from the list, every total would be too low.
        self.assertEqual(wake.ledger_refusal([session()], T0, "routine", "x", "sesn_last"),
                         "the spend ledger is missing the last session")
        self.assertIsNone(wake.ledger_refusal([dict(session(), id="sesn_last")], T0, "routine", "x", "sesn_last"))

    def test_when_refuses_a_time_without_a_zone(self):
        with self.assertRaises(ValueError):
            wake._when("2026-11-11T09:00:00")
        self.assertEqual(wake._when("2026-11-11T09:00:00.5Z").utcoffset(), timedelta(0))

    def test_naive_timestamps_refuse(self):
        self.assertEqual(wake.ledger_refusal([dict(session(), created_at="2026-11-11T09:00:00")], T0, "routine", "x"),
                         "the spend ledger is unreadable")

    def test_limits_are_exact(self):
        self.assertIsNone(wake.ledger_refusal([session("0", T0 - timedelta(minutes=5))] * 5, T0, "routine", "x"))
        self.assertIsNone(wake.ledger_refusal([session("0", T0 - timedelta(minutes=61))] * 59, T0, "routine", "x"))


class Watch(unittest.TestCase):
    def run_watch(self, states, events=()):
        api, mono = FakeAnthropic(states=states, events=list(events)), Mono()
        return wake.watch(api, "sesn_1", mono, mono.sleep), api

    def test_outcomes(self):
        idle = lambda t: {"status": "idle", "stop_reason": {"type": t}}  # noqa: E731
        cases = [([{"status": "running"}, idle("end_turn")], "done"),
                 ([{"status": "terminated"}], "done"),
                 ([{"status": "terminated", "stop_reason": {"type": "retries_exhausted"}}], "the session ended with an error"),
                 ([idle("budget_reached")], "the session reached its budget"),
                 ([idle("retries_exhausted")], "the session ended with an error"),
                 ([{"status": "rescheduling"}, idle("end_turn")], "done")]
        for states, want in cases:
            with self.subTest(want=want, states=str(states)[:60]):
                self.assertEqual(self.run_watch(states)[0], want)

    def test_paused_calls_are_denied_once_with_the_escalate_message(self):
        ask = [{"type": "agent.tool_use", "id": "sevt_1", "evaluated_permission": "ask"},
               {"type": "agent.tool_use", "id": "sevt_2", "evaluated_permission": "allow"},
               {"type": "agent.mcp_tool_use", "id": "sevt_3", "evaluated_permission": "ask"},
               {"type": "agent.tool_use", "id": "sevt_4", "evaluated_permission": "ask"},
               {"type": "user.tool_confirmation", "tool_use_id": "sevt_4", "result": "allow"}]
        ra = {"status": "idle", "stop_reason": {"type": "requires_action"}}
        got, api = self.run_watch([ra, ra, {"status": "idle", "stop_reason": {"type": "end_turn"}}], ask)
        self.assertEqual(got, "done")
        sends = [b for m, p, b in api.calls if m == "POST" and p.endswith("/events")]
        self.assertEqual(len(sends), 1)
        self.assertEqual([(e["tool_use_id"], e["result"]) for e in sends[0]["events"]], [("sevt_1", "deny"), ("sevt_3", "deny")])
        self.assertTrue(all(e["deny_message"] == wake.DENY for e in sends[0]["events"]))

    def test_a_stall_with_nothing_to_deny_escalates(self):
        got, _ = self.run_watch([{"status": "idle", "stop_reason": {"type": "requires_action"}}])
        self.assertEqual(got, "the session stalled waiting for an action the bridge can't give")

    def test_thirty_minutes_interrupts(self):
        got, api = self.run_watch([{"status": "running"}])
        self.assertEqual(got, "the session ran past 30 minutes and was stopped")
        posts = [b for m, p, b in api.calls if m == "POST"]
        self.assertEqual(posts, [{"events": [{"type": "user.interrupt"}]}])
        polls = sum(1 for m, p, _ in api.calls if m == "GET")
        after = wake.INTERRUPT_SECONDS // wake.POLL_SECONDS  # it waits for the stop before the archive
        self.assertEqual(polls, wake.SESSION_MINUTES * 60 // wake.POLL_SECONDS + after)

    def test_interrupt_waits_until_the_session_stops(self):
        api, mono = FakeAnthropic(states=[{"status": "running"}, {"status": "running"}, {"status": "idle"}]), Mono()
        wake._interrupt(api, "sesn_1", mono, mono.sleep)
        self.assertEqual(sum(1 for m, _, _ in api.calls if m == "GET"), 3)

    def test_a_disabled_workflow_stops_a_live_session(self):
        api, mono = FakeAnthropic(states=[{"status": "running"}, {"status": "idle"}]), Mono()
        flips = iter([False, True])
        got = wake.watch(api, "sesn_1", mono, mono.sleep, lambda: next(flips))
        self.assertEqual(got, "operator-wake.yml was disabled, so the session was stopped")
        self.assertIn({"events": [{"type": "user.interrupt"}]}, [b for m, p, b in api.calls if m == "POST"])

    def test_wake_disabled_reads_the_state_and_fails_open_only_on_errors(self):
        env = event_env()
        for state, want in (("active", False), ("disabled_manually", True), (None, False)):
            with self.subTest(state=state):
                self.assertEqual(wake.wake_disabled(env, lambda m, p, b: {"state": state})(), want)

        def boom(m, p, b):
            raise RuntimeError("down")
        self.assertFalse(wake.wake_disabled(env, boom)())


def wake_env(**over):
    marks = {k: f"LEAKMARK{k}{secrets.token_hex(4)}" for k in
             ("ANTHROPIC_API_KEY", "AWS_BRIDGE_ROLE_ARN", "CONTROL_BUCKET", "FLEET_BUCKET_OWNER", "GITHUB_TOKEN",
              "ACTIONS_ID_TOKEN_REQUEST_URL", "ACTIONS_ID_TOKEN_REQUEST_TOKEN")}
    env = event_env(**marks, OPERATOR_APP_ID="12345", OPERATOR_APP_PRIVATE_KEY=rsa_key(),
                    OPERATOR_MODEL_TRIAGE="claude-test-triage")
    env.update(over)
    return env, list(marks.values())


class Wake(unittest.TestCase):
    def setUp(self):
        self.store, self.gh, self.post = MemoryStore(), FakeGitHub(token="ghs_LEAKMARKTOKEN"), FakeGitHub()
        self.store.put(CLAUDE_LOCK_KEY, json.dumps(IDS).encode())
        self.store.calls.clear()
        self.mono = Mono()

    def go(self, env, api):
        out = io.StringIO()
        deps = {"store": self.store, "anthropic": api, "github": self.gh, "channel_transport": self.post,
                "clock": Clock(), "monotonic": self.mono, "sleep": self.mono.sleep}
        with redirect_stdout(out), redirect_stderr(out):
            try:
                rc = wake.main(["wake"], env=env, deps=deps)
            except (SystemExit, Exception) as e:
                rc = e
        return rc, out.getvalue()

    def assert_clean(self, seen, marks):
        for m in marks + ["ghs_LEAKMARKTOKEN", "agent_A", "env_E", "vlt_V", "cred_C", "sesn_FAKE"]:
            self.assertNotIn(m, seen)

    def test_one_session_end_to_end(self):
        env, marks = wake_env()
        api = FakeAnthropic(sessions=[session("100")])
        rc, out = self.go(env, api)
        self.assertEqual(rc, 0)
        creds = [b for m, p, b in api.calls if p == "/v1/vaults/vlt_V/credentials/cred_C"]
        self.assertEqual([c["auth"]["secret_value"] for c in creds], ["ghs_LEAKMARKTOKEN", "revoked"])
        body = next(b for m, p, b in api.calls if m == "POST" and p == "/v1/sessions")
        self.assertEqual(body["agent"], {"type": "agent", "id": "agent_A", "version": 3})
        self.assertEqual(body["vault_ids"], ["vlt_V"])
        self.assertEqual(body["budget"], {"type": "limit", "max_list_cost": {"amount": "300", "currency": "USD"}})
        self.assertEqual(body["initial_events"][0]["content"][0]["text"], INSTRUCTION)
        self.assertEqual(body["metadata"]["instruction_id"], "ping-1")
        order = [p for m, p, _ in api.calls if m == "POST"]
        self.assertEqual(order.index("/v1/vaults/vlt_V/credentials/cred_C"), 0)  # token in before the session
        self.assertIn("/v1/sessions/sesn_FAKE/archive", order)
        self.assertIn(("DELETE", "/installation/token", None, "ghs_LEAKMARKTOKEN"), self.gh.calls)
        self.assertNotIn(LOCK_KEY, self.store.objects)
        self.assertEqual(self.post.bodies(), [])
        self.assert_clean(out + "".join(self.post.bodies()), marks)

    def outputs(self, env, api):
        import os
        import tempfile
        fd, path = tempfile.mkstemp()
        os.close(fd)
        self.addCleanup(os.unlink, path)
        rc, _ = self.go(dict(env, GITHUB_OUTPUT=path), api)
        with open(path) as f:
            return rc, f.read()

    def test_a_started_session_says_so_for_the_sweep(self):
        env, _ = wake_env()
        rc, out = self.outputs(env, FakeAnthropic())
        self.assertEqual((rc, out), (0, "started=true\n"))

    def test_refusals_exit_0_and_start_no_sweep(self):
        # rate limit, duplicate id, $ cap, held lock, not applied: the wake's own refusals
        cases = {
            "rate": lambda: FakeAnthropic(sessions=[session("0", T0 - timedelta(minutes=5))] * 6),
            "duplicate": lambda: FakeAnthropic(sessions=[session(instruction_id="ping-1")]),
            "cap": lambda: FakeAnthropic(sessions=[session("19800")]),
        }
        for name, api in cases.items():
            with self.subTest(name):
                self.post.calls.clear()
                self.assertEqual(self.outputs(wake_env()[0], api()), (0, ""))
                self.assertIn("no session started", self.post.bodies()[0])
        self.store.put(LOCK_KEY, json.dumps({"run_id": "9-1", "expires_at": "2026-11-11T12:30:00Z"}).encode())
        self.assertEqual(self.outputs(wake_env()[0], FakeAnthropic()), (0, ""))
        del self.store.objects[LOCK_KEY]
        del self.store.objects[CLAUDE_LOCK_KEY]
        self.assertEqual(self.outputs(wake_env()[0], FakeAnthropic()), (0, ""))

    def test_a_failure_after_the_start_has_already_said_started(self):
        env, _ = wake_env()

        class Api(FakeAnthropic):
            def __call__(self, method, path, body):
                if method == "GET" and path == "/v1/sessions/sesn_FAKE":
                    raise RuntimeError("poll failed")
                return super().__call__(method, path, body)
        rc, out = self.outputs(env, Api())
        self.assertIsInstance(rc, RuntimeError)
        self.assertEqual(out, "started=true\n")

    def test_triage_overrides_the_model_with_high_effort_and_15_dollars(self):
        env, _ = wake_env(COMMENT_BODY=INSTRUCTION.replace("kind: query", "kind: run\nmode: triage"))
        api = FakeAnthropic()
        self.go(env, api)
        body = next(b for m, p, b in api.calls if m == "POST" and p == "/v1/sessions")
        self.assertEqual(body["agent"], {"type": "agent_with_overrides", "id": "agent_A", "version": 3,
                                         "model": {"id": "claude-test-triage", "effort": "high"}})
        self.assertEqual(body["budget"]["max_list_cost"]["amount"], "1500")

    def test_busy_lock_starts_nothing(self):
        self.store.put(LOCK_KEY, json.dumps({"run_id": "9-1", "expires_at": "2026-11-11T12:30:00Z"}).encode())
        env, _ = wake_env()
        api = FakeAnthropic()
        rc, _ = self.go(env, api)
        self.assertEqual(rc, 0)
        self.assertEqual(api.calls, [])
        self.assertEqual(self.gh.calls, [])
        self.assertEqual(self.post.bodies(), ["`operator-wake.yml`: busy, retry (instruction ping-1)"])

    def test_ledger_refusal_mints_no_token(self):
        env, _ = wake_env()
        api = FakeAnthropic(sessions=[session("19800")])
        self.go(env, api)
        self.assertEqual(self.gh.calls, [])
        self.assertNotIn("/v1/sessions", api.paths("POST"))
        self.assertIn("no session started: the $200 total would be passed", self.post.bodies()[0])
        self.assertNotIn(LOCK_KEY, self.store.objects)

    def test_unapplied_agent_escalates(self):
        del self.store.objects[CLAUDE_LOCK_KEY]
        env, _ = wake_env()
        api = FakeAnthropic()
        self.go(env, api)
        self.assertEqual(api.calls, [])
        self.assertIn("the agent isn't applied yet", self.post.bodies()[0])

    def test_budget_stop_escalates_and_still_cleans_up(self):
        env, marks = wake_env()
        api = FakeAnthropic(states=[{"status": "idle", "stop_reason": {"type": "budget_reached"}}])
        _, out = self.go(env, api)
        self.assertIn("kind: escalation", self.post.bodies()[0])
        self.assertIn("the session reached its budget", self.post.bodies()[0])
        self.assertIn("/v1/sessions/sesn_FAKE/archive", api.paths("POST"))
        self.assertNotIn(LOCK_KEY, self.store.objects)
        self.assert_clean(out + "".join(self.post.bodies()), marks)

    def test_wider_token_starts_no_session_and_releases(self):
        self.gh.perms = {"pull_requests": "write", "metadata": "read", "contents": "read"}
        env, marks = wake_env()
        api = FakeAnthropic()
        rc, out = self.go(env, api)
        self.assertIsInstance(rc, Exception)
        self.assertNotIn("/v1/sessions", api.paths("POST"))
        self.assertIn("the bridge failed (TokenRefused)", self.post.bodies()[0])
        self.assertNotIn(LOCK_KEY, self.store.objects)
        self.assert_clean(out + "".join(self.post.bodies()), marks)

    def test_a_failed_archive_still_revokes_resets_and_releases(self):
        env, _ = wake_env()

        class Api(FakeAnthropic):
            def __call__(self, method, path, body):
                if path.endswith("/archive"):
                    self.calls.append((method, path, body))
                    raise RuntimeError("archive refused")
                return super().__call__(method, path, body)
        api = Api()
        self.go(env, api)
        self.assertIn(("DELETE", "/installation/token", None, "ghs_LEAKMARKTOKEN"), self.gh.calls)
        creds = [b["auth"]["secret_value"] for m, p, b in api.calls if p.endswith("/credentials/cred_C")]
        self.assertEqual(creds[-1], "revoked")
        self.assertNotIn(LOCK_KEY, self.store.objects)
        self.assertTrue(any("couldn't archive the session" in b for b in self.post.bodies()))

    def test_stale_sessions_are_archived_before_a_new_token(self):
        env, _ = wake_env()
        stale = dict(session(run_id="7-1"), id="sesn_STALE", archived_at=None)
        done = dict(session(run_id="6-1"), id="sesn_DONE", archived_at="2026-11-11T09:00:00Z")
        other = dict(session(), id="sesn_OTHER", archived_at=None)  # not the bridge's
        api = FakeAnthropic(sessions=[stale, done, other])
        self.go(env, api)
        posts = api.paths("POST")
        self.assertIn("/v1/sessions/sesn_STALE/archive", posts)
        self.assertNotIn("/v1/sessions/sesn_DONE/archive", posts)
        self.assertNotIn("/v1/sessions/sesn_OTHER/archive", posts)
        self.assertLess(posts.index("/v1/sessions/sesn_STALE/archive"), posts.index("/v1/vaults/vlt_V/credentials/cred_C"))

    def test_the_session_is_recorded_for_the_next_ledger(self):
        env, _ = wake_env()
        self.go(env, FakeAnthropic())
        self.assertEqual(json.loads(self.store.objects[CLAUDE_LOCK_KEY][0])["last_session_id"], "sesn_FAKE")
        # The next wake refuses if the list doesn't show it.
        self.post.calls.clear()
        api = FakeAnthropic(sessions=[session()])
        self.go(wake_env(COMMENT_BODY=INSTRUCTION.replace("ping-1", "ping-2"))[0], api)
        self.assertNotIn("/v1/sessions", api.paths("POST"))
        self.assertIn("missing the last session", self.post.bodies()[0])

    def test_a_failure_before_the_lock_is_answered(self):
        env, _ = wake_env()

        class Broken(MemoryStore):
            def put_if_absent(self, key, body):
                raise RuntimeError("S3 lock request failed")
        self.store = Broken()
        rc, _ = self.go(env, FakeAnthropic())
        self.assertIsInstance(rc, Exception)
        self.assertIn("the bridge failed before the lock (RuntimeError)", self.post.bodies()[0])

    def test_an_oidc_exit_before_the_lock_is_answered(self):
        # apply.oidc_session raises a bare SystemExit when the role can't be assumed.
        env, marks = wake_env()
        self.store = None
        import bridge
        real = bridge.connect_store

        def boom(env_, run_id):
            raise SystemExit("fleet: couldn't assume the role")
        bridge.connect_store = boom
        self.addCleanup(setattr, bridge, "connect_store", real)
        out = io.StringIO()
        deps = {"anthropic": FakeAnthropic(), "github": self.gh, "channel_transport": self.post, "clock": Clock()}
        with redirect_stdout(out), self.assertRaises(SystemExit):
            wake.main(["wake"], env=env, deps=deps)
        self.assertIn("the bridge failed before the lock (SystemExit)", self.post.bodies()[0])
        self.assert_clean(out.getvalue() + "".join(self.post.bodies()), marks)

    def test_a_crash_mid_session_interrupts_before_the_archive(self):
        env, _ = wake_env()

        class Api(FakeAnthropic):
            def __call__(self, method, path, body):
                if method == "GET" and path == "/v1/sessions/sesn_FAKE":
                    if not any(b == {"events": [{"type": "user.interrupt"}]} for _, _, b in self.calls):
                        self.calls.append((method, path, body))
                        raise RuntimeError("poll failed")
                    self.calls.append((method, path, body))
                    return {"status": "idle"}
                return super().__call__(method, path, body)
        api = Api()
        rc, _ = self.go(env, api)
        self.assertIsInstance(rc, Exception)
        posts = [(p, b) for m, p, b in api.calls if m == "POST"]
        paths = [p for p, _ in posts]
        interrupt = posts.index(("/v1/sessions/sesn_FAKE/events", {"events": [{"type": "user.interrupt"}]}))
        self.assertLess(interrupt, paths.index("/v1/sessions/sesn_FAKE/archive"))
        self.assertIn("the bridge failed (RuntimeError)", self.post.bodies()[-1])
        self.assertNotIn(LOCK_KEY, self.store.objects)

    def test_a_refusal_after_the_lock_is_answered_and_releases(self):
        # Refusal is a SystemExit: the post-lock handler must catch it too.
        self.store.put(CLAUDE_LOCK_KEY, b'{"agent_id": "a"}')
        env, _ = wake_env()
        api = FakeAnthropic()
        rc, _ = self.go(env, api)
        self.assertIsInstance(rc, SystemExit)
        self.assertIn("the bridge failed (Refusal)", self.post.bodies()[0])
        self.assertEqual(api.calls, [])
        self.assertNotIn(LOCK_KEY, self.store.objects)

    def test_no_session_id_is_a_refusal_that_is_answered(self):
        env, _ = wake_env()

        class Api(FakeAnthropic):
            def __call__(self, method, path, body):
                if method == "POST" and path == "/v1/sessions":
                    self.calls.append((method, path, body))
                    return {}
                return super().__call__(method, path, body)
        api = Api()
        rc, _ = self.go(env, api)
        self.assertIsInstance(rc, SystemExit)
        self.assertIn("the bridge failed (Refusal)", self.post.bodies()[-1])
        self.assertIn(("DELETE", "/installation/token", None, "ghs_LEAKMARKTOKEN"), self.gh.calls)
        creds = [b["auth"]["secret_value"] for m, p, b in api.calls if p.endswith("/credentials/cred_C")]
        self.assertEqual(creds[-1], "revoked")
        self.assertNotIn(LOCK_KEY, self.store.objects)

    def test_a_failed_interrupt_still_escalates_and_archives(self):
        env, _ = wake_env()

        class Api(FakeAnthropic):
            def __call__(self, method, path, body):
                if method == "GET" and path == "/v1/sessions/sesn_FAKE":
                    self.calls.append((method, path, body))
                    raise RuntimeError("poll failed")
                if body == {"events": [{"type": "user.interrupt"}]}:
                    self.calls.append((method, path, body))
                    raise RuntimeError("interrupt refused")
                return super().__call__(method, path, body)
        api = Api()
        rc, _ = self.go(env, api)
        self.assertIsInstance(rc, RuntimeError)
        self.assertIn("the bridge failed (RuntimeError)", self.post.bodies()[-1])
        self.assertIn("/v1/sessions/sesn_FAKE/archive", api.paths("POST"))
        self.assertNotIn(LOCK_KEY, self.store.objects)

    def test_gated_events_touch_nothing(self):
        for over in ({"AUTHOR_ID": "9"}, {"OPERATOR_STOP": "true"}, {"GITHUB_RUN_ATTEMPT": "2"},
                     {"COMMENT_BODY": "operator-instruction v1\nid: x"}, {"EVENT_ACTION": "edited"}):
            with self.subTest(**over):
                env, _ = wake_env(**over)
                api = FakeAnthropic()
                self.assertEqual(self.go(env, api)[0], 0)
                self.assertEqual((api.calls, self.gh.calls, self.store.calls, self.post.calls), ([], [], [], []))

    def test_missing_setting_is_named_not_shown(self):
        env, marks = wake_env(OPERATOR_APP_PRIVATE_KEY="")
        rc, _ = self.go(env, FakeAnthropic())
        self.assertIn("OPERATOR_APP_PRIVATE_KEY", str(rc))
        for m in marks:
            self.assertNotIn(m, str(rc))
        self.assertEqual(self.store.calls, [])
