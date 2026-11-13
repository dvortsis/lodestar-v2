"""Tests for devvm.py's guard rails. No network: the API layer is stubbed."""
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

import yaml

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import devvm  # noqa: E402

PUB = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExampleExampleExampleExampleExampleExamp owner"


class FakeApi:
    """Records calls; serves a fixed inventory."""

    def __init__(self, linodes=(), firewalls=()):
        self.linodes = [dict(x) for x in linodes]
        self.firewalls = [dict(x) for x in firewalls]
        self.calls = []
        self.bodies = {}

    def all_pages(self, path):
        return {"/linode/instances": self.linodes, "/networking/firewalls": self.firewalls}[path]

    def call(self, method, path, body=None, ok404=False):
        self.calls.append((method, path))
        if body is not None:
            self.bodies[(method, path)] = body
        if method == "DELETE":
            kind, ident = path.rsplit("/", 1)
            pool = self.linodes if kind == "/linode/instances" else self.firewalls
            pool[:] = [x for x in pool if str(x["id"]) != ident]
        if method == "GET" and ok404:
            return None  # deleted
        if method == "POST" and path == "/networking/firewalls":
            return {"id": 7}
        if method == "POST" and path == "/linode/instances":
            return {"id": 9, "ipv4": ["203.0.113.9"], "created": "2026-11-12T07:00:00"}
        if method == "GET" and path.startswith("/linode/instances/"):
            return {"status": "running"}
        return {}

    def mutations(self):
        return [c for c in self.calls if c[0] != "GET"]


def patched(api):
    return mock.patch.multiple(devvm, call=api.call, all_pages=api.all_pages)


class RenderTests(unittest.TestCase):
    def test_user_data_is_valid_yaml_with_the_keys(self):
        with tempfile.TemporaryDirectory() as d:
            k = Path(d) / "k"
            subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(k)], check=True)
            text = devvm.render_user_data(PUB, k.read_text(), (Path(d) / "k.pub").read_text())
        doc = yaml.safe_load(text)
        self.assertEqual(doc["users"][0]["ssh_authorized_keys"], [PUB])
        self.assertTrue(doc["ssh_keys"]["ed25519_private"].startswith("-----BEGIN OPENSSH PRIVATE KEY-----"))
        self.assertFalse(doc["ssh_pwauth"])
        self.assertTrue(doc["disable_root"])
        self.assertEqual(doc["ssh_genkeytypes"], [])
        self.assertNotIn("__", text.split("write_files:")[0])
        sshd = next(f for f in doc["write_files"] if f["path"].startswith("/etc/ssh/"))["content"]
        self.assertEqual([ln for ln in sshd.splitlines() if ln.startswith("HostKey")],
                         ["HostKey /etc/ssh/ssh_host_ed25519_key"])

    def test_worker_gets_small_swap_and_no_toolchain(self):
        with tempfile.TemporaryDirectory() as d:
            k = Path(d) / "k"
            subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(k)], check=True)
            main = devvm.render_user_data(PUB, k.read_text(), (Path(d) / "k.pub").read_text())
            worker = devvm.render_user_data(PUB, k.read_text(), (Path(d) / "k.pub").read_text(), worker=True)
        boot = lambda text: next(f for f in yaml.safe_load(text)["write_files"]
                                 if f["path"].endswith("devvm-bootstrap.sh"))["content"]
        self.assertIn("fallocate -l 8G", boot(main))
        self.assertIn("rustup", boot(main))
        self.assertIn("fallocate -l 256M", boot(worker))
        for word in ("rustup", "nodejs.org", "claude.ai/install", "git clone"):
            self.assertNotIn(word, boot(worker))
        for text in (main, worker):
            self.assertIn("docker-compose-plugin", boot(text))
            self.assertIn("echo \"ok $(date", boot(text))
            self.assertNotIn("__", boot(text))

    def test_template_carries_no_secret_placeholders(self):
        t = (Path(devvm.__file__).parent / "cloud-init.yaml.tmpl").read_text()
        for word in ("PIA", "TOKEN", "PASSWORD", "GITHUB"):
            self.assertNotIn("__" + word, t)


class FirewallTests(unittest.TestCase):
    def test_inbound_drops_everything_but_ssh_from_one_address(self):
        r = devvm.firewall_rules("198.51.100.4")
        self.assertEqual(r["inbound_policy"], "DROP")
        self.assertEqual(len(r["inbound"]), 1)
        rule = r["inbound"][0]
        self.assertEqual((rule["protocol"], rule["ports"], rule["action"]), ("TCP", "22", "ACCEPT"))
        self.assertEqual(rule["addresses"], {"ipv4": ["198.51.100.4/32"]})

    def test_no_admin_address_means_no_inbound_at_all(self):
        r = devvm.firewall_rules(None)
        self.assertEqual((r["inbound_policy"], r["inbound"]), ("DROP", []))

    def test_only_public_ipv4_is_accepted(self):
        for bad in ("10.0.0.1", "192.168.1.2", "127.0.0.1", "0.0.0.0", "100.64.0.1", "::1", "1.2.3.0/24"):
            with self.subTest(bad=bad), self.assertRaises((SystemExit, ValueError)):
                devvm.check_ip(bad)
        self.assertEqual(devvm.check_ip("8.8.8.8"), "8.8.8.8")


