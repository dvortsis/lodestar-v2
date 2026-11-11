import base64
import hashlib
import json
import subprocess
import unittest
from pathlib import Path

from tests.helpers import FakeGitHub, rsa_key
import ghapp


def b64d(s):
    return base64.urlsafe_b64decode(s + "=" * (-len(s) % 4))


class Jwt(unittest.TestCase):
    def test_both_key_formats_sign_a_jwt_openssl_verifies(self):
        for fmt in ("pkcs1", "pkcs8"):
            with self.subTest(fmt=fmt):
                jwt = ghapp.app_jwt("12345", rsa_key(fmt), now=lambda: 1_800_000_000)
                head, claims, sig = jwt.split(".")
                self.assertEqual(json.loads(b64d(head)), {"alg": "RS256", "typ": "JWT"})
                self.assertEqual(json.loads(b64d(claims)), {"iat": 1_799_999_940, "exp": 1_800_000_540, "iss": "12345"})
                d = Path(rsa_key("dir"))
                (d / "msg").write_bytes(f"{head}.{claims}".encode())
                (d / "sig").write_bytes(b64d(sig))
                (d / "pub.pem").write_text(rsa_key("pub"))
                r = subprocess.run(["openssl", "dgst", "-sha256", "-verify", str(d / "pub.pem"), "-signature",
                                    str(d / "sig"), str(d / "msg")], capture_output=True, text=True)
                self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    def test_signature_is_pkcs1_v15(self):
        n, d = ghapp.parse_private_key(rsa_key())
        em = pow(int.from_bytes(ghapp.sign_rs256(b"m", n, d), "big"), 65537, n).to_bytes(256, "big")
        self.assertTrue(em.startswith(b"\x00\x01\xff"))
        self.assertTrue(em.endswith(ghapp._SHA256_INFO + hashlib.sha256(b"m").digest()))

    def test_bad_inputs_refuse(self):
        for pem in ("", "-----BEGIN RSA PRIVATE KEY-----\n!!!\n-----END RSA PRIVATE KEY-----",
                    "-----BEGIN RSA PRIVATE KEY-----\nMAA=\n-----END RSA PRIVATE KEY-----"):
            with self.subTest(pem=pem[:30]), self.assertRaises(ValueError):
                ghapp.parse_private_key(pem)
        with self.assertRaises(ValueError):
            ghapp.app_jwt("12a", rsa_key())


class Mint(unittest.TestCase):
    def test_asks_for_this_repo_and_the_two_permissions_only(self):
        gh = FakeGitHub()
        self.assertEqual(ghapp.mint(gh, "o/r", "JWT"), "ghs_TESTTOKEN")
        (m1, p1, _, b1), (m2, p2, body, b2) = gh.calls
        self.assertEqual((m1, p1, b1), ("GET", "/repos/o/r/installation", "JWT"))
        self.assertEqual((m2, p2, b2), ("POST", "/app/installations/77/access_tokens", "JWT"))
        self.assertEqual(body, {"repositories": ["r"], "permissions": {"pull_requests": "write", "metadata": "read"}})

    def test_a_wider_token_is_revoked_and_refused(self):
        for kw in ({"perms": {"pull_requests": "write", "metadata": "read", "contents": "write"}},
                   {"perms": {"pull_requests": "write", "metadata": "read", "issues": "write"}},
                   {"repos": ["o/r", "o/other"]}, {"repos": []}):
            with self.subTest(**{k: str(v) for k, v in kw.items()}):
                gh = FakeGitHub(**kw)
                with self.assertRaises(ghapp.TokenRefused):
                    ghapp.mint(gh, "o/r", "JWT")
                self.assertEqual(gh.calls[-1][:2], ("DELETE", "/installation/token"))
                self.assertEqual(gh.calls[-1][3], "ghs_TESTTOKEN")

    def test_revoke_uses_the_token_itself(self):
        gh = FakeGitHub()
        ghapp.revoke(gh, "ghs_X")
        self.assertEqual(gh.calls, [("DELETE", "/installation/token", None, "ghs_X")])
