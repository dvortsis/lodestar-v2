#!/usr/bin/env python3
"""
Lodestar PIA VPN Secure Bridge.

Retrieves Private Internet Access (PIA) credentials from AWS Secrets Manager using
the restricted LodestarPiaSecretReader IAM role, negotiates an ephemeral WireGuard
configuration via PIA's authenticated REST API, securely provisions wg0.conf to
the target Linode's root-only /run tmpfs over host-key-verified SSH, and independently
validates VPN egress and leak protection before permitting crawler startup.

Zero AWS credentials or master PIA passwords are ever installed on any Linode.
"""

from __future__ import annotations

import argparse
import base64
import json
import logging
import os
import re
import shlex
import ssl
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from typing import Any, Dict, List, Optional, Tuple

# -----------------------------------------------------------------------------
# Configuration Constants
# -----------------------------------------------------------------------------
DEFAULT_AWS_REGION = os.environ.get("AWS_DEFAULT_REGION", os.environ.get("AWS_REGION", "us-east-1"))
DEFAULT_AWS_ACCOUNT = os.environ.get("LODESTAR_AWS_ACCOUNT_ID", "").strip()
SECRET_NAME = "lodestar/pia-vpn"
ROLE_NAME = "LodestarPiaSecretReader"

# PIA Endpoints & Pinned Root CA
DEFAULT_PIA_REGION = "us-east"
PIA_TOKEN_URL = "https://privateinternetaccess.com/gtoken/generateToken"
PIA_SERVER_LIST_URL = "https://serverlist.piaservers.net/vpninfo/servers/v4"

# Gluetun reviewed immutable container digest
GLUETUN_IMAGE = "qmcgaw/gluetun:v3.40.0@sha256:4af0c804003481e100180e1f8d4d5e9c48163fedc0f88d061881b615aef9d183"

# Runtime filesystem locations on target Linode
REMOTE_CONFIG_DIR = "/run/lodestar/vpn"
REMOTE_CONFIG_FILE = "/run/lodestar/vpn/wg0.conf"
REMOTE_DEFAULT_USER = "lodestar"
REMOTE_DEFAULT_KEY = os.path.expanduser("~/.ssh/lodestar_akamai_ed25519")

# PIA Root CA 4096-bit Certificate PEM
PIA_ROOT_CA_PEM = """-----BEGIN CERTIFICATE-----
MIIHqzCCBZOgAwIBAgIJAJ0u+vODZJntMA0GCSqGSIb3DQEBDQUAMIHoMQswCQYD
VQQGEwJVUzELMAkGA1UECBMCQ0ExEzARBgNVBAcTCkxvc0FuZ2VsZXMxIDAeBgNV
BAoTF1ByaXZhdGUgSW50ZXJuZXQgQWNjZXNzMSAwHgYDVQQLExdQcml2YXRlIElu
dGVybmV0IEFjY2VzczEgMB4GA1UEAxMXUHJpdmF0ZSBJbnRlcm5ldCBBY2Nlc3Mx
IDAeBgNVBCkTF1ByaXZhdGUgSW50ZXJuZXQgQWNjZXNzMS8wLQYJKoZIhvcNAQkB
FiBzZWN1cmVAcHJpdmF0ZWludGVybmV0YWNjZXNzLmNvbTAeFw0xNDA0MTcxNzQw
MzNaFw0zNDA0MTIxNzQwMzNaMIHoMQswCQYDVQQGEwJVUzELMAkGA1UECBMCQ0Ex
EzARBgNVBAcTCkxvc0FuZ2VsZXMxIDAeBgNVBAoTF1ByaXZhdGUgSW50ZXJuZXQg
QWNjZXNzMSAwHgYDVQQLExdQcml2YXRlIEludGVybmV0IEFjY2VzczEgMB4GA1UE
AxMXUHJpdmF0ZSBJbnRlcm5ldCBBY2Nlc3MxIDAeBgNVBCkTF1ByaXZhdGUgSW50
ZXJuZXQgQWNjZXNzMS8wLQYJKoZIhvcNAQkBFiBzZWN1cmVAcHJpdmF0ZWludGVy
bmV0YWNjZXNzLmNvbTCCAiIwDQYJKoZIhvcNAQEBBQADggIPADCCAgoCggIBALVk
hjumaqBbL8aSgj6xbX1QPTfTd1qHsAZd2B97m8Vw31c/2yQgZNf5qZY0+jOIHULN
De4R9TIvyBEbvnAg/OkPw8n/+ScgYOeH876VUXzjLDBnDb8DLr/+w9oVsuDeFJ9K
V2UFM1OYX0SnkHnrYAN2QLF98ESK4NCSU01h5zkcgmQ+qKSfA9Ny0/UpsKPBFqsQ
25NvjDWFhCpeqCHKUJ4Be27CDbSl7lAkBuHMPHJs8f8xPgAbHRXZOxVCpayZ2SND
fCwsnGWpWFoMGvdMbygngCn6jA/W1VSFOlRlfLuuGe7QFfDwA0jaLCxuWt/BgZyl
p7tAzYKR8lnWmtUCPm4+BtjyVDYtDCiGBD9Z4P13RFWvJHw5aapx/5W/CuvVyI7p
Kwvc2IT+KPxCUhH1XI8ca5RN3C9NoPJJf6qpg4g0rJH3aaWkoMRrYvQ+5PXXYUzj
tRHImghRGd/ydERYoAZXuGSbPkm9Y/p2X8unLcW+F0xpJD98+ZI+tzSsI99Zs5wi
jSUGYr9/j18KHFTMQ8n+1jauc5bCCegN27dPeKXNSZ5riXFL2XX6BkY68y58UaNz
meGMiUL9BOV1iV+PMb7B7PYs7oFLjAhh0EdyvfHkrh/ZV9BEhtFa7yXp8XR0J6vz
1YV9R6DYJmLjOEbhU8N0gc3tZm4Qz39lIIG6w3FDAgMBAAGjggFUMIIBUDAdBgNV
HQ4EFgQUrsRtyWJftjpdRM0+925Y6Cl08SUwggEfBgNVHSMEggEWMIIBEoAUrsRt
yWJftjpdRM0+925Y6Cl08SWhge6kgeswgegxCzAJBgNVBAYTAlVTMQswCQYDVQQI
EwJDQTETMBEGA1UEBxMKTG9zQW5nZWxlczEgMB4GA1UEChMXUHJpdmF0ZSBJbnRl
cm5ldCBBY2Nlc3MxIDAeBgNVBAsTF1ByaXZhdGUgSW50ZXJuZXQgQWNjZXNzMSAw
HgYDVQQDExdQcml2YXRlIEludGVybmV0IEFjY2VzczEgMB4GA1UEKRMXUHJpdmF0
ZSBJbnRlcm5ldCBBY2Nlc3MxLzAtBgkqhkiG9w0BCQEWIHNlY3VyZUBwcml2YXRl
aW50ZXJuZXRhY2Nlc3MuY29tggkAnS7684Nkme0wDAYDVR0TBAUwAwEB/zANBgkq
hkiG9w0BAQ0FAAOCAgEAJsfhsPk3r8kLXLxY+v+vHzbr4ufNtqnL9/1Uuf8NrsCt
pXAoyZ0YqfbkWx3NHTZ7OE9ZRhdMP/RqHQE1p4N4Sa1nZKhTKasV6KhHDqSCt/dv
Em89xWm2MVA7nyzQxVlHa9AkcBaemcXEiyT19XdpiXOP4Vhs+J1R5m8zQOxZlV1G
tF9vsXmJqWZpOVPmZ8f35BCsYPvv4yMewnrtAC8PFEK/bOPeYcKN50bol22QYaZu
LfpkHfNiFTnfMh8sl/ablPyNY7DUNiP5DRcMdIwmfGQxR5WEQoHL3yPJ42LkB5zs
6jIm26DGNXfwura/mi105+ENH1CaROtRYwkiHb08U6qLXXJz80mWJkT90nr8Asj3
5xN2cUppg74nG3YVav/38P48T56hG1NHbYF5uOCske19F6wi9maUoto/3vEr0rnX
JUp2KODmKdvBI7co245lHBABWikk8VfejQSlCtDBXn644ZMtAdoxKNfR2WTFVEwJ
iyd1Fzx0yujuiXDROLhISLQDRjVVAvawrAtLZWYK31bY7KlezPlQnl/D9Asxe85l
8jO5+0LdJ6VyOs/Hd4w52alDW/MFySDZSfQHMTIc30hLBJ8OnCEIvluVQQ2UQvoW
+no177N9L2Y+M9TcTA62ZyMXShHQGeh20rb4kK8f+iFX8NxtdHVSkxMEFSfDDyQ=
-----END CERTIFICATE-----
"""

