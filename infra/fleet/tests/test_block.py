"""OP-3b's block steps: the screen-rules file, start-block, the block marker,
arm-switch, the outage watcher and copy-off with its scrubber. No network, no
SSH, no Docker: SSH is a fake; docker, systemctl and iptables are stubs that
check their exact arguments; the leak test, the collector and the analyzer are
fakes in the step's work directory."""

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

import ancestry
import apply
import fleet
from lock import MemoryStore
from steps import remote, rules, run as steps
from tests.helpers import Clock, FakeGitHub, event_env, make_repo
from tests.test_live import ENV, FakeLinodeAPI, backend
from tests.test_steps import FakeBackend, slot

VM = Path(steps.__file__).resolve().parent / "vm"
scrub = steps._scrubber()
DIGEST = "d" * 64
LC, SC, RC = "1" * 40, "2" * 40, "3" * 40
LODESTAR_ID, GLUETUN_ID, IMAGE_ID = "a1" * 32, "b2" * 32, "c3" * 32
RULES = {"version": 1, "screen": "adm-1", "lodestar_image": f"ghcr.io/dvortsis/lodestar-v2@sha256:{DIGEST}",
         "lodestar_commit": LC, "spec_commit": SC, "treatment": "adm", "segment_hours": 2, "notice_version": 1}
# Fake private values: none may survive the scrubber or reach a log.
PRIVATE = ("203.0.113.77", "2001:db8::77", "c0ffee" * 6 + "abcd", "Some.Torrent.Name.2160p",
           "us-east-pia-server", "linode-4242", "i-0123456789abcdef0", "hunter2-password")

STUB_DOCKER = r"""#!/usr/bin/env bash
printf 'docker %s\n' "$*" >> "$STUB/log"
bad() { printf '%s\n' "$*" >> "$STUB/badargs"; exit 125; }
is_running() { grep -qx "$1" "$STUB/running" 2>/dev/null; }
case "$1" in
  ps) [ "$*" = "ps --format {{.Names}}" ] || bad "$*"
      [ -n "$PS_FAIL" ] && exit 1; cat "$STUB/running" 2>/dev/null; exit 0 ;;
  inspect)
    case "$*" in
      "inspect --type container -f {{.State.Running}} "*) is_running "${@: -1}" && echo true || echo false ;;
      "inspect --type container -f {{.Id}} gluetun") echo "$GLUETUN_ID" ;;
      "inspect --type container -f {{.Id}} lodestar") echo "${LODESTAR_ID-$DEFAULT_LID}" ;;
      "inspect --type container -f {{.Image}} lodestar") echo "sha256:$IMAGE_ID" ;;
      "inspect --type container -f {{.HostConfig.NetworkMode}} lodestar") echo "container:${PARENT:-$GLUETUN_ID}" ;;
      *) bad "$*" ;;
    esac ;;
  pull) [ -n "$PULL_FAIL" ] && exit 1; exit 0 ;;
  stop) if [ "$*" = "stop -t 10 gluetun" ]; then  # the host guard couldn't be held
          : > "$STUB/gluetun_stopped"; [ -n "$GLUETUN_STOP_FAIL" ] && exit 1
          grep -vx gluetun "$STUB/running" > "$STUB/running.new" 2>/dev/null; mv "$STUB/running.new" "$STUB/running"
          exit 0
        fi
        [ "$*" = "stop -t 60 lodestar" ] || bad "$*"
        [ -n "$STOP_FAIL" ] && exit 1
        grep -vx lodestar "$STUB/running" > "$STUB/running.new" 2>/dev/null; mv "$STUB/running.new" "$STUB/running"
        : > "$STUB/stopped" ;;
  compose)
    [ -n "$DOCKER_GID" ] || bad "compose without DOCKER_GID"
    case "$*" in
      *"--profile block up -d --no-deps --force-recreate docker-proxy")
        # Rendered before it starts, and never while a collector runs.
        [ -s "$STUB_STACK/proxy/haproxy.cfg" ] || bad "proxy without its config"
        grep -q '^lodestar-collector' "$STUB/running" 2>/dev/null && bad "proxy re-rendered mid-segment"
        [ "$DOCKER_GID" = "$SOCK_GID" ] || bad "proxy without the socket's group"
        n=$(grep -c . "$STUB/proxy.log" 2>/dev/null); cp "$STUB_STACK/proxy/haproxy.cfg" "$STUB/proxy-${n:-0}.cfg"
        echo x >> "$STUB/proxy.log"
        [ -n "$PROXY_FAIL" ] && exit 1
        python3 -c 'import socket,sys; socket.socket(socket.AF_UNIX).bind(sys.argv[1])' "$STUB_STACK/proxy-sock/docker.sock"
        exit 0 ;;
      *"--profile block up -d --no-deps lodestar") ;;
      *"--profile block up -d --no-deps --force-recreate lodestar") echo fresh >> "$STUB/fresh" ;;
      *) bad "$*" ;;
    esac
    [ -e "$STUB_BLOCK/marker" ] || bad "compose before the marker"
    env | grep '^LODESTAR' | sort > "$STUB/arm.env"
    [ -n "$COMPOSE_FAIL" ] && exit 1
    echo lodestar >> "$STUB/running" ;;
  logs)
    [ "$*" = "logs lodestar" ] || bad "$*"
    echo "2026-11-11T00:00:00Z INFO auth First run: open Lodestar in a browser and enter setup token ${TOKEN-ABCD-0123-4567-89EF}" ;;
  kill) [ "$*" = "kill gluetun" ] || bad "$*"
        : > "$STUB/gluetun_killed"
        grep -vx gluetun "$STUB/running" > "$STUB/running.new" 2>/dev/null; mv "$STUB/running.new" "$STUB/running" ;;
  exec) case "$*" in
          "exec lodestar-collector-"[1-4]" python3 /tools/linux/collect.py note --root /evidence/seg-"[1-4]" --category link_outage")
            is_running "$2" || exit 1; echo "$2" >> "$STUB/collector_notes" ;;
          *) bad "$*" ;;
        esac ;;
  rm) [ "$*" = "rm -f lodestar-docker-proxy" ] && : > "$STUB/proxy_removed"; exit 0 ;;
  run)
    case "$*" in
      "run --rm -i --network container:gluetun --cap-drop ALL nicolaka/netshoot@sha256:"*" curl -s -m 20 "*)
        case "$*" in
          *"/healthz") [ -n "$API_DOWN" ] && echo 000 || echo 200 ;;
          *"/api/v1/legal/notice") echo "{\"version\":${NOTICE:-1},\"text\":\"...\"}" ;;
          *"-H Content-Type: application/json --data-binary @- http://127.0.0.1:8090/api/v1/setup")
            cat > "$STUB/setup.json"; echo "${SETUP_CODE:-200}" ;;
          *) bad "$*" ;;
        esac ;;
      "run -d --name lodestar-collector-"*)
        [ -e "$STUB_BLOCK/marker" ] || bad "collector before the marker"
        printf '%s\n' "$*" > "$STUB/collector-$4.args"
        [ -n "$COLLECTOR_FAIL" ] && exit 1
        [ -n "$COLLECTOR_REFUSES" ] && exit 0  # started, then exited at its preflight
        seg=${4#lodestar-collector-}
        printf '{\n  "network": {\n    "gluetun_network_parent": %s\n  }\n}' "${PARENT_SEEN:-true}" \
          > "$STUB_BLOCK/evidence/seg-$seg/run.json"
        echo "$4" >> "$STUB/running" ;;
      "run --rm --network none --cap-drop ALL --security-opt no-new-privileges:true --read-only --tmpfs /tmp "*" python@sha256:"*" python3 /scrub.py collect /block /out")
        printf '%s\n' "$*" > "$STUB/scrub.args"
        [ -n "$SCRUB_FAIL" ] && exit 1; cat "$STUB/doc.json" ;;
      *) bad "$*" ;;
    esac ;;
  *) bad "$*" ;;
esac
"""
STUB_SYSTEMCTL = r"""#!/usr/bin/env bash
printf 'systemctl %s\n' "$*" >> "$STUB/log"
case "$*" in
  "is-active --quiet lodestar-outage-watcher") [ -e "$STUB/watcher_active" ] ;;
  "daemon-reload") [ -s "$UNIT_DIR/lodestar-outage-watcher.service" ] || { echo "reload without the unit" >> "$STUB/badargs"; exit 1; }
                   cp "$UNIT_DIR/lodestar-outage-watcher.service" "$STUB/unit" ;;
  "enable --now lodestar-outage-watcher")
    [ -n "$WATCHER_FAIL" ] && exit 1
    : > "$STUB/watcher_enabled"
    [ -n "$WATCHER_NOACTIVE" ] && exit 0  # accepted, but the unit never becomes active
    : > "$STUB/watcher_active" ;;
  "disable --now lodestar-outage-watcher") rm -f "$STUB/watcher_active" "$STUB/watcher_enabled" ;;
  *) printf '%s\n' "$*" >> "$STUB/badargs"; exit 1 ;;
esac
"""
# `ls` that fails its first $LS_FAILS listings (an unreadable block directory), then works.
STUB_LS = r"""#!/usr/bin/env bash
n=$(cat "$STUB/ls_calls" 2>/dev/null || echo 0); echo $((n + 1)) > "$STUB/ls_calls"
[ "$n" -lt "${LS_FAILS:-0}" ] && exit 2
exec /bin/ls "$@"
"""
# iptables for the host guard (block-lib.sh): a copy count per rule, so
# duplicates, stuck copies and an earlier endpoint's leftovers are real.
STUB_IPTABLES = r"""#!/usr/bin/env bash
printf 'iptables %s\n' "$*" >> "$STUB/log"
host="-s 192.0.2.77/32 -p udp --sport 1337 -m comment --comment lodestar-leak-host -j DROP"
host_s="-s 192.0.2.77/32 -p udp -m udp --sport 1337 -m comment --comment lodestar-leak-host -j DROP"
old_host_s="-s 198.51.100.66/32 -p udp -m udp --sport 51820 -m comment --comment lodestar-leak-host -j DROP"
n() { cat "$STUB/$1" 2>/dev/null || echo 0; }
down() { [ "$(n "$1")" -gt 0 ] || exit 1; echo $(($(n "$1") - 1)) > "$STUB/$1"; }
case "$*" in
  "-I INPUT 1 $host") [ -n "$HOST_FAILS" ] && exit 1; echo $(($(n host) + 1)) > "$STUB/host" ;;
  "-C INPUT $host") [ "$(n host)" -gt 0 ] ;;
  "-D INPUT $host"|"-D INPUT $host_s") [ -n "$HOST_STUCK" ] && exit 1; down host ;;
  "-D INPUT $old_host_s") [ -n "$HOST_STUCK" ] && exit 1; down oldhost ;;
  "-S INPUT")
    echo "-P INPUT ACCEPT"
    for _ in $(seq "$(n host)"); do echo "-A INPUT $host_s"; done
    for _ in $(seq "$(n oldhost)"); do echo "-A INPUT $old_host_s"; done ;;
  *) printf 'iptables %s\n' "$*" >> "$STUB/badargs"; exit 2 ;;
esac
"""
# The fake leak test records the host guard's copies when it runs, and its arguments.
FAKE_LEAK = r"""#!/usr/bin/env bash
cat "$STUB/host" 2>/dev/null > "$STUB/host_at_leak" || echo 0 > "$STUB/host_at_leak"
printf '%s\n' "$*" > "$STUB/leak_args"
case "$LEAK" in
  fail) echo "fail ipv6_blocked_up"; echo "fail leak_test"; exit 1 ;;
  noline) echo "ok tunnel_healthy"; exit 0 ;;
  rc1) echo "ok leak_test"; exit 1 ;;
  nohandshake) echo "fail wireguard_handshake_never_completed"; echo "ok no_egress_captured_down"
               echo "fail leak_test"; exit 1 ;;
  *) echo "ok tunnel_healthy"; echo "ok leak_test" ;;
esac
"""


