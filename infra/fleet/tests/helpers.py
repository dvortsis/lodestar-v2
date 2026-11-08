"""Shared fixtures for the fleet tests. Nothing here touches a network."""

from __future__ import annotations

import atexit
import shutil
import subprocess
import tempfile
from datetime import datetime, timedelta, timezone

T0 = datetime(2026, 11, 8, 20, 0, tzinfo=timezone.utc)


class Clock:
    def __init__(self, t=T0):
        self.t = t

    def __call__(self):
        return self.t

    def sleep(self, seconds):
        self.t += timedelta(seconds=seconds)


def git(repo, *args) -> str:
    return subprocess.run(["git", "-C", repo, *args], capture_output=True, text=True, check=True).stdout.strip()


def make_repo() -> tuple[str, dict]:
    """A repo with main (two commits), a side commit off main, and origin/main."""
    repo = tempfile.mkdtemp(prefix="fleet-test-")
    atexit.register(shutil.rmtree, repo, True)
    git(repo, "init", "-q", "-b", "main")
    git(repo, "config", "user.email", "t@example.invalid")
    git(repo, "config", "user.name", "t")
    git(repo, "commit", "-q", "--allow-empty", "-m", "a")
    first = git(repo, "rev-parse", "HEAD")
    git(repo, "commit", "-q", "--allow-empty", "-m", "b")
    head = git(repo, "rev-parse", "HEAD")
    git(repo, "checkout", "-q", "-b", "side", first)
    git(repo, "commit", "-q", "--allow-empty", "-m", "fork")
    side = git(repo, "rev-parse", "HEAD")
    git(repo, "checkout", "-q", "main")
    git(repo, "update-ref", "refs/remotes/origin/main", head)
    return repo, {"first": first, "head": head, "side": side}


class FakeGitHub:
    """Records API calls; serves the comment list for the status lookup."""

    def __init__(self, comments=None):
        self.calls, self.comments = [], list(comments or [])

    def __call__(self, method, path, body):
        self.calls.append((method, path, body))
        if method == "GET":
            return self.comments
        return {}

    def bodies(self):
        return [b["body"] for _, _, b in self.calls if b]


def event_env(body="/fleet status", **over) -> dict:
    env = {
        "EVENT_NAME": "issue_comment", "EVENT_ACTION": "created", "IS_PR": "true",
        "ISSUE_NUMBER": "700", "CHANNEL_PR": "700", "AUTHOR_ID": "555", "OPERATOR_BOT_USER_ID": "555",
        "OPERATOR_STOP": "false", "COMMENT_BODY": body, "GITHUB_RUN_ID": "42", "GITHUB_RUN_ATTEMPT": "1",
        "GITHUB_REPOSITORY": "o/r", "FLEET_LIVE": "false",
    }
    env.update(over)
    return env