class CreateTests(unittest.TestCase):
    def args(self, d, pub=PUB):
        p = Path(d) / "key.pub"
        p.write_text(pub)
        return SimpleNamespace(ssh_pubkey_file=str(p), admin_ip="8.8.8.8", workdir=str(Path(d) / "w"),
                               role="", type=devvm.TYPE)

    def test_refuses_more_than_the_ceiling(self):
        api = FakeApi(linodes=[{"label": f"claude-dev-2026110{i}", "id": i} for i in range(devvm.MAX_VMS)])
        with tempfile.TemporaryDirectory() as d, patched(api), self.assertRaises(SystemExit):
            devvm.cmd_create(self.args(d))
        self.assertEqual(api.mutations(), [])

    def test_refuses_an_existing_label_and_other_types(self):
        with mock.patch.object(devvm.dt, "datetime", wraps=devvm.dt.datetime) as fake:
            fake.now.return_value = devvm.dt.datetime(2026, 11, 12, tzinfo=devvm.dt.timezone.utc)
            api = FakeApi(linodes=[{"label": "claude-dev-20261112-worker", "id": 1}])
            with tempfile.TemporaryDirectory() as d, patched(api), self.assertRaises(SystemExit):
                a = self.args(d); a.role = "worker"; a.type = "g6-nanode-1"
                devvm.cmd_create(a)
        with tempfile.TemporaryDirectory() as d, patched(FakeApi()), self.assertRaises(SystemExit):
            a = self.args(d); a.type = "g6-standard-32"
            devvm.cmd_create(a)
        self.assertEqual(api.mutations(), [])

    def test_refuses_a_private_key(self):
        api = FakeApi()
        with tempfile.TemporaryDirectory() as d, patched(api), self.assertRaises(SystemExit):
            devvm.cmd_create(self.args(d, "-----BEGIN OPENSSH PRIVATE KEY-----"))
        self.assertEqual(api.mutations(), [])

    def test_creates_firewall_then_vm_and_drops_the_local_host_key(self):
        api = FakeApi(linodes=[{"label": "somebody-else", "id": 3}])
        with tempfile.TemporaryDirectory() as d, patched(api), mock.patch.object(devvm.time, "sleep"):
            devvm.cmd_create(self.args(d))
            work = Path(d) / "w"
            self.assertFalse((work / "host_ed25519").exists())
            self.assertEqual(len(list(work.glob("claude-dev-*.json"))), 1)
        self.assertEqual(api.mutations(), [("POST", "/networking/firewalls"), ("POST", "/linode/instances")])
        body = api.bodies[("POST", "/linode/instances")]
        self.assertEqual(body["swap_size"], 0)
        self.assertEqual(body["disk_encryption"], "enabled")
        self.assertFalse(body["backups_enabled"])


class RefusedCreateTests(unittest.TestCase):
    def test_a_refused_vm_takes_its_firewall_with_it(self):
        api = FakeApi()
        real = api.call

        def refuse(method, path, body=None, ok404=False):
            if (method, path) == ("POST", "/linode/instances"):
                api.calls.append((method, path))
                raise SystemExit("HTTP 400: a limit on your account")
            return real(method, path, body, ok404)

        with tempfile.TemporaryDirectory() as d, \
                mock.patch.multiple(devvm, call=refuse, all_pages=api.all_pages), self.assertRaises(SystemExit):
            devvm.cmd_create(CreateTests.args(None, d))
        self.assertEqual(api.mutations(), [("POST", "/networking/firewalls"), ("POST", "/linode/instances"),
                                           ("DELETE", "/networking/firewalls/7")])


class DeleteTests(unittest.TestCase):
    def test_refuses_outside_prefix_or_without_exact_confirmation(self):
        api = FakeApi(linodes=[{"label": "lodestar-main", "id": 1}])
        for label, confirm in (("lodestar-main", "lodestar-main"), ("claude-dev-20261112", "claude-dev-2026111"),
                               ("claude-dev-20261112", "yes")):
            with self.subTest(label=label), patched(api), self.assertRaises(SystemExit):
                devvm.cmd_delete(SimpleNamespace(label=label, confirm=confirm))
        self.assertEqual(api.mutations(), [])

    def test_deletes_only_the_named_vm_and_its_firewall(self):
        api = FakeApi(
            linodes=[{"label": "claude-dev-20261112", "id": 5}, {"label": "lodestar-main", "id": 6},
                     {"label": "claude-dev-20261112x", "id": 8}],
            firewalls=[{"label": "claude-dev-20261112-fw", "id": 11}, {"label": "lodestar-fw", "id": 12}],
        )
        with patched(api):
            devvm.cmd_delete(SimpleNamespace(label="claude-dev-20261112", confirm="claude-dev-20261112"))
        self.assertEqual(api.mutations(), [("DELETE", "/linode/instances/5"), ("DELETE", "/networking/firewalls/11")])


if __name__ == "__main__":
    unittest.main()
