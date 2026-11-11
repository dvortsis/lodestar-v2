import io
import json
import unittest
from contextlib import redirect_stdout

from tests.helpers import Clock, FakeAnthropic
import agent_apply
import bridge
from lock import MemoryStore
from oplock import CLAUDE_LOCK_KEY, LOCK_KEY


def env(**over):
    e = {"GITHUB_EVENT_NAME": "workflow_dispatch", "GITHUB_RUN_ATTEMPT": "1", "GITHUB_RUN_ID": "5",
         "OWNER_USER_ID": "111", "GITHUB_ACTOR_ID": "111", "OPERATOR_STOP": "false",
         "OPERATOR_MODEL_ROUTINE": "claude-test-routine", "RUNNER_USER_ID": "111", "OPERATOR_BOT_USER_ID": "555",
         "CHANNEL_PR": "700", "ANTHROPIC_API_KEY": "k", "AWS_BRIDGE_ROLE_ARN": "a", "CONTROL_BUCKET": "b",
         "FLEET_BUCKET_OWNER": "111122223333", "ACTIONS_ID_TOKEN_REQUEST_URL": "u", "ACTIONS_ID_TOKEN_REQUEST_TOKEN": "t"}
    e.update(over)
    return e


class Render(unittest.TestCase):
    def test_agent(self):
        agent, environment, vault = bridge.render(env())
        self.assertEqual(agent["model"], {"id": "claude-test-routine", "effort": "medium"})
        self.assertIn("pull request #700", agent["system"])
        self.assertIn("--runner 111 --bot 555", agent["system"])
        self.assertNotIn("{{", json.dumps(agent))
        tools = agent["tools"][0]
        self.assertEqual(tools["default_config"]["permission_policy"], {"type": "auto"})
        self.assertEqual({c["name"]: c["enabled"] for c in tools["configs"]}, {"web_fetch": False, "web_search": False})
        self.assertEqual(environment["config"]["networking"], {"type": "limited", "allowed_hosts": ["api.github.com"],
                                                               "allow_package_managers": False, "allow_mcp_servers": False})
        auth = vault["credential"]["auth"]
        self.assertEqual((auth["type"], auth["secret_name"]), ("environment_variable", "GITHUB_TOKEN"))
        self.assertEqual(auth["networking"], {"type": "limited", "allowed_hosts": ["api.github.com"]})
        self.assertEqual(auth["injection_location"], {"header": True, "body": False})
        self.assertNotIn("secret_value", auth)

    def test_no_model_id_is_committed(self):
        text = "".join(p.read_text() for p in bridge.AGENT_DIR.iterdir())
        self.assertNotRegex(text, r"claude-(opus|sonnet|haiku|fable|mythos)")

    def test_bad_values_refuse(self):
        for over in ({"OPERATOR_MODEL_ROUTINE": "gpt"}, {"OPERATOR_MODEL_ROUTINE": "claude-x\"}"},
                     {"CHANNEL_PR": "7 0"}, {"RUNNER_USER_ID": ""}, {"OPERATOR_BOT_USER_ID": "abc"}):
            with self.subTest(**over), self.assertRaises(bridge.Refusal):
                bridge.render(env(**over))


class Apply(unittest.TestCase):
    def run_apply(self, store, api, **over):
        with redirect_stdout(io.StringIO()) as out:
            agent_apply.main(["apply"], env=env(**over), call=api, store=store, clock=Clock())
        return out.getvalue()

    def test_first_apply_creates_then_updates(self):
        store, api = MemoryStore(), FakeAnthropic()
        out = self.run_apply(store, api)
        self.assertEqual(api.paths("POST"), ["/v1/environments", "/v1/agents", "/v1/vaults",
                                             "/v1/vaults/vlt_FAKE/credentials"])
        cred = api.calls[-1][2]
        self.assertEqual(cred["auth"]["secret_value"], agent_apply.UNSET_TOKEN)
        lock = json.loads(store.objects[CLAUDE_LOCK_KEY][0])
        self.assertEqual(lock, {"agent_id": "agent_FAKE", "agent_version": 1, "environment_id": "env_FAKE",
                                "vault_id": "vlt_FAKE", "credential_id": "vcrd_FAKE"})
        self.assertNotIn("agent_FAKE", out)
        self.assertNotIn(LOCK_KEY, store.objects)
        api.calls.clear()
        self.run_apply(store, api)
        self.assertEqual(api.paths("POST"), ["/v1/environments/env_FAKE", "/v1/agents/agent_FAKE", "/v1/vaults/vlt_FAKE"])
        self.assertEqual(api.calls[1][2]["version"], 1)  # optimistic concurrency on the agent
        self.assertEqual(json.loads(store.objects[CLAUDE_LOCK_KEY][0])["agent_version"], 2)

    def test_refused_runs_touch_nothing(self):
        for over in ({"GITHUB_ACTOR_ID": "9"}, {"OWNER_USER_ID": ""}, {"OPERATOR_STOP": "true"},
                     {"GITHUB_RUN_ATTEMPT": "2"}, {"GITHUB_EVENT_NAME": "push"}, {"ANTHROPIC_API_KEY": ""}):
            with self.subTest(**over):
                store, api = MemoryStore(), FakeAnthropic()
                with self.assertRaises(SystemExit):
                    self.run_apply(store, api, **over)
                self.assertEqual((api.calls, store.calls), ([], []))

    def test_a_live_session_blocks_apply(self):
        store, api = MemoryStore(), FakeAnthropic()
        store.put(LOCK_KEY, json.dumps({"run_id": "9-1", "expires_at": "2026-11-11T12:30:00Z"}).encode())
        with self.assertRaises(SystemExit):
            self.run_apply(store, api)
        self.assertEqual(api.calls, [])

    def test_malformed_claude_lock_refuses(self):
        store, api = MemoryStore(), FakeAnthropic()
        store.put(CLAUDE_LOCK_KEY, b'{"agent_id": "a"}')
        with self.assertRaises(SystemExit):
            self.run_apply(store, api)
        self.assertEqual(api.calls, [])
        self.assertNotIn(LOCK_KEY, store.objects)  # released on the way out


class ExistsFirst(unittest.TestCase):
    def test_a_missing_claude_lock_is_listed_not_read(self):
        # Live, a GET on a missing key is a 403 under the bridge role: list first.
        class Listing(MemoryStore):
            def exists(self, key):
                self.calls.append("exists")
                return key in self.objects
        store = Listing()
        self.assertEqual(bridge.read_claude_lock(store), (None, None))
        self.assertEqual(store.calls, ["exists"])
        store.put(CLAUDE_LOCK_KEY, json.dumps({"agent_id": "a", "agent_version": 2, "environment_id": "e",
                                                "vault_id": "v", "credential_id": "c"}).encode())
        self.assertEqual(bridge.read_claude_lock(store)[0]["agent_version"], 2)
