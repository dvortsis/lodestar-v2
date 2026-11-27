#!/usr/bin/env python3
"""Round 1's 10 s host samples (docs/plan/2026-11-27-revised-path-to-100.md §6.3; the format
`analyze_cr.py gate-round` reads, docs/decisions/2026-11-27-X-cr-round1-gate.md).

  host_samples.py run --out FILE --device NAME --uplink IFACE [--tunnel ARM=CONTAINER:TARGET ...]
  host_samples.py assemble --samples FILE --out FILE --device NAME [--rehearsal FILE]
  host_samples.py median --samples FILE
  host_samples.py drops --samples FILE    (each uplink drop series' rise per hour)

`run` writes one JSON line per 10 s slot of UTC time, stamped with the slot's start:
  - `cpu` and `steal`: shares of every vCPU's time since the previous slot, from /proc/stat;
  - `disk_write_ms` and `disk_inflight`: the average time per write completed on the /data
    device since the previous slot (null when none completed), and the writes in flight now,
    from /proc/diskstats;
  - `uplink_drops_total`: the uplink interface's rx_dropped + tx_dropped, cumulative (each
    also on its own, `uplink_rx_dropped` and `uplink_tx_dropped`);
  - `tunnels`: per arm, one ICMP probe to its WireGuard gateway from inside the arm's gluetun
    network namespace (`nsenter`, so root), as {rtt_ms, lost}.
A value it can't read is null and a slot it misses (a stall, a restart) has no line, so a gap
is always missing, never good. Nothing it records names an address or a host.

`assemble` turns the day's lines into the JSON gate-round reads, with the rehearsal's median
tunnel RTT per arm (the network threshold's base) and each hour's coverage per series.
Python 3.10+, standard library only.
"""

import argparse
import json
import math
import os
import re
import signal
import statistics
import subprocess
import sys
import threading
import time
from datetime import datetime, timedelta, timezone
from pathlib import Path

INTERVAL = 10
PROBE_TIMEOUT = 2
DEVICE = re.compile(r"^[a-z0-9_-]{1,32}$")
IFACE = re.compile(r"^[A-Za-z0-9_.][A-Za-z0-9_.-]{0,14}$")
ARM = re.compile(r"^[A-Z]$")
CONTAINER = re.compile(r"^[a-zA-Z0-9][a-zA-Z0-9_.-]{0,62}$")
TARGET = re.compile(r"^((25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])\.){3}(25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])$")
RTT = re.compile(r"time[=<]([0-9]+(?:\.[0-9]+)?) ?ms")
SERIES = ("cpu", "steal", "disk", "uplink")


def iso(t):
    return t.astimezone(timezone.utc).isoformat().replace("+00:00", "Z")


def parse_utc(text):
    return datetime.fromisoformat(text.replace("Z", "+00:00"))


def read(path):
    try:
        return Path(path).read_text(encoding="ascii")
    except (OSError, UnicodeDecodeError):
        return None


def cpu_times(text):
    """The aggregate cpu line: (busy, steal, total) jiffies, or None. Guest time is already in
    user and nice, so it isn't added again."""
    for line in (text or "").splitlines():
        parts = line.split()
        if parts and parts[0] == "cpu" and len(parts) >= 9:
            try:
                user, nice, system, idle, iowait, irq, softirq, steal = (int(x) for x in parts[1:9])
            except ValueError:
                return None
            return user + nice + system + irq + softirq, steal, user + nice + system + idle + iowait + irq + softirq + steal
    return None


def disk_times(text, device):
    """(writes completed, ms spent writing, writes in flight) for the device, or None."""
    for line in (text or "").splitlines():
        parts = line.split()
        if len(parts) >= 14 and parts[2] == device:
            try:
                return int(parts[7]), int(parts[10]), int(parts[11])
            except ValueError:
                return None
    return None