# Curated East Coast PIA regions and fallback endpoints
PIA_REGION_SERVERS: Dict[str, Dict[str, str]] = {
    "us-east": {
        "name": "US East",
        "hostname": "us-east.privacy.network",
        "ip": "156.146.54.4",
        "port": "1337",
    },
    "us-new-york": {
        "name": "US New York",
        "hostname": "us-newyork.privacy.network",
        "ip": "156.146.54.10",
        "port": "1337",
    },
    "us-new-jersey": {
        "name": "US New Jersey",
        "hostname": "us-newjersey.privacy.network",
        "ip": "156.146.54.20",
        "port": "1337",
    },
    "us-atlanta": {
        "name": "US Atlanta",
        "hostname": "us-atlanta.privacy.network",
        "ip": "156.146.54.30",
        "port": "1337",
    },
    "us-washington-dc": {
        "name": "US Washington DC",
        "hostname": "us-washingtondc.privacy.network",
        "ip": "156.146.54.40",
        "port": "1337",
    },
}

logger = logging.getLogger("lodestar-pia-bridge")


# -----------------------------------------------------------------------------
# Custom Exception Hierarchy
# -----------------------------------------------------------------------------
class PiaBridgeError(Exception):
    """Base error for all Lodestar PIA bridge operations."""


class WrongAccountError(PiaBridgeError):
    """Raised when AWS caller identity does not match authorized account ID."""


class RoleAssumptionError(PiaBridgeError):
    """Raised when STS AssumeRole fails."""


class MfaRequiredError(RoleAssumptionError):
    """Raised when STS AssumeRole fails due to missing or expired MFA authorization."""


class SecretNotFoundError(PiaBridgeError):
    """Raised when the requested secret does not exist in Secrets Manager."""


class SecretAccessDeniedError(PiaBridgeError):
    """Raised when access to an AWS secret is denied or unauthorized."""


class InvalidSecretJsonError(PiaBridgeError):
    """Raised when the secret payload is not valid JSON."""


class InvalidCredentialsError(PiaBridgeError):
    """Raised when secret JSON is missing PIA_USERNAME or PIA_PASSWORD."""


class PiaAuthenticationError(PiaBridgeError):
    """Raised when PIA API rejects credentials or returns an error status."""


class HostKeyMismatchError(PiaBridgeError):
    """Raised when SSH host key verification fails."""


class SshConnectionError(PiaBridgeError):
    """Raised when SSH connection to the Linode fails."""


class RemotePermissionError(PiaBridgeError):
    """Raised when remote filesystem permissions on /run/lodestar/vpn are insecure."""


class VpnConnectionError(PiaBridgeError):
    """Raised when the VPN container fails to establish an active tunnel."""


class LeakDetectedError(PiaBridgeError):
    """Raised when independent verification detects a public IP or DNS leak."""


# -----------------------------------------------------------------------------
# Pure-Python Curve25519 Engine (RFC 7748)
# -----------------------------------------------------------------------------
_P = 2**255 - 19
_A24 = 121665


