"""Posts result comments and edits the status comment on the channel PR.

Uses the job's GITHUB_TOKEN (pull-requests: write). Errors never include the
token, the response body or a URL with parameters.
"""

from __future__ import annotations

import json
import urllib.request
from typing import Callable

from reader import CONTROL_PLANE_USER_ID
from status import MARKER

Transport = Callable[[str, str, dict | None], object]


def urllib_transport(token: str) -> Transport:  # pragma: no cover - network
    def call(method: str, path: str, body: dict | None):
        req = urllib.request.Request(
            "https://api.github.com" + path,
            data=json.dumps(body).encode() if body is not None else None,
            method=method,
            headers={"Authorization": f"Bearer {token}", "Accept": "application/vnd.github+json",
                     "X-GitHub-Api-Version": "2022-11-28", "User-Agent": "lodestar-fleet"},
        )
        try:
            with urllib.request.urlopen(req, timeout=30) as r:
                return json.loads(r.read() or b"null")
        except Exception:
            raise RuntimeError(f"GitHub API {method} failed") from None

    return call


class Channel:
    def __init__(self, transport: Transport, repo: str, pr: str):
        self.t, self.base = transport, f"/repos/{repo}/issues"
        self.pr = pr

    def post(self, body: str) -> None:
        self.t("POST", f"{self.base}/{self.pr}/comments", {"body": body})

    def upsert_status(self, body: str) -> None:
        # The status comment is found by author and marker. Only its ID is
        # used; fleet.yml never reads state back from a comment.
        page = 1
        while page <= 20:
            items = self.t("GET", f"{self.base}/{self.pr}/comments?per_page=100&page={page}", None) or []
            for c in items:
                if str((c.get("user") or {}).get("id")) == CONTROL_PLANE_USER_ID and str(c.get("body", "")).startswith(MARKER):
                    self.t("PATCH", f"{self.base}/comments/{int(c['id'])}", {"body": body})
                    return
            if len(items) < 100:
                break
            page += 1
        self.post(body)