def uplink_drops(sys_root, iface):
    """(rx_dropped, tx_dropped), each None if unreadable. Linux counts frames of protocols the
    host doesn't handle (LLDP, unknown ethertypes) in rx_dropped too, so the two are kept apart
    for the rehearsal's check that the series stays flat on a quiet uplink."""
    base = Path(sys_root) / "class" / "net" / iface / "statistics"
    out = []
    for name in ("rx_dropped", "tx_dropped"):
        out.append(read_int(base / name))
    return tuple(out)


def read_int(path):
    try:
        return int(read(path).strip())
    except (AttributeError, ValueError):
        return None


RX_BREAKDOWN = ("rx_missed_errors", "rx_fifo_errors")  # the NIC's own counts, where it exposes them


def drop_deltas(rows):
    """Each drop series' rise per UTC hour (the rehearsal's flatness check)."""
    names = ("uplink_rx_dropped", "uplink_tx_dropped") + tuple("uplink_" + n for n in RX_BREAKDOWN)
    hours = {}
    for row in sorted(rows, key=lambda r: r["utc"]):
        h = hours.setdefault(row["utc"][:13] + ":00:00Z", {})
        for n in names:
            v = row.get(n)
            if isinstance(v, int):
                first, _ = h.get(n, (v, v))
                h[n] = (first, v)
    return [{"hour_utc": k, **{n: last - first for n, (first, last) in sorted(v.items())}} for k, v in sorted(hours.items())]


class Sampler:
    """One slot's readings from the deltas since the previous slot. Everything it reads goes
    through `read`, `run` and `clock`, so the tests drive it without a host."""

    def __init__(self, device, uplink, tunnels, proc_root="/proc", sys_root="/sys", run=None, pid_ttl=60):
        self.device, self.uplink, self.tunnels = device, uplink, tunnels
        self.proc_root, self.sys_root = Path(proc_root), Path(sys_root)
        self.run = run or self.subprocess_run
        self.prev_cpu = self.prev_disk = None
        self.pids, self.pid_ttl = {}, pid_ttl

    @staticmethod
    def subprocess_run(argv, timeout):
        try:
            done = subprocess.run(argv, capture_output=True, text=True, timeout=timeout, check=False)
        except (OSError, subprocess.SubprocessError):
            return None, ""
        return done.returncode, done.stdout

    def pid_of(self, container, now):
        """gluetun's PID, from Docker, cached briefly: a restarted gluetun has a new one."""
        cached = self.pids.get(container)
        if cached and now - cached[1] < self.pid_ttl:
            return cached[0]
        rc, out = self.run(["docker", "inspect", "--type", "container", "-f", "{{.State.Pid}}", container], 10)
        pid = int(out.strip()) if rc == 0 and out.strip().isdigit() and int(out.strip()) > 0 else None
        self.pids[container] = (pid, now)
        return pid

    def probe(self, container, target, now):
        pid = self.pid_of(container, now)
        if pid is None:
            return None  # unknown, not lost: the slot's tunnel reading is missing
        rc, out = self.run(["nsenter", f"--net={self.proc_root / str(pid) / 'ns' / 'net'}", "ping", "-n", "-q", "-c", "1",
                            "-W", str(PROBE_TIMEOUT), target], PROBE_TIMEOUT + 3)
        if rc is None:
            return None
        if rc == 0:
            m = RTT.search(out) or re.search(r"= [0-9.]+/([0-9.]+)/", out)
            return {"rtt_ms": float(m[1]), "lost": False} if m else None
        return {"rtt_ms": None, "lost": True} if rc == 1 else None  # ping's 1: no reply; 2: an error

    def sample(self, slot, now=None):
        now = time.monotonic() if now is None else now
        row = {"utc": iso(slot)}
        cpu = cpu_times(read(self.proc_root / "stat"))
        if cpu and self.prev_cpu and cpu[2] > self.prev_cpu[2]:
            total = cpu[2] - self.prev_cpu[2]
            row["cpu"] = round((cpu[0] - self.prev_cpu[0]) / total, 4)
            row["steal"] = round((cpu[1] - self.prev_cpu[1]) / total, 4)
        else:
            row["cpu"] = row["steal"] = None
        self.prev_cpu = cpu
        disk = disk_times(read(self.proc_root / "diskstats"), self.device)
        if disk and self.prev_disk:
            writes = disk[0] - self.prev_disk[0]
            row["disk_write_ms"] = round((disk[1] - self.prev_disk[1]) / writes, 3) if writes > 0 else None
            row["disk_inflight"] = disk[2]
        else:
            row["disk_write_ms"] = row["disk_inflight"] = None
        self.prev_disk = disk
        rx, tx = uplink_drops(self.sys_root, self.uplink)
        row["uplink_rx_dropped"], row["uplink_tx_dropped"] = rx, tx
        for name in RX_BREAKDOWN:
            v = read_int(self.sys_root / "class" / "net" / self.uplink / "statistics" / name)
            if v is not None:
                row["uplink_" + name] = v
        row["uplink_drops_total"] = rx + tx if rx is not None and tx is not None else None
        results = {}
        threads = [threading.Thread(target=lambda a=a, c=c, t=t: results.__setitem__(a, self.probe(c, t, now)))
                   for a, (c, t) in sorted(self.tunnels.items())]
        for t in threads:
            t.start()
        for t in threads:
            t.join(PROBE_TIMEOUT + 5)
        row["tunnels"] = {a: results[a] for a in sorted(self.tunnels) if results.get(a) is not None}
        return row


