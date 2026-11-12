"""OP-3a's steps: the control side (remote.py, run.py, act's live `run`), the
committed compose file, and the VM scripts against stubs. No network, no SSH,
no Docker: SSH is a fake and docker, iptables, tcpdump and ip are stub scripts."""

import base64
import io
import json
import os
import subprocess
import tarfile
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from unittest import mock

import yaml

import ancestry
import apply
import fleet
from lock import MemoryStore
from steps import remote, run as steps
from tests.helpers import Clock, FakeGitHub, event_env, make_repo
from tests.test_live import ENV, FakeLinodeAPI, RANGES, backend

VM = Path(steps.__file__).resolve().parent / "vm"
ROOT = Path(__file__).resolve().parents[3]


def host_key(n: int) -> str:
    return "ssh-ed25519 " + base64.b64encode(b"\x00\x00\x00\x0bssh-ed25519\x00\x00\x00\x20" + bytes([n]) * 32).decode()


def slot(n=1, **over):
    k = host_key(n)
    return dict({"address": f"203.0.113.{10 + n}", "host_key": k, "fingerprint": apply.fingerprint(k),
                 "firewall_id": str(20 + n), "admin_ipv4": ["203.0.113.4/32"]}, **over)


class Grammar(unittest.TestCase):
    def test_only_grammar_lines_survive(self):
        out = (b"ok tunnel_healthy\n"
               b"fail no_egress_captured\n"
               b"count captured_packets 3\n"
               b"skip host_ipv6\n"
               b"ok exit 203.0.113.9\n"                      # an address after a name
               b"203.0.113.9\n"                             # a bare address
               b"count disk_free_gib 1.5\n"                 # not an integer
               b"count x " + b"9" * 13 + b"\n"              # too long a number
               b"ok " + b"a" * 60 + b"\n"                   # too long a name
               b"\x1b[31mok coloured\x1b[0m\n"              # ANSI escapes
               b"ok Upper\n"                                # outside the vocabulary
               b"ok\ttab\n"
               b"ok dns_up\r\n"                             # a CR before the newline
               b"ok r\xc3\xa9sum\xc3\xa9\n"                 # non-ASCII
               b"ok last_line")                             # no final newline
        self.assertEqual(remote.parse(out), ["ok tunnel_healthy", "fail no_egress_captured",
                                             "count captured_packets 3", "skip host_ipv6", "ok dns_up", "ok last_line"])

    def test_output_is_bounded(self):
        self.assertEqual(len(remote.parse(b"ok a\n" * 10_000)), remote.MAX_LINES)
        self.assertEqual(remote.parse(b"x" * (remote.MAX_OUTPUT + 10) + b"\nok late\n"), [])


class Payload(unittest.TestCase):
    def test_files_and_args(self):
        data = remote.payload({"step.sh": b"echo", "compose.yaml": b"x"}, {"RULES": "a" * 40})
        with tarfile.open(fileobj=io.BytesIO(data)) as tar:
            members = {m.name: m for m in tar.getmembers()}
            self.assertEqual(set(members), {"step.sh", "compose.yaml", "args"})
            self.assertEqual(members["step.sh"].mode, 0o700)
            self.assertEqual(members["compose.yaml"].mode, 0o600)
            self.assertTrue(all(m.uid == 0 and m.gid == 0 for m in members.values()))
            self.assertEqual(tar.extractfile("args").read(), b"RULES=" + b"a" * 40 + b"\n")

    def test_hostile_args_refused(self):
        for k, v in (("RULES", "a b"), ("RULES", "$(id)"), ("RULES", "x\ny"), ("rules", "x"), ("X;Y", "z"),
                     ("RULES", "../../etc"), ("RULES", ""), ("RULES", "A" * 65)):
            with self.subTest(k=k, v=v), self.assertRaises(remote.StepRefused):
                remote.payload({}, {k: v})

    def test_bootstrap_takes_the_vm_lock_first_and_bounds_the_step(self):
        boot = remote.bootstrap(60)
        lines = boot.splitlines()
        self.assertEqual(lines[:4], ["set -eu", "umask 077", "exec 9>/run/lodestar-pia.lock",
                                     "flock -w 60 9 || { echo 'fail vm_busy'; exit 21; }"])
        self.assertIn("trap 'rm -rf -- \"$d\"' EXIT", lines)
        self.assertEqual(lines[-1], 'timeout -s TERM 60 bash "$d/step.sh" "$d"')
        self.assertTrue(remote.remote_command(60).startswith("sudo -n bash -c "))


class Bootstrap(unittest.TestCase):
    """The real bootstrap, run locally with temporary paths (no sudo)."""

    def test_runs_the_step_and_cleans_up_and_waits_on_the_vm_lock(self):
        import fcntl
        tmp = Path(tempfile.mkdtemp())
        self.addCleanup(subprocess.run, ["rm", "-rf", str(tmp)])
        run_dir, lock = tmp / "lodestar-step", tmp / "lodestar-pia.lock"
        step = b'#!/usr/bin/env bash\necho "ok ran"\necho "203.0.113.9"\ntest -s "$1/args" || echo "fail args"\n'

        def boot():
            r = subprocess.run(["bash", "-c", remote.bootstrap(30, str(run_dir), str(lock))],
                               input=remote.payload({"step.sh": step}, {"X": "y"}), capture_output=True, timeout=60)
            return r.returncode, remote.parse(r.stdout)
        self.assertEqual(boot(), (0, ["ok ran"]))
        self.assertFalse(run_dir.exists())
        with open(lock, "w") as held, mock.patch.object(remote, "FLOCK_WAIT", 1):
            fcntl.flock(held, fcntl.LOCK_EX)  # a delivery (or another step) holds the VM
            self.assertEqual(boot(), (remote.BUSY_VM, ["fail vm_busy"]))
        self.assertFalse(run_dir.exists())


class Target(unittest.TestCase):
    def test_shapes(self):
        remote.check_target(slot(), apply.fingerprint)
        for name, bad in {"hostname": slot(address="vm.example"), "loopback": slot(address="127.0.0.1"),
                          "reserved": slot(address="240.0.0.1"), "multicast": slot(address="224.0.0.1"),
                          "link-local": slot(address="169.254.1.1"), "leading zero": slot(address="203.0.113.011"),
                          "two lines": slot(host_key=host_key(1) + "\n* " + host_key(2)),
                          "swapped key": slot(host_key=host_key(2)),
                          "garbage key": slot(host_key="ssh-ed25519 AAAA", fingerprint="x")}.items():
            with self.subTest(name), self.assertRaises(remote.StepRefused):
                remote.check_target(bad, apply.fingerprint)


class FakeBackend:
    def __init__(self, open_error=None, close_error=None):
        self.calls, self.open_error, self.close_error = [], open_error, close_error

    def set_job_rules(self, fleet_doc, run, attempt, cidr, fence):
        self.calls.append((run, attempt, cidr))
        fence()
        err = self.open_error if cidr else self.close_error
        if err:
            raise err


