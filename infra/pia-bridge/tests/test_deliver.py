"""deliver-pia with fakes for S3, Secrets Manager, the Linode API, the Actions
API and SSH. No network, no AWS, no ssh binary; the end-to-end case runs the
real bootstrap and VM step locally with stub curl and wg."""

import base64
import importlib.util
import io
import json
import subprocess
import tarfile
import tempfile
import unittest
from datetime import timedelta
from pathlib import Path

import deliver  # puts infra/fleet on sys.path
import audit
import limits
from apply import fingerprint
from linode import Linode
from state import FLEET_KEY, new_fleet, tombstone

FLEET_TESTS = Path(deliver.__file__).resolve().parents[1] / "fleet" / "tests"
_spec = importlib.util.spec_from_file_location("fleet_helpers", FLEET_TESTS / "helpers.py")
helpers = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(helpers)
T0, Clock, event_env, make_repo = helpers.T0, helpers.Clock, helpers.event_env, helpers.make_repo

from tests.test_vm_step import ADDKEY, CURL, PRIV, PUB, SERVERS, TOKEN, WG  # noqa: E402

L = limits.load()
RANGES = {"actions": ["198.51.100.0/24"]}  # RFC 5737
MY_IP = "198.51.100.77"
ADMIN = "203.0.113.4/32"


def host_key(n: int) -> str:
    return "ssh-ed25519 " + base64.b64encode(b"\x00\x00\x00\x0bssh-ed25519" + bytes([n]) * 32).decode()


LIVE = {"FLEET_LIVE": "true", "AWS_PIA_ROLE_ARN": "role-x", "STATE_BUCKET": "state-b", "STATE_BUCKET_OWNER": "111122223333",
        "FLEET_SSH_KEY": "-----BEGIN OPENSSH PRIVATE KEY-----\nFLEETKEYBODY\n-----END OPENSSH PRIVATE KEY-----",
        "PIA_SERVERS": json.dumps({"main": "ca_test", "worker": "us_test"}), "GITHUB_TOKEN": "gh",
        "ACTIONS_ID_TOKEN_REQUEST_URL": "u", "ACTIONS_ID_TOKEN_REQUEST_TOKEN": "t"}
LOGIN = {"username": "p1234567", "password": "pw-SECRET-value"}


def rules(extra=()):
    return {"inbound_policy": "DROP", "outbound_policy": "ACCEPT", "outbound": [],
            "inbound": [{"label": "ssh-admin-only", "action": "ACCEPT", "protocol": "TCP", "ports": "22",
                         "addresses": {"ipv4": [ADMIN]}}, *extra]}


class FakeS3:
    def __init__(self, doc):
        self.doc, self.calls = doc, []

    def get_object(self, **kw):
        self.calls.append(kw)
        if self.doc is None:
            raise RuntimeError("AccessDenied")
        return {"Body": io.BytesIO(json.dumps(self.doc).encode())}


class FakeFirewalls:
    """Linode's firewall endpoints only: what a Firewalls-scoped token can reach."""

    def __init__(self):
        self.firewalls = {"21": {"id": 21, "tags": ["lodestar"]}, "22": {"id": 22, "tags": ["lodestar"]},
                          "98": {"id": 98, "tags": []}}
        self.rules = {k: rules() for k in self.firewalls}
        self.log, self.fail_put = [], False

    def __call__(self, method, path, body):
        p = path.split("?")[0]
        if p == "/networking/firewalls":
            return {"data": list(self.firewalls.values()), "page": 1, "pages": 1}
        parts = p.strip("/").split("/")
        if parts[:2] == ["networking", "firewalls"] and parts[3] == "rules":
            if method == "PUT":
                if self.fail_put and not any(r["label"].startswith("job-42") for r in body["inbound"]):
                    raise RuntimeError("Linode 500")
                self.log.append(("PUT", parts[2], [r["label"] for r in body["inbound"]]))
                self.rules[parts[2]] = body
                return {}
            return json.loads(json.dumps(self.rules[parts[2]]))
        raise AssertionError(f"unexpected {method} {p}")