def run_loop(sampler, out, stop, now=lambda: datetime.now(timezone.utc), sleep=time.sleep, until=None):
    """A line per 10 s UTC slot. A slot is sampled only if its work starts within it; a late one
    is skipped (no line), so a stall shows as missing."""
    out = Path(out)
    last_sync = time.monotonic()
    with out.open("a", encoding="utf-8") as fh:
        while not stop.is_set():
            t = now()
            slot = t.replace(microsecond=0) - timedelta(seconds=t.second % INTERVAL)
            nxt = slot + timedelta(seconds=INTERVAL)
            if until is not None and slot >= until:
                break
            if (t - slot).total_seconds() < 1.0:
                fh.write(json.dumps(sampler.sample(slot), separators=(",", ":"), allow_nan=False) + "\n")
                fh.flush()
                if time.monotonic() - last_sync >= 60:
                    os.fsync(fh.fileno())
                    last_sync = time.monotonic()
            wait = (nxt - now()).total_seconds()
            if wait > 0:
                stop.wait(wait) if sleep is time.sleep else sleep(wait)
        fh.flush()
        os.fsync(fh.fileno())


def load_lines(path):
    rows = []
    for line in Path(path).read_text(encoding="utf-8").splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue  # a torn last line after a crash is missing, not an error
        if isinstance(row, dict) and isinstance(row.get("utc"), str):
            rows.append(row)
    return rows


def rehearsal_medians(rows):
    """Each arm's median RTT over the rehearsal's answered probes (§6.3's network base)."""
    seen = {}
    for row in rows:
        for arm, t in (row.get("tunnels") or {}).items():
            if isinstance(t, dict) and t.get("lost") is False and isinstance(t.get("rtt_ms"), (int, float)):
                seen.setdefault(arm, []).append(t["rtt_ms"])
    return {arm: round(statistics.median(v), 3) for arm, v in sorted(seen.items())}


def hourly_coverage(rows, arms):
    """Per UTC hour: the slots expected (360) and the share each series covers. A tunnel slot
    counts when its probe ran (answered or lost); a disk slot when the device was read."""
    hours = {}
    for row in rows:
        t = parse_utc(row["utc"])
        h = hours.setdefault(iso(t.replace(minute=0, second=0, microsecond=0)), {})
        for name, ok in (("cpu", row.get("cpu") is not None), ("steal", row.get("steal") is not None),
                         ("disk", row.get("disk_inflight") is not None),
                         ("uplink", row.get("uplink_drops_total") is not None)):
            h[name] = h.get(name, 0) + ok
        for arm in arms:
            t_ = (row.get("tunnels") or {}).get(arm)
            h["tunnel_" + arm] = h.get("tunnel_" + arm, 0) + (isinstance(t_, dict) and isinstance(t_.get("lost"), bool))
    expected = 3600 // INTERVAL
    return [{"hour_utc": k, "expected": expected,
             "coverage": {s: round(v / expected, 4) for s, v in sorted(h.items())}} for k, h in sorted(hours.items())]


