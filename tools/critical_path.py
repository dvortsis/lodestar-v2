#!/usr/bin/env python3
"""What still stands between main and the 1.0 release, from the plan's live nodes.

Usage: python3 -I tools/critical_path.py <plan_nodes.json> [target=G-v1]
The JSON is the status connector's plan_nodes output. Prints the longest
remaining dependency chains (counted in slices), the ready slices on them,
and the unmet external conditions (owner or host actions). Read-only.
"""
import json, sys
from functools import lru_cache

nodes = json.load(open(sys.argv[1]))
target = sys.argv[2] if len(sys.argv) > 2 else "G-v1"
by = {n["id"]: n for n in nodes}

def deps(i):
    d = by.get(i, {}).get("deps") or ""
    return [x.strip() for x in (d.split(",") if isinstance(d, str) else d) if x.strip()]

def done(i):
    n = by.get(i)
    return n is not None and n.get("state") in ("done", "closed")

@lru_cache(None)
def chain(i):
    """Longest chain of unfinished nodes ending at i (i included if unfinished)."""
    if i.startswith("ext:") or done(i):
        return ()
    best = ()
    for d in deps(i):
        c = chain(d)
        if len(c) > len(best):
            best = c
    return best + (i,)

seen, ext, ready, stack = set(), set(), set(), [target]
while stack:
    i = stack.pop()
    if i in seen:
        continue
    seen.add(i)
    if i.startswith("ext:"):
        ext.add(i)
        continue
    if done(i):
        continue
    if by.get(i, {}).get("state") == "ready":
        ready.add(i)
    stack += deps(i)

c = chain(target)
print(f"Remaining to {target}: {sum(1 for i in seen if i in by and not done(i))} nodes; longest chain {len(c)}:")
print("  " + " -> ".join(c))
on_path = [i for i in c if i in ready]
print(f"Ready on the longest chain (start these first): {on_path}")
# Urgency: the longest chain of unfinished nodes from i up to the target.
users = {}
for i in seen:
    for d in deps(i):
        users.setdefault(d, set()).add(i)

@lru_cache(None)
def ahead(i):
    if i == target:
        return 1
    ups = [u for u in users.get(i, ()) if not done(u)]
    return 1 + max((ahead(u) for u in ups), default=0)

lens = sorted(((ahead(i), i) for i in ready), reverse=True)
print("Ready slices by the chain still ahead of them to the target (higher = more urgent):")
for l, i in lens[:15]:
    print(f"  {l:3d}  {i}  {by[i].get('title','')[:70]}")
print(f"External conditions still in the way ({len(ext)}): {sorted(ext)}")
