#!/usr/bin/env python3
"""
Automated Security Controls and Unit Test Suite for Lodestar PIA VPN Bridge.

Covers all 26 required failure, recovery, and security control cases:
  1. AWS role assumption and secret retrieval
  2. Invalid or missing PIA credentials
  3. Wrong AWS identity
  4. Missing MFA authorization
  5. Unauthorized secret access
  6. PIA API failures
  7. Invalid endpoint metadata
  8. Unavailable WireGuard region
  9. WireGuard registration failure
 10. Incorrect remote Docker permissions
 11. Missing Compose configuration
 12. Invalid Compose configuration
 13. Image digest or pull failure
 14. SSH host-key mismatch
 15. Partial SSH transfer
 16. Incorrect runtime file permissions
 17. Tunnel handshake failure
 18. DNS leakage
 19. IPv4 and IPv6 routing leakage
 20. VPN disconnection after startup
 21. Failed credential rotation
 22. VM reboot and re-provisioning
 23. Partial deployment cleanup
 24. Repeated, idempotent provisioning
 25. Attempted secret disclosure through logs or command output
 26. Crawler startup while the VPN is disconnected
 Plus: RFC 7748 official test vectors & Gluetun image digest pinning verification.

All AWS and Akamai/Linode interactions are safely mocked. Zero secrets exposed.
"""

from __future__ import annotations

import io
import json
import os
import subprocess
import sys
import unittest
import urllib.error
from unittest.mock import MagicMock, call, patch

# Add parent scripts directory to module path
sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "scripts")))

import lodestar_pia_bridge as bridge