def _clamp(k: bytes) -> bytes:
    arr = bytearray(k)
    arr[0] &= 248
    arr[31] &= 127
    arr[31] |= 64
    return bytes(arr)


def _cswap(swap: int, x_2: int, x_3: int) -> Tuple[int, int]:
    dummy = swap * ((x_2 - x_3) % _P)
    return (x_2 - dummy) % _P, (x_3 + dummy) % _P


def _x25519_scalar_mult(scalar: bytes, u_coord: int = 9) -> bytes:
    """RFC 7748 Montgomery ladder scalar multiplication on Curve25519."""
    k = int.from_bytes(scalar, "little")
    x_1 = u_coord
    x_2 = 1
    z_2 = 0
    x_3 = u_coord
    z_3 = 1
    swap = 0
    for t in reversed(range(255)):
        k_t = (k >> t) & 1
        swap ^= k_t
        x_2, x_3 = _cswap(swap, x_2, x_3)
        z_2, z_3 = _cswap(swap, z_2, z_3)
        swap = k_t
        a = (x_2 + z_2) % _P
        aa = (a * a) % _P
        b = (x_2 - z_2) % _P
        bb = (b * b) % _P
        e = (aa - bb) % _P
        c = (x_3 + z_3) % _P
        d = (x_3 - z_3) % _P
        da = (d * a) % _P
        cb = (c * b) % _P
        x_3 = pow(da + cb, 2, _P)
        z_3 = (x_1 * pow(da - cb, 2, _P)) % _P
        x_2 = (aa * bb) % _P
        z_2 = (e * (aa + _A24 * e)) % _P
    x_2, x_3 = _cswap(swap, x_2, x_3)
    z_2, z_3 = _cswap(swap, z_2, z_3)
    res = (x_2 * pow(z_2, _P - 2, _P)) % _P
    return res.to_bytes(32, "little")


def generate_wireguard_keypair() -> Tuple[str, str]:
    """Generates an ephemeral WireGuard private key and derives its public key (base64)."""
    raw_priv = _clamp(os.urandom(32))
    raw_pub = _x25519_scalar_mult(raw_priv, 9)
    priv_b64 = base64.b64encode(raw_priv).decode("ascii")
    pub_b64 = base64.b64encode(raw_pub).decode("ascii")
    return priv_b64, pub_b64


def _resolve_aws_cli() -> str:
    """Finds the AWS CLI executable on PATH or default Windows installation paths."""
    import shutil
    found = shutil.which("aws")
    if found:
        return found
    default_win = r"C:\Program Files\Amazon\AWSCLIV2\aws.exe"
    if os.path.exists(default_win):
        return default_win
    raise FileNotFoundError("AWS CLI not found on PATH or default installation directory.")


