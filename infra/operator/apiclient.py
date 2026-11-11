"""HTTP for the bridge: GitHub and the Anthropic Managed Agents API (OP-4).

Standard library only, as the rest of the control plane. Errors carry the
method, a fixed path label and the status, never a body, a header or a token.
Tests pass their own transports; nothing here runs in CI.
"""

from __future__ import annotations

import json
import urllib.error
import urllib.parse
import urllib.request
from typing import Callable

ANTHROPIC = "https://api.anthropic.com"
ANTHROPIC_VERSION = "2023-06-01"
MANAGED_AGENTS_BETA = "managed-agents-2026-04-01"

# method, path, body -> parsed JSON (None for an empty reply)
Call = Callable[[str, str, dict | None], object]


class ApiError(RuntimeError):
    def __init__(self, label: str, status: int | None):
        super().__init__(f"{label} failed" + (f": HTTP {status}" if status else ""))
        self.status = status


def _label(method: str, path: str) -> str:
    # The first two path segments only: never an ID, a query or a token.
    return method + " /" + "/".join(path.split("?")[0].strip("/").split("/")[:2])


def _send(url: str, method: str, path: str, body, headers: dict) -> object:  # pragma: no cover - network
    req = urllib.request.Request(url, method=method, headers=headers,
                                 data=json.dumps(body).encode() if body is not None else None)
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            raw = r.read()
    except urllib.error.HTTPError as e:
        raise ApiError(_label(method, path), e.code) from None
    except Exception:
        raise ApiError(_label(method, path), None) from None
    return json.loads(raw) if raw else None


def github(token_for_call: str | None = None):  # pragma: no cover - network
    """GitHub transport: (method, path, body, bearer) -> JSON."""
    def call(method, path, body, bearer=None):
        return _send("https://api.github.com" + path, method, path, body, {
            "Authorization": f"Bearer {bearer or token_for_call}", "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28", "User-Agent": "lodestar-operator",
            "Content-Type": "application/json"})
    return call


def anthropic(api_key: str) -> Call:  # pragma: no cover - network
    def call(method, path, body):
        return _send(ANTHROPIC + path, method, path, body, {
            "x-api-key": api_key, "anthropic-version": ANTHROPIC_VERSION,
            "anthropic-beta": MANAGED_AGENTS_BETA, "content-type": "application/json",
            "User-Agent": "lodestar-operator"})
    return call


def pages(call: Call, path: str, params: dict, max_pages: int = 50) -> list[dict]:
    """Every item of a Managed Agents list (the page / next_page cursor).

    Refuses (raises) past max_pages rather than return a partial list: the
    spend and rate checks fail closed on an incomplete ledger."""
    out, page = [], None
    for _ in range(max_pages):
        q = dict(params, limit=100)
        if page:
            q["page"] = page
        res = call("GET", f"{path}?{urllib.parse.urlencode(q)}", None) or {}
        data = res.get("data")
        if not isinstance(data, list):
            raise ApiError(_label("GET", path), None)
        out += [d for d in data if isinstance(d, dict)]
        page = res.get("next_page")
        if not page:
            return out
    raise ApiError(_label("GET", path) + " (too many pages)", None)
