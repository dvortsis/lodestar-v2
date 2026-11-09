"""A small Linode API client for the fleet's live mode (OP-2). Standard library only.

It sees only resources tagged `lodestar` and refuses to change any other: the
fleet's account is dedicated, but a mis-scoped token must still not reach an
untagged machine. The token goes only into the Authorization header; errors
name a fixed operation and an HTTP status, never the token, a path with an ID,
or a response body.
"""

from __future__ import annotations

import json
import urllib.error
import urllib.request
from typing import Callable

API = "https://api.linode.com/v4"
TAG = "lodestar"

Transport = Callable[[str, str, dict | None], dict]


class LinodeError(RuntimeError):
    pass


def urllib_transport(token: str) -> Transport:  # pragma: no cover - network
    def call(method: str, path: str, body: dict | None) -> dict:
        req = urllib.request.Request(
            API + path, method=method,
            data=json.dumps(body).encode() if body is not None else None,
            headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json",
                     "User-Agent": "lodestar-fleet"},
        )
        try:
            with urllib.request.urlopen(req, timeout=30) as r:
                return json.loads(r.read() or b"{}")
        except urllib.error.HTTPError as e:
            raise LinodeError(f"Linode API {method} failed: HTTP {e.code}") from None
        except Exception:
            raise LinodeError(f"Linode API {method} failed") from None

    return call


class Linode:
    def __init__(self, transport: Transport):
        self._t = transport

    def _pages(self, path: str, op: str) -> list[dict]:
        out, page = [], 1
        while True:
            try:
                r = self._t("GET", f"{path}?page={page}&page_size=100", None)
                out += list(r["data"])
                pages = int(r.get("pages", 1))
            except LinodeError:
                raise
            except Exception:
                raise LinodeError(f"Linode API {op} returned an unexpected shape") from None
            if page >= pages or page >= 50:
                return out
            page += 1

    def account_euuid(self) -> str:
        r = self._t("GET", "/account", None)
        euuid = r.get("euuid") if isinstance(r, dict) else None
        if not isinstance(euuid, str) or not euuid:
            raise LinodeError("Linode API account returned no euuid")
        return euuid

    def check_account(self, expected: str) -> None:
        if not expected or self.account_euuid() != expected:
            raise LinodeError("the Linode token is not for the fleet's account")

    def linodes(self) -> list[dict]:
        return [n for n in self._pages("/linode/instances", "list linodes") if TAG in (n.get("tags") or [])]

    def firewalls(self) -> list[dict]:
        return [f for f in self._pages("/networking/firewalls", "list firewalls") if TAG in (f.get("tags") or [])]

    def _tagged_firewall(self, fid) -> dict:
        for f in self.firewalls():
            if str(f.get("id")) == str(fid):
                return f
        raise LinodeError("refused: that firewall isn't a tagged fleet firewall")

    def firewall_rules(self, fid) -> dict:
        self._tagged_firewall(fid)
        return self._t("GET", f"/networking/firewalls/{int(fid)}/rules", None)

    def firewall_devices(self, fid) -> list[dict]:
        self._tagged_firewall(fid)
        return self._pages(f"/networking/firewalls/{int(fid)}/devices", "list firewall devices")

    def put_firewall_rules(self, fid, rules: dict) -> None:
        """A whole-rule-set write. Callers fence on the fleet lock first."""
        self._tagged_firewall(fid)
        self._t("PUT", f"/networking/firewalls/{int(fid)}/rules", rules)

    def delete_linode(self, lid) -> None:
        if not any(str(n.get("id")) == str(lid) for n in self.linodes()):
            raise LinodeError("refused: that Linode isn't tagged for the fleet")
        self._t("DELETE", f"/linode/instances/{int(lid)}", None)

    def delete_firewall(self, fid) -> None:
        self._tagged_firewall(fid)
        self._t("DELETE", f"/networking/firewalls/{int(fid)}", None)