# -----------------------------------------------------------------------------
# AWS Secret Retrieval and Identity Validation
# -----------------------------------------------------------------------------
class AwsSecretClient:
    """Safely retrieves PIA credentials from AWS Secrets Manager using STS AssumeRole."""

    def __init__(
        self,
        account_id: Optional[str] = None,
        region: str = DEFAULT_AWS_REGION,
        profile: str = "lodestar-admin",
        role_arn: Optional[str] = None,
        sts_client: Any = None,
        sm_client: Any = None,
    ) -> None:
        self.account_id = account_id or DEFAULT_AWS_ACCOUNT
        self.region = region
        self.profile = profile
        self.role_arn = role_arn or (f"arn:aws:iam::{self.account_id}:role/{ROLE_NAME}" if self.account_id else "")
        self._sts_client = sts_client
        self._sm_client = sm_client
        self._temporary_credentials: Optional[Dict[str, str]] = None

    def validate_identity(self) -> str:
        """Validates that the active AWS caller identity matches the authorized account."""
        if not self.account_id or not re.match(r"^[0-9]{12}$", self.account_id):
            raise WrongAccountError(
                "Set LODESTAR_AWS_ACCOUNT_ID or pass --account-id with the approved 12-digit AWS account ID before AWS operations."
            )
        if self._sts_client is not None:
            identity = self._sts_client.get_caller_identity()
            actual_account = identity.get("Account", "")
            if actual_account != self.account_id:
                raise WrongAccountError(
                    f"AWS account mismatch: caller is {actual_account}, expected {self.account_id}"
                )
            return identity.get("Arn", "")

        # Use AWS CLI if boto3 is not supplied
        try:
            aws_bin = _resolve_aws_cli()
        except FileNotFoundError:
            raise RoleAssumptionError("AWS CLI not found on PATH. Install AWS CLI or supply boto3 client.")

        cmd = [
            aws_bin,
            "sts",
            "get-caller-identity",
            "--profile",
            self.profile,
            "--output",
            "json",
        ]
        try:
            res = subprocess.run(cmd, capture_output=True, text=True, check=True)
            data = json.loads(res.stdout)
            actual_account = data.get("Account", "")
            if actual_account != self.account_id:
                raise WrongAccountError(
                    f"AWS account mismatch: caller is {actual_account}, expected {self.account_id}"
                )
            return data.get("Arn", "")
        except subprocess.CalledProcessError as exc:
            err = exc.stderr or ""
            if "expired" in err.lower() or "mfa" in err.lower() or "token" in err.lower():
                raise MfaRequiredError(f"AWS authentication expired or requires MFA: {err.strip()}")
            raise RoleAssumptionError(f"Failed to get caller identity: {err.strip()}")

    def assume_reader_role(self, mfa_token: Optional[str] = None) -> Dict[str, str]:
        """Assumes the restricted LodestarPiaSecretReader role for at most 1 hour."""
        self.validate_identity()

        if self._sts_client is not None:
            kwargs: Dict[str, Any] = {
                "RoleArn": self.role_arn,
                "RoleSessionName": "LodestarPiaBridgeSession",
                "DurationSeconds": 900,
            }
            if mfa_token:
                # If MFA token provided
                kwargs["TokenCode"] = mfa_token
            try:
                assumed = self._sts_client.assume_role(**kwargs)
                creds = assumed["Credentials"]
                self._temporary_credentials = {
                    "AccessKeyId": creds["AccessKeyId"],
                    "SecretAccessKey": creds["SecretAccessKey"],
                    "SessionToken": creds["SessionToken"],
                }
                return self._temporary_credentials
            except Exception as exc:
                err_str = str(exc)
                if "MFA" in err_str or "MultiFactor" in err_str:
                    raise MfaRequiredError(f"MFA authorization required for {self.role_arn}: {err_str}")
                raise RoleAssumptionError(f"Failed to assume role {self.role_arn}: {err_str}")

        aws_bin = _resolve_aws_cli()
        cmd = [
            aws_bin,
            "sts",
            "assume-role",
            "--role-arn",
            self.role_arn,
            "--role-session-name",
            "LodestarPiaBridgeSession",
            "--duration-seconds",
            "900",
            "--profile",
            self.profile,
            "--output",
            "json",
        ]
        if mfa_token:
            cmd.extend(["--token-code", mfa_token])

        try:
            res = subprocess.run(cmd, capture_output=True, text=True, check=True)
            data = json.loads(res.stdout)
            creds = data.get("Credentials", {})
            self._temporary_credentials = {
                "AccessKeyId": creds.get("AccessKeyId", ""),
                "SecretAccessKey": creds.get("SecretAccessKey", ""),
                "SessionToken": creds.get("SessionToken", ""),
            }
            return self._temporary_credentials
        except subprocess.CalledProcessError as exc:
            err = exc.stderr or ""
            if "mfa" in err.lower() or "token" in err.lower():
                raise MfaRequiredError(f"MFA authorization required to assume {self.role_arn}: {err.strip()}")
            raise RoleAssumptionError(f"Failed to assume role {self.role_arn}: {err.strip()}")

    def get_pia_credentials(self, secret_id: str = SECRET_NAME) -> Tuple[str, str]:
        """
        Retrieves and validates PIA credentials from Secrets Manager.
        Ensures only the authorized PIA secret is requested and keys are non-empty.
        """
        # Strict scope enforcement: reject unauthorized secret identifiers
        expected_arn_prefix = f"arn:aws:secretsmanager:{self.region}:{self.account_id}:secret:{SECRET_NAME}"
        if secret_id != SECRET_NAME and not secret_id.startswith(expected_arn_prefix):
            raise SecretAccessDeniedError(
                f"Access denied: unauthorized secret '{secret_id}'. Only '{SECRET_NAME}' is permitted."
            )

        if not self._temporary_credentials:
            self.assume_reader_role()

        secret_str: Optional[str] = None
        if self._sm_client is not None:
            try:
                res = self._sm_client.get_secret_value(SecretId=secret_id)
                secret_str = res.get("SecretString")
            except Exception as exc:
                err_str = str(exc)
                if "ResourceNotFoundException" in err_str or "does not exist" in err_str:
                    raise SecretNotFoundError(f"Secret '{secret_id}' not found in Secrets Manager")
                if "AccessDeniedException" in err_str or "not authorized" in err_str:
                    raise SecretAccessDeniedError(f"Access denied retrieving secret '{secret_id}': {err_str}")
                raise PiaBridgeError(f"Failed to retrieve secret '{secret_id}': {err_str}")
        else:
            aws_bin = _resolve_aws_cli()
            env = os.environ.copy()
            if self._temporary_credentials:
                env["AWS_ACCESS_KEY_ID"] = self._temporary_credentials["AccessKeyId"]
                env["AWS_SECRET_ACCESS_KEY"] = self._temporary_credentials["SecretAccessKey"]
                env["AWS_SESSION_TOKEN"] = self._temporary_credentials["SessionToken"]
                env["AWS_DEFAULT_REGION"] = self.region

            cmd = [
                aws_bin,
                "secretsmanager",
                "get-secret-value",
                "--secret-id",
                secret_id,
                "--region",
                self.region,
                "--output",
                "json",
            ]
            try:
                res = subprocess.run(cmd, capture_output=True, text=True, check=True, env=env)
                data = json.loads(res.stdout)
                secret_str = data.get("SecretString")
            except subprocess.CalledProcessError as exc:
                err = exc.stderr or ""
                if "ResourceNotFoundException" in err or "does not exist" in err:
                    raise SecretNotFoundError(f"Secret '{secret_id}' not found in Secrets Manager")
                if "AccessDeniedException" in err or "AccessDenied" in err:
                    raise SecretAccessDeniedError(f"Access denied retrieving secret '{secret_id}': {err.strip()}")
                raise PiaBridgeError(f"Failed to retrieve secret '{secret_id}': {err.strip()}")

        if not secret_str:
            raise SecretNotFoundError(f"Secret '{secret_id}' is empty or has no SecretString")

        try:
            payload = json.loads(secret_str)
        except json.JSONDecodeError as exc:
            raise InvalidSecretJsonError(f"Secret payload is not valid JSON: {exc}")

        if not isinstance(payload, dict):
            raise InvalidSecretJsonError("Secret JSON root must be an object")

        user = payload.get("PIA_USERNAME")
        password = payload.get("PIA_PASSWORD")

        if not user or not isinstance(user, str) or not user.strip():
            raise InvalidCredentialsError("Missing or empty required key 'PIA_USERNAME' in secret")
        if not password or not isinstance(password, str) or not password.strip():
            raise InvalidCredentialsError("Missing or empty required key 'PIA_PASSWORD' in secret")

        return user.strip(), password.strip()


