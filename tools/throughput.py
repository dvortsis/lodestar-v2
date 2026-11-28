#!/usr/bin/env python3
"""Runner throughput review: speed and quality signals for the last N hours.

Usage: python3 -I tools/throughput.py [hours=24] [state.json path]
Reads only public GitHub API data and runner-state's state.json. Prints no ids
beyond PR numbers. Read-only.
"""
import json, subprocess, sys, statistics as st
from datetime import datetime, timezone, timedelta

REPO = "dvortsis/lodestar-v2"
API = "https://api.github.com/repos/" + REPO
H = float(sys.argv[1]) if len(sys.argv) > 1 else 24.0
STATE = sys.argv[2] if len(sys.argv) > 2 else "state.json"
NOW = datetime.now(timezone.utc)
SINCE = NOW - timedelta(hours=H)
SKIP = {632, 821, 920}

def get(path):
    out = subprocess.run(["curl", "-sf", "--max-time", "30", API + path],
                         capture_output=True, text=True)
    return json.loads(out.stdout) if out.returncode == 0 and out.stdout else None

def ts(s):
    return datetime.fromisoformat(s.replace("Z", "+00:00")) if s else None

def med(xs):
    return round(st.median(xs), 1) if xs else None

def p90(xs):
    xs = sorted(xs)
    return round(xs[min(len(xs) - 1, int(0.9 * len(xs)))], 1) if xs else None

# 1. Merged PRs in the window.
prs = []
for page in (1, 2, 3):
    batch = get(f"/pulls?state=closed&sort=updated&direction=desc&per_page=100&page={page}") or []
    prs += batch
    if not batch or ts(batch[-1]["updated_at"]) < SINCE:
        break
merged = [p for p in prs if p.get("merged_at") and ts(p["merged_at"]) >= SINCE and p["number"] not in SKIP]
open_to_merge = [(ts(p["merged_at"]) - ts(p["created_at"])).total_seconds() / 3600 for p in merged]
commits = []
fixups = 0
for p in merged:
    d = get(f"/pulls/{p['number']}")
    if d:
        commits.append(d.get("commits", 0))
    t = p["title"].lower()
    if any(w in t for w in ("flake", "revert", "fix-up", "followup", "follow-up", "regression")):
        fixups += 1

# 2. CI: queue delay and the slowest jobs on the latest merged heads.
queue, dur = [], {}
for p in merged[:8]:
    runs = get(f"/actions/runs?head_sha={p['head']['sha']}&per_page=20") or {}
    for r in runs.get("workflow_runs", []):
        jobs = get(f"/actions/runs/{r['id']}/jobs?per_page=100") or {}
        for j in jobs.get("jobs", []):
            c, s, e = ts(j.get("created_at")), ts(j.get("started_at")), ts(j.get("completed_at"))
            if c and s and s >= c:
                queue.append((s - c).total_seconds() / 60)
            if s and e and j.get("conclusion") == "success":
                dur.setdefault(j["name"], []).append((e - s).total_seconds() / 60)
slow = sorted(((med(v), k) for k, v in dur.items()), reverse=True)[:5]

# 3. main health: pushes to main in the window.
mainruns = get(f"/actions/runs?branch=main&event=push&per_page=60") or {}
mr = [r for r in mainruns.get("workflow_runs", []) if ts(r["created_at"]) >= SINCE and r.get("conclusion")]
main_red = [r["name"] for r in mr if r["conclusion"] not in ("success", "skipped", "cancelled", "neutral")]

# 4. Open PRs: green and waiting on the runner, or stale.
waiting, stale = [], []
for p in get("/pulls?state=open&per_page=50") or []:
    if p["number"] in SKIP:
        continue
    age = (NOW - ts(p["updated_at"])).total_seconds() / 3600
    cr = get(f"/commits/{p['head']['sha']}/check-runs?per_page=100") or {}
    runs = cr.get("check_runs", [])
    pend = [c for c in runs if c["status"] != "completed"]
    bad = [c for c in runs if c.get("conclusion") in ("failure", "timed_out", "cancelled", "action_required")]
    done = [ts(c["completed_at"]) for c in runs if c.get("completed_at")]
    if runs and not pend and not bad and done:
        waiting.append((p["number"], round((NOW - max(done)).total_seconds() / 3600, 1)))
    elif age > 6:
        stale.append((p["number"], round(age, 1), "red" if bad else "pending"))

# 5. Slots.
try:
    s = json.load(open(STATE))
    items = s.get("slots", {}).get("items", [])
    active = [i for i in items if i.get("status") != "idle"]
    old = [(i["slot"], i["slice"], round((NOW - ts(i["since"])).total_seconds() / 3600, 1))
           for i in active if i.get("since") and (NOW - ts(i["since"])).total_seconds() > 12 * 3600]
    slot_line = f"{len(active)} active of the cap; same task over 12 h: {old}"
except Exception as e:  # noqa: BLE001
    slot_line = f"state.json unreadable: {e}"

print(f"Throughput review, last {H:g} h, at {NOW.strftime('%Y-%m-%dT%H:%MZ')}")
print(f"- merged PRs: {len(merged)}; open->merge h median {med(open_to_merge)}, p90 {p90(open_to_merge)}")
print(f"- commits per merged PR: median {med(commits)}, p90 {p90(commits)} (review rounds proxy)")
print(f"- fix/flake/revert-titled merges: {fixups}")
print(f"- CI job queue delay min: median {med(queue)}, p90 {p90(queue)} over {len(queue)} jobs")
print(f"- slowest CI jobs (median min): {slow}")
print(f"- main push runs: {len(mr)}, red: {len(main_red)} {sorted(set(main_red))[:5]}")
print(f"- open PRs green and unmerged (PR, h since CI done): {waiting}")
print(f"- open PRs idle over 6 h (PR, h, state): {stale}")
print(f"- slots: {slot_line}")