class RunStep(unittest.TestCase):
    def setUp(self):
        self.fleet = {"commit": "a" * 40, "roles": {"main": slot(1), "worker": slot(2)}}
        self.emitted, self.ssh_calls, self.slept = [], [], []
        self.now = 1000.0  # a fake monotonic clock: sleeping advances it, nothing waits
        self.files = mock.patch.object(steps, "step_files", return_value={"step.sh": b"echo"})
        self.files.start()
        self.addCleanup(self.files.stop)

    def ssh(self, rc=0, out=b"ok a\nok b\nfail c\ncount n 4\n203.0.113.11\n"):
        def call(argv, data, timeout):
            self.ssh_calls.append((argv, data, timeout))
            return rc, out
        return call

    def go(self, b=None, ssh=None, ip="198.51.100.7\n", step="vm-status", vm="main", **env):
        self.b = b or FakeBackend()
        return steps.run_step({"FLEET_SSH_KEY": "KEYLINE1\nKEYLINE2", **env}, self.b, self.fleet, step, vm, "42-1",
                              lambda: None, ssh=ssh or self.ssh(), my_ip=lambda: ip, emit=self.emitted.append,
                              repo=".", fingerprint=apply.fingerprint, sleep=self.sleep, monotonic=lambda: self.now)

    def sleep(self, s):
        self.slept.append(s)
        self.now += s

    def test_happy_path(self):
        line = self.go()
        steps.step_files.assert_called_once_with("vm-status", "a" * 40, ".")  # the fleet's pinned commit
        self.assertEqual(line, "done; 2 ok; failed: c; n 4")
        self.assertEqual(self.b.calls, [("42", "1", "198.51.100.7/32"), ("42", "1", None)])  # opened, then closed
        argv, data, timeout = self.ssh_calls[0]
        self.assertEqual(argv[-2:], ["203.0.113.11", remote.remote_command(60)])
        self.assertEqual(timeout, 60 + remote.FLOCK_WAIT + 60)
        self.assertIn("::add-mask::KEYLINE1", self.emitted)
        self.assertIn("::add-mask::198.51.100.7", self.emitted)
        self.assertNotIn("203.0.113.11", line)

    def test_known_hosts_holds_only_the_pin(self):
        seen = {}

        def ssh(argv, data, timeout):
            kh = [a for a in argv if a.startswith("UserKnownHostsFile=")][0].split("=", 1)[1]
            seen["kh"] = Path(kh).read_text()
            return 0, b""
        self.go(ssh=ssh, vm="worker")
        self.assertEqual(seen["kh"], f"203.0.113.12 {host_key(2)}\n")

    def test_outcomes(self):
        for rc, head in ((255, "couldn't connect, or its host key didn't match the pin"),
                         (256, "the session didn't finish in time"), (21, "the VM is busy"), (124, "the step timed out"),
                         (3, "failed (exit 3)")):
            with self.subTest(rc=rc):
                self.assertTrue(self.go(ssh=self.ssh(rc=rc, out=b"")).startswith(head))

    def test_ssh_255_is_retried_until_it_connects(self):
        rcs = [255, 255, 0]

        def ssh(argv, data, timeout):
            self.ssh_calls.append(argv)
            return rcs.pop(0), b"ok a\n"
        b = FakeBackend()
        self.assertEqual(self.go(b=b, ssh=ssh), "done; 1 ok")
        self.assertEqual(len(self.ssh_calls), 3)
        self.assertEqual(self.slept, [steps.SSH_RETRY_PAUSE] * 2)
        self.assertEqual([c[2] for c in b.calls], ["198.51.100.7/32", None])  # one rule, closed once at the end

    def test_ssh_255_retries_stop_after_about_two_minutes(self):
        line = self.go(ssh=self.ssh(rc=255, out=b""))
        self.assertTrue(line.startswith("couldn't connect"))
        self.assertGreaterEqual(sum(self.slept), steps.SSH_ATTEMPTS_SECONDS)
        self.assertLess(sum(self.slept), steps.SSH_ATTEMPTS_SECONDS + 2 * steps.SSH_RETRY_PAUSE)
        self.assertEqual(steps.SSH_ATTEMPTS_SECONDS, 120)

    def test_other_failures_are_never_retried(self):
        for rc in (0, 256, 21, 124, 1):  # a session ran (or may still be running): never a second one
            with self.subTest(rc=rc):
                self.ssh_calls.clear()
                self.go(ssh=self.ssh(rc=rc, out=b""))
                self.assertEqual(len(self.ssh_calls), 1)

    def test_refusals(self):
        with self.assertRaises(remote.StepRefused):
            self.go(step="renew-vpn")  # deliver-pia's, never a step here
        with self.assertRaisesRegex(remote.StepRefused, "needs a rules commit"):
            self.go(step="start-block")
        with self.assertRaises(remote.StepRefused):
            self.go(FLEET_SSH_KEY=" ")
        for ip in ("", "198.51.100.7\n198.51.100.8", "2001:db8::1", "junk"):
            with self.subTest(ip=ip), self.assertRaises(remote.StepRefused):
                self.go(ip=ip)
        self.assertEqual(self.ssh_calls, [])

    def test_outside_actions_ranges_still_closes(self):
        b = FakeBackend(open_error=ValueError("refused"))
        with self.assertRaises(remote.StepRefused) as e:
            self.go(b=b)
        self.assertEqual(str(e.exception), "this runner's address isn't in GitHub's Actions ranges")
        self.assertEqual([c[2] for c in b.calls], ["198.51.100.7/32", None])
        self.assertEqual(self.ssh_calls, [])
        b = FakeBackend(open_error=ValueError("refused"), close_error=RuntimeError("Linode 500"))
        with self.assertRaises(remote.StepRefused) as e:
            self.go(b=b)
        self.assertIn("couldn't be closed; the next sweep removes it; escalate", str(e.exception))

    def test_a_rule_that_wont_close_is_escalated_with_the_result(self):
        line = self.go(b=FakeBackend(close_error=RuntimeError("Linode 500")))
        self.assertTrue(line.startswith("done; 2 ok"))
        self.assertTrue(line.endswith("its firewall rule couldn't be closed; the next sweep removes it; escalate"))

    def test_a_firewall_error_is_a_refusal_and_a_lost_lock_stops(self):
        from linode import LinodeError
        from lock import LockLost
        b = FakeBackend(open_error=LinodeError("Linode API PUT failed: HTTP 500"))
        with self.assertRaises(remote.StepRefused) as e:
            self.go(b=b)
        self.assertEqual(str(e.exception), "its firewall rule couldn't be opened")
        self.assertEqual([c[2] for c in b.calls], ["198.51.100.7/32", None])
        with self.assertRaises(LockLost):
            self.go(b=FakeBackend(open_error=LockLost("lost")))
        self.assertEqual(self.ssh_calls, [])

    def test_the_close_uses_the_tolerant_fence(self):
        used = []
        b = FakeBackend()
        steps.run_step({"FLEET_SSH_KEY": "K"}, b, self.fleet, "vm-status", "main", "42-1", lambda: used.append("strict"),
                       ssh=self.ssh(), my_ip=lambda: "198.51.100.7", emit=[].append, repo=".",
                       fingerprint=apply.fingerprint, close_fence=lambda: used.append("tolerant"))
        self.assertEqual(used, ["strict", "tolerant"])  # opened under the strict fence, closed under the tolerant one

    def test_an_ssh_error_still_closes_the_rule(self):
        def boom(argv, data, timeout):
            raise OSError("ssh missing")
        b = FakeBackend()
        with self.assertRaises(OSError):
            self.go(b=b, ssh=boom)
        self.assertEqual([c[2] for c in b.calls], ["198.51.100.7/32", None])