# -----------------------------------------------------------------------------
# PIA WireGuard API Negotiation
# -----------------------------------------------------------------------------
class PiaWireGuardClient:
    """Negotiates ephemeral WireGuard tunnel parameters with PIA's manual-connection API."""

    def __init__(self, ca_cert_pem: str = PIA_ROOT_CA_PEM) -> None:
        self.ca_cert_pem = ca_cert_pem

    def get_auth_token(self, username: str, password: str) -> str:
        """Authenticates to PIA gtoken API with HTTP Basic Auth, returning a 24-hour token."""
        auth_bytes = f"{username}:{password}".encode("utf-8")
        auth_header = "Basic " + base64.b64encode(auth_bytes).decode("ascii")

        req = urllib.request.Request(PIA_TOKEN_URL, headers={"Authorization": auth_header})
        ssl_ctx = ssl.create_default_context()

        try:
            with urllib.request.urlopen(req, timeout=15, context=ssl_ctx) as resp:
                data = json.loads(resp.read().decode("utf-8"))
        except urllib.error.HTTPError as exc:
            if exc.code in (401, 403):
                raise PiaAuthenticationError(f"PIA credentials rejected by API (HTTP {exc.code})")
            raise PiaAuthenticationError(f"PIA token request failed with HTTP {exc.code}")
        except Exception as exc:
            raise PiaAuthenticationError(f"Connection failure contacting PIA token API: {exc}")

        if data.get("status") != "OK" or not data.get("token"):
            raise PiaAuthenticationError(f"PIA token API returned non-OK status: {data.get('status')}")

        return data["token"]

    def resolve_region(self, region_id: str) -> Dict[str, str]:
        """Resolves region ID to server metadata, ensuring East Coast location for Newark."""
        norm = region_id.strip().lower().replace("_", "-")
        if norm not in PIA_REGION_SERVERS:
            supported = ", ".join(sorted(PIA_REGION_SERVERS.keys()))
            raise PiaBridgeError(f"Unsupported PIA region '{region_id}'. Supported regions: {supported}")
        return PIA_REGION_SERVERS[norm]

    def register_key(
        self,
        server_ip: str,
        server_hostname: str,
        pia_token: str,
        public_key: str,
    ) -> Dict[str, Any]:
        """Registers the ephemeral public key on the selected PIA WireGuard endpoint."""
        query = urllib.parse.urlencode({"pt": pia_token, "pubkey": public_key})
        url = f"https://{server_hostname}:1337/addKey?{query}"

        ssl_ctx = ssl.create_default_context()
        ssl_ctx.load_verify_locations(cdata=self.ca_cert_pem)

        req = urllib.request.Request(url, headers={"Host": server_hostname})
        try:
            with urllib.request.urlopen(req, timeout=15, context=ssl_ctx) as resp:
                data = json.loads(resp.read().decode("utf-8"))
        except Exception as exc:
            raise VpnConnectionError(f"Failed to register WireGuard key with PIA endpoint {server_hostname}: {exc}")

        if data.get("status") != "OK":
            raise VpnConnectionError(f"PIA WireGuard endpoint returned failure status: {data.get('status')}")

        return {
            "peer_ip": data.get("peer_ip"),
            "server_key": data.get("server_key"),
            "server_port": data.get("server_port", 1337),
            "server_ip": server_ip,
            "dns_servers": data.get("dns_servers", ["10.0.0.242"]),
        }

    def build_wg0_conf(
        self,
        private_key: str,
        peer_ip: str,
        server_key: str,
        server_ip: str,
        server_port: int,
        dns_servers: List[str],
    ) -> str:
        """Formats standard WireGuard configuration text."""
        dns_line = f"DNS = {dns_servers[0]}" if dns_servers else "DNS = 10.0.0.242"
        return f"""[Interface]
Address = {peer_ip}/32
PrivateKey = {private_key}
{dns_line}

[Peer]
PublicKey = {server_key}
Endpoint = {server_ip}:{server_port}
AllowedIPs = 0.0.0.0/0
PersistentKeepalive = 25
"""


