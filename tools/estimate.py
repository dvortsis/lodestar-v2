#!/usr/bin/env python3
"""Estimates to each milestone, from the live plan and measured slice times.

Usage: python3 -I tools/estimate.py <plan_nodes.json> [days=7] [state.json] [fixed.json]

- Slice time: from a slice's brief landing on runner-state (briefs/<id>.md, the
  launch) to its PR's merge on main, over merges in the last `days` days. Only
  PRs titled "<id>: ..." with a brief count.
- For each target in fixed.json's "targets" it prints the unfinished nodes
  upstream, the longest chain, and p50/p90 finish times:
    serial   = chain length x slice time (one after another)
    parallel = remaining slices / slots x slice time (all slots busy)
    estimate = max(serial, parallel); serial includes the fixed waits on the chain
  Fixed waits (soaks, live crawls, owner steps) are hours per node id in
  fixed.json's "hours"; a node with fixed hours is not counted as a slice.
- Owner steps (state "you" or an unmet ext: condition) have no measured time.
  The estimate assumes each is done when reached and lists them, so a late one
  moves the date by its delay.
Read-only. Public GitHub API only; prints PR numbers and slice ids, no other ids.
"""
import json, subprocess, sys, statistics as st
from datetime import datetime, timezone, timedelta
from functools import lru_cache

REPO = "dvortsis/lodestar-v2"
API = "https://api.github.com/repos/" + REPO
nodes = json.load(open(sys.argv[1]))
DAYS = float(sys.argv[2]) if len(sys.argv) > 2 else 7.0
STATE = sys.argv[3] if len(sys.argv) > 3 else "state.json"
FIXED = sys.argv[4] if len(sys.argv) > 4 else "reviews/estimate-fixed.json"
NOW = datetime.now(timezone.utc)
SINCE = NOW - timedelta(days=DAYS)
by = {n["id"]: n for n in nodes}
fixed = json.load(open(FIXED))
hours_fixed = fixed.get("hours", {})
slots = len(json.load(open(STATE)).get("slots", {}).get("items", [])) or 6
_raw = subprocess.run(["curl", "-sf", "--max-time", "30",
                       "https://raw.githubusercontent.com/" + REPO + "/main/runner/dashboard.yaml"],
                      capture_output=True, text=True).stdout
try:
    import yaml
    _cond = (yaml.safe_load(_raw) or {}).get("conditions", {}) if _raw else {}
except ImportError:
    _cond = {}
met_ext = {"ext:" + k for k, v in _cond.items() if isinstance(v, dict) and v.get("met")}


def ts(s):
    return datetime.fromisoformat(s.replace("Z", "+00:00")) if s else None


def get(path):
    out = subprocess.run(["curl", "-sf", "--max-time", "30", API + path], capture_output=True, text=True)
    return json.loads(out.stdout) if out.returncode == 0 and out.stdout else None


# 1. Measured slice time, launch (brief added) to merge.
added = {}
log = subprocess.run(["git", "log", "--diff-filter=A", "--format=C %cI", "--name-only", "--", "briefs"],
                     capture_output=True, text=True).stdout.splitlines()
when = None
for line in log:
    if line.startswith("C "):
        when = ts(line[2:])
    elif line.startswith("briefs/") and line.endswith(".md"):
        added.setdefault(line[7:-3], when)  # newest first: keep the latest brief
prs = []
for page in range(1, 6):
    batch = get(f"/pulls?state=closed&sort=updated&direction=desc&per_page=100&page={page}") or []
    prs += batch
    if not batch or ts(batch[-1]["updated_at"]) < SINCE:
        break
cycle, planned = [], []
for p in prs:
    m = ts(p.get("merged_at"))
    sid = p["title"].split(":", 1)[0].strip()
    if m and m >= SINCE and sid in added and added[sid] and added[sid] < m:
        h = (m - added[sid]).total_seconds() / 3600
        cycle.append(h)
        if not sid.startswith(("X-", "D-", "P-")):
            planned.append(h)


def pct(xs, q):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(q * len(xs)))] if xs else None


# Remaining work is mostly planned slices, which run longer than the X- fixes,
# so they set the rate when there are enough of them (5 or more).
basis = planned if len(planned) >= 5 else cycle
c50 = st.median(basis) if basis else 8.0
c90 = pct(basis, 0.75) or 24.0  # slow case: p75 (p90 is dominated by slices held for owner answers)
print(f"Slice time, launch to merge, last {DAYS:g} days: all n={len(cycle)} p50={st.median(cycle) if cycle else 0:.1f}h; "
      f"planned n={len(planned)} p50={st.median(planned) if planned else 0:.1f}h p90={pct(planned, 0.9) or 0:.1f}h; "
      f"using {'planned' if basis is planned else 'all'}: p50={c50:.1f}h slow(p75)={c90:.1f}h; slots={slots}")


# 2. Per target.
def deps(i):
    d = by.get(i, {}).get("deps") or ""
    return [x.strip() for x in (d.split(",") if isinstance(d, str) else d) if x.strip()]


def done(i):
    n = by.get(i)
    return n is not None and n.get("state") in ("done", "closed")


@lru_cache(None)
def path_hours(i, c):
    """Longest path to i in hours: a slice costs c, a fixed node its hours."""
    if i.startswith("ext:") or done(i) or i not in by:
        return 0.0, ()
    best = (0.0, ())
    for d in deps(i):
        h = path_hours(d, c)
        if h[0] > best[0]:
            best = h
    own = float(hours_fixed.get(i, c))
    return best[0] + own, best[1] + (i,)


for t in fixed.get("targets", []):
    tid, label = t["id"], t.get("label", t["id"])
    seen, ext, you, stack = set(), set(), set(), [tid]
    while stack:
        i = stack.pop()
        if i in seen:
            continue
        seen.add(i)
        if i.startswith("ext:"):
            ext.add(i)
            continue
        if done(i) or i not in by:
            continue
        if by[i].get("state") == "you":
            you.add(i)
        stack += deps(i)
    open_ = [i for i in seen if i in by and not done(i)]
    slices = [i for i in open_ if i not in hours_fixed and by[i].get("kind") != "gate"]
    waits = max((float(hours_fixed[i]) for i in open_ if i in hours_fixed), default=0.0)
    out = []
    for c in (c50, c90):
        serial, chain = path_hours(tid, c)
        parallel = len(slices) / slots * c
        h = max(serial, parallel)
        out.append((h, chain, serial, parallel))
    d50 = NOW + timedelta(hours=out[0][0])
    d90 = NOW + timedelta(hours=out[1][0])
    bound = "chain" if out[0][2] >= out[0][3] else "slots"
    print(f"\n{label} ({tid}): {len(open_)} open, {len(slices)} slices, longest fixed wait {waits:.0f}h")
    print(f"  p50 {d50:%Y-%m-%d} ({out[0][0] / 24:.1f} d)  slow {d90:%Y-%m-%d} ({out[1][0] / 24:.1f} d)  bound by {bound}")
    print("  chain: " + " -> ".join(out[0][1]))
    unmet = sorted(e for e in ext if e not in met_ext)
    if you or unmet:
        print(f"  owner/external steps assumed on time: {sorted(you) + unmet}")
