"""The operator App's installation token: minted per wake, revoked after (plan §4).

The App JWT is signed RS256 in pure Python (PKCS#1 v1.5 over SHA-256), so the
private key never reaches a file, a subprocess or a log. The token is asked for
this one repository and exactly Pull requests write + Metadata read; a token
that comes back with anything else is revoked and refused.
"""

from __future__ import annotations

import base64
import hashlib
import json
import time
from typing import Callable

# The only permissions the operator's token may carry (plan §4).
PERMISSIONS = {"pull_requests": "write", "metadata": "read"}
_SHA256_INFO = bytes.fromhex("3031300d060960864801650304020105000420")

Transport = Callable[[str, str, dict | None, str], object]  # method, path, body, bearer


class TokenRefused(RuntimeError):
    pass


def _der(data: bytes, i: int) -> tuple[int, int, int]:
    """(tag, start of content, end of content) of the DER element at i."""
    tag, n = data[i], data[i + 1]
    i += 2
    if n & 0x80:
        k = n & 0x7F
        if not 1 <= k <= 4:
            raise ValueError("bad key encoding")
        n = int.from_bytes(data[i:i + k], "big")
        i += k
    if i + n > len(data):
        raise ValueError("bad key encoding")
    return tag, i, i + n


def _ints(seq: bytes) -> list[int]:
    out, i = [], 0
    while i < len(seq):
        tag, s, e = _der(seq, i)
        if tag != 0x02:
            raise ValueError("bad key encoding")
        out.append(int.from_bytes(seq[s:e], "big"))
        i = e
    return out


def parse_private_key(pem: str) -> tuple[int, int]:
    """(n, d) from a PKCS#1 (RSA PRIVATE KEY) or PKCS#8 (PRIVATE KEY) PEM."""
    lines = [ln.strip() for ln in pem.strip().splitlines()]
    if len(lines) < 3 or not lines[0].startswith("-----BEGIN ") or not lines[-1].startswith("-----END "):
        raise ValueError("not a PEM private key")
    der = base64.b64decode("".join(lines[1:-1]), validate=True)
    tag, s, e = _der(der, 0)
    if tag != 0x30:
        raise ValueError("bad key encoding")
    body = der[s:e]
    if "RSA PRIVATE KEY" not in lines[0]:  # PKCS#8: version, algorithm, OCTET STRING(PKCS#1)
        _, _, e1 = _der(body, 0)
        _, _, e2 = _der(body, e1)
        tag, s3, e3 = _der(body, e2)
        if tag != 0x04:
            raise ValueError("bad key encoding")
        tag, s, e = _der(body[s3:e3], 0)
        body = body[s3:e3][s:e]
    ints = _ints(body)
    if len(ints) < 4 or ints[0] != 0:
        raise ValueError("bad key encoding")
    return ints[1], ints[3]


def _b64url(b: bytes) -> str:
    return base64.urlsafe_b64encode(b).rstrip(b"=").decode()


def sign_rs256(message: bytes, n: int, d: int) -> bytes:
    k = (n.bit_length() + 7) // 8
    t = _SHA256_INFO + hashlib.sha256(message).digest()
    if k < len(t) + 11:
        raise ValueError("key too short")
    em = b"\x00\x01" + b"\xff" * (k - len(t) - 3) + b"\x00" + t
    return pow(int.from_bytes(em, "big"), d, n).to_bytes(k, "big")


def app_jwt(app_id: str, pem: str, now: Callable[[], float] = time.time) -> str:
    if not (app_id.isascii() and app_id.isdigit()):
        raise ValueError("OPERATOR_APP_ID must be numeric")
    n, d = parse_private_key(pem)
    t = int(now())
    head = _b64url(json.dumps({"alg": "RS256", "typ": "JWT"}, separators=(",", ":")).encode())
    claims = _b64url(json.dumps({"iat": t - 60, "exp": t + 540, "iss": app_id}, separators=(",", ":")).encode())
    signing = f"{head}.{claims}".encode()
    return f"{head}.{claims}.{_b64url(sign_rs256(signing, n, d))}"


def mint(transport: Transport, repo: str, jwt: str) -> str:
    """A 1-hour installation token for `repo` only, with PERMISSIONS only."""
    owner, name = repo.split("/", 1)
    inst = transport("GET", f"/repos/{owner}/{name}/installation", None, jwt) or {}
    inst_id = inst.get("id")
    if isinstance(inst_id, bool) or not isinstance(inst_id, int):
        raise TokenRefused("no installation for this repository")
    got = transport("POST", f"/app/installations/{inst_id}/access_tokens",
                    {"repositories": [name], "permissions": dict(PERMISSIONS)}, jwt) or {}
    token = got.get("token")
    if not isinstance(token, str) or not token:
        raise TokenRefused("no token returned")
    repos = [r.get("full_name") for r in got.get("repositories") or [] if isinstance(r, dict)]
    if got.get("permissions") != PERMISSIONS or repos != [repo] or got.get("repository_selection") != "selected":
        revoke(transport, token)
        raise TokenRefused("token scope differs from the operator's")
    return token


def revoke(transport: Transport, token: str) -> None:
    transport("DELETE", "/installation/token", None, token)
