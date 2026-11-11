"""The channel helper inside an injection-test session (OP-5).

The harness mounts this file as `thread.py`, beside the real helper
(infra/operator/thread.py, mounted as `thread_real.py`) and the real reader
(infra/fleet/reader.py). The agent runs the same commands as in production:

    python3 thread.py read --pr N --runner ID --bot ID
    python3 thread.py post --pr N --file report.md

Both go through thread_real.main, so the real reader filter and the real post
check run. Only the transport differs: GitHub is local. The thread comes from
`comments.json`. Each post is recorded in `posts.json`, and later reads show
it as the bot's comment. Nothing here opens a connection.

Every post attempt, every accepted post and every other call prints a
`ghmock-<kind> <base64 JSON>` line. The harness reads those lines from the
session's tool results.
"""

from __future__ import annotations

import base64
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import thread_real  # noqa: E402

STAMP = "2026-11-11T12:00:00Z"


def _mark(kind: str, data: dict) -> None:
    print(f"ghmock-{kind} " + base64.b64encode(json.dumps(data).encode()).decode(), flush=True)


def _load(name: str, default):
    try:
        return json.loads((HERE / name).read_text(encoding="utf-8"))
    except FileNotFoundError:
        return default


def _issue(path: str) -> tuple[str, str] | None:
    """(number, rest) for /repos/<REPO>/issues/<n>/<rest>, else None."""
    base = f"/repos/{thread_real.REPO}/issues/"
    if not path.startswith(base):
        return None
    n, _, rest = path[len(base):].partition("/")
    return n, rest


def call(method: str, path: str, body=None):
    fx = _load("comments.json", {})
    channel, bot = str(fx.get("channel_pr")), int(fx.get("bot_id", 0))
    posts = _load("posts.json", [])
    hit = _issue(path)
    if method == "GET" and hit and hit[1].startswith("comments?"):
        if hit[0] != channel or not hit[1].endswith("&page=1"):
            return []
        return list(fx.get("comments", [])) + posts
    if method == "POST" and hit and hit[1] == "comments":
        text = (body or {}).get("body", "")
        _mark("post", {"pr": hit[0], "body": text})
        comment = {"id": 990000 + len(posts), "user": {"id": bot}, "created_at": STAMP, "updated_at": STAMP,
                   "body": text}
        if hit[0] == channel:
            (HERE / "posts.json").write_text(json.dumps(posts + [comment]), encoding="utf-8")
        return {"id": comment["id"]}
    _mark("other", {"method": method, "path": path.split("?")[0]})
    raise SystemExit(f"thread: GitHub {method} failed")


def _opt(argv: list[str], name: str) -> str | None:
    for i, tok in enumerate(argv):
        if tok == name and i + 1 < len(argv):
            return argv[i + 1]
        if tok.startswith(name + "="):
            return tok.split("=", 1)[1]
    return None


def main(argv=None) -> int:
    argv = sys.argv[1:] if argv is None else list(argv)
    if argv[:1] == ["post"]:
        attempt = {"pr": _opt(argv, "--pr"), "body": None}
        f = _opt(argv, "--file")
        try:
            attempt["body"] = Path(f).read_text(encoding="utf-8", errors="replace") if f else None
        except OSError:
            pass
        _mark("attempt", attempt)
    return thread_real.main(argv, call=call)


if __name__ == "__main__":
    sys.exit(main())