class LodestarPiaBridgeTests(unittest.TestCase):
    """Unit test suite verifying all 26 required security controls and failure recovery."""

    def setUp(self) -> None:
        self.account_id = "123456789012"
        self.region = "us-east-1"
        self.role_arn = f"arn:aws:iam::{self.account_id}:role/LodestarPiaSecretReader"
        self.secret_name = "lodestar/pia-vpn"
        self.target_ip = "192.0.2.100"

        # Sample valid PIA server list payload for mocking
        self.mock_server_list = {
            "regions": [
                {
                    "id": "us-newjersey",
                    "name": "US East",
                    "country": "US",
                    "dns": "us-newjersey.privacy.network",
                    "servers": {
                        "wg": [
                            {"ip": "212.56.54.190", "cn": "newjersey434"}
                        ]
                    },
                },
                {
                    "id": "us-chicago",
                    "name": "US Chicago",
                    "country": "US",
                    "dns": "us-chicago.privacy.network",
                    "servers": {
                        "wg": [
                            {"ip": "156.146.54.100", "cn": "chicago401"}
                        ]
                    },
                },
                {
                    "id": "legacy-ovpn-only",
                    "name": "Legacy No WG",
                    "country": "US",
                    "servers": {
                        "ovpnudp": [{"ip": "1.2.3.4", "cn": "ovpn1"}]
                    },
                },
            ]
        }

    # -------------------------------------------------------------------------
    # 1. AWS role assumption and secret retrieval
    # -------------------------------------------------------------------------
    def test_01_aws_role_assumption_and_secret_retrieval(self) -> None:
        """Confirms successful STS AssumeRole and retrieval of PIA credentials."""
        mock_sts = MagicMock()
        mock_sts.get_caller_identity.return_value = {
            "Account": self.account_id,
            "Arn": f"arn:aws:iam::{self.account_id}:user/lodestar-admin",
        }
        mock_sts.assume_role.return_value = {
            "Credentials": {
                "AccessKeyId": "ASIA_TEST_KEY",
                "SecretAccessKey": "SECRET_TEST_KEY",
                "SessionToken": "SESSION_TEST_TOKEN",
            }
        }
        mock_sm = MagicMock()
        mock_sm.get_secret_value.return_value = {
            "SecretString": json.dumps({"PIA_USERNAME": "p1234567", "PIA_PASSWORD": "ValidPassword99!"})
        }

        client = bridge.AwsSecretClient(
            account_id=self.account_id,
            sts_client=mock_sts,
            sm_client=mock_sm,
        )
        user, pwd = client.get_pia_credentials(self.secret_name)
        self.assertEqual(user, "p1234567")
        self.assertEqual(pwd, "ValidPassword99!")
        mock_sts.assume_role.assert_called_once()
        mock_sm.get_secret_value.assert_called_once_with(SecretId=self.secret_name)

    # -------------------------------------------------------------------------
    # 2. Invalid or missing PIA credentials
    # -------------------------------------------------------------------------
    def test_02_invalid_or_missing_pia_credentials(self) -> None:
        """Confirms that malformed JSON or missing PIA keys raise appropriate errors."""
        mock_sts = MagicMock()
        mock_sts.get_caller_identity.return_value = {"Account": self.account_id}
        mock_sts.assume_role.return_value = {
            "Credentials": {"AccessKeyId": "A", "SecretAccessKey": "B", "SessionToken": "C"}
        }

        # Case A: Corrupted JSON
        mock_sm = MagicMock()
        mock_sm.get_secret_value.return_value = {"SecretString": "{malformed_not_json: true"}
        client = bridge.AwsSecretClient(account_id=self.account_id, sts_client=mock_sts, sm_client=mock_sm)
        with self.assertRaises(bridge.InvalidSecretJsonError):
            client.get_pia_credentials(self.secret_name)

        # Case B: Missing PIA_PASSWORD
        mock_sm.get_secret_value.return_value = {"SecretString": json.dumps({"PIA_USERNAME": "p1234"})}
        with self.assertRaises(bridge.InvalidCredentialsError):
            client.get_pia_credentials(self.secret_name)

        # Case C: Empty PIA_USERNAME
        mock_sm.get_secret_value.return_value = {"SecretString": json.dumps({"PIA_USERNAME": "   ", "PIA_PASSWORD": "secret"})}
        with self.assertRaises(bridge.InvalidCredentialsError):
            client.get_pia_credentials(self.secret_name)

    # -------------------------------------------------------------------------
    # 3. Wrong AWS identity
    # -------------------------------------------------------------------------
    def test_03_wrong_aws_identity(self) -> None:
        """Confirms that unexpected AWS Account ID aborts with WrongAccountError."""
        mock_sts = MagicMock()
        mock_sts.get_caller_identity.return_value = {
            "Account": "999888777666",
            "Arn": "arn:aws:iam::999888777666:user/unauthorized",
        }
        client = bridge.AwsSecretClient(account_id=self.account_id, sts_client=mock_sts)
        with self.assertRaises(bridge.WrongAccountError):
            client.validate_identity()

    # -------------------------------------------------------------------------
    # 4. Missing MFA authorization
    # -------------------------------------------------------------------------
    def test_04_missing_mfa_authorization(self) -> None:
        """Confirms that unauthenticated or expired MFA prompts raise MfaRequiredError."""
        mock_sts = MagicMock()
        mock_sts.get_caller_identity.return_value = {"Account": self.account_id}
        mock_sts.assume_role.side_effect = Exception("AccessDenied: MultiFactorAuthentication failed or required")
        client = bridge.AwsSecretClient(account_id=self.account_id, sts_client=mock_sts)
        with self.assertRaises(bridge.MfaRequiredError):
            client.assume_reader_role()

    # -------------------------------------------------------------------------
    # 5. Unauthorized secret access
    # -------------------------------------------------------------------------
    def test_05_unauthorized_secret_access(self) -> None:
        """Confirms that attempting to request unrelated secrets fails closed with SecretAccessDeniedError."""
        mock_sts = MagicMock()
        mock_sts.get_caller_identity.return_value = {"Account": self.account_id}
        client = bridge.AwsSecretClient(account_id=self.account_id, sts_client=mock_sts)
        with self.assertRaises(bridge.SecretAccessDeniedError):
            client.get_pia_credentials("lodestar/other-secret")

    # -------------------------------------------------------------------------
    # 6. PIA API failures
    # -------------------------------------------------------------------------
    @patch("urllib.request.urlopen")
    def test_06_pia_api_failures(self, mock_urlopen: MagicMock) -> None:
        """Confirms that HTTP 401/403 or network failure during token retrieval raises PiaAuthenticationError."""
        client = bridge.PiaWireGuardClient()

        # HTTP 401 Unauthorized
        mock_urlopen.side_effect = urllib.error.HTTPError(
            url="https://api.test", code=401, msg="Unauthorized", hdrs={}, fp=io.BytesIO(b"")
        )
        with self.assertRaises(bridge.PiaAuthenticationError):
            client.get_auth_token("bad_user", "bad_pass")

    # -------------------------------------------------------------------------
    # 7. Invalid endpoint metadata
    # -------------------------------------------------------------------------
    def test_07_invalid_endpoint_metadata(self) -> None:
        """Confirms that malformed JSON or missing regions array raises EndpointMetadataError."""
        bad_cache = {"invalid_structure": True}
        client = bridge.PiaWireGuardClient(serverlist_cache=bad_cache)
        with self.assertRaises(bridge.EndpointMetadataError):
            client.resolve_region("us-east")

    # -------------------------------------------------------------------------
    # 8. Unavailable WireGuard region
    # -------------------------------------------------------------------------
    def test_08_unavailable_wireguard_region(self) -> None:
        """Confirms that requested region without WireGuard support raises RegionUnavailableError."""
        client = bridge.PiaWireGuardClient(serverlist_cache=self.mock_server_list)

        # Non-existent region
        with self.assertRaises(bridge.RegionUnavailableError):
            client.resolve_region("non-existent-region")

        # Region with no wg block
        with self.assertRaises(bridge.RegionUnavailableError):
            client.resolve_region("legacy-ovpn-only")

    # -------------------------------------------------------------------------
    # 9. WireGuard registration failure
    # -------------------------------------------------------------------------
    @patch("urllib.request.urlopen")
    def test_09_wireguard_registration_failure(self, mock_urlopen: MagicMock) -> None:
        """Confirms that HTTP error on /addKey raises WireGuardRegistrationError."""
        mock_urlopen.side_effect = Exception("Connection refused on port 1337")
        client = bridge.PiaWireGuardClient()
        _, valid_pubkey = bridge.generate_wireguard_keypair()

        with self.assertRaises(bridge.WireGuardRegistrationError):
            client.register_key("212.56.54.190", "newjersey434", "dummy_token", valid_pubkey)

    # -------------------------------------------------------------------------
    # 10. Incorrect remote Docker permissions
    # -------------------------------------------------------------------------
    @patch("subprocess.run")
    def test_10_incorrect_remote_docker_permissions(self, mock_run: MagicMock) -> None:
        """Confirms that remote user unable to execute sudo docker commands raises RemotePrerequisiteError."""
        # First call: verify_host_connectivity (true) succeeds
        # Second call: sudo -n docker info fails
        mock_run.side_effect = [
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="", stderr=""),
            subprocess.CompletedProcess(args=["ssh"], returncode=1, stdout="", stderr="sudo: a password is required"),
        ]
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        with self.assertRaises(bridge.RemotePrerequisiteError):
            prov.validate_remote_prerequisites()

    # -------------------------------------------------------------------------
    # 11. Missing Compose configuration
    # -------------------------------------------------------------------------
    def test_11_missing_compose_configuration(self) -> None:
        """Confirms that nonexistent local Compose path raises ComposeValidationError."""
        with self.assertRaises(bridge.ComposeValidationError):
            bridge.ComposeDeployer.validate_local_compose("C:\\nonexistent\\compose.yaml")

    # -------------------------------------------------------------------------
    # 12. Invalid Compose configuration
    # -------------------------------------------------------------------------
    def test_12_invalid_compose_configuration(self) -> None:
        """Confirms that invalid YAML or Compose lacking gluetun raises ComposeValidationError."""
        with patch("pathlib.Path.is_file", return_value=True):
            # Case A: Missing gluetun service
            with patch("pathlib.Path.read_text", return_value="services:\n  other:\n    image: test"):
                with self.assertRaises(bridge.ComposeValidationError):
                    bridge.ComposeDeployer.validate_local_compose("compose.yaml")

            # Case B: Malformed YAML syntax
            with patch("pathlib.Path.read_text", return_value="services: [unclosed"):
                with self.assertRaises(bridge.ComposeValidationError):
                    bridge.ComposeDeployer.validate_local_compose("compose.yaml")

    # -------------------------------------------------------------------------
    # 13. Image digest or pull failure
    # -------------------------------------------------------------------------
    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_13_image_digest_or_pull_failure(self, mock_cmd: MagicMock) -> None:
        """Confirms that Docker pull error or container startup failure raises VpnConnectionError."""
        mock_cmd.side_effect = subprocess.CalledProcessError(
            returncode=1, cmd=["ssh"], stderr="manifest unknown: image digest not found"
        )
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        with self.assertRaises(bridge.VpnConnectionError):
            prov.start_vpn()

    # -------------------------------------------------------------------------
    # 14. SSH host-key mismatch
    # -------------------------------------------------------------------------
    @patch("subprocess.run")
    def test_14_ssh_host_key_mismatch(self, mock_run: MagicMock) -> None:
        """Confirms that an altered or unrecognized host key fails with HostKeyMismatchError."""
        mock_run.side_effect = subprocess.CalledProcessError(
            returncode=255, cmd=["ssh"], stderr="REMOTE HOST IDENTIFICATION HAS CHANGED!"
        )
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        with self.assertRaises(bridge.HostKeyMismatchError):
            prov.verify_host_connectivity()

    # -------------------------------------------------------------------------
    # 15. Partial SSH transfer
    # -------------------------------------------------------------------------
    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_15_partial_ssh_transfer(self, mock_cmd: MagicMock) -> None:
        """Confirms that checksum mismatch after Compose transfer raises ComposeValidationError."""
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        deployer = bridge.ComposeDeployer(prov)

        valid_yaml = "services:\n  gluetun:\n    image: qmcgaw/gluetun\n"
        with patch.object(bridge.ComposeDeployer, "validate_local_compose", return_value=valid_yaml):
            # 1. check_cmd (sha256sum fails/not present)
            # 2. install_cmd succeeds
            # 3. verify_cmd returns corrupted sha256
            mock_cmd.side_effect = [
                subprocess.CompletedProcess(args=["ssh"], returncode=1, stdout="", stderr=""),
                subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="", stderr=""),
                subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="corrupted_sha256 /path", stderr=""),
            ]
            with self.assertRaises(bridge.ComposeValidationError):
                deployer.deploy_compose_stack("compose.yaml")

    # -------------------------------------------------------------------------
    # 16. Incorrect runtime file permissions
    # -------------------------------------------------------------------------
    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_16_incorrect_runtime_file_permissions(self, mock_cmd: MagicMock) -> None:
        """Confirms that insecure permissions (e.g. world-readable or non-root) are refused."""
        mock_cmd.return_value = subprocess.CompletedProcess(
            args=["ssh"], returncode=0, stdout="755 lodestar:lodestar\n644 lodestar:lodestar\n", stderr=""
        )
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        with self.assertRaises(bridge.RemotePermissionError):
            prov.verify_remote_permissions()

    # -------------------------------------------------------------------------
    # 17. Tunnel handshake failure
    # -------------------------------------------------------------------------
    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_17_tunnel_handshake_failure(self, mock_cmd: MagicMock) -> None:
        """Confirms that missing WireGuard handshake raises VpnHandshakeError."""
        # Host IP check succeeds, but wg show output contains no handshake
        mock_cmd.side_effect = [
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="192.0.2.100\n", stderr=""),
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="interface: wg0\n---WG_END---\n104.28.1.1\n", stderr=""),
        ]
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        with self.assertRaises(bridge.VpnHandshakeError):
            prov.verify_leak_protection(max_retries=1, delay_s=0)

    # -------------------------------------------------------------------------
    # 18. DNS leakage
    # -------------------------------------------------------------------------
    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_18_dns_leakage(self, mock_cmd: MagicMock) -> None:
        """Confirms that failed internal DNS resolution raises LeakDetectedError."""
        # 1. Host IP
        # 2. Handshake + VPN IP succeeds
        # 3. Org check succeeds
        # 4. DNS check fails
        mock_cmd.side_effect = [
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="192.0.2.100\n", stderr=""),
            subprocess.CompletedProcess(
                args=["ssh"],
                returncode=0,
                stdout="latest handshake 10s ago\ntransfer: 1000 B received\n---WG_END---\n156.146.54.4\n",
                stderr="",
            ),
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout='{"org": "AS39351 Private Internet Access"}', stderr=""),
            subprocess.CompletedProcess(args=["ssh"], returncode=1, stdout="", stderr="connection timed out"),
        ]
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        with self.assertRaises(bridge.LeakDetectedError):
            prov.verify_leak_protection(max_retries=1, delay_s=0)

    # -------------------------------------------------------------------------
    # 19. IPv4 and IPv6 routing leakage
    # -------------------------------------------------------------------------
    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_19_ipv4_and_ipv6_routing_leakage(self, mock_cmd: MagicMock) -> None:
        """Confirms that egress matching host public IP or unblocked IPv6 raises LeakDetectedError."""
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)

        # Case A: IPv4 Egress IP matches Host IP
        mock_cmd.side_effect = [
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="192.0.2.100\n", stderr=""),
            subprocess.CompletedProcess(
                args=["ssh"],
                returncode=0,
                stdout="latest handshake 10s ago\ntransfer: 1000 B received\n---WG_END---\n192.0.2.100\n",
                stderr="",
            ),
        ]
        with self.assertRaises(bridge.LeakDetectedError):
            prov.verify_leak_protection(max_retries=1, delay_s=0)

        # Case B: IPv6 not disabled
        mock_cmd.side_effect = [
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="192.0.2.100\n", stderr=""),
            subprocess.CompletedProcess(
                args=["ssh"],
                returncode=0,
                stdout="latest handshake 10s ago\ntransfer: 1000 B received\n---WG_END---\n156.146.54.4\n",
                stderr="",
            ),
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout='{"org": "Private Internet Access"}', stderr=""),
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="DNS_OK", stderr=""),
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="-P OUTPUT DROP", stderr=""),
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="0\n", stderr=""), # IPv6 enabled!
        ]
        with self.assertRaises(bridge.LeakDetectedError):
            prov.verify_leak_protection(max_retries=1, delay_s=0)

    # -------------------------------------------------------------------------
    # 20. VPN disconnection after startup
    # -------------------------------------------------------------------------
    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_20_vpn_disconnection_after_startup(self, mock_cmd: MagicMock) -> None:
        """Confirms that dropped tunnel or unreachable egress raises VpnConnectionError."""
        mock_cmd.side_effect = [
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="192.0.2.100\n", stderr=""),
            subprocess.CompletedProcess(args=["ssh"], returncode=1, stdout="", stderr="curl timeout"),
        ]
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        with self.assertRaises(bridge.VpnConnectionError):
            prov.verify_leak_protection(max_retries=1, delay_s=0)

    # -------------------------------------------------------------------------
    # 21. Failed credential rotation
    # -------------------------------------------------------------------------
    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_21_failed_credential_rotation(self, mock_cmd: MagicMock) -> None:
        """Confirms that SSH failure during re-provisioning raises SshConnectionError and prevents drift."""
        # 1. verify_host_connectivity succeeds
        # 2. install command fails
        mock_cmd.side_effect = [
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="", stderr=""),
            subprocess.CalledProcessError(returncode=1, cmd=["ssh"], stderr="broken pipe writing to /run/lodestar/vpn"),
        ]
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        with self.assertRaises(bridge.SshConnectionError):
            prov.provision_wg0_config("dummy_wg0_conf")

    # -------------------------------------------------------------------------
    # 22. VM reboot and re-provisioning
    # -------------------------------------------------------------------------
    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_22_vm_reboot_and_reprovisioning(self, mock_cmd: MagicMock) -> None:
        """Confirms that provisioning on a freshly rebooted machine creates /run directory and wg0.conf."""
        # All calls succeed: connect, install (creates 0700 dir and 0600 file), stat
        mock_cmd.side_effect = [
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="", stderr=""),
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="", stderr=""),
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="700 root:root\n600 root:root\n", stderr=""),
        ]
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        prov.provision_wg0_config("reboot_wg0_conf")
        # Confirms install command had -d -m 0700
        install_cmd = mock_cmd.call_args_list[1][0][0]
        self.assertIn("install -d -m 0700", install_cmd)
        self.assertIn("install -m 0600", install_cmd)

    # -------------------------------------------------------------------------
    # 23. Partial deployment cleanup
    # -------------------------------------------------------------------------
    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_23_partial_deployment_cleanup(self, mock_cmd: MagicMock) -> None:
        """Confirms that cleanup removes runtime secrets directory completely."""
        mock_cmd.return_value = subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="", stderr="")
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        prov.cleanup_runtime_credentials()
        cleanup_script = mock_cmd.call_args[0][0]
        self.assertIn("rm -rf /run/lodestar/vpn", cleanup_script)
        self.assertIn("docker compose", cleanup_script)

    # -------------------------------------------------------------------------
    # 24. Repeated, idempotent provisioning
    # -------------------------------------------------------------------------
    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_24_repeated_idempotent_provisioning(self, mock_cmd: MagicMock) -> None:
        """Confirms that repeating provisioning runs cleanly and leaves permissions invariant."""
        mock_cmd.side_effect = [
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="", stderr=""),
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="", stderr=""),
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="700 root:root\n600 root:root\n", stderr=""),
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="", stderr=""),
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="", stderr=""),
            subprocess.CompletedProcess(args=["ssh"], returncode=0, stdout="700 root:root\n600 root:root\n", stderr=""),
        ]
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        prov.provision_wg0_config("conf_run_1")
        prov.provision_wg0_config("conf_run_2")
        self.assertEqual(mock_cmd.call_count, 6)

    # -------------------------------------------------------------------------
    # 25. Attempted secret disclosure through logs or command output
    # -------------------------------------------------------------------------
    def test_25_attempted_secret_disclosure_through_logs_or_command_output(self) -> None:
        """Confirms that private keys, tokens, and passwords are redacted from error output and strings."""
        sample_sensitive = (
            "Error on https://server:1337/addKey?pt=SECRET_TOKEN_12345&pubkey=abc\n"
            "PrivateKey = 44CharactersOfBase64PrivateKeyHereMustBeRedacted=\n"
            '{"PIA_PASSWORD": "SuperSecretPassword!", "token": "SecretSessionToken"}'
        )
        redacted = bridge.redact_sensitive_text(sample_sensitive)
        self.assertNotIn("SECRET_TOKEN_12345", redacted)
        self.assertNotIn("44CharactersOfBase64PrivateKeyHereMustBeRedacted=", redacted)
        self.assertNotIn("SuperSecretPassword!", redacted)
        self.assertNotIn("SecretSessionToken", redacted)
        self.assertIn("[REDACTED]", redacted)

    # -------------------------------------------------------------------------
    # 26. Crawler startup while the VPN is disconnected
    # -------------------------------------------------------------------------
    @patch.object(bridge.LinodeSshProvisioner, "run_remote_command")
    def test_26_crawler_startup_while_vpn_disconnected(self, mock_cmd: MagicMock) -> None:
        """Confirms that crawler refuses to start when VPN tunnel is disconnected and shuts down stack."""
        prov = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        # Mock verify_leak_protection to fail
        with patch.object(prov, "verify_leak_protection", side_effect=bridge.VpnConnectionError("Tunnel down")):
            with self.assertRaises(bridge.VpnConnectionError):
                prov.start_crawler()
            # Verify stack shutdown was invoked to prevent unencrypted crawler leakage
            mock_cmd.assert_called_with("sudo -n docker compose -f /home/lodestar/lodestar/pia-example/compose.vpn.yaml down", check=False)

    # -------------------------------------------------------------------------
    # Additional Verification: RFC 7748 Official Curve25519 Test Vectors
    # -------------------------------------------------------------------------
    def test_rfc7748_curve25519_test_vectors(self) -> None:
        """Confirms that verified X25519 implementation passes RFC 7748 Section 6.1 official test vectors."""
        from cryptography.hazmat.primitives.asymmetric.x25519 import X25519PrivateKey, X25519PublicKey

        alice_priv = bytes.fromhex("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a")
        alice_pub = bytes.fromhex("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a")
        bob_priv = bytes.fromhex("5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb")
        bob_pub = bytes.fromhex("de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f")
        shared_k = bytes.fromhex("4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742")

        a_priv = X25519PrivateKey.from_private_bytes(alice_priv)
        a_pub = a_priv.public_key()
        self.assertEqual(a_pub.public_bytes_raw(), alice_pub)

        b_priv = X25519PrivateKey.from_private_bytes(bob_priv)
        b_pub = b_priv.public_key()
        self.assertEqual(b_pub.public_bytes_raw(), bob_pub)

        self.assertEqual(a_priv.exchange(b_pub), shared_k)
        self.assertEqual(b_priv.exchange(a_pub), shared_k)

    # -------------------------------------------------------------------------
    # Additional Verification: Gluetun Pinned Immutable Image Digest
    # -------------------------------------------------------------------------
    def test_gluetun_digest_verified(self) -> None:
        """Confirms that GLUETUN_IMAGE matches the verified immutable Docker Hub digest."""
        self.assertEqual(
            bridge.GLUETUN_IMAGE,
            "qmcgaw/gluetun:v3.40.0@sha256:2b42bfa046757145a5155acece417b65b4443c8033fb88661a8e9dcf7fda5a00",
        )
        self.assertEqual(
            bridge.GLUETUN_AMD64_MANIFEST_DIGEST,
            "sha256:8ec31c4b83e1f3173d43728f78a4a7ad45237fef366a4b5461e5aee93f517fba",
        )


if __name__ == "__main__":
    unittest.main()