class FakeSSH:
    """Records each call; answers delivery calls from `rcs` (default 0) and
    clean checks from `clean` (default 0, clean)."""

    def __init__(self, rcs=(), clean=0, firewalls=None):
        self.calls, self.rcs, self.clean, self.fw = [], list(rcs), clean, firewalls

    def __call__(self, argv, data, timeout):
        open_rules = None
        if self.fw is not None:
            open_rules = {k: [r["label"] for r in v["inbound"]] for k, v in self.fw.rules.items()}
        self.calls.append({"argv": argv, "data": data, "timeout": timeout, "rules": open_rules})
        if argv[-1] == deliver.clean_check_command():
            return self.clean
        return self.rcs.pop(0) if self.rcs else 0

    def deliveries(self):
        return [c for c in self.calls if c["argv"][-1] == deliver.remote_command()]


def opt(argv, name):
    return [argv[i + 1] for i, a in enumerate(argv) if a == "-o" and argv[i + 1].startswith(name + "=")]


class Deliver(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.repo, cls.c = make_repo()

    def fleet(self, commit=None, start=T0 - timedelta(hours=1)):
        f = new_fleet(L, "screen", commit or self.c["head"], start)
        for i, r in enumerate(("main", "worker")):
            k = host_key(i + 1)
            f["roles"][r] = {"address": f"203.0.113.{11 + i}", "host_key": k, "fingerprint": fingerprint(k),
                             "firewall_id": str(21 + i), "admin_ipv4": [ADMIN]}
        return f

    def go(self, body="/fleet up screen", fleet_doc=None, ssh=None, fw=None, runs=None, my_ip=MY_IP, live=True, **over):
        self.fw = fw or FakeFirewalls()
        self.ssh = ssh if ssh is not None else FakeSSH(firewalls=self.fw)
        self.s3 = FakeS3(self.fleet() if fleet_doc is None else fleet_doc)
        self.reports, self.emitted, self.connected = [], [], False
        runs = runs or {}

        def connect(env, emit):
            self.connected = True
            return {"s3": self.s3, "login": lambda: dict(LOGIN), "linode": Linode(self.fw), "ssh": self.ssh,
                    "my_ip": lambda: my_ip + "\n", "meta": lambda: RANGES, "lookup_run": runs.get}
        env = event_env(body, **{**(LIVE if live else {}), **over})
        out, clock = io.StringIO(), Clock()
        from contextlib import redirect_stdout
        with redirect_stdout(out):
            rc = deliver.deliver(env, self.reports.append, connect=connect, emit=self.emitted.append,
                                 clock=clock, sleep=clock.sleep, repo=self.repo)
        self.stdout = out.getvalue()
        return rc

    def test_dry_run_names_targets_and_connects_nothing(self):
        self.assertEqual(self.go(live=False), 0)
        self.assertEqual(self.reports, ["deliver-pia: would deliver the PIA login to main, worker"])
        self.assertFalse(self.connected)

    def test_gates_again(self):
        self.assertEqual(self.go(GITHUB_RUN_ATTEMPT="2"), 0)
        self.assertEqual(self.reports, ["deliver-pia: refused, a re-run"])
        self.go("/fleet run renew-vpn main", OPERATOR_STOP="true")
        self.assertEqual(self.reports, ["deliver-pia: refused: OPERATOR_STOP is set: only down runs"])
        self.go("/fleet status")
        self.assertEqual(self.reports, [])
        self.go(AUTHOR_ID="1")  # a stranger: silent
        self.assertEqual((self.reports, self.stdout), ([], ""))
        self.go("/fleet run leak-test main")
        self.assertEqual(self.reports, [])
        self.assertFalse(self.connected)

    def test_up_delivers_to_every_role_inside_its_own_rule(self):
        stale = audit.job_rule("7", "1", "198.51.100.5/32")  # run 7 is unknown: swept first
        fw = FakeFirewalls()
        fw.rules["21"]["inbound"].append(stale)
        rc = self.go(fw=fw)
        self.assertEqual(rc, 0, self.reports)
        self.assertEqual(self.reports, ["deliver-pia: main: delivered; /run clean",
                                        "deliver-pia: worker: delivered; /run clean"])
        self.assertEqual(self.s3.calls, [{"Bucket": "state-b", "Key": FLEET_KEY, "ExpectedBucketOwner": "111122223333"}])
        # Each delivery ran with its /32 open, labelled for this run, and the stale rule gone.
        for call in self.ssh.calls:
            for fid in ("21", "22"):
                self.assertEqual(call["rules"][fid], ["ssh-admin-only", "job-42-1"])
        self.assertEqual(fw.rules["21"], rules())  # closed again afterwards
        self.assertEqual(fw.rules["22"], rules())
        self.assertEqual(fw.rules["98"], rules())  # an untagged firewall is never touched
        opened = [r for r in fw.log if "job-42-1" in r[2]]
        self.assertEqual({r[1] for r in opened}, {"21", "22"})
        self.assertEqual(fw.log[0][0], "PUT")

    def test_ssh_is_pinned_and_carries_the_login_only_on_stdin(self):
        self.go()
        main, worker = self.ssh.deliveries()
        for call, addr, n, region in ((main, "203.0.113.11", 1, "ca_test"), (worker, "203.0.113.12", 2, "us_test")):
            argv = call["argv"]
            self.assertEqual(argv[:3], ["ssh", "-F", "/dev/null"])
            self.assertEqual(argv[-2:], [addr, deliver.remote_command()])
            self.assertEqual(opt(argv, "StrictHostKeyChecking"), ["StrictHostKeyChecking=yes"])
            self.assertEqual(opt(argv, "BatchMode"), ["BatchMode=yes"])
            self.assertEqual(opt(argv, "GlobalKnownHostsFile"), ["GlobalKnownHostsFile=/dev/null"])
            self.assertEqual(argv[argv.index("-l") + 1], "lodestar")
            joined = " ".join(argv)
            for secret in (LOGIN["username"], LOGIN["password"], "FLEETKEYBODY", region):
                self.assertNotIn(secret, joined)
            with tarfile.open(fileobj=io.BytesIO(call["data"])) as tar:
                names = {m.name: m for m in tar.getmembers()}
                self.assertEqual(set(names), {"pia-wireguard.sh", "pia-ca.crt", "login.json", "region"})
                self.assertTrue(all(m.uid == 0 and m.mode & 0o077 == 0 for m in names.values()))
                self.assertEqual(json.loads(tar.extractfile("login.json").read()), LOGIN)
                self.assertEqual(tar.extractfile("region").read().decode(), region)
        self.assertEqual(len([c for c in self.ssh.calls if c["argv"][-1] == deliver.clean_check_command()]), 2)

    def test_known_hosts_holds_only_the_pinned_key(self):
        seen = {}

        class Peek(FakeSSH):
            def __call__(s, argv, data, timeout):
                kh = opt(argv, "UserKnownHostsFile")[0].split("=", 1)[1]
                seen[argv[-2]] = Path(kh).read_text()
                return super().__call__(argv, data, timeout)
        self.go(ssh=Peek())
        self.assertEqual(seen, {"203.0.113.11": f"203.0.113.11 {host_key(1)}\n",
                                "203.0.113.12": f"203.0.113.12 {host_key(2)}\n"})

    def test_renew_vpn_reaches_only_its_vm(self):
        self.assertEqual(self.go("/fleet run renew-vpn worker"), 0)
        self.assertEqual([c["argv"][-2] for c in self.ssh.deliveries()], ["203.0.113.12"])
        self.assertEqual({r[1] for r in self.fw.log}, {"22"})
        self.assertEqual(self.reports, ["deliver-pia: worker: delivered; /run clean"])

    def test_values_are_masked_and_never_printed(self):
        self.go()
        masked = "\n".join(self.emitted)
        for v in ("ca_test", "us_test", MY_IP, "FLEETKEYBODY"):
            self.assertIn("::add-mask::" + v, masked)
        seen = self.stdout + "\n".join(self.reports)
        for v in (LOGIN["username"], LOGIN["password"], "ca_test", "us_test", MY_IP, "203.0.113", "FLEETKEYBODY",
                  "111122223333", "state-b", host_key(1).split()[1], "SHA256:"):
            self.assertNotIn(v, seen)

    def test_mask_escapes_and_splits_lines(self):
        out = []
        deliver.mask("a%b\nsecond line\n\n", out.append)
        self.assertEqual(out, ["::add-mask::a%25b", "::add-mask::second line"])

    def test_refusals_never_connect_to_a_vm(self):
        f = self.fleet()
        cases = {
            "same region": (dict(PIA_SERVERS=json.dumps({"main": "ca_test", "worker": "ca_test"})), None,
                            "PIA_SERVERS gives two roles the same region"),
            "missing role": (dict(PIA_SERVERS=json.dumps({"main": "ca_test"})), None,
                             "PIA_SERVERS must name exactly one region per role"),
            "bad region": (dict(PIA_SERVERS=json.dumps({"main": "ca test", "worker": "x"})), None,
                           "PIA_SERVERS has a malformed region"),
            "no fleet": ({}, tombstone(T0), "no fleet"),
            "unreadable": ({}, "unreadable", "fleet.json couldn't be read"),
            "off main": ({}, self.fleet(commit=self.c["side"]), "the fleet's pinned commit is not an ancestor of main"),
            "expired": ({}, self.fleet(start=T0 - timedelta(hours=45)), "the fleet is past its expiry"),
            "swapped key": ({}, dict(f, roles=dict(f["roles"], main=dict(f["roles"]["main"], host_key=host_key(9)))),
                            "a target's host key doesn't match its recorded fingerprint"),
            "unfilled": ({}, dict(f, roles=dict(f["roles"], worker=dict(f["roles"]["worker"], address=None))),
                         "a target's slot in fleet.json isn't filled"),
        }
        for name, (over, doc, reason) in cases.items():
            with self.subTest(name):
                if doc == "unreadable":
                    rc = self._go_with_s3(FakeS3(None))
                else:
                    rc = self.go(fleet_doc=doc, **over)
                self.assertEqual(rc, 1)
                self.assertEqual(self.reports, [f"deliver-pia: refused: {reason}"])
                self.assertEqual(self.ssh.calls, [])
                self.assertEqual(self.fw.log, [])

    def _go_with_s3(self, s3):
        self.fw, self.ssh, self.reports, self.emitted = FakeFirewalls(), FakeSSH(), [], []

        def connect(env, emit):
            return {"s3": s3, "login": lambda: dict(LOGIN), "linode": Linode(self.fw), "ssh": self.ssh,
                    "my_ip": lambda: MY_IP, "meta": lambda: RANGES, "lookup_run": {}.get}
        clock = Clock()
        return deliver.deliver(event_env("/fleet up screen", **LIVE), self.reports.append, connect=connect,
                               emit=self.emitted.append, clock=clock, sleep=clock.sleep, repo=self.repo)

    def test_a_runner_outside_actions_ranges_opens_nothing(self):
        rc = self.go(my_ip="192.0.2.50")
        self.assertEqual(rc, 1)
        self.assertEqual(self.reports, ["deliver-pia: refused: this runner's address isn't in GitHub's Actions ranges"])
        self.assertEqual(self.ssh.calls, [])
        self.assertFalse([r for r in self.fw.log if "job-42-1" in r[2]])

    def test_a_checkip_answer_that_isnt_one_ipv4_refuses(self):
        for answer in ("198.51.100.77\n198.51.100.78", "", "2001:db8::1", "198.51.100.0/24", "not an address",
                       "198.51.100.77 extra"):
            with self.subTest(answer=answer):
                rc = self.go(my_ip=answer)
                self.assertEqual(rc, 1)
                self.assertEqual(self.reports, ["deliver-pia: refused: this runner's address isn't an IPv4 address"])
                self.assertEqual(self.ssh.calls, [])
                self.assertFalse([r for r in self.fw.log if "job-42-1" in r[2]])

    def test_an_unreachable_vm_is_retried_then_reported_and_the_rule_still_closes(self):
        ssh = FakeSSH(rcs=[255] * 100)
        rc = self.go(ssh=ssh)
        self.assertEqual(rc, 1)
        self.assertIn("deliver-pia: main: couldn't connect, or its host key didn't match the pin", self.reports)
        tries = len([c for c in ssh.deliveries() if c["argv"][-2] == "203.0.113.11"])
        self.assertEqual(tries, deliver.SSH_ATTEMPTS_SECONDS // 5 + 1)  # bounded by the deadline, not a count
        self.assertEqual(self.fw.rules["21"], rules())

    def test_a_failed_step_still_checks_run_is_clean(self):
        rc = self.go(ssh=FakeSSH(rcs=[12, 0]))
        self.assertEqual(rc, 1)
        self.assertEqual(self.reports, ["deliver-pia: main: the VM step failed (exit 12); /run clean",
                                        "deliver-pia: worker: delivered; /run clean"])

    def test_dirty_run_escalates(self):
        rc = self.go(ssh=FakeSSH(clean=1))
        self.assertEqual(rc, 1)
        self.assertIn("deliver-pia: main: delivered; /run NOT clean; escalate", self.reports)

    def test_a_rule_that_wont_close_escalates(self):
        fw = FakeFirewalls()
        fw.fail_put = True  # opening works, closing fails
        rc = self.go(fw=fw)
        self.assertEqual(rc, 1)
        self.assertEqual(self.reports[-1],
                         "deliver-pia: its firewall rule couldn't be closed; the next sweep removes it; escalate")

    def test_a_live_rule_of_another_run_is_kept(self):
        fw = FakeFirewalls()
        other = audit.job_rule("77", "1", "198.51.100.6/32")
        fw.rules["21"]["inbound"].append(other)
        live = {"77": {"status": "in_progress", "path": ".github/workflows/fleet.yml", "run_attempt": 1,
                       "run_started_at": (T0 - timedelta(minutes=5)).strftime("%Y-%m-%dT%H:%M:%SZ")}}
        self.go(fw=fw, runs=live)
        self.assertIn(other, fw.rules["21"]["inbound"])

    def test_pia_login_shape(self):
        self.assertEqual(deliver.pia_login(json.dumps(LOGIN)), LOGIN)
        for bad in ("not json", "[]", json.dumps({"username": "u"}), json.dumps({"username": "u", "password": 5})):
            with self.assertRaises(deliver.Refusal):
                deliver.pia_login(bad)


class EndToEnd(unittest.TestCase):
    """The real payload, the real bootstrap and the real VM step, run locally:
    an SSH stand-in that executes the bootstrap with temporary paths."""

    def test_payload_bootstrap_and_step(self):
        tmp = Path(tempfile.mkdtemp())
        self.addCleanup(subprocess.run, ["rm", "-rf", str(tmp)])
        stub = tmp / "stub"
        (stub / "bin").mkdir(parents=True)
        for name, body in (("curl", CURL), ("wg", WG)):
            (stub / "bin" / name).write_text(body)
            (stub / "bin" / name).chmod(0o755)
        (stub / "token.json").write_text(json.dumps({"token": TOKEN}))
        (stub / "servers.json").write_text(json.dumps(SERVERS))
        (stub / "addkey.json").write_text(json.dumps(ADDKEY))
        run_dir, out = tmp / "run" / "lodestar-pia", tmp / "secrets" / "wg0.conf"
        run_dir.parent.mkdir()  # /run itself always exists
        env = {"PATH": f"{stub / 'bin'}:/usr/bin:/bin", "STUB": str(stub), "STUB_MODE": "", "TOKEN": TOKEN,
               "PRIV": PRIV, "PUB": PUB}
        r = subprocess.run(["bash", "-c", deliver.bootstrap(str(run_dir), str(out))],
                           input=deliver.payload(LOGIN, "ca_test"), env=env, capture_output=True, timeout=60)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn(f"PrivateKey = {PRIV}", out.read_text())
        self.assertFalse(run_dir.exists())
        clean = subprocess.run(["bash", "-c", deliver.clean_check_command(str(run_dir)).replace("sudo -n ", "")])
        self.assertEqual(clean.returncode, 0)
        # A failing step: the bootstrap's own trap still empties /run.
        r = subprocess.run(["bash", "-c", deliver.bootstrap(str(run_dir), str(out))],
                           input=deliver.payload(LOGIN, "nowhere"), env=env, capture_output=True, timeout=60)
        self.assertEqual(r.returncode, 13)
        self.assertFalse(run_dir.exists())

    def test_remote_command_is_fixed(self):
        cmd = deliver.remote_command()
        self.assertTrue(cmd.startswith("sudo -n bash -c "))
        self.assertIn("/run/lodestar-pia", cmd)
        self.assertIn("trap '\"'\"'rm -rf -- \"$d\"'\"'\"' EXIT", cmd)
        for sig in ("HUP", "INT", "TERM"):
            self.assertIn(f" {sig}\n", cmd)


if __name__ == "__main__":
    unittest.main()
