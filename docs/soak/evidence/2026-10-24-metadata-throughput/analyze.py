"""Recalculate the saved soak evidence; reads files only, emits JSON to stdout."""
import csv
import json
import re
from datetime import datetime
from pathlib import Path

root = Path(__file__).parent


def metrics(path):
    return {
        line.split()[0]: float(line.split()[1])
        for line in path.read_text(encoding="utf-8-sig").splitlines()
        if line and not line.startswith("#")
    }


names = {
    "stored": "lodestar_torrents",
    "reports": "lodestar_intake_received_total",
    "evicted": 'lodestar_fetch_lost_total{reason="evicted"}',
    "lookups": "lodestar_dht_lookups_completed_total",
    "attempts": 'lodestar_fetch_attempts_total{transport="tcp"}',
    "fetched": 'lodestar_fetch_success_total{transport="tcp"}',
    "connect_timeouts": 'lodestar_fetch_connect_timeouts_total{transport="tcp"}',
    "active": "lodestar_fetch_active",
    "backlog": "lodestar_fetch_lookup_backlog",
    "throttled": "lodestar_dht_throttled_total",
    "reply_ratio": "lodestar_dht_reply_ratio",
}
hourly = []
previous = None
for path in sorted(root.glob("h*.prom")):
    m = metrics(path)
    row = {"sample": path.stem, **{k: m[v] for k, v in names.items()}}
    if previous:
        row["deltas"] = {k: row[k] - previous[k] for k in names if k not in ("active", "backlog", "reply_ratio")}
    hourly.append(row)
    previous = row

with (root / "samples.csv").open(encoding="utf-8-sig", newline="") as file:
    samples = list(csv.DictReader(file))
dates = [datetime.fromisoformat(r["time"]) for r in samples]
latest = samples[-1]
first = samples[0]
stats = {
    "samples": len(samples), "first": first, "latest": latest,
    "health_not_200": sum(r["healthz"] != "200" for r in samples),
    "max_gap_seconds": max((b-a).total_seconds() for a,b in zip(dates, dates[1:])),
    "private_mb_min_max": [min(float(r["private_mb"]) for r in samples), max(float(r["private_mb"]) for r in samples)],
    "cpu_core_percent": (float(latest["cpu_s"])-float(first["cpu_s"])) / (dates[-1]-dates[0]).total_seconds() * 100,
}
harvest = []
for line in (root / "summary-events.log").read_text().splitlines():
    if "dht.harvest_summary:" in line:
        values = {k: int(v) for k,v in re.findall(r"(seconds|sample|get_peers|announce|lookups|dropped)=(\d+)", line)}
        harvest.append(values)
recent = harvest[-60:]
stats["last_60_harvest_summaries"] = {k: sum(row[k] for row in recent) for k in recent[0]}
live = {k: metrics(root / "live-start.prom")[v] for k,v in names.items()}
result = {"hourly": hourly, "supervisor": stats, "live_start": live}
if (root / "live-end.prom").exists():
    start = metrics(root / "live-start.prom")
    end = metrics(root / "live-end.prom")
    seconds = (datetime.fromisoformat((root / "live-end-time.txt").read_text().strip()) - datetime.fromisoformat((root / "live-start-time.txt").read_text().strip())).total_seconds()
    result["live_window"] = {"seconds": seconds, "deltas": {k: end[v] - start[v] for k,v in names.items() if k not in ("active", "backlog", "reply_ratio")}, "end": {k: end[v] for k,v in names.items()}}
print(json.dumps(result, indent=2))
