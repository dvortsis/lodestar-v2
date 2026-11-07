#!/usr/bin/env python3
"""X-soak-screen's pre-soak sweep: judge two harness outputs against the frozen thresholds.

Reads the output of `bench/tests/mmap_retention.rs` (its "retained N B per commit"
line) and of `bench/examples/mem-cheap` (its CSV), compares each with
`docs/soak/screen-thresholds.json` (`sweep.counters`) and prints one SCREEN line
per counter and a summary. A counter trips when its value is strictly above its
threshold. The sweep is an early warning: it can fail a candidate, never pass one,
and no gate reads it. Standard library only; no network, no clock.

Exit status: 0 when every given counter is as expected (ok, or with --expect trip,
a trip: the control arms, which show the screen still catches the known leaks),
1 otherwise, 2 for unreadable input.
"""

import argparse
import csv
import io
import json
from pathlib import Path
import re
import sys

THRESHOLDS = Path(__file__).resolve().parent.parent / "screen-thresholds.json"
RETAINED = re.compile(r"\bretained (-?[0-9]+) B per commit \(pruning (on|off)\)")  # may share a line with the test name
CHEAP_COLUMNS = ("<128K", "128K-512K")  # outside-arena mappings under 512 KiB


def sweep_thresholds(path):
    data = json.loads(Path(path).read_text(encoding="utf-8"))
    counters = data.get("sweep", {}).get("counters") if isinstance(data, dict) else None
    if not isinstance(counters, dict):
        raise ValueError("No sweep counters")
    out = {}
    for name in ("retention_bytes_per_commit", "musl_anon_under_512k_per_1000_torrents"):
        c = counters.get(name)
        v = c.get("value") if isinstance(c, dict) else None
        if not isinstance(v, (int, float)) or isinstance(v, bool) or c.get("trips") != "above":
            raise ValueError(f"Invalid sweep threshold {name}")
        out[name] = v
    return out


def retention(text):
    """Live-heap bytes per commit from mmap_retention's output."""
    found = RETAINED.findall(text)
    if len(found) != 1:
        raise ValueError("Expected one 'retained N B per commit' line")
    return float(found[0][0])


def mem_cheap(text):
    """OLS slope of the outside-arena anonymous mappings under 512 KiB per 1,000
    torrents, from 1,000 torrents on (the first rows are startup). r4's growth was
    in 128-512 KiB; without override the harness grows both classes, mostly the
    smaller, so the sum keeps the control arm well clear of the threshold."""
    # The header and the numeric rows; comments and anything else around them are skipped.
    lines = [line for line in text.splitlines() if line.startswith("torrents,") or line[:1].isdigit()]
    rows = list(csv.DictReader(io.StringIO("\n".join(lines))))
    pts = []
    for r in rows:
        try:
            pts.append((int(r["torrents"]) / 1000, sum(int(r[c]) for c in CHEAP_COLUMNS)))
        except (KeyError, TypeError, ValueError):
            raise ValueError("Malformed mem-cheap row") from None
    pts = [p for p in pts if p[0] >= 1]
    if len(pts) < 3:
        raise ValueError("Too few mem-cheap rows from 1,000 torrents on")
    n = len(pts)
    mx, my = sum(x for x, _ in pts) / n, sum(y for _, y in pts) / n
    sxx = sum((x - mx) ** 2 for x, _ in pts)
    if sxx == 0:
        raise ValueError("No spread in mem-cheap rows")
    return sum((x - mx) * (y - my) for x, y in pts) / sxx


def judge(name, value, threshold):
    status = "trip" if value > threshold else "ok"
    line = "ok" if status == "ok" else f"trip({name}, {value:.2f}, {threshold:g})"
    return status, line


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--thresholds", default=THRESHOLDS)
    p.add_argument("--retention-log", type=Path, help="mmap_retention's output")
    p.add_argument("--mem-cheap-csv", type=Path, help="mem-cheap's output")
    p.add_argument("--expect", choices=("ok", "trip"), default="ok",
                   help="trip for a control arm (pruning off, override off)")
    p.add_argument("--label", default="candidate", help="Arm name for the summary")
    args = p.parse_args(argv)
    if not args.retention_log and not args.mem_cheap_csv:
        p.error("give --retention-log or --mem-cheap-csv")
    try:
        limits = sweep_thresholds(args.thresholds)
        measured = []
        if args.retention_log:
            measured.append(("retention_bytes_per_commit",
                             retention(args.retention_log.read_text(encoding="utf-8", errors="replace"))))
        if args.mem_cheap_csv:
            measured.append(("musl_anon_under_512k_per_1000_torrents",
                             mem_cheap(args.mem_cheap_csv.read_text(encoding="utf-8"))))
    except (OSError, ValueError) as error:
        print(f"SCREEN sweep ({args.label}): not_available({error})")
        return 2
    good = True
    for name, value in measured:
        status, line = judge(name, value, limits[name])
        print(f"SCREEN sweep ({args.label}) {name} = {value:.2f} (threshold {limits[name]:g}): {line}")
        good &= status == args.expect
    print(f"SCREEN sweep ({args.label}): {'as expected' if good else 'NOT as expected'} "
          f"(expected {args.expect}); an early warning, never a pass")
    return 0 if good else 1


if __name__ == "__main__":
    sys.exit(main())
