"""Fakes for the operator tests: GitHub, the Managed Agents API and a clock.
Nothing here touches a network."""

from __future__ import annotations

import subprocess
import sys
import tempfile
from datetime import datetime, timedelta, timezone
from pathlib import Path

OP = Path(__file__).resolve().parents[1]
sys.path[:0] = [str(OP), str(OP.parent / "fleet")]

T0 = datetime(2026, 11, 11, 12, 0, tzinfo=timezone.utc)
INSTRUCTION = "operator-instruction v1\nid: ping-1\nkind: query\n\nping"


class Clock:
    def __init__(self, t=T0):
        self.t = t

    def __call__(self):
        return self.t


class Mono:
    """A monotonic clock that sleep() advances: no real waiting in tests."""

    def __init__(self):
        self.t = 1000.0

    def __call__(self):
        return self.t

    def sleep(self, s):
        self.t += s


def event_env(body=INSTRUCTION, **over) -> dict:
    env = {"EVENT_NAME": "issue_comment", "EVENT_ACTION": "created", "IS_PR": "true", "ISSUE_NUMBER": "700",
           "CHANNEL_PR": "700", "AUTHOR_ID": "111", "RUNNER_USER_ID": "111", "OPERATOR_BOT_USER_ID": "555",
           "OPERATOR_STOP": "false", "COMMENT_BODY": body, "GITHUB_RUN_ID": "42", "GITHUB_RUN_ATTEMPT": "1",
           "GITHUB_REPOSITORY": "o/r"}
    env.update(over)
    return env


class FakeGitHub:
    """Comment posts, the App installation and token calls, and list endpoints."""

    def __init__(self, token="ghs_TESTTOKEN", perms=None, repos=None, lists=None, selection="selected"):
        self.calls, self.token, self.selection = [], token, selection
        self.perms = {"pull_requests": "write", "metadata": "read"} if perms is None else perms
        self.repos = ["o/r"] if repos is None else repos
        self.lists = lists or {}

    def __call__(self, method, path, body, bearer=None):
        self.calls.append((method, path, body, bearer))
        if path.endswith("/installation"):
            return {"id": 77}
        if path.endswith("/access_tokens"):
            return {"token": self.token, "permissions": self.perms, "repository_selection": self.selection,
                    "repositories": [{"full_name": r} for r in self.repos]}
        if method == "POST" and path == "/graphql":
            return graphql_reviews(body, lambda n: self.lists.get(f"/repos/o/r/pulls/{n}/reviews", []))
        if method == "GET":
            for prefix, items in self.lists.items():
                if path.startswith(prefix):
                    return items if path.endswith("&page=1") or "&page=" not in path else []
            return no_runs(path)
        return None

    def bodies(self):
        return [c[2]["body"] for c in self.calls if isinstance(c[2], dict) and "body" in c[2]]


def no_runs(path):
    """A GET answer for the tripwire's fakes: no operator-wake.yml runs in the
    window for the session check, an empty page for anything else."""
    if "/actions/workflows/operator-wake.yml/runs" in path:
        return {"total_count": 0, "workflow_runs": []}
    return []


def graphql_reviews(body, reviews_of, total_of=None) -> dict:
    """The tripwire's review-summary query, answered from REST-shaped reviews:
    each `pN:` alias gets PR N's reviews (its last 50) and their total."""
    import re
    repo = {}
    for n in re.findall(r"p(\d+):pullRequest", (body or {}).get("query", "")):
        rs = list(reviews_of(n))
        total = len(rs) if total_of is None else total_of(n)
        repo[f"p{n}"] = {"reviews": {"totalCount": total, "nodes": [
            {"submittedAt": r.get("submitted_at"), "author": {"__typename": "User", "databaseId": (r.get("user") or {}).get("id")}}
            for r in rs[-50:]]}}
    return {"data": {"repository": repo}}


class FakeAnthropic:
    """The Managed Agents calls the bridge and apply make, with scripted session states."""

    def __init__(self, sessions=None, states=None, events=None):
        self.calls, self.sessions = [], list(sessions or [])
        self.states = list(states or [{"status": "idle", "stop_reason": {"type": "end_turn"}}])
        self.events = list(events or [])
        self.agent_version = 1

    def __call__(self, method, path, body):
        self.calls.append((method, path, body))
        p = path.split("?")[0]
        if method == "GET" and p == "/v1/sessions":
            return {"data": self.sessions, "next_page": None}
        if method == "POST" and p == "/v1/sessions":
            return {"id": "sesn_FAKE", "status": "running"}
        if method == "GET" and p.endswith("/events"):
            return {"data": self.events, "next_page": None}
        if method == "GET" and p.startswith("/v1/sessions/"):
            return self.states.pop(0) if len(self.states) > 1 else self.states[0]
        if method == "POST" and p == "/v1/environments":
            return {"id": "env_FAKE"}
        if method == "POST" and p == "/v1/agents":
            return {"id": "agent_FAKE", "version": 1}
        if method == "POST" and p.startswith("/v1/agents/"):
            self.agent_version += 1
            return {"id": "agent_FAKE", "version": self.agent_version}
        if method == "POST" and p == "/v1/vaults":
            return {"id": "vlt_FAKE"}
        if method == "POST" and p.endswith("/credentials"):
            return {"id": "vcrd_FAKE"}
        return {}

    def paths(self, method=None):
        return [p for m, p, _ in self.calls if method is None or m == method]


_KEY = None


def rsa_key(fmt="pkcs1") -> str:
    """A throwaway 2048-bit test key from openssl (cached per run)."""
    global _KEY
    if _KEY is None:
        d = tempfile.mkdtemp(prefix="op-key-")
        k8 = Path(d) / "k8.pem"
        subprocess.run(["openssl", "genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048", "-out", str(k8)],
                       check=True, capture_output=True)
        k1 = subprocess.run(["openssl", "rsa", "-in", str(k8), "-traditional"], check=True, capture_output=True,
                            text=True).stdout
        pub = subprocess.run(["openssl", "rsa", "-in", str(k8), "-pubout"], check=True, capture_output=True,
                             text=True).stdout
        _KEY = {"pkcs8": k8.read_text(), "pkcs1": k1, "pub": pub, "dir": d}
    return _KEY[fmt]


def session(cents="100", created=T0 - timedelta(hours=3), **meta) -> dict:
    return {"id": "sesn_x", "created_at": created.strftime("%Y-%m-%dT%H:%M:%S.123456Z"),
            "usage": {"list_cost": {"amount": cents, "currency": "USD"}}, "metadata": meta}