class StepFiles(unittest.TestCase):
    def test_read_at_the_pinned_commit(self):
        repo, c = make_repo()
        d = Path(repo) / steps.STEPS_PATH
        d.mkdir(parents=True)
        (d / "vm-status.sh").write_text("echo pinned\n")
        (d / "block-lib.sh").write_text("lib\n")
        subprocess.run(["git", "-C", repo, "add", "-A"], check=True)
        subprocess.run(["git", "-C", repo, "commit", "-qm", "steps"], check=True)
        pinned = subprocess.run(["git", "-C", repo, "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()
        (d / "vm-status.sh").write_text("echo later\n")  # the checkout differs: the pinned commit wins
        # The first file is the step; the rest keep their names.
        self.assertEqual(steps.step_files("vm-status", pinned, repo), {"step.sh": b"echo pinned\n",
                                                                       "block-lib.sh": b"lib\n"})
        with self.assertRaises(remote.StepRefused):  # pinned before the step existed
            steps.step_files("vm-status", c["head"], repo)


class LiveAct(unittest.TestCase):
    """act's live `run`, end to end with the live backend, a fake Linode API and fake SSH."""

    def test_run_vm_status(self):
        repo, c = make_repo()
        api = FakeLinodeAPI()
        api.linodes, api.firewalls = {"99": api.linodes["99"]}, {"98": api.firewalls["98"]}
        b, s3, api, _ = backend(api=api)
        out, gh = io.StringIO(), FakeGitHub()
        env = event_env("/fleet up screen", FLEET_LIVE="true", **ENV)
        with redirect_stdout(out), mock.patch.object(ancestry, "remote_main_head", return_value=c["head"]):
            fleet.main(["act"], env=env, backend=b, store=MemoryStore(), transport=gh, clock=Clock(), repo=repo)
        full = FakeLinodeAPI()  # the fleet's firewalls, as Terraform made them
        api.firewalls.update({k: full.firewalls[k] for k in ("21", "22")})
        api.rules.update({k: full.rules[k] for k in ("21", "22")})
        calls = []

        def ssh(argv, data, timeout):
            calls.append(argv)
            # While the step runs, this job's rule is open on the fleet's firewalls.
            self.assertTrue(any(r.get("label") == "job-42-1" for r in api.rules["21"]["inbound"]))
            return 0, b"ok gluetun_running\ncount collectors_running 0\n203.0.113.11 leaked\n"
        env = event_env("/fleet run vm-status main", FLEET_LIVE="true", FLEET_SSH_KEY="KEY", **ENV)
        out, gh = io.StringIO(), FakeGitHub()
        with redirect_stdout(out), redirect_stderr(out), \
                mock.patch.object(ancestry, "remote_main_head", return_value=c["head"]), \
                mock.patch.object(steps, "step_files", return_value={"step.sh": b"x"}):
            rc = fleet.main(["act"], env=env, backend=b, store=MemoryStore(), transport=gh, clock=Clock(), repo=repo,
                            step_io={"ssh": ssh, "my_ip": lambda: "198.51.100.7"})
        seen = out.getvalue() + "\n".join(gh.bodies())
        self.assertEqual(rc, 0, seen[:400])
        self.assertIn("run vm-status main: done; 1 ok; collectors_running 0", seen)
        self.assertNotIn("leaked", seen)
        # Nothing private outside the add-mask lines (which the runner consumes, never shows).
        shown = "\n".join(ln for ln in seen.splitlines() if not ln.startswith("::add-mask::"))
        slot_doc = s3.doc("state-b", "fleet/fleet.json")["roles"]["main"]
        for value in ("KEY", "198.51.100.7", slot_doc["address"], slot_doc["host_key"].split()[1],
                      slot_doc["fingerprint"], "111122223333", "state-b", "control-b"):
            self.assertNotIn(value, shown)
        self.assertEqual(len(calls), 1)
        self.assertFalse(any(r.get("label", "").startswith("job-") for r in api.rules["21"]["inbound"]))  # closed


class Compose(unittest.TestCase):
    """The committed VM stack (runner's Q1 and Q2)."""

    @classmethod
    def setUpClass(cls):
        cls.doc = yaml.safe_load((VM / "compose.yaml").read_text())
        cls.svc = cls.doc["services"]

    def test_gluetun_pinned_by_digest_with_its_firewall_on(self):
        g = self.svc["gluetun"]
        self.assertRegex(g["image"], r"^qmcgaw/gluetun@sha256:[0-9a-f]{64}$")  # never a tag
        self.assertEqual(g["environment"]["VPN_SERVICE_PROVIDER"], "custom")
        self.assertEqual(g["environment"]["VPN_TYPE"], "wireguard")
        self.assertEqual(g["environment"]["VPN_INTERFACE"], "wg0")  # the leak test's phase B reads it
        self.assertEqual(g["environment"]["TZ"], "Etc/UTC")
        self.assertEqual(g["environment"]["FIREWALL_VPN_INPUT_PORTS"], "")
        self.assertFalse(any(k.startswith("FIREWALL_ENABLED") for k in g["environment"]))  # its firewall stays on
        self.assertEqual(g["oom_score_adj"], -500)
        self.assertEqual(g["volumes"], ["./secrets/wg0.conf:/gluetun/wireguard/wg0.conf:ro"])

    def test_gluetun_loads_the_post_rules_read_only(self):
        """X-gluetun-leak: gluetun's post-rules close its established-flow leak.
        Compose mounts a config read-only; the text is the shipped file's."""
        g = self.svc["gluetun"]
        self.assertEqual(g["configs"], [{"source": "gluetun-post-rules", "target": "/iptables/post-rules.txt"}])
        text = self.doc["configs"]["gluetun-post-rules"]["content"]
        self.assertEqual(text, (ROOT / "packaging/companions/gluetun-post-rules.txt").read_text())
        self.assertNotIn("$", text)  # nothing for compose to interpolate
        rules = [ln for ln in text.splitlines() if ln.startswith(("iptables", "ip6tables"))]
        # gluetun runs only lines that start with these words; every other line is a comment.
        for ln in text.splitlines():
            self.assertTrue(ln.startswith(("iptables ", "ip6tables ", "#")), ln)
        self.assertEqual(rules, [
            "iptables -I OUTPUT 1 -o wg0 -j CONNMARK --set-xmark 0x800000/0x800000",
            "ip6tables -I OUTPUT 1 -o wg0 -j CONNMARK --set-xmark 0x800000/0x800000",
            "iptables -I OUTPUT 2 ! -o wg0 -m connmark --mark 0x800000/0x800000 -j DROP",
            "ip6tables -I OUTPUT 2 ! -o wg0 -m connmark --mark 0x800000/0x800000 -j DROP",
            "iptables -I OUTPUT 3 ! -o wg0 -p icmp --icmp-type port-unreachable -j DROP",
            "ip6tables -I OUTPUT 3 ! -o wg0 -p ipv6-icmp --icmpv6-type port-unreachable -j DROP",
        ])
        # Never loosens gluetun's firewall: each rule marks or drops, on the interface
        # the leak test reads, for IPv4 and IPv6 alike.
        for r in rules:
            self.assertRegex(r, r"-j (DROP|CONNMARK --set-xmark 0x800000/0x800000)$")
            self.assertNotRegex(r, r"ACCEPT|RETURN|-P |-F|-X|-D |-t ")
            self.assertIn(" wg0 ", r)
        self.assertEqual(g["environment"]["VPN_INTERFACE"], "wg0")  # gluetun's image defaults to tun0

    def test_the_pia_example_ships_the_same_post_rules(self):
        doc = yaml.safe_load((ROOT / "infra/akamai/terraform/pia-example/compose.vpn.yaml").read_text())
        g = doc["services"]["gluetun"]
        self.assertEqual(g["image"], self.svc["gluetun"]["image"])  # pinned, the version the rules were checked on
        self.assertEqual(g["environment"]["VPN_INTERFACE"], "wg0")
        self.assertEqual(g["configs"], self.svc["gluetun"]["configs"])
        self.assertEqual(doc["configs"], self.doc["configs"])

    def test_only_the_ui_on_the_hosts_loopback(self):
        ports = [p for s in self.svc.values() for p in s.get("ports", [])]
        self.assertEqual(ports, ["127.0.0.1:8090:8090"])
        self.assertNotIn("network_mode", self.svc["gluetun"])

    def test_lodestar_in_gluetuns_namespace_unprivileged(self):
        lod = self.svc["lodestar"]
        self.assertEqual(lod["network_mode"], "service:gluetun")
        self.assertEqual(lod["image"], "${LODESTAR_IMAGE:?start-block sets it from the screen rules}")
        self.assertEqual(lod["profiles"], ["block"])  # tunnel-up never starts it
        for k in ("privileged", "devices", "pid", "ports"):
            self.assertNotIn(k, lod)
        # No raw sockets past gluetun's firewall: everything dropped but the three
        # capabilities the image needs to drop root.
        self.assertEqual(lod["cap_drop"], ["ALL"])
        # The restart witness on the host, outside the data folder (CR-SPEC 2.3; OP-3b).
        self.assertEqual(lod["volumes"], ["./data:/data", "./witness:/var/lib/lodestar-witness"])
        self.assertEqual(lod["cap_add"], ["CHOWN", "SETGID", "SETUID"])
        # The arm's switches pass through by name; nothing else is set.
        self.assertEqual(lod["environment"], ["TZ=Etc/UTC", "LODESTAR_DHT_SAFE_RESTART",
                                              "LODESTAR_DHT_COORDINATED_ADMISSION", "LODESTAR_DHT_UTP",
                                              "LODESTAR_DHT_ROUTE_BACKOFF"])
        self.assertIn("no-new-privileges:true", lod["security_opt"])
        self.assertEqual(set(self.svc), {"gluetun", "lodestar", "docker-proxy"})

    def test_the_probe_image_is_pinned_too(self):
        text = (VM / "block-lib.sh").read_text()  # the leak test and the block steps share it
        pins = [ln for ln in text.splitlines() if ln.startswith("PROBE_IMAGE=")]
        self.assertEqual(len(pins), 1)
        self.assertRegex(pins[0], r"^PROBE_IMAGE=nicolaka/netshoot@sha256:[0-9a-f]{64}$")
        pins = [ln for ln in text.splitlines() if ln.startswith("COLLECTOR_IMAGE=")]
        self.assertEqual(len(pins), 1)
        self.assertRegex(pins[0], r"^COLLECTOR_IMAGE=python@sha256:[0-9a-f]{64}$")
        self.assertNotRegex((VM / "leak-test.sh").read_text(), r"(?m)^(PROBE|COLLECTOR)_IMAGE=")


STUB_DOCKER = r"""#!/usr/bin/env bash
printf 'docker %s\n' "$*" >> "$STUB/log"
bad() { printf '%s\n' "$*" >> "$STUB/badargs"; exit 125; }
case "$1" in
  info) [ -n "$NO_DOCKER" ] && exit 1; echo 27.0 ;;
  compose) [ -n "$COMPOSE_FAIL" ] && exit 1; rm -f "$STUB/ifdown"; exit 0 ;;  # a recreated gluetun has its interface back
  inspect)
    case "$*" in
      *Health*) if [ -e "$STUB/was_blocked" ] && [ -n "$NO_RECOVER" ]; then echo unhealthy; else echo healthy; fi ;;
      *Config.Env*) printf 'PATH=/usr/bin\nVPN_INTERFACE=%s\n' "${VPN_IFACE-wg0}" ;;
      *Running*gluetun) echo true ;;
      *Running*lodestar) [ -n "$LODESTAR" ] && echo true || echo false ;;
    esac ;;
  ps)
    case "$*" in
      *--filter*) [ -n "$COLLECTORS" ] && printf 'a\nb\n'; exit 0 ;;  # vm-status's count
      "ps --format {{.Names}}")
        [ -n "$PS_FAIL" ] && exit 1
        echo gluetun; [ -n "$LODESTAR" ] && echo lodestar
        [ -n "$COLLECTORS" ] && printf 'lodestar-collector-1\nlodestar-collector-2\n'
        exit 0 ;;
      *) bad "$*" ;;
    esac ;;
  pull) [ -n "$PULL_FAIL" ] && exit 1; exit 0 ;;
  exec)
    case "$*" in
      "exec gluetun cat /sys/class/net/eth0/iflink") echo 7 ;;
      # post_rules_on: exactly the script's check, with the family and gluetun's interface.
      "exec gluetun sh -c "*" rules iptables ${VPN_IFACE-wg0}"|"exec gluetun sh -c "*" rules ip6tables ${VPN_IFACE-wg0}")
        case "$*" in *'-C OUTPUT ! -o "$2" -m connmark --mark 0x800000/0x800000 -j DROP'*) ;; *) bad "$*" ;; esac
        [ -n "$NO_POST_RULES" ] && exit 1; exit 0 ;;
      *) bad "$*" ;;
    esac ;;
  network) [ "$*" = "network inspect -f {{.EnableIPv6}} lodestar_default" ] || bad "$*"
           [ -n "$BRIDGE_V6" ] && echo true || echo false ;;
  run)
    # Exactly: run --rm --network container:gluetun --cap-drop ALL [--cap-add NET_ADMIN]
    #          <pinned netshoot> sh -c '"$@"; printf "\nprobe_rc=%s\n" "$?"' probe <command...>
    [ "$1 $2 $3 $4 $5 $6" = "run --rm --network container:gluetun --cap-drop ALL" ] || bad "$*"
    shift 6
    admin=
    if [ "$1 $2" = "--cap-add NET_ADMIN" ]; then admin=1; shift 2; fi
    case "$1" in nicolaka/netshoot@sha256:????????????????????????????????????????????????????????????????) ;; *) bad "$*" ;; esac
    shift
    [ "$1" = sh ] && [ "$2" = -c ] && [ "$3" = '"$@"; printf "\nprobe_rc=%s\n" "$?"' ] && [ "$4" = probe ] || bad "$*"
    shift 4
    if [ -n "$admin" ] && [ "$*" != "ip link set dev ${VPN_IFACE-wg0} down" ]; then bad "$*"; fi
    if [ -e "$STUB/blocked" ]; then down=1; else down=; fi
    [ -n "$down" ] && [ -n "$PROBE_NOT_RUN" ] && exit 125  # the container never ran the command
    # With the interface down, only gluetun's firewall holds: off, everything leaks.
    # (Without its post-rules it leaks only flows opened earlier: the capture and
    # the host counter see those; a fresh probe still fails.)
    if [ -e "$STUB/ifdown" ] && [ -n "$FIREWALL_OFF" ]; then leak=1; else leak=; fi
    echo "$*" >> "$STUB/probes"
    (
      case "$*" in
        "curl -4"*) [ -n "$down" ] && [ -z "$LEAK_V4" ] && [ -z "$leak" ] && exit 1
                    if [ -n "$down" ] || [ ! -e "$STUB/was_blocked" ] || [ -z "$NEW_EXIT" ]; then echo "${EXIT_IP:-198.51.100.200}"; else echo 198.51.100.201; fi ;;
        "curl -6"*) [ -n "$V6_OPEN" ] || [ -n "$leak" ] || exit 1 ;;
        "nc -z"*) [ -n "$down" ] && [ -z "$LEAK_TCP" ] && [ -z "$leak" ] && exit 1; [ -n "$NO_TCP" ] && exit 1; exit 0 ;;
        # The script's own dig | grep pipeline runs, against the stub dig, which
        # answers like the real one (a timeout prints ";;" lines on stdout too).
        "sh -c dig"*) if [ -n "$down" ] && [ -z "$leak" ]; then DIG_FAIL=1; else DIG_FAIL=$DIG_UP_FAIL; fi
                      DIG_FAIL=$DIG_FAIL "$@" ;;
        "sh -c ip -o link show up dev \"\$1\" | grep -q . iface ${VPN_IFACE-wg0}")
                      [ -e "$STUB/ifdown" ] && [ -z "$IFACE_BACK" ] && exit 1; exit 0 ;;
        "sh -c command"*) [ -n "$NO_TOOLS" ] && exit 1; exit 0 ;;
        "sh -c ip -6 -o addr show scope global | grep -q .") [ -n "$NS_V6" ] || exit 1; exit 0 ;;
        "grep"*) [ -n "$DNS_REMOTE" ] && exit 1; exit 0 ;;
        "sh -c echo"*) exit 0 ;;
        "ip link set dev "*) [ -n "$IFDOWN_FAILS" ] && exit 2; : > "$STUB/ifdown"; exit 0 ;;
        *) exit 99 ;;
      esac
    )
    printf '\nprobe_rc=%s\n' "$?" ;;
  *) bad "$*" ;;
esac
"""
STUB_DIG = r"""#!/usr/bin/env bash
if [ -n "$DIG_FAIL" ]; then
  echo ";; communications error to 1.1.1.1#53: timed out"
  echo ";; no servers could be reached"
  exit 9
fi
echo 93.184.215.14
"""
STUB_IPTABLES = r"""#!/usr/bin/env bash
printf 'iptables %s\n' "$*" >> "$STUB/log"
bad() { printf 'iptables %s\n' "$*" >> "$STUB/badargs"; exit 2; }
counter="-i br-abc123 -o eth0 -m comment --comment lodestar-leak-egress"
case "$*" in
  "-I DOCKER-USER 1 -d 192.0.2.77/32 -p udp --dport 1337 -j DROP") : > "$STUB/blocked"; : > "$STUB/was_blocked" ;;
  "-I DOCKER-USER 2 $counter")
    [ -e "$STUB/blocked" ] || bad "counter without the block above it: $*"
    [ -n "$COUNTER_FAILS" ] && exit 1; : > "$STUB/counter" ;;
  "-D DOCKER-USER $counter") rm -f "$STUB/counter" ;;
  "-D DOCKER-USER -d 192.0.2.77/32 -p udp --dport 1337 -j DROP")
      if [ -n "$FAIL_FIRST_DELETE" ] && [ ! -e "$STUB/deleted_once" ]; then : > "$STUB/deleted_once"; exit 1; fi
      rm -f "$STUB/blocked" ;;
  "-L DOCKER-USER -v -x -n")
    [ -e "$STUB/counter" ] || exit 0
    [ -n "$COUNTER_UNREADABLE" ] && exit 1
    n=${EGRESS:-0}
    if [ -e "$STUB/ifdown" ] && { [ -n "$FIREWALL_OFF" ] || [ -n "$NO_POST_RULES" ]; }; then n=$((n + 3)); fi
    echo "Chain DOCKER-USER (1 references)"
    echo "    pkts      bytes target     prot opt in     out     source               destination"
    echo "       0        0 DROP       17   --  *      *       0.0.0.0/0            192.0.2.77           udp dpt:1337"
    printf '%8d %8d            0    --  br-abc123 eth0    0.0.0.0/0            0.0.0.0/0            /* lodestar-leak-egress */\n' "$n" $((n * 60)) ;;
  *) bad "$*" ;;
esac
"""
# The capture must be on gluetun's veth, with exactly the filter that leaves out
# only ARP and the tunnel's own UDP to the endpoint.
STUB_TCPDUMP = r"""#!/usr/bin/env bash
case "$*" in
  "-r "*)
    [ -n "$READ_FAILS" ] && exit 1
    n=${CAPTURED:-0}
    [ "$n" -gt 0 ] && printf 'pkt\n%.0s' $(seq "$n")
    case "$2" in *cap-ifdown.pcap)
      # What the VM saw (X-gluetun-leak): an open flow's teardown, and a port
      # unreachable to the endpoint.
      if [ -n "$FIREWALL_OFF" ] || [ -n "$NO_POST_RULES" ]; then
        echo "00:00:01.000000 IP 10.2.0.2.40000 > 192.0.2.53.853: Flags [F.], seq 1, ack 1, length 0"
        echo "00:00:01.100000 IP 10.2.0.2.40000 > 192.0.2.53.853: Flags [.], ack 2, length 0"
        echo "00:00:01.200000 IP 172.20.0.2 > 192.0.2.77: ICMP 172.20.0.2 udp port 40001 unreachable, length 36"
      fi ;;
    esac
    exit 0 ;;
  *)
    if [ "$1 $2 $3 $4 $5" != "-i vethabc123 -n -U -w" ] || [ $# -ne 7 ] \
       || [ "$7" != "not arp and not (udp and host 192.0.2.77 and port 1337)" ]; then
      printf 'tcpdump %s\n' "$*" >> "$STUB/badargs"; exit 1
    fi
    printf 'PCAPHEADER' > "$6"
    [ -n "$CAPTURE_DIES" ] && exit 1  # wrote its header, then went away
    trap 'exit 0' TERM; while :; do /bin/sleep 0.05; done ;;
esac
"""
STUB_IP = r"""#!/usr/bin/env bash
case "$*" in
  "-o link show dev vethabc123")
    [ -n "$NO_BRIDGE" ] && { echo "7: vethabc123@if6: <BROADCAST> mtu 1500 state UP"; exit 0; }
    echo "7: vethabc123@if6: <BROADCAST,UP> mtu 1500 qdisc noqueue master br-abc123 state UP mode DEFAULT" ;;
  *"route get"*) echo "1.1.1.1 via 192.0.2.1 dev eth0 src ${HOST_IP:-192.0.2.10} uid 0" ;;
  *"-o link"*) printf '7: vethabc123@if6: <BROADCAST> mtu 1500\n2: eth0: <UP>\n' ;;
esac
"""


class VMScripts(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(subprocess.run, ["rm", "-rf", str(self.tmp)])
        self.stub, self.stack, self.work = self.tmp / "stub", self.tmp / "stack", self.tmp / "work"
        (self.stub / "bin").mkdir(parents=True)
        # `sleep` is stubbed too: the scripts' waits poll the (stubbed) state, so no
        # test waits on a clock.
        for name, body in (("docker", STUB_DOCKER), ("iptables", STUB_IPTABLES), ("tcpdump", STUB_TCPDUMP),
                           ("ip", STUB_IP), ("dig", STUB_DIG),
                           ("sleep", "#!/bin/sh\nexit 0\n")):
            (self.stub / "bin" / name).write_text(body)
            (self.stub / "bin" / name).chmod(0o755)
        (self.stack / "secrets").mkdir(parents=True)
        (self.stack / "secrets" / "wg0.conf").write_text("[Peer]\nEndpoint = 192.0.2.77:1337\n")
        self.work.mkdir()
        (self.work / "compose.yaml").write_text((VM / "compose.yaml").read_text())
        (self.work / "block-lib.sh").write_text((VM / "block-lib.sh").read_text())
        self.block = self.tmp / "lodestar-block"
        (self.tmp / "v6fwd").write_text("0\n")

    def run_script(self, name, **flags):
        env = {"PATH": f"{self.stub / 'bin'}:/usr/bin:/bin", "STUB": str(self.stub),
               "IPV6_FORWARDING": str(self.tmp / "v6fwd"), **{k: str(v) for k, v in flags.items()}}
        r = subprocess.run(["bash", str(VM / name), str(self.work), str(self.stack), str(self.tmp / "pia-run"),
                            str(self.block)],
                           env=env, capture_output=True, text=True, timeout=120)
        # Every docker and tcpdump call had exactly the arguments the guards need.
        bad = self.stub / "badargs"
        self.assertFalse(bad.exists(), bad.read_text() if bad.exists() else "")
        return r.returncode, remote.parse(r.stdout.encode()), r.stdout

    def log(self):
        p = self.stub / "log"
        return p.read_text() if p.exists() else ""

    def test_vm_status(self):
        rc, lines, raw = self.run_script("vm-status.sh", COLLECTORS=1)
        self.assertEqual(rc, 0)
        self.assertEqual(lines[:5], ["ok docker_up", "ok wg_conf_present", "ok gluetun_running", "fail lodestar_running",
                                     "ok run_clean"])
        self.assertIn("count collectors_running 2", lines)
        self.assertTrue(any(ln.startswith("count disk_free_gib ") for ln in lines))
        self.assertEqual(remote.parse(raw.encode()), raw.splitlines())  # every line it prints is in the grammar
        (self.tmp / "pia-run").mkdir()
        _, lines, _ = self.run_script("vm-status.sh", NO_DOCKER=1)
        self.assertIn("fail run_clean", lines)
        self.assertIn("fail docker_up", lines)

    def test_leak_test_passes_on_a_tight_tunnel(self):
        rc, lines, raw = self.run_script("leak-test.sh")
        self.assertEqual(rc, 0, raw)
        self.assertEqual(lines[-1], "ok leak_test")
        names = ["ipv4_through_tunnel", "tcp_literal_up", "ipv6_blocked_up", "dns_up", "dns_local", "probe_tools",
                 "veth_found", "endpoint_blocked", "vpn_interface_down", "tunnel_recovers", "run_clean",
                 "host_ipv6_not_forwarded", "bridge_ipv6_off", "namespace_no_global_ipv6"]
        for phase in ("down", "ifdown"):  # the dead tunnel, then gluetun's firewall alone
            names += [f"{n}_{phase}" for n in ("ipv4_blocked", "tcp_literal_blocked", "ipv6_blocked", "dns_blocked",
                                                 "dns_direct_blocked", "capture_alive", "capture_read",
                                                 "no_egress_captured")]
            names += [f"no_host_egress_{phase}"]
            for n in ("captured_packets", "captured_tcp", "captured_udp", "captured_icmp", "captured_other",
                      "host_egress_packets"):
                self.assertIn(f"count {n}_{phase} 0", lines)
        names += ["post_rules_ipv4", "post_rules_ipv6", "egress_found"]
        for name in names:
            self.assertIn(f"ok {name}", lines)
        self.assertFalse(any(ln.startswith("fail ") for ln in lines))
        self.assertEqual(remote.parse(raw.encode()), raw.splitlines())  # nothing outside the grammar, no address
        self.assertIn("iptables -I DOCKER-USER 1 -d 192.0.2.77/32 -p udp --dport 1337 -j DROP", self.log())
        self.assertIn("iptables -D DOCKER-USER -d 192.0.2.77/32 -p udp --dport 1337 -j DROP", self.log())
        # The host counter sits just below the block, once per phase, and is gone after.
        counter = "-i br-abc123 -o eth0 -m comment --comment lodestar-leak-egress"
        self.assertEqual(self.log().count(f"iptables -I DOCKER-USER 2 {counter}"), 2)
        self.assertEqual(self.log().count(f"iptables -D DOCKER-USER {counter}"), 2)
        self.assertFalse((self.stub / "counter").exists())
        self.assertIn("docker compose -f " + str(self.stack / "compose.yaml"), self.log())
        self.assertIn(" up -d --force-recreate gluetun", self.log())  # gluetun alone, from the current wg0.conf
        probes = (self.stub / "probes").read_text()
        self.assertIn("sh -c dig +short +time=5 +tries=1 example.com", probes)  # up
        self.assertIn("sh -c dig +short +time=3 +tries=1 example.net  | grep -Eq", probes)  # down: a name not asked before
        self.assertIn("sh -c dig +short +time=3 +tries=1 example.net @1.1.1.1", probes)  # and straight to a resolver
        self.assertIn("sh -c dig +short +time=3 +tries=1 example.edu  | grep -Eq", probes)  # phase B: another fresh name
        self.assertIn("sh -c dig +short +time=3 +tries=1 example.edu @1.1.1.1", probes)
        self.assertIn("ip link set dev wg0 down", probes)  # gluetun's own interface, from its config
        self.assertIn("ok vpn_interface_stayed_down", lines)
        # A timed-out dig's ";;" lines never count as an answer: only an address does.
        self.assertIn("grep -Eq '^[0-9]{1,3}(\\.[0-9]{1,3}){3}$'", probes)
        # Phase B runs with the interface down; recovery recreates gluetun.
        self.assertLess(probes.index("ip link set dev wg0 down"), probes.index("example.edu"))
        self.assertIn("nc -z -w 5 1.1.1.1 443", probes)  # TCP to a literal address, no DNS needed

    def test_leak_test_failures(self):
        cases = {
            "a captured packet": ({"CAPTURED": 2}, "no_egress_captured_down"),
            "IPv4 leaves with the tunnel down": ({"LEAK_V4": 1}, "ipv4_blocked_down"),
            "IPv6 open": ({"V6_OPEN": 1}, "ipv6_blocked_up"),
            "the exit is the host's address": ({"EXIT_IP": "192.0.2.10"}, "ipv4_through_tunnel"),
            "a non-address exit": ({"EXIT_IP": "<html>"}, "ipv4_through_tunnel"),
            "DNS not through gluetun": ({"DNS_REMOTE": 1}, "dns_local"),
            "no recovery": ({"NO_RECOVER": 1}, "tunnel_recovers"),
            "healthy again, but through another exit": ({"NEW_EXIT": 1}, "tunnel_recovers"),
            "TCP leaks with the tunnel down": ({"LEAK_TCP": 1}, "tcp_literal_blocked_down"),
            "no TCP even with the tunnel up": ({"NO_TCP": 1}, "tcp_literal_up"),
            "the probe image lacks its tools": ({"NO_TOOLS": 1}, "probe_tools"),
            "the capture died after its header": ({"CAPTURE_DIES": 1}, "capture_alive_down"),
            "the capture can't be read": ({"READ_FAILS": 1}, "capture_read_down"),
            # Phase B: gluetun with its firewall off holds while the tunnel is merely
            # dead, and leaks once its interface is down.
            "gluetun's firewall off": ({"FIREWALL_OFF": 1}, "no_egress_captured_ifdown"),
            # X-gluetun-leak: without its post-rules, open flows leave by eth0 in phase B.
            "gluetun without its post-rules": ({"NO_POST_RULES": 1}, "post_rules_ipv4"),
            "a packet forwarded out of the host": ({"EGRESS": 1}, "no_host_egress_down"),
            "the host counter can't be read": ({"COUNTER_UNREADABLE": 1}, "no_host_egress_down"),
            "the host counter can't be set": ({"COUNTER_FAILS": 1}, "no_host_egress_down"),
            "no compose bridge on the veth": ({"NO_BRIDGE": 1}, "egress_found"),
            "the interface won't go down": ({"IFDOWN_FAILS": 1}, "vpn_interface_down"),
            # A probe container that never runs its command is never read as blocked.
            "the probes never run": ({"PROBE_NOT_RUN": 1}, "probe_ran"),
            "no DNS answer with the tunnel up": ({"DIG_UP_FAIL": 1}, "dns_up"),
            # gluetun's own VPN restart brings the interface back mid-phase.
            "the interface comes back up": ({"IFACE_BACK": 1}, "vpn_interface_stayed_down"),
            # The recovery's delete fails: the exit trap still lifts the block.
            "a failed delete": ({"FAIL_FIRST_DELETE": 1, "NO_RECOVER": 1}, "tunnel_recovers"),
            # The host's own IPv6 (0d's separate check, OP-3b).
            "the stack's network has IPv6": ({"BRIDGE_V6": 1}, "bridge_ipv6_off"),
            "the host forwards IPv6": ({"V6_FORWARDING": 1}, "host_ipv6_not_forwarded"),
            "a global IPv6 address in gluetun's namespace": ({"NS_V6": 1}, "namespace_no_global_ipv6"),
        }
        for name, (flags, failed) in cases.items():
            with self.subTest(name):
                self.setUp()
                if flags.pop("V6_FORWARDING", None):
                    (self.tmp / "v6fwd").write_text("1\n")
                rc, lines, _ = self.run_script("leak-test.sh", **flags)
                self.assertEqual(rc, 1)
                self.assertIn(f"fail {failed}", lines)
                self.assertEqual(lines[-1], "fail leak_test")
                self.assertFalse((self.stub / "blocked").exists())  # the block is always lifted
                self.assertFalse((self.stub / "counter").exists())  # and so is the counter
                if failed.startswith(("capture_alive", "capture_read")):  # never read as zero packets
                    self.assertIn("fail no_egress_captured_down", lines)

    def test_leak_test_refuses_bad_inputs(self):
        for name, conf in {"hostile endpoint": "Endpoint = 192.0.2.77;rm -rf /:1337\n",
                           "port out of range": "Endpoint = 192.0.2.77:70000\n",
                           "port zero": "Endpoint = 192.0.2.77:0\n",
                           "leading-zero port out of range": "Endpoint = 192.0.2.77:099999\n",
                           "hostname": "Endpoint = vpn.example:1337\n",
                           "no endpoint": "[Peer]\n"}.items():
            with self.subTest(name):
                self.setUp()
                (self.stack / "secrets" / "wg0.conf").write_text(conf)
                rc, lines, _ = self.run_script("leak-test.sh")
                self.assertEqual((rc, lines[-2:]), (1, ["fail endpoint_valid", "fail leak_test"]))
                self.assertNotIn("iptables", self.log())
        self.setUp()
        (self.stack / "secrets" / "wg0.conf").unlink()
        rc, lines, _ = self.run_script("leak-test.sh")
        self.assertEqual(lines, ["fail wg_conf_present", "fail leak_test"])
        self.setUp()
        rc, lines, _ = self.run_script("leak-test.sh", COMPOSE_FAIL=1)
        self.assertEqual(lines[-2:], ["fail tunnel_started", "fail leak_test"])
        self.setUp()
        rc, lines, _ = self.run_script("leak-test.sh", PULL_FAIL=1)
        self.assertEqual(lines[-2:], ["fail probe_image", "fail leak_test"])
        self.assertNotIn("iptables", self.log())

    def test_leak_test_honours_the_block_marker(self):
        """OP-3b: a recorded block (stopped between segments) is mid-block; an unreadable one fails closed."""
        self.block.mkdir()
        (self.block / "marker").write_text("block=1-1\nsegment=2\n")
        rc, lines, _ = self.run_script("leak-test.sh")
        self.assertEqual((rc, lines[-2:]), (1, ["fail block_running", "fail leak_test"]))
        self.assertNotIn("iptables", self.log())
        self.assertNotIn("compose", self.log())
        (self.block / "marker").unlink()
        self.block.rmdir()
        self.block.write_text("")
        rc, lines, _ = self.run_script("leak-test.sh")
        self.assertEqual((rc, lines[-2:]), (1, ["fail block_check", "fail leak_test"]))
        self.assertNotIn("compose", self.log())

    def test_vm_status_reports_the_block(self):
        _, lines, _ = self.run_script("vm-status.sh")
        self.assertIn("skip block_recorded", lines)
        self.block.mkdir()
        (self.block / "marker").write_text("block=1-1\nsegment=3\n")
        _, lines, _ = self.run_script("vm-status.sh")
        self.assertIn("ok block_recorded", lines)
        self.assertIn("count block_segment 3", lines)
        (self.block / "marker").unlink()
        self.block.rmdir()
        self.block.write_text("")
        _, lines, _ = self.run_script("vm-status.sh")
        self.assertIn("fail block_recorded", lines)

    def test_leak_test_refuses_mid_block(self):
        for flags in ({"LODESTAR": 1}, {"COLLECTORS": 1}):
            with self.subTest(flags=flags):
                self.setUp()
                rc, lines, _ = self.run_script("leak-test.sh", **flags)
                self.assertEqual((rc, lines[-2:]), (1, ["fail block_running", "fail leak_test"]))
                self.assertNotIn("iptables", self.log())
                self.assertNotIn("compose", self.log())  # the running stack is never touched

    def test_a_firewall_off_gluetun_fails_phase_b_only(self):
        rc, lines, _ = self.run_script("leak-test.sh", FIREWALL_OFF=1)
        self.assertEqual(rc, 1)
        for n in ("ipv4_blocked", "tcp_literal_blocked", "ipv6_blocked", "dns_blocked", "dns_direct_blocked",
                  "no_egress_captured"):
            self.assertIn(f"ok {n}_down", lines)  # a dead tunnel alone can't tell
            self.assertIn(f"fail {n}_ifdown", lines)
        self.assertIn("count captured_packets_ifdown 3", lines)
        self.assertIn("fail no_host_egress_ifdown", lines)
        self.assertFalse((self.stub / "blocked").exists())

    def test_gluetun_without_its_post_rules_leaks_open_flows_in_phase_b(self):
        """X-gluetun-leak: what the VM saw. No new connection leaves, so every probe
        passes; the capture and the host counter both catch the open flows."""
        rc, lines, _ = self.run_script("leak-test.sh", NO_POST_RULES=1)
        self.assertEqual(rc, 1)
        for n in ("post_rules_ipv4", "post_rules_ipv6", "no_egress_captured_ifdown", "no_host_egress_ifdown"):
            self.assertIn(f"fail {n}", lines)
        for n in ("ipv4_blocked", "tcp_literal_blocked", "ipv6_blocked", "dns_blocked", "dns_direct_blocked"):
            self.assertIn(f"ok {n}_ifdown", lines)
        self.assertIn("ok no_egress_captured_down", lines)
        for n, v in (("captured_packets", 3), ("captured_tcp", 2), ("captured_udp", 0), ("captured_icmp", 1),
                     ("captured_other", 0), ("host_egress_packets", 3)):
            self.assertIn(f"count {n}_ifdown {v}", lines)

    def test_probes_that_never_ran_never_pass(self):
        rc, lines, _ = self.run_script("leak-test.sh", PROBE_NOT_RUN=1)
        self.assertEqual(rc, 1)
        for n in ("ipv4_blocked", "tcp_literal_blocked", "ipv6_blocked", "dns_blocked", "dns_direct_blocked"):
            self.assertIn(f"fail {n}_down", lines)
        self.assertEqual(lines[-2:], ["fail probe_ran", "fail leak_test"])

    def test_the_vpn_interface_comes_from_gluetuns_config(self):
        rc, lines, _ = self.run_script("leak-test.sh", VPN_IFACE="tun7")
        self.assertEqual(rc, 0, lines)
        self.assertIn("ip link set dev tun7 down", (self.stub / "probes").read_text())
        for hostile in ("wg0;reboot", "-x", "a" * 16):
            with self.subTest(iface=hostile):
                self.setUp()
                rc, lines, _ = self.run_script("leak-test.sh", VPN_IFACE=hostile)
                self.assertEqual((rc, lines[-2:]), (1, ["fail vpn_interface", "fail leak_test"]))
                self.assertNotIn("iptables", self.log())

    def test_leak_test_fails_closed_when_docker_cant_list(self):
        rc, lines, _ = self.run_script("leak-test.sh", PS_FAIL=1)
        self.assertEqual((rc, lines[-2:]), (1, ["fail block_check", "fail leak_test"]))
        self.assertNotIn("iptables", self.log())
        self.assertNotIn("compose", self.log())

    def test_leak_test_flags_a_left_over_login(self):
        (self.tmp / "pia-run").mkdir()
        rc, lines, _ = self.run_script("leak-test.sh")
        self.assertEqual(rc, 1)
        self.assertIn("fail run_clean", lines)

    def test_scripts_print_nothing_but_grammar_lines(self):
        for name in ("vm-status.sh", "leak-test.sh"):
            for line in (VM / name).read_text().splitlines():
                if "echo " in line and not line.lstrip().startswith("#") and "probe" not in line:
                    with self.subTest(script=name, line=line.strip()):
                        self.assertRegex(line, r'echo "(ok|fail|skip|count) |echo "\$|echo \'fail vm_busy\'')


if __name__ == "__main__":
    unittest.main()