class Base(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(subprocess.run, ["rm", "-rf", str(self.tmp)])
        self.stub, self.stack, self.work = self.tmp / "stub", self.tmp / "stack", self.tmp / "work"
        self.block = self.tmp / "lodestar-block"
        self.units = self.tmp / "units"
        self.units.mkdir()
        (self.stub / "bin").mkdir(parents=True)
        for name, body in (("docker", STUB_DOCKER), ("systemctl", STUB_SYSTEMCTL),
                           ("ls", STUB_LS), ("iptables", STUB_IPTABLES),
                           ("sleep", "#!/bin/sh\nexit 0\n")):
            (self.stub / "bin" / name).write_text(body)
            (self.stub / "bin" / name).chmod(0o755)
        (self.stack / "data").mkdir(parents=True)
        (self.stack / "data" / "old.db").write_text("a past block's database")
        (self.stack / "secrets").mkdir()
        (self.stack / "secrets" / "wg0.conf").write_text("[Peer]\nEndpoint = 192.0.2.77:1337\n")
        self.work.mkdir()
        for name in ("block-lib.sh", "compose.yaml", "watcher.sh", "scrub.py", "docker-proxy.cfg"):
            (self.work / name).write_text((VM / name).read_text())
        (self.work / "leak-test.sh").write_text(FAKE_LEAK)
        (self.work / "collect.py").write_text("# the collector at the rules commit\n")
        (self.work / "analyze_cr.py").write_text("# the analyzer at the rules commit\n")

    def args(self, **over):
        a = {**dict(IMAGE_DIGEST=DIGEST, LODESTAR_COMMIT=LC, SPEC_COMMIT=SC, RULES_COMMIT=RC, TREATMENT="adm",
                    SEGMENT_HOURS="2", ROLE="main", NOTICE_VERSION="1", BLOCK_ID="42-1"), **over}
        (self.work / "args").write_text("".join(f"{k}={v}\n" for k, v in a.items()))

    def run_script(self, name, **flags):
        sock = self.tmp / "docker.sock"
        sock.write_text("")
        if os.getuid() == 0:
            os.chown(sock, 0, 4242)
        env = {"PATH": f"{self.stub / 'bin'}:/usr/bin:/bin", "STUB": str(self.stub), "STUB_BLOCK": str(self.block),
               "STUB_STACK": str(self.stack), "HOME": str(self.tmp), "DOCKER_SOCK": str(sock),
               "PROXY_UID": str(os.getuid()), "PROXY_WAIT": "0", "UNIT_DIR": str(self.units), "COLLECT_WAIT": "0", "GLUETUN_ID": GLUETUN_ID, "IMAGE_ID": IMAGE_ID,
               "DEFAULT_LID": LODESTAR_ID, "SOCK_GID": str(sock.stat().st_gid), **{k: str(v) for k, v in flags.items()}}
        r = subprocess.run(["bash", str(VM / name), str(self.work), str(self.stack), str(self.tmp / "pia-run"),
                            str(self.block)], env=env, capture_output=True, text=True, timeout=120)
        bad = self.stub / "badargs"
        self.assertFalse(bad.exists(), bad.read_text() if bad.exists() else "")
        self.assertEqual(remote.parse(r.stdout.encode()),
                         [ln for ln in r.stdout.splitlines() if not ln.startswith("evidence ")])
        return r.returncode, remote.parse(r.stdout.encode()), r.stdout

    def log(self):
        p = self.stub / "log"
        return p.read_text() if p.exists() else ""

    def running(self, *names):
        (self.stub / "running").write_text("".join(n + "\n" for n in names))

    def make_block(self, segment=1, **env):
        """A block as start-block leaves it."""
        (self.block / "tools" / "linux").mkdir(parents=True)
        (self.block / "evidence").mkdir()
        (self.block / "witness").mkdir()
        (self.block / "tools" / "analyze_cr.py").write_text("#\n")
        (self.block / "password").write_text("p" * 48 + "\n")
        values = {**dict(IMAGE_DIGEST=DIGEST, LODESTAR_COMMIT=LC, SPEC_COMMIT=SC, RULES_COMMIT=RC, TREATMENT="adm",
                         SEGMENT_HOURS="2", ROLE="main"), **env}
        (self.block / "block.env").write_text("".join(f"{k}={v}\n" for k, v in values.items()))
        (self.block / "marker").write_text(f"block=42-1\nsegment={segment}\nstate=running\n")
        (self.stub / "host").write_text("1")  # start-block's host guard, held for the block

    def finish_segment(self, n, reason="baseline_window_complete"):
        seg = self.block / "evidence" / f"seg-{n}"
        seg.mkdir(parents=True, exist_ok=True)
        (seg / "result.json").write_text(json.dumps({"reason": reason}, indent=2))


class Rules(unittest.TestCase):
    def test_valid_rules(self):
        got = rules.parse(json.dumps(RULES).encode())
        self.assertEqual(got["image_digest"], DIGEST)
        self.assertEqual(rules.args(got, RC, "worker", "42-1"),
                         {"IMAGE_DIGEST": DIGEST, "LODESTAR_COMMIT": LC, "SPEC_COMMIT": SC, "RULES_COMMIT": RC,
                          "TREATMENT": "adm", "SEGMENT_HOURS": "2", "ROLE": "worker", "NOTICE_VERSION": "1",
                          "BLOCK_ID": "42-1"})
        remote.payload({}, rules.args(got, RC, "main", "42-1"))  # every value fits the argument shape

    def test_malformed_rules_refuse(self):
        cases = {
            "a tag, not a digest": {"lodestar_image": "ghcr.io/dvortsis/lodestar-v2:latest"},
            "another repository": {"lodestar_image": f"docker.io/someone/lodestar@sha256:{DIGEST}"},
            "a short digest": {"lodestar_image": "ghcr.io/dvortsis/lodestar-v2@sha256:" + "d" * 63},
            "a short commit": {"spec_commit": "2" * 39},
            "an uppercase commit": {"lodestar_commit": "A" * 40},
            "an unknown treatment": {"treatment": "turbo"},
            "a six-hour segment": {"segment_hours": 6},
            "hours as a string": {"segment_hours": "2"},
            "hours as a boolean": {"segment_hours": True},
            "version 2": {"version": 2},
            "version as a boolean": {"version": True},
            "no notice version": {"notice_version": 0},
            "a hostile screen name": {"screen": "a;rm -rf /"},
            "an extra key": {"command": "sh"},
        }
        for name, over in cases.items():
            with self.subTest(name), self.assertRaises(remote.StepRefused):
                rules.parse(json.dumps({**RULES, **over}).encode())
        for text in (b"", b"[]", b"{", "{\"é\":1}".encode(), json.dumps({**RULES, "pad": "x" * 5000}).encode()):
            with self.subTest(text=text[:20]), self.assertRaises(remote.StepRefused):
                rules.parse(text)
        missing = dict(RULES)
        del missing["treatment"]
        with self.assertRaises(remote.StepRefused):
            rules.parse(json.dumps(missing).encode())

    def test_read_at_the_rules_commit(self):
        repo, c = make_repo()
        files = {rules.RULES_PATH: json.dumps(RULES), rules.TOOLS["collect.py"]: "collector",
                 rules.TOOLS["analyze_cr.py"]: "analyzer"}
        for path, text in files.items():
            (Path(repo) / path).parent.mkdir(parents=True, exist_ok=True)
            (Path(repo) / path).write_text(text)
        subprocess.run(["git", "-C", repo, "add", "-A"], check=True)
        subprocess.run(["git", "-C", repo, "commit", "-qm", "rules"], check=True)
        commit = subprocess.run(["git", "-C", repo, "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()
        (Path(repo) / rules.RULES_PATH).write_text("{}")  # the checkout differs: the rules commit wins
        got, tools = rules.read(commit, repo)
        self.assertEqual(got["screen"], "adm-1")
        self.assertEqual(tools, {"collect.py": b"collector", "analyze_cr.py": b"analyzer"})
        with self.assertRaisesRegex(remote.StepRefused, "no screen rules"):
            rules.read(c["head"], repo)


class StartBlockControl(unittest.TestCase):
    """run_step for start-block: the rules commit's files and the arguments."""

    def setUp(self):
        self.fleet = {"commit": "a" * 40, "roles": {"main": slot(1), "worker": slot(2)}}
        self.sent = []
        for target, value in ((steps, "step_files"), (rules, "read")):
            p = mock.patch.object(target, value)
            self.addCleanup(p.stop)
            setattr(self, value, p.start())
        self.step_files.return_value = {"step.sh": b"x"}
        self.read.return_value = (rules.parse(json.dumps(RULES).encode()),
                                  {"collect.py": b"collector", "analyze_cr.py": b"analyzer"})

    def go(self, ancestors=True, commit=RC):
        def ssh(argv, data, timeout):
            self.sent.append((argv, data, timeout))
            return 0, b"ok start_block\n"
        with mock.patch.object(steps, "is_ancestor", return_value=ancestors) as anc:
            line = steps.run_step({"FLEET_SSH_KEY": "K"}, FakeBackend(), self.fleet, "start-block", "worker", "42-1",
                                  lambda: None, ssh=ssh, my_ip=lambda: "198.51.100.7", emit=lambda _: None, repo=".",
                                  fingerprint=apply.fingerprint, commit=commit)
        return line, anc

    def test_payload_carries_the_rules_commits_tools_and_the_arguments(self):
        line, anc = self.go()
        self.assertEqual(line, "done; 1 ok")
        self.read.assert_called_once_with(RC, ".")
        self.assertEqual({c.args[0] for c in anc.call_args_list}, {LC, SC})  # both rule commits are on main
        argv, data, timeout = self.sent[0]
        self.assertEqual(timeout, steps.BUILT["start-block"]["timeout"] + remote.FLOCK_WAIT + 60)
        with tarfile.open(fileobj=io.BytesIO(data)) as tar:
            names = set(tar.getnames())
            arg_text = tar.extractfile("args").read().decode()
        self.assertEqual(names, {"step.sh", "collect.py", "analyze_cr.py", "args"})
        self.assertIn(f"IMAGE_DIGEST={DIGEST}\n", arg_text)
        self.assertIn("ROLE=worker\n", arg_text)
        self.assertIn(f"RULES_COMMIT={RC}\n", arg_text)
        self.assertIn("BLOCK_ID=42-1\n", arg_text)

    def test_commits_off_main_refuse(self):
        with self.assertRaisesRegex(remote.StepRefused, "not an ancestor of main"):
            self.go(ancestors=False)
        self.assertEqual(self.sent, [])

    def test_the_built_steps_files(self):
        for step, spec in steps.BUILT.items():
            for name in spec["files"]:
                with self.subTest(step=step, file=name):
                    self.assertTrue((VM / name).is_file())
        self.assertEqual(set(steps.BUILT) | {"renew-vpn"}, set(fleet.limits_mod.load()["steps"]))
        # act's 50-minute job holds the longest session, with the SSH retries.
        longest = max(s["timeout"] for s in steps.BUILT.values()) + remote.FLOCK_WAIT + 60
        self.assertLess(longest + steps.SSH_ATTEMPTS_SECONDS, 45 * 60)


class StartBlock(Base):
    def test_starts_a_block(self):
        self.args()
        old = self.block / "evidence" / "seg-1"
        old.mkdir(parents=True)
        (old / "result.json").write_text("{}")
        (self.block / "copied").write_text("")  # copy-off took it
        (self.stack / "witness").mkdir()
        (self.stack / "witness" / "old").write_text("a past block's witness")
        rc, lines, raw = self.run_script("start-block.sh", LODESTAR_DHT_UTP="true")
        self.assertEqual(rc, 0, raw)
        self.assertEqual(lines[-1], "ok start_block")
        for n in ("args_valid", "no_block", "leak_test", "leak_test_passed", "marker_written", "lodestar_started",
                  "setup_done", "collector_started", "outage_watcher"):
            self.assertIn(f"ok {n}", lines)
        self.assertIn("count block_segment 1", lines)
        self.assertEqual((self.block / "marker").read_text(), "block=42-1\nsegment=1\nstate=running\n")
        # A past block's directory is kept beside the new one, never wiped.
        kept = [p for p in self.tmp.iterdir() if p.name.startswith("lodestar-block.")]
        self.assertEqual(len(kept), 1)
        self.assertTrue((kept[0] / "evidence" / "seg-1" / "result.json").exists())
        self.assertEqual(list((self.stack / "data").iterdir()), [])  # a fresh data directory
        self.assertEqual(list((self.stack / "witness").iterdir()), [])  # and a fresh witness
        self.assertTrue((self.stub / "fresh").exists())  # the first start recreates whatever compose kept
        self.assertEqual((self.block / "tools" / "linux" / "collect.py").read_text(),
                         "# the collector at the rules commit\n")
        self.assertEqual((self.block / "block.env").read_text().count("\n"), 7)
        # Segment 1 is the control arm; safe restart is on for the whole block.
        # (LODESTAR_DHT_UTP=true in the caller's environment never reaches compose.)
        self.assertEqual((self.stub / "arm.env").read_text(),
                         "LODESTAR_DHT_COORDINATED_ADMISSION=false\nLODESTAR_DHT_SAFE_RESTART=true\n"
                         f"LODESTAR_IMAGE=ghcr.io/dvortsis/lodestar-v2@sha256:{DIGEST}\n")
        # Setup: the token from the log, the pinned notice version, a random password kept only here.
        setup = json.loads((self.stub / "setup.json").read_text())
        pw = (self.block / "password").read_text().strip()
        self.assertEqual(setup, {"token": "ABCD-0123-4567-89EF", "password": pw, "notice_version": 1})
        self.assertRegex(pw, r"^[0-9a-f]{48}$")
        self.assertEqual((self.block / "password").stat().st_mode & 0o777, 0o600)
        self.assertNotIn(pw, raw + self.log())  # never in argv or output
        self.assertNotIn("ABCD-0123-4567-89EF", raw + self.log())  # nor the setup token
        args = (self.stub / "collector-lodestar-collector-1.args").read_text()
        for part in ("--network container:gluetun", "--oom-score-adj -500", "--cap-drop ALL", "--read-only",
                     "--security-opt no-new-privileges:true", f"{self.stack}/witness:/witness:ro",
                     f"{self.block}/evidence/seg-1:/evidence/seg-1 ",
                     "--run-kind ab --treatment adm --intensity balanced --stop --rehearsal --baseline-hours 2",
                     f"--commit {LC} --spec-commit {SC}", "--root /evidence/seg-1",
                     "--checklist auto_update=excluded", "--checklist backup=excluded",
                     "--password-file /secrets/password"):
            self.assertIn(part, args)
        # Read-only mounts, but for its own evidence folder; Docker only through the proxy.
        mounts = args.split(" -v ")[1:]
        rw = [m.split(" ")[0] for m in mounts if not m.split(" ")[0].endswith(":ro")]
        self.assertEqual(rw, [f"{self.block}/evidence/seg-1:/evidence/seg-1"])  # its own segment only
        self.assertNotIn("docker.sock:", args)
        self.assertIn(f"{self.stack}/proxy-sock:/sock:ro", args)
        self.assertIn("--docker-socket /sock/docker.sock", args)
        # The proxy, rendered with the exact IDs read on the host, before the collector.
        cfg = (self.stub / "proxy-0.cfg").read_text()
        self.assertNotIn("@", cfg.split("bind")[1].split("default_backend")[0].replace("unix@", ""))
        for value in (LODESTAR_ID, GLUETUN_ID, IMAGE_ID):
            self.assertIn(value, cfg)
        self.assertIn(f"{self.block}/password:/secrets/password:ro", args)
        self.assertNotIn("--min-free-bytes", args)  # main keeps the collector's own guardrails
        # The watcher's unit: enabled, so it comes back after a reboot, and only while a block is recorded.
        unit = (self.stub / "unit").read_text().splitlines()
        self.assertIn("RuntimeMaxSec=44h", unit)
        self.assertIn(f"ConditionPathExists={self.block}/marker", unit)
        self.assertIn(f"ExecStart=/bin/bash {self.block}/tools/watcher.sh {self.block} {self.stack}", unit)
        self.assertIn("WantedBy=multi-user.target", unit)
        self.assertTrue((self.stub / "watcher_enabled").exists())
        self.assertEqual((self.block / "tools" / "block-lib.sh").read_text(), (VM / "block-lib.sh").read_text())
        log = self.log()
        self.assertLess(log.index("compose"), log.index("run -d --name lodestar-collector-1"))

    def test_a_past_blocks_evidence_must_be_copied_off_first(self):
        self.args()
        self.make_block(segment=4)
        (self.block / "marker").unlink()  # arm-switch ended it; copy-off hasn't run
        self.finish_segment(4)
        rc, lines, _ = self.run_script("start-block.sh")
        self.assertEqual((rc, lines), (1, ["ok args_valid", "fail evidence_copied", "fail start_block"]))
        self.assertTrue((self.block / "evidence" / "seg-4" / "result.json").exists())
        self.assertEqual(self.log().count("compose"), 0)

    def test_a_failed_start_stops_lodestar_and_copy_off_ends_the_block(self):
        self.args()
        rc, lines, _ = self.run_script("start-block.sh", NOTICE=5)
        self.assertEqual((rc, lines[-2:]), (1, ["fail notice_version", "fail start_block"]))
        self.assertTrue((self.stub / "stopped").exists())  # nothing crawls after a stop
        self.assertTrue((self.stub / "proxy_removed").exists())  # nor holds Docker's socket
        self.assertNotIn("lodestar", (self.stub / "running").read_text().split())
        (self.stub / "doc.json").write_text('{"version":1,"segments":[]}')
        rc, lines, _ = self.run_script("copy-off.sh")
        self.assertEqual((rc, lines[-2:]), (0, ["ok block_ended", "ok copy_off"]))
        self.assertFalse((self.block / "marker").exists())

    def test_a_collector_that_doesnt_really_start(self):
        for flags in ({"COLLECTOR_REFUSES": 1}, {"PARENT_SEEN": "false"}):
            with self.subTest(flags=flags):
                self.setUp()
                self.args()
                rc, lines, _ = self.run_script("start-block.sh", **flags)
                self.assertEqual((rc, lines[-2:]), (1, ["fail collector_started", "fail start_block"]))
                self.assertTrue((self.stub / "stopped").exists())

    def test_worker_gets_the_small_disk_guardrails(self):
        self.args(ROLE="worker")
        rc, _, raw = self.run_script("start-block.sh")
        self.assertEqual(rc, 0, raw)
        args = (self.stub / "collector-lodestar-collector-1.args").read_text()
        self.assertIn("--min-free-bytes 12884901888 --warn-free-bytes 8589934592 --stop-free-bytes 4294967296", args)

    def test_a_failing_leak_test_is_a_stop(self):
        for mode in ("fail", "noline", "rc1"):  # exit 0 and the final `ok leak_test` line, both
            with self.subTest(mode=mode):
                self.setUp()
                self.args()
                rc, lines, _ = self.run_script("start-block.sh", LEAK=mode)
                self.assertEqual((rc, lines[-2:]), (1, ["fail leak_test_passed", "fail start_block"]))
                self.assertFalse(self.block.exists())  # no marker
                self.assertNotIn("compose", self.log())
                self.assertNotIn("pull", self.log())

    def test_a_handshake_that_never_completed_is_reported_and_stops(self):
        self.args()
        rc, lines, _ = self.run_script("start-block.sh", LEAK="nohandshake")
        self.assertEqual(rc, 1)
        self.assertIn("fail wireguard_handshake_never_completed", lines)  # the leak test's own reason, passed on
        self.assertEqual(lines[-2:], ["fail leak_test_passed", "fail start_block"])
        self.assertFalse(self.block.exists())
        self.assertNotIn("compose", self.log())

    def test_refuses_mid_block_and_fails_closed(self):
        cases = {"a recorded block": ({}, "block_running", "marker"),
                 "lodestar running": ({}, "block_running", "lodestar"),
                 "a collector running": ({}, "block_running", "lodestar-collector-3"),
                 "docker can't list": ({"PS_FAIL": 1}, "block_check", None),
                 "the block path isn't a directory": ({}, "block_check", "file")}
        for name, (flags, failed, setup) in cases.items():
            with self.subTest(name):
                self.setUp()
                self.args()
                if setup == "marker":
                    self.make_block(segment=2)
                elif setup == "file":
                    self.block.write_text("")
                elif setup:
                    self.running(setup)
                rc, lines, _ = self.run_script("start-block.sh", **flags)
                self.assertEqual((rc, lines[-2:]), (1, [f"fail {failed}", "fail start_block"]))
                self.assertNotIn("leak_test", "\n".join(lines[:-2]))  # the leak test never ran
                self.assertNotIn("compose", self.log())

    def test_hostile_arguments_refuse(self):
        for over in ({"IMAGE_DIGEST": "d" * 63}, {"TREATMENT": "adm;reboot"}, {"SEGMENT_HOURS": "6"},
                     {"ROLE": "soak"}, {"NOTICE_VERSION": "0"}, {"BLOCK_ID": "../x"}, {"EXTRA": "1"}):
            with self.subTest(over=over):
                self.setUp()
                self.args(**over)
                rc, lines, _ = self.run_script("start-block.sh")
                self.assertEqual((rc, lines), (1, ["fail args_valid", "fail start_block"]))
                self.assertEqual(self.log(), "")

    def test_after_the_marker_a_failure_leaves_the_block_recorded(self):
        cases = {"a changed Legal Notice": ({"NOTICE": 2}, "notice_version"),
                 "no setup token": ({"TOKEN": "none", "TOKEN_WAIT": 0}, "setup_token"),
                 "setup refused": ({"SETUP_CODE": 401}, "setup_done"),
                 "the image won't pull": ({"PULL_FAIL": 1}, "lodestar_image"),
                 "Lodestar outside gluetun's namespace": ({"PARENT": "other"}, "network_parent"),
                 "the API never answers": ({"API_DOWN": 1, "API_WAIT": 0}, "api_up"),
                 "the collector won't start": ({"COLLECTOR_FAIL": 1}, "collector_started"),
                 "the proxy won't start": ({"PROXY_FAIL": 1}, "docker_proxy"),
                 "a hostile container ID": ({"LODESTAR_ID": "a1" * 31 + "/x"}, "docker_proxy"),
                 "no container ID": ({"LODESTAR_ID": ""}, "docker_proxy")}
        for name, (flags, failed) in cases.items():
            with self.subTest(name):
                self.setUp()
                self.args()
                rc, lines, _ = self.run_script("start-block.sh", **flags)
                self.assertEqual((rc, lines[-2:]), (1, [f"fail {failed}", "fail start_block"]))
                # Stopped but not finished: still mid-block for every other check, until copy-off.
                self.assertTrue((self.block / "marker").exists())
                if failed == "notice_version":
                    self.assertFalse((self.stub / "setup.json").exists())  # never accepted silently
                    self.assertIn("count notice_version_served 2", lines)  # the stop names both versions
                    self.assertIn("count notice_version_pinned 1", lines)
                    self.assertEqual(self.log().count("legal/notice"), 1)  # never retried


    HOST = "-s 192.0.2.77/32 -p udp --sport 1337 -m comment --comment lodestar-leak-host -j DROP"

    def host_copies(self, name="host"):
        p = self.stub / name
        return int(p.read_text()) if p.exists() else 0

    def test_the_host_guard_is_held_from_before_the_leak_test_to_copy_off(self):
        """X-gluetun-host-guard: drill 3 stopped gluetun mid-block, and the host
        answered the endpoint's leftover UDP from its own address."""
        self.args()
        rc, lines, raw = self.run_script("start-block.sh")
        self.assertEqual(rc, 0, raw)
        self.assertLess(lines.index("ok host_guarded"), lines.index("ok leak_test"))
        self.assertEqual((self.stub / "host_at_leak").read_text().strip(), "1")  # in before gluetun is recreated
        self.assertEqual((self.stub / "leak_args").read_text().split()[-1], "held")  # the leak test leaves it
        self.assertEqual(self.host_copies(), 1)  # held while the block runs
        self.assertNotIn("192.0.2.77", raw)  # the endpoint is never printed
        (self.stub / "doc.json").write_text('{"version":1,"segments":[]}')
        self.running("gluetun")
        rc, lines, raw = self.run_script("copy-off.sh")
        self.assertEqual((rc, lines[-3:]), (0, ["ok host_guard_released", "ok block_ended", "ok copy_off"]))
        self.assertEqual(self.host_copies(), 0)

    def test_re_running_start_block_keeps_one_guard(self):
        """Idempotent: a stuck copy, a duplicate and an earlier endpoint's leftover
        all end as this endpoint's single guard, and a new copy goes in first."""
        self.args()
        (self.stub / "host").write_text("2")
        (self.stub / "oldhost").write_text("1")
        rc, lines, raw = self.run_script("start-block.sh", LEAK="fail")  # a failed start leaves the guard
        self.assertEqual(rc, 1, raw)
        self.assertEqual((self.host_copies(), self.host_copies("oldhost")), (1, 0))
        log = self.log()
        self.assertLess(log.index(f"-I INPUT 1 {self.HOST}"), log.index("-D INPUT"))
        rc, lines, raw = self.run_script("start-block.sh")
        self.assertEqual(rc, 0, raw)
        self.assertEqual((self.host_copies(), self.host_copies("oldhost")), (1, 0))

    def test_no_host_guard_no_leak_test(self):
        for flags, setup, failed in (({"HOST_FAILS": 1}, None, "host_guarded"),
                                     ({"HOST_STUCK": 1}, "oldhost", "host_guarded"),
                                     ({}, "noconf", "wg_conf_present")):
            with self.subTest(failed=failed, setup=setup):
                self.setUp()
                self.args()
                if setup == "oldhost":
                    (self.stub / "oldhost").write_text("1")  # an earlier endpoint's copy that won't come off
                elif setup == "noconf":
                    (self.stack / "secrets" / "wg0.conf").unlink()
                rc, lines, _ = self.run_script("start-block.sh", **flags)
                self.assertEqual((rc, lines[-2:]), (1, [f"fail {failed}", "fail start_block"]))
                self.assertFalse((self.stub / "leak_args").exists())  # gluetun was never recreated
                self.assertFalse(self.block.exists())

    def test_a_bad_endpoint_is_refused_before_iptables(self):
        for endpoint in ("999.0.2.77:1337", "192.0.2.77:0", "192.0.2.77:65536", "[2001:db8::1]:1337",
                         "host.example:1337", "192.0.2.77:1337 -j ACCEPT", "192.0.2.77;reboot:1337", ""):
            with self.subTest(endpoint=endpoint):
                self.setUp()
                self.args()
                (self.stack / "secrets" / "wg0.conf").write_text(f"[Peer]\nEndpoint = {endpoint}\n")
                rc, lines, raw = self.run_script("start-block.sh")
                self.assertEqual((rc, lines[-2:]), (1, ["fail endpoint_valid", "fail start_block"]))
                self.assertNotIn("iptables", self.log())
                self.assertNotIn("192.0.2.77", raw)


class ArmSwitch(Base):
    def test_switches_to_the_next_arm(self):
        self.make_block(segment=1)
        self.finish_segment(1)
        self.running("gluetun")
        rc, lines, raw = self.run_script("arm-switch.sh", LODESTAR_ID="d4" * 32)
        self.assertEqual(rc, 0, raw)
        for n in ("segment_ended", "segment_complete", "lodestar_stopped", "lodestar_started", "collector_started",
                  "arm_switch"):
            self.assertIn(f"ok {n}", lines)
        self.assertIn("count block_segment 2", lines)
        self.assertTrue(any(ln.startswith("count switch_seconds ") for ln in lines))
        self.assertEqual((self.block / "marker").read_text(), "block=42-1\nsegment=2\nstate=running\n")
        self.assertIn("LODESTAR_DHT_COORDINATED_ADMISSION=true\n", (self.stub / "arm.env").read_text())  # B
        args = (self.stub / "collector-lodestar-collector-2.args").read_text()
        self.assertIn("--root /evidence/seg-2", args)
        self.assertIn("--no-deps lodestar", self.log())  # gluetun isn't recreated
        # The new container's ID, rendered between the segments.
        self.assertIn("d4" * 32, (self.stub / "proxy-0.cfg").read_text())

    def test_the_arms_are_abba(self):
        for seg, want in ((2, "true"), (3, "false")):  # 3 -> 4 is the second control
            with self.subTest(seg=seg):
                self.setUp()
                self.make_block(segment=seg)
                self.finish_segment(seg)
                rc, _, raw = self.run_script("arm-switch.sh")
                self.assertEqual(rc, 0, raw)
                self.assertIn(f"LODESTAR_DHT_COORDINATED_ADMISSION={want}\n", (self.stub / "arm.env").read_text())

    def test_a_failed_switch_stops_lodestar(self):
        self.make_block(segment=1)
        self.finish_segment(1)
        rc, lines, _ = self.run_script("arm-switch.sh", API_DOWN=1, API_WAIT=0)
        self.assertEqual((rc, lines[-2:]), (1, ["fail api_up", "fail arm_switch"]))
        self.assertTrue((self.stub / "stopped").exists())
        self.assertTrue((self.stub / "proxy_removed").exists())
        self.assertIn("segment=2", (self.block / "marker").read_text())  # still recorded, for copy-off

    def test_after_the_last_segment_the_block_ends(self):
        self.make_block(segment=4)
        self.finish_segment(4)
        (self.stub / "watcher_active").write_text("")
        rc, lines, raw = self.run_script("arm-switch.sh")
        self.assertEqual(rc, 0, raw)
        self.assertIn("ok block_ended", lines)
        self.assertFalse((self.block / "marker").exists())
        self.assertFalse((self.stub / "watcher_active").exists())
        self.assertTrue((self.stub / "proxy_removed").exists())  # nothing left that reaches the socket
        self.assertNotIn("compose", self.log())

    def test_refusals(self):
        cases = {
            "no block": (None, {}, "block_recorded"),
            "the block path isn't a directory": ("file", {}, "block_check"),
            "a hostile marker": ("hostile", {}, "marker_valid"),
            "a hostile block.env": ("env", {}, "block_env"),
            "a hostile treatment alone": ("treatment", {}, "block_env"),
            "the window didn't complete": ("incomplete", {}, "segment_complete"),
            "the segment hasn't ended": ("running", {"WAIT_SECONDS": 0}, "segment_ended"),
            "Lodestar still running": ("lodestar", {}, "lodestar_stopped"),
        }
        for name, (setup, flags, failed) in cases.items():
            with self.subTest(name):
                self.setUp()
                if setup == "file":
                    self.block.write_text("")
                elif setup is not None:
                    self.make_block(segment=1, **({"TREATMENT": "adm\nROLE=x;y"} if setup == "env" else
                                                  {"TREATMENT": "adm;reboot"} if setup == "treatment" else {}))
                    self.finish_segment(1, reason="process_exited_1" if setup == "incomplete" else
                                        "baseline_window_complete")
                if setup == "hostile":
                    (self.block / "marker").write_text("block=42-1\nsegment=5;reboot\n")
                if setup == "running":
                    (self.block / "evidence" / "seg-1" / "result.json").unlink()
                    self.running("lodestar", "lodestar-collector-1")
                if setup == "lodestar":
                    self.running("lodestar")
                before = (self.block / "marker").read_text() if (self.block / "marker").exists() else None
                rc, lines, _ = self.run_script("arm-switch.sh", **flags)
                self.assertEqual((rc, lines[-2:]), (1, [f"fail {failed}", "fail arm_switch"]))
                self.assertNotIn("compose", self.log())
                after = (self.block / "marker").read_text() if (self.block / "marker").exists() else None
                self.assertEqual(before, after)  # the block stays recorded as it was


class OutageWatcher(Base):
    def test_the_step(self):
        rc, lines, _ = self.run_script("outage-watcher.sh")
        self.assertEqual((rc, lines), (1, ["fail block_recorded"]))
        self.block.write_text("")
        rc, lines, _ = self.run_script("outage-watcher.sh")
        self.assertEqual((rc, lines), (1, ["fail block_check"]))
        self.block.unlink()
        self.make_block()
        rc, lines, _ = self.run_script("outage-watcher.sh")
        self.assertEqual(rc, 0)
        self.assertEqual(lines, ["ok host_guarded", "count host_guard_drops 0", "ok outage_watcher_started",
                                 "count outages_seen 0", "ok outage_watcher"])
        (self.block / "outages").write_text("1 link_outage\n2 link_outage\n")
        rc, lines, _ = self.run_script("outage-watcher.sh")  # idempotent
        self.assertEqual(lines, ["ok host_guarded", "count host_guard_drops 0", "skip outage_watcher_started",
                                 "count outages_seen 2", "ok outage_watcher"])
        self.assertEqual(self.log().count("enable --now"), 1)
        (self.stub / "watcher_active").unlink()
        rc, lines, _ = self.run_script("outage-watcher.sh", WATCHER_NOACTIVE=1)  # started, never active
        self.assertEqual((rc, lines[-1:]), (1, ["fail outage_watcher_started"]))

    def test_an_unlistable_block_directory_is_a_block(self):
        self.make_block(segment=2)
        (self.block / "marker").unlink()
        for script, failed in (("outage-watcher.sh", ["fail block_check"]),
                               ("arm-switch.sh", ["fail block_check", "fail arm_switch"]),
                               ("copy-off.sh", ["fail block_check", "fail copy_off"])):
            with self.subTest(script=script):
                (self.stub / "ls_calls").unlink(missing_ok=True)
                rc, lines, _ = self.run_script(script, LS_FAILS=1)
                self.assertEqual((rc, lines), (1, failed))

    def watch_stub(self):
        """watcher.sh against a docker stub whose gluetun drops once, then the block ends."""
        docker = r"""#!/usr/bin/env bash
case "$1" in
  events) [ "$*" = "events --filter type=container --filter container=gluetun --format event {{.Action}}" ] \
            || echo "$*" >> "$STUB/badargs"
          # The block ends once all three outages are counted and their notes written
          # (an outage is counted before its note, so both are waited for).
          if [ -e "$STUB/events_done" ]; then
            [ "$(grep -c . "$STUB_BLOCK/outages" 2>/dev/null)" = 3 ] \
              && [ "$(grep -c . "$STUB/notes" 2>/dev/null || echo 0)" -ge "${EXPECT_NOTES:-1}" ] \
              && rm -f "$STUB_BLOCK/marker"; exit 0
          fi
          : > "$STUB/events_done"; echo "event die"; echo "event start"; echo "event restart" ;;
  logs) [ "$*" = "logs -f --since 5s gluetun" ] || echo "$*" >> "$STUB/badargs"
        if [ ! -e "$STUB/logs_done" ]; then : > "$STUB/logs_done"
          echo "2026-11-11T00:00:00Z INFO [healthcheck] program has been unhealthy for 6s: restarting VPN"
          echo "2026-11-11T00:00:01Z INFO [dns] ready"; fi ;;
  exec) [ "$*" = "exec lodestar-collector-2 python3 /tools/linux/collect.py note --root /evidence/seg-2 --category link_outage" ] \
          || echo "$*" >> "$STUB/badargs"
        echo x >> "$STUB/notes" ;;
  *) echo "$*" >> "$STUB/badargs"; exit 1 ;;
esac
"""
        (self.stub / "bin" / "docker").write_text(docker)

    def watch(self, **flags):
        self.watch_stub()
        self.make_block(segment=2)
        env = {"PATH": f"{self.stub / 'bin'}:/usr/bin:/bin", "STUB": str(self.stub), "STUB_BLOCK": str(self.block),
               **{k: str(v) for k, v in flags.items()}}
        r = subprocess.run(["bash", str(VM / "watcher.sh"), str(self.block), str(self.stack)], env=env,
                           capture_output=True, text=True, timeout=60)
        bad = self.stub / "badargs"
        self.assertFalse(bad.exists(), bad.read_text() if bad.exists() else "")
        self.assertEqual(r.stdout, "")  # it prints nothing
        return r.returncode

    def lines(self, name):
        p = self.stub / name if name == "notes" else self.block / name
        return p.read_text().splitlines() if p.exists() else []

    def test_records_each_outage_at_once_and_ends_with_the_block(self):
        self.assertEqual(self.watch(), 0)  # it ended by itself once the marker went
        self.assertEqual(len(self.lines("outages")), 3)  # die, restart, and gluetun's "restarting VPN"
        self.assertEqual(len(self.lines("notes")), 1)  # one note per burst, into segment 2's evidence

    def test_an_unreadable_block_never_ends_the_watcher(self):
        """Fail closed: a listing that fails isn't "no marker"."""
        self.make_block(segment=2)
        (self.block / "marker").unlink()
        (self.block / "outages").write_text("")
        docker = (self.stub / "bin" / "docker")
        self.watch_stub()
        # Without the marker the stub never ends the block, so the followers stop
        # once the listing works again: the watcher still saw the outages first.
        env = {"PATH": f"{self.stub / 'bin'}:/usr/bin:/bin", "STUB": str(self.stub), "STUB_BLOCK": str(self.block),
               "LS_FAILS": "2"}
        r = subprocess.run(["bash", str(VM / "watcher.sh"), str(self.block), str(self.stack)], env=env,
                           capture_output=True, text=True, timeout=60)
        self.assertEqual(r.returncode, 0)
        self.assertTrue(docker.exists())
        self.assertGreater(len(self.lines("outages")), 0)

    def test_every_outage_noted_without_the_gap_and_bounded(self):
        self.assertEqual(self.watch(GAP=0, EXPECT_NOTES=3), 0)
        self.assertEqual(len(self.lines("notes")), 3)
        self.setUp()
        self.assertEqual(self.watch(GAP=0, MAX_NOTES=1), 0)
        self.assertEqual(len(self.lines("notes")), 1)
        self.assertEqual(len(self.lines("outages")), 3)  # still counted


class GuardResume(Base):
    """X-gluetun-guard-reboot: a reboot or an iptables flush mid-block drops the
    host guard while the marker still records the block, and Docker restarts
    gluetun by itself. The outage-watcher step and the watcher put it back, or
    mark the block damaged and stop gluetun."""

    def copies(self, name="host"):
        p = self.stub / name
        return int(p.read_text()) if p.exists() else 0

    def outages(self):
        p = self.block / "outages"
        return [ln.split(" ", 1)[1] for ln in p.read_text().splitlines()] if p.exists() else []

    def test_a_dropped_guard_is_re_held_and_recorded(self):
        self.make_block(segment=2)
        (self.stub / "host").write_text("0")  # the reboot flushed INPUT
        self.running("gluetun", "lodestar", "lodestar-collector-2")
        rc, lines, raw = self.run_script("outage-watcher.sh")
        self.assertEqual(rc, 0, raw)
        self.assertEqual(lines[:2], ["ok host_guard_reheld", "count host_guard_drops 1"])
        self.assertEqual(self.copies(), 1)
        self.assertEqual(self.outages(), ["host_guard_dropped"])  # copy-off's evidence counts it
        self.assertEqual((self.stub / "collector_notes").read_text(), "lodestar-collector-2\n")  # in the segment
        self.assertEqual(len((self.block / "notes").read_text().splitlines()), 1)
        self.assertFalse((self.block / "damaged").exists())
        self.assertFalse((self.stub / "gluetun_stopped").exists())
        self.assertNotIn("192.0.2.77", raw)
        rc, lines, _ = self.run_script("outage-watcher.sh")  # held now: nothing more is recorded
        self.assertEqual((rc, lines[:2]), (0, ["ok host_guarded", "count host_guard_drops 1"]))
        self.assertEqual((self.copies(), self.outages()), (1, ["host_guard_dropped"]))

    def test_another_endpoints_guard_is_kept(self):
        """A renewal mid-block changes wg0.conf, not gluetun's session: the old
        endpoint's copy stays, and the change is recorded."""
        self.make_block(segment=2)
        (self.stub / "host").write_text("0")
        (self.stub / "oldhost").write_text("1")
        rc, lines, raw = self.run_script("outage-watcher.sh")
        self.assertEqual((rc, lines[:2]), (0, ["ok host_guard_reheld", "count host_guard_drops 1"]))
        self.assertEqual((self.copies(), self.copies("oldhost")), (1, 1))
        self.assertEqual(self.outages(), ["host_guard_endpoint_changed"])
        self.assertNotIn("-D INPUT", self.log())

    def test_a_guard_that_cant_be_held_damages_the_block_and_stops_gluetun(self):
        for name, flags, conf in (("iptables refuses", {"HOST_FAILS": 1}, None),
                                  ("gluetun won't stop: killed", {"HOST_FAILS": 1, "GLUETUN_STOP_FAIL": 1}, None),
                                  ("no valid endpoint", {}, "[Peer]\nEndpoint = 192.0.2.77;reboot:1337\n"),
                                  ("no wg0.conf", {}, "")):
            with self.subTest(name):
                self.setUp()
                self.make_block(segment=2)
                (self.stub / "host").write_text("0")
                if conf is not None:
                    (self.stack / "secrets" / "wg0.conf").write_text(conf)
                self.running("gluetun", "lodestar")
                rc, lines, raw = self.run_script("outage-watcher.sh", **flags)
                self.assertEqual(rc, 1, raw)
                self.assertEqual(lines[:2], ["fail host_guarded", "fail block_intact"])
                self.assertEqual(lines[-1], "fail outage_watcher")
                self.assertTrue((self.stub / "gluetun_stopped").exists())
                self.assertNotIn("gluetun", (self.stub / "running").read_text())  # never left running unguarded
                self.assertEqual((self.stub / "gluetun_killed").exists(), "GLUETUN_STOP_FAIL" in flags)
                self.assertEqual((self.block / "damaged").read_text(), "host_guard_lost\n")
                self.assertIn("host_guard_lost", self.outages())
                self.assertTrue((self.block / "marker").exists())  # copy-off still ends it
                self.assertNotIn("192.0.2.77", raw)
                if conf is not None:
                    self.assertNotIn("iptables", self.log())  # nothing reaches iptables unvalidated
                before = self.outages()
                rc, lines, _ = self.run_script("outage-watcher.sh", **flags)  # again: stopped again, recorded once
                self.assertEqual(rc, 1)
                self.assertEqual(self.outages().count("host_guard_lost"), 1)
                self.assertEqual(len(self.outages()), len(before) + (1 if conf is None else 0))

    def test_no_marker_nothing_is_held(self):
        self.make_block(segment=4)
        (self.block / "marker").unlink()  # the block ended
        (self.stub / "host").write_text("0")
        rc, lines, _ = self.run_script("outage-watcher.sh")
        self.assertEqual((rc, lines), (1, ["fail block_recorded"]))
        self.assertNotIn("iptables", self.log())
        self.assertEqual(self.copies(), 0)
        # The watcher, started without a marker, holds nothing either.
        self.watch_stub()
        self.assertEqual(self.watch(), 0)
        self.assertEqual(self.copies(), 0)
        self.assertNotIn("iptables -I", self.log())

    def watch_stub(self):
        """A docker stub for watcher.sh: gluetun's events and log are quiet, and
        the block ends once the guard is back or gluetun was stopped."""
        docker = r"""#!/usr/bin/env bash
printf 'docker %s\n' "$*" >> "$STUB/log"
host() { cat "$STUB/host" 2>/dev/null || echo 0; }
case "$1" in
  events)
    # A flush mid-block: the guard goes once the watcher is following events.
    if [ -n "$FLUSH" ] && [ ! -e "$STUB/flushed" ]; then : > "$STUB/flushed"; echo 0 > "$STUB/host"; exit 0; fi
    # The block ends once the guard is back (or gluetun was stopped).
    if [ "$(host)" -gt 0 ] || [ -e "$STUB/gluetun_stopped" ]; then
      [ -n "$FLUSH" ] && [ ! -e "$STUB/flushed" ] || rm -f "$STUB_BLOCK/marker"
    fi
    exit 0 ;;
  logs) exit 0 ;;
  stop) [ "$*" = "stop -t 10 gluetun" ] || echo "$*" >> "$STUB/badargs"; : > "$STUB/gluetun_stopped" ;;
  inspect) [ "$*" = "inspect --type container -f {{.State.Running}} gluetun" ] || echo "$*" >> "$STUB/badargs"
           [ -e "$STUB/gluetun_stopped" ] && echo false || echo true ;;
  exec) exit 1 ;;  # no collector runs
  *) echo "$*" >> "$STUB/badargs"; exit 1 ;;
esac
"""
        (self.stub / "bin" / "docker").write_text(docker)

    def watch(self, **flags):
        env = {"PATH": f"{self.stub / 'bin'}:/usr/bin:/bin", "STUB": str(self.stub), "STUB_BLOCK": str(self.block),
               "GUARD_EVERY": "1", **{k: str(v) for k, v in flags.items()}}
        r = subprocess.run(["bash", str(VM / "watcher.sh"), str(self.block), str(self.stack)], env=env,
                           capture_output=True, text=True, timeout=60)
        bad = self.stub / "badargs"
        self.assertFalse(bad.exists(), bad.read_text() if bad.exists() else "")
        self.assertEqual(r.stdout, "")  # it prints nothing
        return r.returncode

    def test_the_watcher_re_holds_the_guard_after_a_reboot(self):
        """Its unit is enabled, so after a reboot it starts again and checks first."""
        self.make_block(segment=2)
        (self.stub / "host").write_text("0")
        self.watch_stub()
        self.assertEqual(self.watch(), 0)
        self.assertEqual(self.copies(), 1)
        self.assertEqual(self.outages(), ["host_guard_dropped"])
        self.assertFalse((self.stub / "gluetun_stopped").exists())

    def test_the_watcher_re_holds_the_guard_after_a_flush_mid_block(self):
        self.make_block(segment=2)
        self.watch_stub()
        self.assertEqual(self.watch(FLUSH=1), 0)
        self.assertTrue((self.stub / "flushed").exists())
        self.assertEqual(self.copies(), 1)  # put back by a later check
        self.assertEqual(self.outages(), ["host_guard_dropped"])

    def test_the_watcher_stops_gluetun_when_it_cant_hold_the_guard(self):
        self.make_block(segment=2)
        (self.stub / "host").write_text("0")
        self.watch_stub()
        self.assertEqual(self.watch(HOST_FAILS=1), 0)
        self.assertTrue((self.stub / "gluetun_stopped").exists())
        self.assertEqual((self.block / "damaged").read_text(), "host_guard_lost\n")
        self.assertEqual(self.outages(), ["host_guard_dropped", "host_guard_lost"])

    def test_the_watcher_leaves_a_held_guard_alone(self):
        self.make_block(segment=2)
        self.watch_stub()
        self.assertEqual(self.watch(), 0)
        self.assertEqual(self.outages(), [])
        self.assertNotIn("iptables -I", self.log())


class CopyOff(Base):
    def setUp(self):
        super().setUp()
        self.doc = {"version": 1, "segments": [{"segment": 1, "arm": "control"}, {"segment": 2, "arm": "treatment"}]}
        (self.stub / "doc.json").write_text(json.dumps(self.doc, separators=(",", ":")))

    def test_copies_off_and_ends_the_block(self):
        self.make_block(segment=2)
        (self.block / "outages").write_text("1 link_outage\n")
        (self.stub / "watcher_active").write_text("")
        rc, lines, raw = self.run_script("copy-off.sh")
        self.assertEqual(rc, 0, raw)
        self.assertEqual(lines, ["ok nothing_running", "ok scrubbed", "count segments_copied 2",
                                 "ok host_guard_released", "ok block_ended", "ok copy_off"])
        ev = [ln for ln in raw.splitlines() if ln.startswith("evidence ")]
        self.assertEqual(len(ev), 1)
        self.assertEqual(json.loads(base64.b64decode(ev[0].split(" ", 1)[1])), self.doc)
        self.assertFalse((self.block / "marker").exists())
        self.assertFalse((self.stub / "watcher_active").exists())
        self.assertTrue((self.stub / "proxy_removed").exists())
        # The scrubber sees only what it reads: never the password, never the network.
        args = (self.stub / "scrub.args").read_text()
        self.assertNotIn("password", args)
        self.assertIn(f"{self.block}/evidence:/block/evidence:ro", args)
        self.assertIn(f"{self.block}/outages:/block/outages:ro", args)
        self.assertNotIn(":/block/notes", args)  # absent files aren't mounted (Docker would make directories)
        rc, lines, _ = self.run_script("copy-off.sh")  # again: the same evidence, the block already ended
        self.assertEqual((rc, lines[-2:]), (0, ["skip block_ended", "ok copy_off"]))

    def test_a_lone_lodestar_is_stopped_first(self):
        self.make_block(segment=2)
        self.running("gluetun", "lodestar")
        rc, lines, _ = self.run_script("copy-off.sh")
        self.assertEqual(rc, 0)
        self.assertEqual(lines[:2], ["ok lodestar_stopped", "ok nothing_running"])
        self.assertTrue((self.block / "copied").exists())

    def test_refusals(self):
        cases = {"Lodestar alone, and it won't stop": ("lodestar", {"STOP_FAIL": 1}, "lodestar_stopped"),
                 "a collector running": ("lodestar-collector-2", {}, "block_running"),
                 "docker can't list": (None, {"PS_FAIL": 1}, "block_check"),
                 "the scrubber failed": (None, {"SCRUB_FAIL": 1}, "scrubbed")}
        for name, (proc, flags, failed) in cases.items():
            with self.subTest(name):
                self.setUp()
                self.make_block(segment=2)
                if proc:
                    self.running(proc)
                rc, lines, raw = self.run_script("copy-off.sh", **flags)
                self.assertEqual((rc, lines[-2:]), (1, [f"fail {failed}", "fail copy_off"]))
                self.assertNotIn("evidence ", raw)
                self.assertTrue((self.block / "marker").exists())
        self.setUp()
        self.block.write_text("")
        rc, lines, _ = self.run_script("copy-off.sh")
        self.assertEqual(lines, ["fail block_check", "fail copy_off"])
        self.block.unlink()
        rc, lines, _ = self.run_script("copy-off.sh")
        self.assertEqual(lines, ["fail evidence_present", "fail copy_off"])

    def test_with_no_marker_copy_off_still_releases_a_leftover_guard(self):
        """state=none: the block already ended (or a start failed after staging),
        and a leftover guard is still removed."""
        self.make_block(segment=4)
        (self.block / "marker").unlink()
        (self.stub / "host").write_text("1")
        (self.stub / "oldhost").write_text("1")
        rc, lines, raw = self.run_script("copy-off.sh")
        self.assertEqual((rc, lines[-3:]), (0, ["ok host_guard_released", "skip block_ended", "ok copy_off"]))
        self.assertEqual((self.stub / "host").read_text().strip(), "0")
        self.assertEqual((self.stub / "oldhost").read_text().strip(), "0")

    def test_every_tagged_guard_comes_off_and_a_stuck_one_keeps_the_block(self):
        self.make_block(segment=4)
        (self.stub / "host").write_text("2")  # the block's, and a stuck duplicate
        (self.stub / "oldhost").write_text("1")  # an earlier endpoint's
        rc, lines, raw = self.run_script("copy-off.sh", HOST_STUCK=1)
        self.assertEqual((rc, lines[-2:]), (1, ["fail host_guard_released", "fail copy_off"]))
        self.assertTrue((self.block / "marker").exists())  # still recorded: copy-off runs again
        rc, lines, raw = self.run_script("copy-off.sh")
        self.assertEqual((rc, lines[-3:]), (0, ["ok host_guard_released", "ok block_ended", "ok copy_off"]))
        self.assertEqual([(self.stub / n).read_text().strip() for n in ("host", "oldhost")], ["0", "0"])


def raw_result():
    return {"schema_version": 3, "state": "stopped", "reason": "baseline_window_complete", "stop_mode": "exited_0",
            "samples": 7200, "ended": "2026-11-11T08:00:00Z", "elapsed_seconds": 36000.5,
            "finalization_complete": True, "publication_failures": [],
            "shutdown": {"stop_mode": "exited_0", "exit_code": 0, "exit_confirmed": True, "signal_delivered": True,
                         "errors": [PRIVATE[0]]},
            "process_identity": {"container_id": PRIVATE[2], "started_at": "2026-11-10T22:00:00Z"},
            "ntp_end": {"source": "chronyc", "server": PRIVATE[4]},
            "identity_change": {"observed": PRIVATE[1]}}


def raw_report():
    return {"schema": 1, "spec": "docs/decisions/2026-11-02-CR-SPEC.md", "kind": "ab", "treatment": "adm",
            "rehearsal": True, "window_hours": 2, "hour_seconds": 3600, "clean_stop": True, "verdict": "valid",
            "verdict_reasons": [], "checks": [{"rule": "6.6", "check": "no_long_gaps", "ok": True, "effect": "invalid",
                                               "detail": {"host": PRIVATE[5]}},
                                              {"rule": "5.1", "check": PRIVATE[3], "ok": True, "effect": "void"}],
            "identity": {"image_digest": "sha256:" + DIGEST, "switches": {"coordinated_admission": True},
                         "hostname": PRIVATE[5]},
            "numerator": {"window": {"mean_net_per_min": 41.5, "stored_per_min": 39.0, "net": 4980},
                          "hourly": [{"label": PRIVATE[3]}], "worst_hour_net_per_min": 30.25},
            "latency": {"measured": True, "p95_seconds": 120.0}, "fetch": {"attempts": 9000, "successes": 4000},
            "resources": {"peak_working_set_bytes": 700_000_000, "net": {"peer": PRIVATE[0]}},
            "activity_notes": [{"category": "link_outage", "text": PRIVATE[7]}],
            "breakdowns": {"by_peer": {PRIVATE[0]: 1}}, "timeline": {"window_start_utc": "2026-11-11T06:00:00Z"}}


class Scrubber(unittest.TestCase):
    def doc(self):
        return {"version": 1, "rules_commit": RC, "spec_commit": SC, "lodestar_commit": LC,
                "image_digest": "sha256:" + DIGEST, "treatment": "adm", "segment_hours": 2, "role": "main",
                "outages": 1, "outage_notes": 1, "region": "us-east", "vm_address": PRIVATE[0],
                "segments": [{"segment": 1, "arm": "control", "result": raw_result(), "report": raw_report(),
                              "log": "raw soak log " + PRIVATE[2]}]}

    def test_only_allow_listed_fields_and_shapes_survive(self):
        out = scrub.scrub(self.doc())
        text = scrub.dumps(out).decode()
        for value in PRIVATE + ("us-east", "raw soak log", "hostname", "process_identity", "ntp_end", "breakdowns",
                                "hourly", "timeline", "CR-SPEC.md"):
            with self.subTest(value=value):
                self.assertNotIn(value, text)
        seg = out["segments"][0]
        self.assertEqual(seg["result"]["reason"], "baseline_window_complete")
        self.assertEqual(seg["result"]["shutdown"], {"stop_mode": "exited_0", "exit_code": 0, "exit_confirmed": True,
                                                     "signal_delivered": True})
        self.assertEqual(seg["report"]["checks"], [{"rule": "6.6", "check": "no_long_gaps", "ok": True,
                                                    "effect": "invalid"}, {"rule": "5.1", "ok": True, "effect": "void"}])
        self.assertEqual(seg["report"]["numerator"], {"window": {"mean_net_per_min": 41.5, "stored_per_min": 39.0,
                                                                 "net": 4980}, "worst_hour_net_per_min": 30.25})
        self.assertEqual(seg["report"]["identity"], {"image_digest": "sha256:" + DIGEST,
                                                     "switches": {"coordinated_admission": True}})
        self.assertEqual(seg["report"]["activity_notes"], [{"category": "link_outage"}])
        self.assertEqual(out["rules_commit"], RC)
        self.assertEqual(scrub.scrub(out), out)  # the control plane's second pass is stable

    def test_value_shapes(self):
        for v in ("1.2.3.4", "deadbeefdeadbeef", "a" * 65, "Name", "with space", "x\n", "", 1.5, None, [], {}):
            with self.subTest(v=v):
                self.assertFalse(scrub.word(v))
        for v in ("6.6", "2.3a", "10.1.2"):
            self.assertTrue(scrub.rule(v))
        self.assertFalse(scrub.rule("1.2.3.4"))
        for v in (True, float("nan"), float("inf"), 10**16, "1"):
            self.assertFalse(scrub.num(v))
        self.assertFalse(scrub.time("2026-11-11 08:00:00Z"))
        # Fixed vocabularies: a word-shaped server or region name never passes.
        out = scrub.scrub({"version": 1, "role": "us_newjersey", "treatment": "nj_exit", "segments": [
            {"segment": 1, "arm": "us_east", "report": {"kind": "ab", "verdict": "server_nj"}}]})
        self.assertEqual(out, {"version": 1, "segments": [{"segment": 1, "report": {"kind": "ab"}}]})
        with self.assertRaises(ValueError):
            scrub.scrub(["not a doc"])
        with self.assertRaises(ValueError):
            scrub.dumps({"version": 1, "segments": [{"x": "a" * scrub.MAX_BYTES}]})

    def test_collect_on_the_vm(self):
        tmp = Path(tempfile.mkdtemp())
        self.addCleanup(subprocess.run, ["rm", "-rf", str(tmp)])
        block, out = tmp / "block", tmp / "out"
        out.mkdir()
        for n in (1, 2):
            (block / "evidence" / f"seg-{n}").mkdir(parents=True)
            (block / "evidence" / f"seg-{n}" / "result.json").write_text(json.dumps(raw_result()))
        (block / "tools").mkdir()
        (block / "block.env").write_text(f"IMAGE_DIGEST={DIGEST}\nLODESTAR_COMMIT={LC}\nSPEC_COMMIT={SC}\n"
                                         f"RULES_COMMIT={RC}\nTREATMENT=adm\nSEGMENT_HOURS=2\nROLE=worker\n")
        (block / "outages").write_text("1 link_outage\n2 link_outage\n")
        (block / "password").write_text(PRIVATE[7] + "\n")  # a planted password: never read, never copied
        # The analyzer at the rules commit: segment 2's run fails.
        (tmp / "report.json").write_text(json.dumps(raw_report()))
        (block / "tools" / "analyze_cr.py").write_text(
            "import sys\n"
            "if sys.argv[2].endswith('seg-2'): sys.exit(2)\n"
            f"open(sys.argv[4], 'x').write(open({str(tmp / 'report.json')!r}).read())\n")
        r = subprocess.run([sys_executable(), str(VM / "scrub.py"), "collect", str(block), str(out)],
                           capture_output=True, text=True, timeout=60)
        self.assertEqual(r.returncode, 0, r.stderr)
        doc = json.loads(r.stdout)
        self.assertEqual([s["arm"] for s in doc["segments"]], ["control", "treatment"])
        self.assertEqual(doc["segments"][0]["report"]["verdict"], "valid")
        self.assertEqual(doc["segments"][1]["report"], {"state": "analysis_failed"})
        self.assertEqual((doc["outages"], doc["outage_notes"], doc["role"]), (2, 0, "worker"))
        for value in PRIVATE:
            self.assertNotIn(value, r.stdout)
        self.assertFalse(any(p.name == "seg-1.json" for p in (block / "evidence").rglob("*")))  # reports go to out


def sys_executable():
    import sys
    return sys.executable


class CopyOffControl(unittest.TestCase):
    def evidence(self, doc):
        return b"ok scrubbed\nevidence " + base64.b64encode(json.dumps(doc).encode()) + b"\nok copy_off\n"

    def test_scrubbed_again_and_stored_with_the_bucket_owner(self):
        calls = []

        class Client:
            def put_object(self, **kw):
                calls.append(kw)
        b = type("B", (), {"s3": apply.OwnedS3(Client(), "111122223333")})()
        put = steps.bucket_writer(b, {"EVIDENCE_BUCKET": "evidence-b"}, "42-1", "main")
        doc = Scrubber().doc()
        line = steps.copied_off(self.evidence(doc), put)
        self.assertEqual(line, "evidence stored in the private bucket")
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0]["ExpectedBucketOwner"], "111122223333")
        self.assertEqual((calls[0]["Bucket"], calls[0]["Key"]), ("evidence-b", "fleet/42-1/copy-off-main.json"))
        stored = calls[0]["Body"].decode()
        for value in PRIVATE + ("us-east",):
            self.assertNotIn(value, stored)  # the control plane's pass drops what a VM let through

    def test_a_large_evidence_line_never_hides_the_report(self):
        """The evidence line is taken out before the grammar's 64 KiB cut."""
        out = (b"ok scrubbed\nevidence " + base64.b64encode(json.dumps({"version": 1, "pad": "x" * 80000}).encode())
               + b"\nok block_ended\nok copy_off\n")
        with mock.patch.object(steps, "step_files", return_value={"step.sh": b"x"}):
            line = steps.run_step({"FLEET_SSH_KEY": "K"}, FakeBackend(), {"commit": "a" * 40, "roles": {"main": slot(1)}},
                                  "copy-off", "main", "42-1", lambda: None, ssh=lambda a, d, t: (0, out),
                                  my_ip=lambda: "198.51.100.7", emit=lambda _: None, repo=".",
                                  fingerprint=apply.fingerprint, evidence=lambda body: None)
        self.assertTrue(line.startswith("done; 3 ok; evidence stored"), line)

    def test_bad_evidence(self):
        put = mock.Mock()
        cases = {b"ok copy_off\n": "no evidence came back",
                 b"evidence AAAA\nevidence AAAA\n": "no evidence came back",
                 b"evidence !!!!\n": "no evidence came back",
                 b"evidence " + base64.b64encode(b"not json") + b"\n": "the evidence didn't pass the scrubber",
                 b"evidence " + base64.b64encode(b"[1]") + b"\n": "the evidence didn't pass the scrubber"}
        for out, want in cases.items():
            with self.subTest(out=out[:30]):
                self.assertEqual(steps.copied_off(out, put), want)
        put.assert_not_called()
        self.assertEqual(steps.copied_off(self.evidence({"version": 1}), None), "the evidence couldn't be stored")
        boom = mock.Mock(side_effect=RuntimeError("evidence-b AccessDenied 111122223333"))
        line = steps.copied_off(self.evidence({"version": 1}), boom)
        self.assertNotIn("evidence-b", line)
        self.assertIn("copy-off can run again", line)
        self.assertIsNone(steps.bucket_writer(object(), {"EVIDENCE_BUCKET": "x"}, "1-1", "main"))


def render_proxy():
    text = (VM / "docker-proxy.cfg").read_text()
    return text.replace("@LODESTAR_ID@", LODESTAR_ID).replace("@IMAGE_ID@", IMAGE_ID).replace("@GLUETUN_ID@", GLUETUN_ID)


ALLOWED = [("GET", "/containers/lodestar/json"), ("GET", f"/containers/{LODESTAR_ID}/json"),
           ("GET", f"/v1.47/containers/{LODESTAR_ID}/json"), ("GET", f"/containers/{GLUETUN_ID}/json"),
           ("GET", f"/images/sha256%3A{IMAGE_ID}/json"), ("GET", f"/images/sha256:{IMAGE_ID}/json"),
           ("HEAD", f"/containers/{LODESTAR_ID}/archive?path=%2Fdata%2Fconfig.toml"),
           ("GET", f"/containers/{LODESTAR_ID}/stats?stream=false&one-shot=true"),
           ("POST", f"/containers/{LODESTAR_ID}/stop?t=60")]
OTHER = "e5" * 32
DENIED = [("GET", "/_ping"), ("GET", "/version"), ("GET", "/info"), ("GET", "/containers/json"),
          ("POST", f"/containers/{LODESTAR_ID}/exec"), ("POST", "/containers/create"),
          ("POST", f"/containers/{LODESTAR_ID}/attach"), ("GET", f"/containers/{LODESTAR_ID}/logs"),
          ("GET", "/events"), ("PUT", f"/containers/{LODESTAR_ID}/archive?path=/"),
          ("GET", f"/containers/{LODESTAR_ID}/archive?path=/data"), ("GET", f"/containers/{OTHER}/json"),
          ("POST", f"/containers/{OTHER}/stop"), ("POST", f"/containers/{GLUETUN_ID}/stop"),
          ("POST", "/containers/lodestar/stop"), ("GET", f"/containers/{LODESTAR_ID}/stats"),
          ("GET", f"/containers/{LODESTAR_ID}/stats?stream=true"),
          ("GET", f"/containers/{LODESTAR_ID}/stats?stream=true&stream=false"),
          ("GET", f"/containers/{LODESTAR_ID}/stats?stream=false&stream=true"),
          ("GET", f"/containers/{LODESTAR_ID}/stats?stream=false&one-shot=true&stream=true"),
          ("GET", f"/containers/{LODESTAR_ID}/stats?one-shot=true&stream=false"),
          ("GET", f"/containers/{LODESTAR_ID}/stats?stream=false&one-shot=true&x=1"),
          ("GET", f"/containers/{LODESTAR_ID}/stats?stream=false"),
          ("GET", f"/containers/lodestar/json/../../{OTHER}/exec"), ("GET", f"/containers/x/../{LODESTAR_ID}/json"),
          ("GET", "/containers/lodestar%2F..%2Fexec/json"), ("GET", "/containers/LODESTAR/json"),
          ("GET", f"//containers/{LODESTAR_ID}/json"), ("GET", f"/v1.47/../containers/{OTHER}/json"),
          ("DELETE", f"/containers/{LODESTAR_ID}"), ("POST", f"/containers/{LODESTAR_ID}/json"),
          ("GET", f"/images/sha256:{OTHER}/json"), ("POST", f"/containers/{LODESTAR_ID}/kill"),
          ("POST", f"/containers/{LODESTAR_ID}/stop/../exec"), ("GET", f"/containers/{LODESTAR_ID}/stop"),
          ("PUT", f"/containers/{LODESTAR_ID}/stop"), ("HEAD", f"/containers/{LODESTAR_ID}/stop"),
          ("POST", f"/containers/{LODESTAR_ID}/archive"), ("POST", f"/containers/{LODESTAR_ID}/stats"), ("GET", f"/v1.47x/containers/{LODESTAR_ID}/json")]


def policy(cfg):
    """The rendered frontend's rules, evaluated as HAProxy does (first allow
    wins, else the final deny): `path` is the raw path, case-sensitive;
    `query` the raw query string. Any other fetch fails the test."""
    import re
    acls, allows = {}, []
    for line in cfg.splitlines():
        words = line.split()
        if words[:1] == ["acl"]:
            name, fetch = words[1], words[2]
            if fetch == "method":
                acls.setdefault(name, []).append(lambda m, p, q, v=words[3]: m == v)
            elif fetch == "path":
                # One pattern only: HAProxy ORs any further patterns on the line, which this wouldn't model.
                assert len(words) == 6 and words[3:5] == ["-m", "reg"], line
                acls.setdefault(name, []).append(lambda m, p, q, r=re.compile(words[5]): bool(r.search(p)))
            elif fetch == "query":  # the raw query string, matched whole
                assert len(words) == 6 and words[3:5] == ["-m", "str"], line
                acls.setdefault(name, []).append(lambda m, p, q, v=words[5]: q == v)
            else:
                raise AssertionError(line)
        elif words[:3] == ["http-request", "allow", "if"]:
            allows.append(words[3:])
        elif words[:2] == ["http-request", "deny"]:
            assert line.strip() == "http-request deny deny_status 403"

    def allowed(method, url):
        path, _, query = url.partition("?")
        return any(all(any(f(method, path, query) for f in acls[a]) for a in rule) for rule in allows)
    return allowed


class DockerProxy(unittest.TestCase):
    def test_a_second_pattern_on_an_acl_line_fails_the_model(self):
        """HAProxy ORs every pattern on an ACL line; the model reads one, so it
        refuses any other shape rather than miss an allowed path."""
        cfg = render_proxy()
        for fetch in ("path", "query"):
            line = next(ln for ln in cfg.splitlines() if ln.split()[:1] == ["acl"] and ln.split()[2] == fetch)
            for extra in (" ^/containers/", " -i"):
                with self.subTest(fetch=fetch, extra=extra), self.assertRaises(AssertionError):
                    policy(cfg.replace(line, line + extra, 1))

    def test_the_rendered_policy(self):
        allowed = policy(render_proxy())
        for method, url in ALLOWED:
            with self.subTest(allow=(method, url)):
                self.assertTrue(allowed(method, url))
        for method, url in DENIED:
            with self.subTest(deny=(method, url)):
                self.assertFalse(allowed(method, url))
        self.assertNotIn("@", render_proxy().replace("unix@", ""))

    def test_compose_gives_docker_to_the_proxy_alone(self):
        import yaml
        svc = yaml.safe_load((VM / "compose.yaml").read_text())["services"]
        for name, s in svc.items():
            socks = [v for v in s.get("volumes", []) if "docker.sock" in v]
            with self.subTest(service=name):
                self.assertEqual(socks, ["/var/run/docker.sock:/var/run/docker.sock:ro"] if name == "docker-proxy" else [])
        p = svc["docker-proxy"]
        self.assertRegex(p["image"], r"^haproxy@sha256:[0-9a-f]{64}$")
        self.assertEqual(p["image"].split("=")[0], "haproxy@sha256:" + p["image"].rsplit(":", 1)[1])
        self.assertEqual((p["network_mode"], p["cap_drop"], p["read_only"], p["profiles"]),
                         ("none", ["ALL"], True, ["block"]))
        self.assertIn("no-new-privileges:true", p["security_opt"])
        self.assertNotIn("user", p)  # the image's own non-root haproxy user
        self.assertNotIn("ports", p)
        lib = (VM / "block-lib.sh").read_text()
        self.assertIn("PROXY_IMAGE=" + p["image"] + " ", lib)  # start-block pulls the same pin
        self.assertNotIn("/var/run/docker.sock:", lib)  # no other container gets the socket
        for name in ("start-block.sh", "arm-switch.sh", "copy-off.sh", "outage-watcher.sh", "watcher.sh"):
            self.assertNotIn("docker.sock:", (VM / name).read_text())

    @unittest.skipUnless(subprocess.run(["sh", "-c", "command -v haproxy"], capture_output=True).returncode == 0,
                         "haproxy isn't installed here (CI); the policy test above covers the rules")
    def test_real_haproxy_enforces_it(self):
        """The rendered config, checked and run by a real HAProxy against a fake Docker on a unix socket."""
        import http.client
        import socket
        import socketserver
        import threading
        import time
        tmp = Path(tempfile.mkdtemp())
        self.addCleanup(subprocess.run, ["rm", "-rf", str(tmp)])
        seen = []

        class Engine(socketserver.StreamRequestHandler):
            def handle(self):
                line = self.rfile.readline().decode()
                while self.rfile.readline() not in (b"\r\n", b""):
                    pass
                seen.append(line.split(" ")[:2])
                self.wfile.write(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")

        engine = socketserver.UnixStreamServer(str(tmp / "engine.sock"), Engine)
        threading.Thread(target=engine.serve_forever, daemon=True).start()
        self.addCleanup(engine.shutdown)
        cfg = render_proxy().replace("unix@/sock/docker.sock", f"unix@{tmp}/proxy.sock") \
            .replace("server docker /var/run/docker.sock", f"server docker {tmp}/engine.sock")
        (tmp / "haproxy.cfg").write_text(cfg)
        check = subprocess.run(["haproxy", "-c", "-f", str(tmp / "haproxy.cfg")], capture_output=True, text=True)
        self.assertEqual(check.returncode, 0, check.stdout + check.stderr)
        proc = subprocess.Popen(["haproxy", "-db", "-f", str(tmp / "haproxy.cfg")], stdout=subprocess.DEVNULL,
                                stderr=subprocess.DEVNULL)
        self.addCleanup(proc.kill)
        deadline = time.monotonic() + 15
        while not (tmp / "proxy.sock").exists():
            self.assertLess(time.monotonic(), deadline, "haproxy didn't open its socket")
            time.sleep(0.05)  # polls for the socket, with a deadline

        def ask(method, url):
            conn = http.client.HTTPConnection("localhost", timeout=10)
            conn.sock = socket.socket(socket.AF_UNIX)
            conn.sock.connect(str(tmp / "proxy.sock"))
            conn.request(method, url)
            status = conn.getresponse().status
            conn.close()
            return status
        for method, url in ALLOWED:
            with self.subTest(allow=(method, url)):
                self.assertEqual(ask(method, url), 200)
        before = len(seen)
        for method, url in DENIED:
            with self.subTest(deny=(method, url)):
                self.assertEqual(ask(method, url), 403)
        self.assertEqual(len(seen), before)  # nothing denied reached the engine


class LiveCopyOff(unittest.TestCase):
    """act's live `run copy-off`, end to end: the evidence goes to the bucket only."""

    def fleet(self):
        repo, c = make_repo()
        api = FakeLinodeAPI()
        api.linodes, api.firewalls = {"99": api.linodes["99"]}, {"98": api.firewalls["98"]}
        b, s3, api, _ = backend(api=api)
        with redirect_stdout(io.StringIO()), mock.patch.object(ancestry, "remote_main_head", return_value=c["head"]):
            fleet.main(["act"], env=event_env("/fleet up screen", FLEET_LIVE="true", **ENV), backend=b,
                       store=MemoryStore(), transport=FakeGitHub(), clock=Clock(), repo=repo)
        full = FakeLinodeAPI()
        api.firewalls.update({k: full.firewalls[k] for k in ("21", "22")})
        api.rules.update({k: full.rules[k] for k in ("21", "22")})
        return repo, c, b, s3

    def test_run_start_block_prints_no_token_or_password(self):
        repo, c, b, _ = self.fleet()
        token, pw = "ABCD-0123-4567-89EF", "f00d" * 12  # planted: a VM that printed them anyway
        reply = (f"ok leak_test\nok setup_done\nenter setup token {token}\n{pw}\nok {pw[:40]}x\n"
                 "count block_segment 1\nok start_block\n").encode()
        sent = []
        env = event_env(f"/fleet run start-block main {c['head']}", FLEET_LIVE="true", FLEET_SSH_KEY="KEY", **ENV)
        out, gh = io.StringIO(), FakeGitHub()
        with redirect_stdout(out), redirect_stderr(out), \
                mock.patch.object(ancestry, "remote_main_head", return_value=c["head"]), \
                mock.patch.object(steps, "step_files", return_value={"step.sh": b"x"}), \
                mock.patch.object(rules, "read", return_value=(rules.parse(json.dumps(RULES).encode()), {})), \
                mock.patch.object(steps, "is_ancestor", return_value=True):
            rc = fleet.main(["act"], env=env, backend=b, store=MemoryStore(), transport=gh, clock=Clock(), repo=repo,
                            step_io={"ssh": lambda argv, data, t: sent.append(data) or (0, reply),
                                     "my_ip": lambda: "198.51.100.7"})
        seen = out.getvalue() + "\n".join(gh.bodies())
        self.assertEqual(rc, 0, seen[:400])
        self.assertIn("run start-block main: done; 4 ok; block_segment 1", seen)
        for value in (token, pw, pw[:40], "KEY", "198.51.100.7"):
            self.assertNotIn(value, "\n".join(ln for ln in seen.splitlines() if not ln.startswith("::add-mask::")))
        with tarfile.open(fileobj=io.BytesIO(sent[0])) as tar:
            self.assertIn(f"RULES_COMMIT={c['head']}\n", tar.extractfile("args").read().decode())

    def test_run_copy_off(self):
        repo, c, b, s3 = self.fleet()
        doc = Scrubber().doc()
        reply = (b"ok nothing_running\nok scrubbed\ncount segments_copied 1\n" + b"evidence "
                 + base64.b64encode(json.dumps(doc).encode()) + b"\nok block_ended\nok copy_off\n")
        env = event_env("/fleet run copy-off main", FLEET_LIVE="true", FLEET_SSH_KEY="KEY", **ENV)
        out, gh = io.StringIO(), FakeGitHub()
        with redirect_stdout(out), redirect_stderr(out), \
                mock.patch.object(ancestry, "remote_main_head", return_value=c["head"]), \
                mock.patch.object(steps, "step_files", return_value={"step.sh": b"x"}):
            rc = fleet.main(["act"], env=env, backend=b, store=MemoryStore(), transport=gh, clock=Clock(), repo=repo,
                            step_io={"ssh": lambda argv, data, t: (0, reply), "my_ip": lambda: "198.51.100.7"})
        seen = out.getvalue() + "\n".join(gh.bodies())
        self.assertEqual(rc, 0, seen[:400])
        self.assertIn("run copy-off main: done; 4 ok; segments_copied 1; evidence stored in the private bucket", seen)
        keys = [k for k in s3.objects if k[1].endswith("copy-off-main.json")]
        self.assertEqual(len(keys), 1)
        self.assertEqual(keys[0][0], ENV["EVIDENCE_BUCKET"])
        stored = s3.objects[keys[0]].decode()
        self.assertIn('"reason":"baseline_window_complete"', stored)
        shown = "\n".join(ln for ln in seen.splitlines() if not ln.startswith("::add-mask::"))
        self.assertNotIn(base64.b64encode(json.dumps(doc).encode()).decode()[:40], shown)
        for value in PRIVATE + ("KEY", "198.51.100.7", ENV["EVIDENCE_BUCKET"], "111122223333"):
            self.assertNotIn(value, shown)


if __name__ == "__main__":
    unittest.main()