# -----------------------------------------------------------------------------
# Host-Key Verified SSH Provisioner & Leak Inspector
# -----------------------------------------------------------------------------
class LinodeSshProvisioner:
    """Manages secure transfer, permission verification, and leak checks on target Linodes."""

    def __init__(
        self,
        target_ip: str,
        user: str = REMOTE_DEFAULT_USER,
        key_path: str = REMOTE_DEFAULT_KEY,
        strict_host_key: bool = True,
        known_hosts_file: Optional[str] = None,
    ) -> None:
        if not re.match(r"^[a-zA-Z0-9.-]+$", target_ip):
            raise ValueError(f"Invalid target IP/hostname format: {target_ip}")
        self.target_ip = target_ip
        self.user = user
        self.key_path = os.path.expanduser(key_path)
        self.strict_host_key = strict_host_key
        self.known_hosts_file = known_hosts_file

    def _build_ssh_cmd(self, remote_command: Optional[str] = None) -> List[str]:
        cmd = [
            "ssh",
            "-i",
            self.key_path,
            "-o",
            "BatchMode=yes",
            "-o",
            f"StrictHostKeyChecking={'yes' if self.strict_host_key else 'accept-new'}",
            "-o",
            "ConnectTimeout=10",
        ]
        if self.known_hosts_file:
            cmd.extend(["-o", f"UserKnownHostsFile={self.known_hosts_file}"])
        cmd.append(f"{self.user}@{self.target_ip}")
        if remote_command:
            cmd.append(remote_command)
        return cmd

    def verify_host_connectivity(self) -> None:
        """Verifies SSH host connectivity and strict host key match."""
        cmd = self._build_ssh_cmd("true")
        try:
            subprocess.run(cmd, capture_output=True, text=True, check=True)
        except subprocess.CalledProcessError as exc:
            err = (exc.stderr or "").strip()
            if "host key verification failed" in err.lower() or "remote host identification has changed" in err.lower():
                raise HostKeyMismatchError(f"SSH host key verification failed for {self.target_ip}: {err}")
            raise SshConnectionError(f"Failed to connect to {self.target_ip} via SSH: {err}")
        except FileNotFoundError:
            raise SshConnectionError("ssh command not available on local PATH")

    def provision_wg0_config(self, wg0_conf: str) -> None:
        """
        Transfers wg0.conf via standard input directly into /run/lodestar/vpn/wg0.conf.
        Enforces 0700 directory permissions and 0600 root-only file permissions.
        """
        self.verify_host_connectivity()

        # Remote install command reads from stdin; file contents never appear in process arguments
        remote_script = (
            f"sudo install -d -m 0700 -o root -g root {REMOTE_CONFIG_DIR} && "
            f"sudo install -m 0600 -o root -g root /dev/stdin {REMOTE_CONFIG_FILE}"
        )
        cmd = self._build_ssh_cmd(remote_script)

        try:
            subprocess.run(
                cmd,
                input=wg0_conf,
                capture_output=True,
                text=True,
                check=True,
            )
        except subprocess.CalledProcessError as exc:
            raise SshConnectionError(f"Failed to stream wg0.conf to {self.target_ip}: {exc.stderr.strip()}")

        self.verify_remote_permissions()

    def verify_remote_permissions(self) -> None:
        """Confirms that remote runtime credentials are strictly root-only (0600 / 0700)."""
        check_script = (
            f"stat -c '%a %U:%G' {REMOTE_CONFIG_DIR} && "
            f"stat -c '%a %U:%G' {REMOTE_CONFIG_FILE}"
        )
        cmd = self._build_ssh_cmd(check_script)
        try:
            res = subprocess.run(cmd, capture_output=True, text=True, check=True)
            lines = res.stdout.strip().splitlines()
            if len(lines) < 2:
                raise RemotePermissionError("Incomplete stat output for remote VPN files")

            dir_stat, file_stat = lines[0].strip(), lines[1].strip()
            if dir_stat != "700 root:root":
                raise RemotePermissionError(
                    f"Insecure permissions on {REMOTE_CONFIG_DIR}: expected '700 root:root', got '{dir_stat}'"
                )
            if file_stat != "600 root:root":
                raise RemotePermissionError(
                    f"Insecure permissions on {REMOTE_CONFIG_FILE}: expected '600 root:root', got '{file_stat}'"
                )
        except subprocess.CalledProcessError as exc:
            raise RemotePermissionError(f"Failed to verify remote file permissions: {exc.stderr.strip()}")

    def cleanup_runtime_credentials(self) -> None:
        """Securely removes runtime secrets from the Linode's tmpfs."""
        self.verify_host_connectivity()
        cleanup_script = (
            f"if [ -d {REMOTE_CONFIG_DIR} ]; then "
            f"  sudo rm -rf {REMOTE_CONFIG_DIR}; "
            f"fi"
        )
        cmd = self._build_ssh_cmd(cleanup_script)
        try:
            subprocess.run(cmd, capture_output=True, text=True, check=True)
        except subprocess.CalledProcessError as exc:
            raise SshConnectionError(f"Failed to clean up runtime credentials on {self.target_ip}: {exc.stderr.strip()}")

    def start_vpn(self, compose_file: str = "/home/lodestar/lodestar/pia-example/compose.vpn.yaml") -> None:
        """Starts the Gluetun container via Docker Compose."""
        safe_path = shlex.quote(compose_file)
        cmd = self._build_ssh_cmd(f"docker compose -f {safe_path} up -d gluetun")
        try:
            subprocess.run(cmd, capture_output=True, text=True, check=True)
        except subprocess.CalledProcessError as exc:
            raise VpnConnectionError(f"Failed to start Gluetun container: {exc.stderr.strip()}")

    def verify_leak_protection(self, max_retries: int = 6, delay_s: int = 5) -> Dict[str, str]:
        """
        Independently validates that egress traffic routes exclusively through PIA VPN
        and that public IP and DNS resolution do not leak host details.
        """
        # Step 1: Query Linode host public IP
        host_ip_cmd = self._build_ssh_cmd("curl -s --max-time 5 https://ipinfo.io/ip || curl -s --max-time 5 https://icanhazip.com")
        try:
            res = subprocess.run(host_ip_cmd, capture_output=True, text=True, check=True)
            host_ip = res.stdout.strip()
        except subprocess.CalledProcessError:
            host_ip = self.target_ip

        # Step 2: Query public IP from INSIDE Gluetun container
        vpn_ip: Optional[str] = None
        probe_cmd = self._build_ssh_cmd(
            "docker exec gluetun wget -qO- --timeout=5 https://ipinfo.io/ip || "
            "docker exec gluetun wget -qO- --timeout=5 https://icanhazip.com"
        )

        for attempt in range(1, max_retries + 1):
            try:
                res = subprocess.run(probe_cmd, capture_output=True, text=True, check=True)
                raw_ip = res.stdout.strip()
                if raw_ip and re.match(r"^\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}$", raw_ip):
                    vpn_ip = raw_ip
                    break
            except subprocess.CalledProcessError:
                pass
            time.sleep(delay_s)

        if not vpn_ip:
            raise VpnConnectionError("Gluetun container has no outbound VPN connectivity or timed out")

        # Step 3: Leak check - VPN egress must NOT match Linode host public IP
        if vpn_ip == host_ip or vpn_ip == self.target_ip:
            raise LeakDetectedError(
                f"LEAK DETECTED: Container egress IP ({vpn_ip}) matches Linode host public IP ({host_ip})!"
            )

        # Step 4: Validate DNS query originates inside VPN network
        dns_cmd = self._build_ssh_cmd(
            "docker exec gluetun nslookup -timeout=5 google.com 127.0.0.1 >/dev/null 2>&1 && echo OK"
        )
        try:
            res = subprocess.run(dns_cmd, capture_output=True, text=True, check=True)
            if "OK" not in res.stdout:
                raise LeakDetectedError("Container DNS lookup did not resolve through Gluetun local resolver")
        except subprocess.CalledProcessError:
            raise LeakDetectedError("Container DNS lookup failed; leak protection incomplete")

        return {
            "status": "protected",
            "host_ip": host_ip,
            "vpn_ip": vpn_ip,
        }

    def start_crawler(self, compose_file: str = "/home/lodestar/lodestar/pia-example/compose.vpn.yaml") -> None:
        """Starts the crawler container only after leak protection has been independently proven."""
        leak_info = self.verify_leak_protection()
        logger.info(f"Leak verification passed: egress IP is {leak_info['vpn_ip']} (Host is {leak_info['host_ip']})")

        safe_path = shlex.quote(compose_file)
        cmd = self._build_ssh_cmd(f"docker compose -f {safe_path} up -d crawler")
        try:
            subprocess.run(cmd, capture_output=True, text=True, check=True)
        except subprocess.CalledProcessError as exc:
            raise VpnConnectionError(f"Failed to start crawler service: {exc.stderr.strip()}")


