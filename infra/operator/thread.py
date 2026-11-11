"""The operator's channel helper, run inside its Managed Agents sandbox (OP-4).

    thread.py read --pr N --runner ID --bot ID     trusted comments, as JSON
    thread.py post --pr N --file F                  one report or /fleet command

`read` pages through the channel PR's comments and keeps only what the reader
helper (infra/fleet/reader.py, fetched beside this file) trusts: unedited,
from the runner, the operator App's bot or the control plane. `post` refuses
anything but an `operator-report v1` or a one-line `/fleet` command, and only
to the channel PR. GITHUB_TOKEN is the vault's placeholder; the platform
substitutes it at egress, header only.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path[:0] = [str(HERE), str(HERE.parent / "fleet")]  # beside it in the sandbox, or the repo layout

import reader  # noqa: E402

REPO = "dvortsis/lodestar-v2"
REPORT = "operator-report v1\n"
MAX_BODY = 16000
MAX_PAGES = 30


def _call(method: str, path: str, body=None):  # pragma: no cover - network
    req = urllib.request.Request("https://api.github.com" + path, method=method,
                                 data=json.dumps(body).encode() if body is not None else None, headers={
                                     "Authorization": f"Bearer {os.environ.get('GITHUB_TOKEN', '')}",
                                     "Accept": "application/vnd.github+json", "User-Agent": "lodestar-operator",
                                     "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return json.loads(r.read() or b"null")
    except Exception:
        raise SystemExit(f"thread: GitHub {method} failed") from None


def read(pr: int, runner: str, bot: str, call=_call) -> list[dict]:
    allowed = reader.allowed_ids({"RUNNER_USER_ID": runner, "OPERATOR_BOT_USER_ID": bot})
    out = []
    for page in range(1, MAX_PAGES + 1):
        items = call("GET", f"/repos/{REPO}/issues/{pr}/comments?per_page=100&page={page}") or []
        out += reader.trusted(items, allowed)
        if len(items) < 100:
            break
    return out


def check_post(body: str) -> str:
    body = body.strip()
    if len(body) > MAX_BODY:
        raise SystemExit("thread: too long")
    if body.startswith(REPORT):
        return body
    if body.startswith("/fleet ") and "\n" not in body:
        return body
    raise SystemExit("thread: only an operator-report v1 or a one-line /fleet command may be posted")


def main(argv=None, call=_call) -> int:
    p = argparse.ArgumentParser(prog="thread.py")
    sub = p.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("read")
    r.add_argument("--pr", type=int, required=True)
    r.add_argument("--runner", required=True)
    r.add_argument("--bot", required=True)
    w = sub.add_parser("post")
    w.add_argument("--pr", type=int, required=True)
    w.add_argument("--file", required=True)
    a = p.parse_args(argv)
    if a.cmd == "read":
        json.dump(read(a.pr, a.runner, a.bot, call), sys.stdout, indent=1)
        sys.stdout.write("\n")
    else:
        body = check_post(Path(a.file).read_text(encoding="utf-8"))
        call("POST", f"/repos/{REPO}/issues/{a.pr}/comments", {"body": body})
        print("thread: posted")
    return 0


if __name__ == "__main__":
    sys.exit(main())
