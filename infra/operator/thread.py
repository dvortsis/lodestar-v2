"""The operator's channel helper, run inside its Managed Agents sandbox (OP-4).

    thread.py read --pr N --runner ID --bot ID     trusted comments, as JSON
    thread.py post --pr N --file F                  one report or /fleet command

`read` reads the channel PR's comments newest page first (the list runs oldest
first) and keeps only what the reader helper (infra/fleet/reader.py, fetched
beside this file) trusts: unedited, from the runner, the operator App's bot or
the control plane. A comment by the operator's bot counts only in a shape the
bot may post (channel_shape); any other is dropped. Past MAX_PAGES pages it
prints what it read, newest included, then exits non-zero (X-operator-hardening).
`post` refuses
anything but an `operator-report v1` or a one-line `/fleet` command; the PR
number is the agent's to pass (the system prompt names the channel PR, and the
tripwire catches a comment anywhere else). GITHUB_TOKEN is the vault's placeholder; the platform
substitutes it at egress, header only.
"""

from __future__ import annotations

import argparse
import json
import os
import re
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
CAPPED = 3  # read's exit code past MAX_PAGES: a helper failure, which the prompt escalates
_FIELD = re.compile(r"(id|in-reply-to|kind): ([a-z0-9][a-z0-9-]{0,39})")
# A /fleet command's words, as infra/fleet/command.py reads them: lower-case
# words of letters, digits and hyphens, or a full commit; one space between.
_WORD = re.compile(r"[a-z0-9-]{1,32}|[0-9a-f]{40}")
MAX_WORDS = 4  # `run <step> <vm> <commit>`, the longest command


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


def channel_shape(body) -> bool:
    """The only comments the operator's bot may post (check_post's rule): an
    operator-report v1 with its three header fields and a body, or a one-line
    /fleet command in the command parser's words (never free text). Shared with
    the tripwire's sweep."""
    if not isinstance(body, str) or len(body) > MAX_BODY:
        return False
    text = body.strip()
    if text.startswith("/fleet "):
        words = text[len("/fleet "):].split(" ")
        return text.isascii() and 1 <= len(words) <= MAX_WORDS and all(_WORD.fullmatch(w) for w in words)
    lines = text.replace("\r\n", "\n").split("\n")
    if lines[0] != REPORT.rstrip("\n"):
        return False
    fields, i = {}, 1
    while i < len(lines) and lines[i]:
        m = _FIELD.fullmatch(lines[i])
        if not m or m[1] in fields:
            return False
        fields[m[1]] = m[2]
        i += 1
    return (set(fields) == {"id", "in-reply-to", "kind"} and fields["kind"] in ("ack", "report", "escalation", "done")
            and bool("\n".join(lines[i + 1:]).strip()))


def _page(call, pr: int, page: int) -> list:
    items = call("GET", f"/repos/{REPO}/issues/{pr}/comments?per_page=100&page={page}")
    items = [] if items is None else items
    if not isinstance(items, list):
        raise SystemExit("thread: a page of comments wasn't a list")
    return items


def read(pr: int, runner: str, bot: str, call=_call) -> tuple[list[dict], bool]:
    """The trusted comments, oldest first, and whether MAX_PAGES cut the read
    short. The newest pages are read first: the issue's comment count names the
    last page, the read goes forward from there while pages are full (comments
    that arrived since), then back towards the first."""
    allowed = reader.allowed_ids({"RUNNER_USER_ID": runner, "OPERATOR_BOT_USER_ID": bot})
    issue = call("GET", f"/repos/{REPO}/issues/{pr}")
    count = issue.get("comments") if isinstance(issue, dict) else None
    if isinstance(count, bool) or not isinstance(count, int) or count < 0:
        raise SystemExit("thread: the channel's comment count isn't readable")
    start = max(1, -(-count // 100))
    page, got, full = start, {}, False
    while len(got) < MAX_PAGES:
        got[page] = _page(call, pr, page)
        full = len(got[page]) == 100
        if not full:
            if page > start and not got[page]:
                del got[page]  # an empty page past the count was only a check: it holds nothing to count
            break
        page += 1
    page = min(got) - 1
    while page >= 1 and len(got) < MAX_PAGES:
        got[page] = _page(call, pr, page)
        page -= 1
    capped = full or min(got) > 1
    return keep([c for n in sorted(got) for c in got[n]], allowed, bot), capped


def keep(items: list, allowed: frozenset[str], bot: str) -> list[dict]:
    """reader.trusted, less any bot comment of no shape the bot may post, and
    less a repeat (a comment deleted between page reads shifts the pages)."""
    seen, out = set(), []
    for c in reader.trusted(items, allowed):
        if c["id"] in seen or (str(c["author_id"]) == str(bot) and not channel_shape(c["body"])):
            continue
        seen.add(c["id"])
        out.append(c)
    return out


def check_post(body: str) -> str:
    body = body.strip()
    if len(body) > MAX_BODY:
        raise SystemExit("thread: too long")
    if channel_shape(body):  # the same rule the reader and the tripwire apply to the bot's comments
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
        comments, capped = read(a.pr, a.runner, a.bot, call)
        json.dump(comments, sys.stdout, indent=1)
        sys.stdout.write("\n")
        if capped:
            print(f"thread: the channel has more than {MAX_PAGES * 100} comments; only the newest were read",
                  file=sys.stderr)
            return CAPPED
    else:
        body = check_post(Path(a.file).read_text(encoding="utf-8"))
        call("POST", f"/repos/{REPO}/issues/{a.pr}/comments", {"body": body})
        print("thread: posted")
    return 0


if __name__ == "__main__":
    sys.exit(main())