# -----------------------------------------------------------------------------
# End-to-End Orchestrator
# -----------------------------------------------------------------------------
def run_bridge_workflow(
    target_ip: str,
    pia_region: str = DEFAULT_PIA_REGION,
    aws_profile: str = "lodestar-admin",
    aws_account: str = DEFAULT_AWS_ACCOUNT,
    ssh_user: str = REMOTE_DEFAULT_USER,
    ssh_key: str = REMOTE_DEFAULT_KEY,
    compose_file: str = "/home/lodestar/lodestar/pia-example/compose.vpn.yaml",
    start_crawler_workload: bool = False,
    cleanup_only: bool = False,
) -> Dict[str, Any]:
    """Coordinates AWS secret retrieval, WireGuard negotiation, provisioning, and leak verification."""
    provisioner = LinodeSshProvisioner(target_ip=target_ip, user=ssh_user, key_path=ssh_key)

    if cleanup_only:
        provisioner.cleanup_runtime_credentials()
        return {"action": "cleanup", "status": "completed", "target": target_ip}

    # Step 1: Retrieve PIA secret from AWS Secrets Manager
    secret_client = AwsSecretClient(account_id=aws_account, profile=aws_profile)
    pia_user, pia_pass = secret_client.get_pia_credentials()

    # Step 2: Negotiate WireGuard config via PIA REST API
    pia_client = PiaWireGuardClient()
    region_info = pia_client.resolve_region(pia_region)
    token = pia_client.get_auth_token(pia_user, pia_pass)
    privkey, pubkey = generate_wireguard_keypair()
    tunnel_params = pia_client.register_key(
        server_ip=region_info["ip"],
        server_hostname=region_info["hostname"],
        pia_token=token,
        public_key=pubkey,
    )
    wg0_conf = pia_client.build_wg0_conf(
        private_key=privkey,
        peer_ip=tunnel_params["peer_ip"],
        server_key=tunnel_params["server_key"],
        server_ip=tunnel_params["server_ip"],
        server_port=tunnel_params["server_port"],
        dns_servers=tunnel_params["dns_servers"],
    )

    # Step 3: Stream wg0.conf to Linode tmpfs (/run/lodestar/vpn/wg0.conf)
    provisioner.provision_wg0_config(wg0_conf)

    # Step 4: Start Gluetun container
    provisioner.start_vpn(compose_file)

    # Step 5: Verify leak protection
    leak_res = provisioner.verify_leak_protection()

    # Step 6: Optionally start crawler
    if start_crawler_workload:
        provisioner.start_crawler(compose_file)

    return {
        "status": "success",
        "target": target_ip,
        "region": pia_region,
        "vpn_ip": leak_res["vpn_ip"],
        "crawler_started": start_crawler_workload,
    }


