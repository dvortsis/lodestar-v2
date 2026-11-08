"""The ancestor-of-main check on every commit argument (plan §2).

A fork PR's commit is fetchable through refs/pull/N/head, so 40 hex characters
aren't enough: the commit must be an ancestor of main. Unknown commits fail.
"""

from __future__ import annotations

import subprocess

from command import HEX40


def is_ancestor(commit: str, ref: str = "origin/main", repo: str = ".") -> bool:
    if not isinstance(commit, str) or not HEX40.match(commit):
        return False
    r = subprocess.run(
        ["git", "-C", repo, "merge-base", "--is-ancestor", commit, ref],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    return r.returncode == 0


def remote_main_head(repo: str = ".") -> str | None:
    """main's head on GitHub right now, or None (callers fail closed).

    `git ls-remote` needs no credential on this public repo. It's used instead
    of the local `origin/main`, which actions/checkout points at GITHUB_SHA.
    """
    try:
        out = subprocess.run(
            ["git", "-C", repo, "ls-remote", "origin", "refs/heads/main"],
            capture_output=True, text=True, check=True, timeout=60,
        ).stdout.split()
    except (subprocess.SubprocessError, OSError):
        return None
    if len(out) != 2 or out[1] != "refs/heads/main" or not HEX40.match(out[0]):
        return None
    return out[0]


def head_of(ref: str = "origin/main", repo: str = ".") -> str:
    out = subprocess.run(
        ["git", "-C", repo, "rev-parse", "--verify", f"{ref}^{{commit}}"],
        capture_output=True, text=True, check=True,
    ).stdout.strip()
    if not HEX40.match(out):
        raise ValueError("rev-parse returned no commit")
    return out
