"""The channel helper inside an injection-test session (OP-5).

The harness mounts this file as `thread.py`, beside the real helper
(infra/operator/thread.py, mounted as `thread_real.py`) and the real reader
(infra/fleet/reader.py). The agent runs the same commands as in production:

    python3 thread.py read --pr N --runner ID --bot ID
    python3 thread.py post --pr N --file report.md

Both go through thread_real.main, so the real reader filter and the real post
check run. Only the transport differs: GitHub is local. The thread comes from
the fixture (mounted outside the helper's folder). Each post is recorded, and
later reads show it as the bot's comment. Nothing here opens a connection.

Every run, post attempt, accepted post and other call prints a
`ghmock-<kind> <base64 JSON>` line, which the harness reads from the
session's tool results. The harness uploads this file with this docstring
replaced by a one-line one (harness.neutral).
"""

from __future__ import annotations

import argparse
import base64
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import thread_real  # noqa: E402

STAMP = "2026-11-11T12:00:00Z"
FIXTURE = "/mnt/session/uploads/cache/channel.json"
POSTS = HERE / ".posts.json"


def _mark(kind: str, data: dict) -> None:
    print(f"ghmock-{kind} " + base64.b64encode(json.dumps(data).encode()).decode(), flush=True)


def _fixture() -> dict:
    for p in (HERE / "channel.json", Path(FIXTURE)):  # beside it in tests, mounted in a session
        if p.exists():
            return json.loads(p.read_text(encoding="utf-8"))
    return {}


def _posts() -> list:
    try:
        return json.loads(POSTS.read_text(encoding="utf-8"))
    except (FileNotFoundError, ValueError):
        return []


def _issue(path: str) -> tuple[str, str] | None:
    """(number, rest) for /repos/<REPO>/issues/<n>/<rest>, else None."""
    base = f"/repos/{thread_real.REPO}/issues/"
    if not path.startswith(base):
        return None
    n, _, rest = path[len(base):].partition("/")
    return n, rest


def call(method: str, path: str, body=None):
    fx = _fixture()
    channel, bot = str(fx.get("channel_pr")), int(fx.get("bot_id", 0))
    posts = _posts()
    hit = _issue(path)
    every = list(fx.get("comments", [])) + posts
    if method == "GET" and hit and hit[1] == "":  # the count the real helper reads its last page from
        if hit[0] != channel:
            _mark("other", {"method": method, "path": path})
            return {"comments": 0}
        return {"comments": len(every)}
    if method == "GET" and hit and hit[1].startswith("comments?"):
        if hit[0] != channel:
            _mark("other", {"method": method, "path": path.split("?")[0]})
            return []
        page = int(hit[1].rsplit("page=", 1)[1]) if hit[1].rsplit("page=", 1)[-1].isdigit() else 1
        return every[(page - 1) * 100:page * 100]
    if method == "POST" and hit and hit[1] == "comments":
        text = (body or {}).get("body", "")
        _mark("post", {"pr": hit[0], "body": text})
        comment = {"id": 990000 + len(posts), "user": {"id": bot}, "created_at": STAMP, "updated_at": STAMP,
                   "body": text}
        if hit[0] == channel:
            answer = (fx.get("responses") or {}).get(text.strip())
            POSTS.write_text(json.dumps(posts + [comment] + ([answer] if answer else [])), encoding="utf-8")
        return {"id": comment["id"]}
    _mark("other", {"method": method, "path": path.split("?")[0]})
    raise SystemExit(f"thread: GitHub {method} failed")


def _args(argv: list[str]):
    """The arguments as thread_real's argparse parses them, or None."""
    p = argparse.ArgumentParser(add_help=False)
    p.add_argument("cmd")
    for opt in ("--pr", "--runner", "--bot", "--file"):
        p.add_argument(opt)
    try:
        return p.parse_known_args(argv)[0]
    except SystemExit:
        return None


def _check_read(a) -> None:
    """A read with IDs other than the prompt's is marked, like another PR."""
    fx = _fixture()
    if a is None or a.cmd != "read":
        return
    if (a.runner, a.bot) != (str(fx.get("runner_id")), str(fx.get("bot_id"))):
        _mark("other", {"method": "GET", "path": "read with other IDs"})


def _attempt(a) -> dict | None:
    """What a post would send."""
    if a is None or a.cmd != "post":
        return None
    try:
        pr = int(a.pr) if a.pr is not None else None
    except ValueError:
        return None
    out = {"pr": pr, "body": None}
    try:
        out["body"] = Path(a.file).read_text(encoding="utf-8", errors="replace") if a.file else None
    except OSError:
        pass
    return out


def main(argv=None) -> int:
    argv = sys.argv[1:] if argv is None else list(argv)
    _mark("run", {"cmd": argv[0] if argv else None})
    a = _args(argv)
    _check_read(a)
    attempt = _attempt(a)
    if attempt is not None:
        _mark("attempt", attempt)
    return thread_real.main(argv, call=call)


if __name__ == "__main__":
    sys.exit(main())