# -----------------------------------------------------------------------------
# CLI Entrypoint
# -----------------------------------------------------------------------------
def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Lodestar PIA VPN Secure Bridge for Akamai Linodes",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    subparsers = parser.add_subparsers(dest="command", required=True)

    # 1. verify-aws
    p_verify = subparsers.add_parser("verify-aws", help="Verify AWS identity and restricted secret access")
    p_verify.add_argument("--profile", default="lodestar-admin", help="AWS administrative profile")
    p_verify.add_argument("--account-id", default=DEFAULT_AWS_ACCOUNT, help="Authorized AWS Account ID")
    p_verify.add_argument("--secret-id", default=SECRET_NAME, help="Secrets Manager Secret ID")

    # 2. generate-config
    p_gen = subparsers.add_parser("generate-config", help="Generate WireGuard config locally without provisioning")
    p_gen.add_argument("--pia-region", default=DEFAULT_PIA_REGION, help="PIA VPN region (e.g. us-east, us-new-jersey)")
    p_gen.add_argument("--profile", default="lodestar-admin", help="AWS administrative profile")
    p_gen.add_argument("--account-id", default=DEFAULT_AWS_ACCOUNT, help="Authorized AWS Account ID")
    p_gen.add_argument("--output", help="Optional local path to save generated config")

    # 3. provision
    p_prov = subparsers.add_parser("provision", help="Provision wg0.conf to target Linode /run tmpfs")
    p_prov.add_argument("--target", required=True, help="Target Linode IP address")
    p_prov.add_argument("--ssh-user", default=REMOTE_DEFAULT_USER, help="SSH username")
    p_prov.add_argument("--ssh-key", default=REMOTE_DEFAULT_KEY, help="Path to Ed25519 private key")
    p_prov.add_argument("--pia-region", default=DEFAULT_PIA_REGION, help="PIA VPN region")
    p_prov.add_argument("--profile", default="lodestar-admin", help="AWS administrative profile")
    p_prov.add_argument("--account-id", default=DEFAULT_AWS_ACCOUNT, help="Authorized AWS Account ID")

    # 4. start-vpn
    p_vpn = subparsers.add_parser("start-vpn", help="Start Gluetun container on target Linode")
    p_vpn.add_argument("--target", required=True, help="Target Linode IP address")
    p_vpn.add_argument("--ssh-user", default=REMOTE_DEFAULT_USER, help="SSH username")
    p_vpn.add_argument("--ssh-key", default=REMOTE_DEFAULT_KEY, help="Path to Ed25519 private key")
    p_vpn.add_argument("--compose-file", default="/home/lodestar/lodestar/pia-example/compose.vpn.yaml")

    # 5. verify-tunnel
    p_leak = subparsers.add_parser("verify-tunnel", help="Independently verify VPN egress and leak protection")
    p_leak.add_argument("--target", required=True, help="Target Linode IP address")
    p_leak.add_argument("--ssh-user", default=REMOTE_DEFAULT_USER, help="SSH username")
    p_leak.add_argument("--ssh-key", default=REMOTE_DEFAULT_KEY, help="Path to Ed25519 private key")

    # 6. start-crawler
    p_crawl = subparsers.add_parser("start-crawler", help="Start crawler container after proving leak protection")
    p_crawl.add_argument("--target", required=True, help="Target Linode IP address")
    p_crawl.add_argument("--ssh-user", default=REMOTE_DEFAULT_USER, help="SSH username")
    p_crawl.add_argument("--ssh-key", default=REMOTE_DEFAULT_KEY, help="Path to Ed25519 private key")
    p_crawl.add_argument("--compose-file", default="/home/lodestar/lodestar/pia-example/compose.vpn.yaml")

    # 7. health
    p_health = subparsers.add_parser("health", help="Check VPN container and tunnel status")
    p_health.add_argument("--target", required=True, help="Target Linode IP address")
    p_health.add_argument("--ssh-user", default=REMOTE_DEFAULT_USER, help="SSH username")
    p_health.add_argument("--ssh-key", default=REMOTE_DEFAULT_KEY, help="Path to Ed25519 private key")

    # 8. cleanup
    p_clean = subparsers.add_parser("cleanup", help="Remove runtime secrets and stop containers")
    p_clean.add_argument("--target", required=True, help="Target Linode IP address")
    p_clean.add_argument("--ssh-user", default=REMOTE_DEFAULT_USER, help="SSH username")
    p_clean.add_argument("--ssh-key", default=REMOTE_DEFAULT_KEY, help="Path to Ed25519 private key")

    return parser


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()
    logging.basicConfig(level=logging.INFO, format="%(asctime)s [%(levelname)s] %(message)s")

    try:
        if args.command == "verify-aws":
            client = AwsSecretClient(account_id=args.account_id, profile=args.profile)
            arn = client.validate_identity()
            print(f"[OK] Identity verified: {arn}")
            user, _ = client.get_pia_credentials(args.secret_id)
            print(f"[OK] Secret '{args.secret_id}' retrieved and validated successfully (User: {user[:3]}***)")
            return 0

        if args.command == "generate-config":
            secret_client = AwsSecretClient(account_id=args.account_id, profile=args.profile)
            pia_user, pia_pass = secret_client.get_pia_credentials()
            pia_client = PiaWireGuardClient()
            reg = pia_client.resolve_region(args.pia_region)
            token = pia_client.get_auth_token(pia_user, pia_pass)
            priv, pub = generate_wireguard_keypair()
            params = pia_client.register_key(reg["ip"], reg["hostname"], token, pub)
            cfg = pia_client.build_wg0_conf(
                private_key=priv,
                peer_ip=params["peer_ip"],
                server_key=params["server_key"],
                server_ip=params["server_ip"],
                server_port=params["server_port"],
                dns_servers=params["dns_servers"],
            )
            if args.output:
                with open(args.output, "w", encoding="utf-8") as f:
                    f.write(cfg)
                print(f"[OK] WireGuard configuration saved to {args.output}")
            else:
                print(cfg)
            return 0

        if args.command == "provision":
            res = run_bridge_workflow(
                target_ip=args.target,
                pia_region=args.pia_region,
                aws_profile=args.profile,
                aws_account=args.account_id,
                ssh_user=args.ssh_user,
                ssh_key=args.ssh_key,
                start_crawler_workload=False,
            )
            print(f"[OK] Provisioned and verified: target={res['target']}, vpn_ip={res['vpn_ip']}")
            return 0

        if args.command == "start-vpn":
            prov = LinodeSshProvisioner(target_ip=args.target, user=args.ssh_user, key_path=args.ssh_key)
            prov.start_vpn(args.compose_file)
            print(f"[OK] Gluetun started on {args.target}")
            return 0

        if args.command == "verify-tunnel":
            prov = LinodeSshProvisioner(target_ip=args.target, user=args.ssh_user, key_path=args.ssh_key)
            leak = prov.verify_leak_protection()
            print(f"[OK] Tunnel active and protected: egress={leak['vpn_ip']} (host={leak['host_ip']})")
            return 0

        if args.command == "start-crawler":
            prov = LinodeSshProvisioner(target_ip=args.target, user=args.ssh_user, key_path=args.ssh_key)
            prov.start_crawler(args.compose_file)
            print(f"[OK] Crawler started behind verified VPN on {args.target}")
            return 0

        if args.command == "health":
            prov = LinodeSshProvisioner(target_ip=args.target, user=args.ssh_user, key_path=args.ssh_key)
            leak = prov.verify_leak_protection()
            print(f"[HEALTH OK] Egress: {leak['vpn_ip']}, DNS: internal resolver verified")
            return 0

        if args.command == "cleanup":
            prov = LinodeSshProvisioner(target_ip=args.target, user=args.ssh_user, key_path=args.ssh_key)
            prov.cleanup_runtime_credentials()
            print(f"[OK] Runtime credentials cleaned up on {args.target}")
            return 0

    except PiaBridgeError as exc:
        print(f"[ERROR] {exc.__class__.__name__}: {exc}", file=sys.stderr)
        return 1
    except Exception as exc:
        print(f"[UNEXPECTED ERROR] {exc}", file=sys.stderr)
        return 2

    return 0


if __name__ == "__main__":
    sys.exit(main())