def assemble(rows, device, rehearsal_rows=None, vcpus=None):
    arms = sorted({a for r in rows for a in (r.get("tunnels") or {})}
                  | {a for r in (rehearsal_rows or []) for a in (r.get("tunnels") or {})})
    seen, samples = set(), []
    for row in sorted(rows, key=lambda r: r["utc"]):
        if row["utc"] in seen:
            continue
        seen.add(row["utc"])
        samples.append(row)
    return {"schema": 1, "interval_seconds": INTERVAL, "vcpus": vcpus or os.cpu_count(), "backing_device": device,
            "rehearsal_rtt_median_ms": rehearsal_medians(rehearsal_rows or []),
            "coverage_by_hour": hourly_coverage(samples, arms), "samples": samples}


def tunnel_arg(text):
    arm, _, rest = text.partition("=")
    container, _, target = rest.partition(":")
    if not (ARM.fullmatch(arm) and CONTAINER.fullmatch(container) and TARGET.fullmatch(target)):
        raise argparse.ArgumentTypeError("--tunnel takes ARM=CONTAINER:IPV4 (e.g. B=gluetun-b:10.0.0.1)")
    return arm, (container, target)


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="command", required=True)
    r = sub.add_parser("run")
    r.add_argument("--out", required=True)
    r.add_argument("--device", required=True, help="The /data device's kernel name, as the declaration names it")
    r.add_argument("--uplink", required=True, help="The host's uplink interface")
    r.add_argument("--tunnel", action="append", default=[], type=tunnel_arg)
    r.add_argument("--until", help="Stop at this UTC time (default: on SIGTERM)")
    a = sub.add_parser("assemble")
    a.add_argument("--samples", required=True)
    a.add_argument("--out", required=True)
    a.add_argument("--device", required=True)
    a.add_argument("--rehearsal", help="The rehearsal's samples, for each arm's median RTT")
    m = sub.add_parser("median")
    m.add_argument("--samples", required=True)
    d = sub.add_parser("drops")
    d.add_argument("--samples", required=True)
    args = p.parse_args(argv)
    if args.command == "run":
        if not DEVICE.fullmatch(args.device) or not IFACE.fullmatch(args.uplink):
            p.error("--device and --uplink are kernel names")
        if args.device not in {ln.split()[2] for ln in (read("/proc/diskstats") or "").splitlines() if len(ln.split()) > 2}:
            p.error("--device isn't in /proc/diskstats")
        stop = threading.Event()
        for sig in (signal.SIGTERM, signal.SIGINT):
            signal.signal(sig, lambda *_: stop.set())
        until = parse_utc(args.until) if args.until else None
        run_loop(Sampler(args.device, args.uplink, dict(args.tunnel)), args.out, stop, until=until)
        return 0
    if args.command == "drops":
        for line in drop_deltas(load_lines(args.samples)):
            print(json.dumps(line, sort_keys=True))
        return 0
    if args.command == "median":
        print(json.dumps(rehearsal_medians(load_lines(args.samples)), sort_keys=True))
        return 0
    rows = load_lines(args.samples)
    rehearsal = load_lines(args.rehearsal) if args.rehearsal else None
    doc = assemble(rows, args.device, rehearsal)
    tmp = Path(args.out + ".tmp")
    tmp.write_text(json.dumps(doc, indent=1, sort_keys=True, allow_nan=False) + "\n", encoding="utf-8")
    os.replace(tmp, args.out)
    low = [h["hour_utc"] for h in doc["coverage_by_hour"] if any(v < 0.98 for v in h["coverage"].values())]
    print(f"{len(doc['samples'])} samples; hours under 98 % coverage in some series: {len(low)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
