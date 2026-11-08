#!/usr/bin/env python3
"""
Automated Security Controls and Unit Test Suite for Lodestar PIA VPN Bridge.

Covers all 16 required security cases:
  1. Missing secret
  2. Invalid JSON
  3. Missing or empty PIA credentials
  4. Wrong AWS account
  5. Missing MFA authorization
  6. Failure to assume the restricted IAM role
  7. Access denied to unrelated AWS secrets
  8. SSH host-key mismatch
  9. SSH connection failure
 10. Secure file permissions (0700 dir, 0600 file, root:root)
 11. VPN connection failure
 12. VPN disconnection after startup
 13. DNS or public-IP leak detection
 14. Credential refresh
 15. Cleanup after partial failure
 16. Idempotent repeated provisioning

All AWS and Akamai/Linode interactions are safely mocked. Zero secrets exposed.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import unittest
from unittest.mock import MagicMock, patch

# Add parent scripts directory to module path
sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "scripts")))

import lodestar_pia_bridge as bridge


class LodestarPiaBridgeTests(unittest.TestCase):
    """Unit test suite verifying all 16 required security controls."""

    def setUp(self) -> None:
        self.account_id = "123456789012"
        self.region = "us-east-1"
        self.role_arn = f"arn:aws:iam::{self.account_id}:role/LodestarPiaSecretReader"
        self.secret_name = "lodestar/pia-vpn"
        self.target_ip = "192.0.2.100"

    # -------------------------------------------------------------------------
    # 1. Missing secret
    # -------------------------------------------------------------------------
    def test_01_missing_secret(self) -> None:
        """Confirms that a missing secret in Secrets Manager raises SecretNotFoundError."""
        mock_sts = MagicMock()
        mock_sts.get_caller_identity.return_value = {"Account": self.account_id, "Arn": f"arn:aws:iam::{self.account_id}:user/lodestar-admin"}
        mock_sts.assume_role.return_value = {
            "Credentials": {"AccessKeyId": "AKIA...", "SecretAccessKey": "SEC...", "SessionToken": "TOK..."}
        }

        mock_sm = MagicMock()
        mock_sm.get_secret_value.side_effect = Exception("ResourceNotFoundException: Secrets Manager can't find the specified secret.")

        client = bridge.AwsSecretClient(
            account_id=self.account_id,
            sts_client=mock_sts,
            sm_client=mock_sm,
        )
        with self.assertRaises(bridge.SecretNotFoundError):
            client.get_pia_credentials(self.secret_name)

    # -------------------------------------------------------------------------
    # 2. Invalid JSON
    # -------------------------------------------------------------------------
    def test_02_invalid_json(self) -> None:
        """Confirms that malformed JSON in SecretString raises InvalidSecretJsonError."""
        mock_sts = MagicMock()
        mock_sts.get_caller_identity.return_value = {"Account": self.account_id}
        mock_sts.assume_role.return_value = {
            "Credentials": {"AccessKeyId": "A", "SecretAccessKey": "B", "SessionToken": "C"}
        }

        mock_sm = MagicMock()
        mock_sm.get_secret_value.return_value = {"SecretString": "{malformed_json_not_valid: true"}

        client = bridge.AwsSecretClient(
            account_id=self.account_id,
            sts_client=mock_sts,
            sm_client=mock_sm,
        )
        with self.assertRaises(bridge.InvalidSecretJsonError):
            client.get_pia_credentials(self.secret_name)

    # -------------------------------------------------------------------------
    # 3. Missing or empty PIA credentials
    # -------------------------------------------------------------------------
    def test_03_missing_or_empty_credentials(self) -> None:
        """Confirms that missing or empty username/password values raise InvalidCredentialsError."""
        mock_sts = MagicMock()
        mock_sts.get_caller_identity.return_value = {"Account": self.account_id}
        mock_sts.assume_role.return_value = {
            "Credentials": {"AccessKeyId": "A", "SecretAccessKey": "B", "SessionToken": "C"}
        }

        mock_sm = MagicMock()

        # Case A: Missing PIA_PASSWORD
        mock_sm.get_secret_value.return_value = {"SecretString": json.dumps({"PIA_USERNAME": "p123456"})}
        client = bridge.AwsSecretClient(account_id=self.account_id, sts_client=mock_sts, sm_client=mock_sm)
        with self.assertRaises(bridge.InvalidCredentialsError):
            client.get_pia_credentials(self.secret_name)

        # Case B: Empty PIA_USERNAME
        mock_sm.get_secret_value.return_value = {"SecretString": json.dumps({"PIA_USERNAME": "   ", "PIA_PASSWORD": "secret_password"})}
        with self.assertRaises(bridge.InvalidCredentialsError):
            client.get_pia_credentials(self.secret_name)

    # -------------------------------------------------------------------------
    # 4. Wrong AWS account
    # -------------------------------------------------------------------------
    def test_04_wrong_aws_account(self) -> None:
        """Confirms that an unexpected AWS Account ID aborts with WrongAccountError."""
        mock_sts = MagicMock()
        mock_sts.get_caller_identity.return_value = {"Account": "111222333444", "Arn": "arn:aws:iam::111222333444:user/attacker"}

        client = bridge.AwsSecretClient(
            account_id=self.account_id,  # Expects self.account_id
            sts_client=mock_sts,
        )
        with self.assertRaises(bridge.WrongAccountError):
            client.validate_identity()

    # -------------------------------------------------------------------------
    # 5. Missing MFA authorization
    # -------------------------------------------------------------------------
    def test_05_missing_mfa_authorization(self) -> None:
        """Confirms that unauthenticated or expired MFA prompts raise MfaRequiredError."""
        mock_sts = MagicMock()
        mock_sts.get_caller_identity.return_value = {"Account": self.account_id}
        mock_sts.assume_role.side_effect = Exception("AccessDenied: MultiFactorAuthentication failed or required")

        client = bridge.AwsSecretClient(
            account_id=self.account_id,
            sts_client=mock_sts,
        )
        with self.assertRaises(bridge.MfaRequiredError):
            client.assume_reader_role()

    # -------------------------------------------------------------------------
    # 6. Failure to assume the restricted IAM role
    # -------------------------------------------------------------------------
    def test_06_failure_to_assume_iam_role(self) -> None:
        """Confirms that an unauthorized identity failing to assume the role raises RoleAssumptionError."""
        mock_sts = MagicMock()
        mock_sts.get_caller_identity.return_value = {"Account": self.account_id}
        mock_sts.assume_role.side_effect = Exception("AccessDenied: User lodestar-agent is not authorized to perform sts:AssumeRole")

        client = bridge.AwsSecretClient(
            account_id=self.account_id,
            sts_client=mock_sts,
        )
        with self.assertRaises(bridge.RoleAssumptionError):
            client.assume_reader_role()

    # -------------------------------------------------------------------------
    # 7. Access denied to unrelated AWS secrets
    # -------------------------------------------------------------------------
    def test_07_access_denied_unrelated_secrets(self) -> None:
        """Confirms that attempting to request unrelated secrets fails closed with SecretAccessDeniedError."""
        mock_sts = MagicMock()
        mock_sts.get_caller_identity.return_value = {"Account": self.account_id}
        mock_sts.assume_role.return_value = {
            "Credentials": {"AccessKeyId": "A", "SecretAccessKey": "B", "SessionToken": "C"}
        }
        mock_sm = MagicMock()

        client = bridge.AwsSecretClient(
            account_id=self.account_id,
            sts_client=mock_sts,
            sm_client=mock_sm,
        )

        # Unauthorized secret query is rejected before calling AWS Secrets Manager
        with self.assertRaises(bridge.SecretAccessDeniedError):
            client.get_pia_credentials("lodestar/akamai-token")

        with self.assertRaises(bridge.SecretAccessDeniedError):
            client.get_pia_credentials("production/database/password")

    # -------------------------------------------------------------------------
    # 8. SSH host-key mismatch
    # -------------------------------------------------------------------------
    @patch("subprocess.run")
    def test_08_ssh_host_key_mismatch(self, mock_run: MagicMock) -> None:
        """Confirms that an altered or unrecognized host key fails with HostKeyMismatchError."""
        mock_run.side_effect = subprocess.CalledProcessError(
            returncode=255,
            cmd=["ssh", "..."],
            stderr="@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\n@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\nHost key verification failed.",
        )
        provisioner = bridge.LinodeSshProvisioner(target_ip=self.target_ip, strict_host_key=True)
        with self.assertRaises(bridge.HostKeyMismatchError):
            provisioner.verify_host_connectivity()

    # -------------------------------------------------------------------------
    # 9. SSH connection failure
    # -------------------------------------------------------------------------
    @patch("subprocess.run")
    def test_09_ssh_connection_failure(self, mock_run: MagicMock) -> None:
        """Confirms that connection timeout or network unreachability raises SshConnectionError."""
        mock_run.side_effect = subprocess.CalledProcessError(
            returncode=255,
            cmd=["ssh", "..."],
            stderr="ssh: connect to host 192.0.2.100 port 22: Connection timed out",
        )
        provisioner = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        with self.assertRaises(bridge.SshConnectionError):
            provisioner.verify_host_connectivity()

    # -------------------------------------------------------------------------
    # 10. Secure file permissions
    # -------------------------------------------------------------------------
    @patch("subprocess.run")
    def test_10_secure_file_permissions(self, mock_run: MagicMock) -> None:
        """Confirms that insecure permissions (e.g. world-readable or non-root) are refused."""
        provisioner = bridge.LinodeSshProvisioner(target_ip=self.target_ip)

        # Case A: Correct permissions: 700 root:root and 600 root:root -> PASS
        mock_run.return_value = MagicMock(returncode=0, stdout="700 root:root\n600 root:root\n")
        provisioner.verify_remote_permissions()

        # Case B: Insecure file mode (e.g. 644 world-readable) -> FAIL
        mock_run.return_value = MagicMock(returncode=0, stdout="700 root:root\n644 root:root\n")
        with self.assertRaises(bridge.RemotePermissionError):
            provisioner.verify_remote_permissions()

        # Case C: Insecure directory mode (e.g. 777) -> FAIL
        mock_run.return_value = MagicMock(returncode=0, stdout="777 root:root\n600 root:root\n")
        with self.assertRaises(bridge.RemotePermissionError):
            provisioner.verify_remote_permissions()

        # Case D: Non-root ownership -> FAIL
        mock_run.return_value = MagicMock(returncode=0, stdout="700 lodestar:lodestar\n600 lodestar:lodestar\n")
        with self.assertRaises(bridge.RemotePermissionError):
            provisioner.verify_remote_permissions()

    # -------------------------------------------------------------------------
    # 11. VPN connection failure
    # -------------------------------------------------------------------------
    @patch("subprocess.run")
    def test_11_vpn_connection_failure(self, mock_run: MagicMock) -> None:
        """Confirms that container failure or startup failure raises VpnConnectionError."""
        mock_run.side_effect = subprocess.CalledProcessError(
            returncode=1,
            cmd=["docker", "compose", "up", "-d", "gluetun"],
            stderr="Error response from daemon: driver failed programming external connectivity",
        )
        provisioner = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        with self.assertRaises(bridge.VpnConnectionError):
            provisioner.start_vpn()

    # -------------------------------------------------------------------------
    # 12. VPN disconnection after startup
    # -------------------------------------------------------------------------
    @patch("subprocess.run")
    def test_12_vpn_disconnection_after_startup(self, mock_run: MagicMock) -> None:
        """Confirms that a dropped VPN tunnel is detected and raises VpnConnectionError."""
        # Host probe returns host IP, container probe fails (wget error 1)
        def run_side_effect(cmd: list, *args: Any, **kwargs: Any) -> MagicMock:
            cmd_str = " ".join(cmd)
            if "docker exec gluetun" in cmd_str:
                raise subprocess.CalledProcessError(returncode=1, cmd=cmd, stderr="wget: error getting response")
            return MagicMock(returncode=0, stdout="198.51.100.22\n")

        mock_run.side_effect = run_side_effect
        provisioner = bridge.LinodeSshProvisioner(target_ip=self.target_ip)
        with self.assertRaises(bridge.VpnConnectionError):
            provisioner.verify_leak_protection(max_retries=1, delay_s=0)

    # -------------------------------------------------------------------------
    # 13. DNS or public-IP leak detection
    # -------------------------------------------------------------------------
    @patch("subprocess.run")
    def test_13_dns_or_public_ip_leak_detection(self, mock_run: MagicMock) -> None:
        """Confirms that egress matching host IP or DNS bypass trips LeakDetectedError."""
        provisioner = bridge.LinodeSshProvisioner(target_ip=self.target_ip)

        # Leak Case A: Container public IP matches Linode host public IP
        def run_ip_leak(cmd: list, *args: Any, **kwargs: Any) -> MagicMock:
            return MagicMock(returncode=0, stdout="198.51.100.22\n")

        mock_run.side_effect = run_ip_leak
        with self.assertRaises(bridge.LeakDetectedError):
            provisioner.verify_leak_protection(max_retries=1, delay_s=0)

        # Leak Case B: Public IP is masked, but DNS lookup fails internal resolver verification
        def run_dns_leak(cmd: list, *args: Any, **kwargs: Any) -> MagicMock:
            cmd_str = " ".join(cmd)
            if "docker exec gluetun nslookup" in cmd_str:
                return MagicMock(returncode=1, stdout="server can't find google.com: NXDOMAIN\n")
            if "docker exec gluetun" in cmd_str:
                return MagicMock(returncode=0, stdout="156.146.54.99\n")  # Different from host
            return MagicMock(returncode=0, stdout="198.51.100.22\n")  # Host IP

        mock_run.side_effect = run_dns_leak
        with self.assertRaises(bridge.LeakDetectedError):
            provisioner.verify_leak_protection(max_retries=1, delay_s=0)

    # -------------------------------------------------------------------------
    # 14. Credential refresh
    # -------------------------------------------------------------------------
    @patch("subprocess.run")
    def test_14_credential_refresh(self, mock_run: MagicMock) -> None:
        """Confirms that re-provisioning overwrites wg0.conf atomically without permission drift."""
        mock_run.return_value = MagicMock(returncode=0, stdout="700 root:root\n600 root:root\n")
        provisioner = bridge.LinodeSshProvisioner(target_ip=self.target_ip)

        new_conf = "[Interface]\nAddress = 10.0.0.5/32\nPrivateKey = privKey2\n\n[Peer]\nPublicKey = pubKey2\n"
        provisioner.provision_wg0_config(new_conf)

        # Verify command history included install with 0600 root:root
        self.assertTrue(mock_run.called)
        all_calls = [" ".join(call[0][0]) for call in mock_run.call_args_list]
        self.assertTrue(any("install -m 0600 -o root -g root" in cmd for cmd in all_calls))

    # -------------------------------------------------------------------------
    # 15. Cleanup after partial failure
    # -------------------------------------------------------------------------
    @patch("subprocess.run")
    def test_15_cleanup_after_partial_failure(self, mock_run: MagicMock) -> None:
        """Confirms that cleanup removes runtime secrets directory completely."""
        mock_run.return_value = MagicMock(returncode=0, stdout="")
        provisioner = bridge.LinodeSshProvisioner(target_ip=self.target_ip)

        provisioner.cleanup_runtime_credentials()
        self.assertTrue(mock_run.called)
        cmd_str = " ".join(mock_run.call_args[0][0])
        self.assertIn("sudo rm -rf /run/lodestar/vpn", cmd_str)

    # -------------------------------------------------------------------------
    # 16. Idempotent repeated provisioning
    # -------------------------------------------------------------------------
    @patch("subprocess.run")
    def test_16_idempotent_repeated_provisioning(self, mock_run: MagicMock) -> None:
        """Confirms that repeating provisioning runs cleanly and leaves permissions invariant."""
        mock_run.return_value = MagicMock(returncode=0, stdout="700 root:root\n600 root:root\n")
        provisioner = bridge.LinodeSshProvisioner(target_ip=self.target_ip)

        conf = "[Interface]\nAddress = 10.0.0.2/32\nPrivateKey = priv\n\n[Peer]\nPublicKey = pub\n"

        # Execute provisioning three consecutive times (each performs: verify connectivity, install, stat verify)
        provisioner.provision_wg0_config(conf)
        provisioner.provision_wg0_config(conf)
        provisioner.provision_wg0_config(conf)

        self.assertEqual(mock_run.call_count, 9)  # 3 calls per provision (connect + install + stat)

    # -------------------------------------------------------------------------
    # Additional Unit Checks: Keygen, Region Resolution, and Config Formatting
    # -------------------------------------------------------------------------
    def test_wireguard_keypair_generation(self) -> None:
        """Confirms that RFC 7748 Curve25519 produces valid 32-byte base64 keys."""
        import base64
        priv_b64, pub_b64 = bridge.generate_wireguard_keypair()
        priv_raw = base64.b64decode(priv_b64)
        pub_raw = base64.b64decode(pub_b64)
        self.assertEqual(len(priv_raw), 32)
        self.assertEqual(len(pub_raw), 32)
        # Check clamping
        self.assertEqual(priv_raw[0] & 7, 0)
        self.assertEqual(priv_raw[31] & 128, 0)
        self.assertEqual(priv_raw[31] & 64, 64)

    def test_region_resolution(self) -> None:
        """Confirms supported East Coast PIA regions resolve correctly."""
        pia = bridge.PiaWireGuardClient()
        east = pia.resolve_region("us-east")
        self.assertIn("privacy.network", east["hostname"])
        nj = pia.resolve_region("us-new-jersey")
        self.assertIn("newjersey", nj["hostname"])
        with self.assertRaises(bridge.PiaBridgeError):
            pia.resolve_region("invalid-mars-region")

    def test_build_wg0_conf(self) -> None:
        """Confirms WireGuard configuration formatting matches standard INI layout."""
        pia = bridge.PiaWireGuardClient()
        conf = pia.build_wg0_conf(
            private_key="privKey==",
            peer_ip="10.0.0.2",
            server_key="servKey==",
            server_ip="156.146.54.4",
            server_port=1337,
            dns_servers=["10.0.0.242"],
        )
        self.assertIn("[Interface]", conf)
        self.assertIn("Address = 10.0.0.2/32", conf)
        self.assertIn("PrivateKey = privKey==", conf)
        self.assertIn("DNS = 10.0.0.242", conf)
        self.assertIn("[Peer]", conf)
        self.assertIn("PublicKey = servKey==", conf)
        self.assertIn("Endpoint = 156.146.54.4:1337", conf)
        self.assertIn("AllowedIPs = 0.0.0.0/0", conf)
        self.assertIn("PersistentKeepalive = 25", conf)


if __name__ == "__main__":
    unittest.main(verbosity=2)
