"""fleet.yml's entry point (OP-1, dry-run only; plan §2-§3).

    fleet.py gate            filter job: no secrets; writes allowed/reason
    fleet.py refuse          answers a refused /fleet command
    fleet.py act             runs one allowed command under the fleet lock
    fleet.py reap            the hourly reaper (exempt from OPERATOR_STOP)
    fleet.py dispatch-down   the owner's workflow_dispatch down: no input,
                             never through the parser, pre-empts the lock

Everything comes from the environment the workflow sets; comment text only
through COMMENT_BODY. Logs carry fixed strings and counts, never a value from
a secret, fleet.json's slots or an inventory ID. Live mode is OP-2's: with
FLEET_LIVE=true every acting subcommand refuses.
"""

from __future__ import annotations

import os
import sys
import time
from datetime import datetime, timezone

import limits as limits_mod
from ancestry import head_of, is_ancestor
from comments import Channel, urllib_transport
from command import REASONS, Command, Refused
from gates import Gated, gate
from lock import ACQUIRED, REAPING, FleetLock, MemoryStore
from reaper import DOWN, EMERGENCY, decide, only_down
from state import DryRunBackend, new_fleet, validate
from status import render

REAPER_WAIT_SECONDS = 300


class Ctx:
    def __init__(self, env, backend, store, transport, clock, sleep, repo):
        self.env, self.repo = env, repo
        self.limits = limits_mod.load()
        self.backend = backend if backend is not None else DryRunBackend()
        self.clock = clock or (lambda: datetime.now(timezone.utc))
        self.sleep = sleep or time.sleep
        self.lock = FleetLock(store if store is not None else MemoryStore(), self.limits["lock_ttl_minutes"], self.clock)
        self.channel = None
        repo_name, pr, token = env.get("GITHUB_REPOSITORY", ""), env.get("CHANNEL_PR", ""), env.get("GITHUB_TOKEN", "")
        if pr.isdigit() and repo_name and (transport or token):
            self.channel = Channel(transport or urllib_transport(token), repo_name, pr)

    def run_id(self) -> str:
        rid, att = self.env.get("GITHUB_RUN_ID", ""), self.env.get("GITHUB_RUN_ATTEMPT", "1")
        if not (rid.isdigit() and att.isdigit()):
            raise SystemExit("fleet: GITHUB_RUN_ID missing")
        return f"{rid}-{att}"

    def report(self, line: str) -> None:
        print(line)
        if self.channel:
            self.channel.post(f"`fleet.yml` (dry-run): {line}")

    def status(self) -> None:
        if self.channel:
            self.channel.upsert_status(render(self.backend.read_fleet(), self.limits, self.clock(), dry_run=True))


def _write_outputs(env, **kv) -> None:
    path = env.get("GITHUB_OUTPUT")
    if path:
        with open(path, "a", encoding="utf-8") as f:
            f.writelines(f"{k}={v}\n" for k, v in kv.items())


def cmd_gate(ctx: Ctx) -> int:
    try:
        cmd = gate(ctx.env, ctx.limits)
    except Gated:
        print("gate: ignored")
        _write_outputs(ctx.env, allowed="false", reason="")
        return 0
    except Refused as r:
        print(f"gate: refused: {r.reason}")
        _write_outputs(ctx.env, allowed="false", reason=r.reason)
        return 0
    print(f"gate: allowed: {cmd.verb}")
    _write_outputs(ctx.env, allowed="true", reason="")
    return 0


def cmd_refuse(ctx: Ctx) -> int:
    reason = ctx.env.get("REFUSAL_REASON", "")
    if reason not in REASONS:
        return 0
    ctx.report(f"refused: {reason}")
    return 0


def _execute(ctx: Ctx, cmd: Command, run_id: str, fleet) -> str:
    b, now = ctx.backend, ctx.clock()
    if cmd.verb == "status":
        return "status: updated the status comment"
    if cmd.verb == "up":
        why = limits_mod.check_up(ctx.limits, cmd.preset, b, now)
        if why:
            return f"refused: up {cmd.preset}: {why}"
        commit = head_of("origin/main", ctx.repo)
        new = new_fleet(ctx.limits, cmd.preset, commit, now)
        ctx.lock.fenced(run_id, lambda: b.write_fleet(new))
        roles = ", ".join(new["roles"])
        cost = limits_mod.projected_usd(ctx.limits, cmd.preset)
        return f"up {cmd.preset}: would create {roles}, pinned to main; expires {new['expires_at']}; at most ${cost:.2f}"
    if cmd.verb == "down":
        if fleet is None:
            return "down: no fleet recorded"
        ctx.lock.fenced(run_id, b.destroy)
        return "down: would destroy the fleet in state"
    # run <step> <vm> [<commit>]
    if fleet is None:
        return f"refused: run {cmd.step}: no fleet"
    if not is_ancestor(fleet["commit"], "origin/main", ctx.repo):
        return f"refused: run {cmd.step}: the fleet's pinned commit is not an ancestor of main"
    if cmd.commit is not None and not is_ancestor(cmd.commit, "origin/main", ctx.repo):
        return f"refused: run {cmd.step}: the commit is not an ancestor of main"
    return f"run {cmd.step} {cmd.vm}: would run the step at the fleet's pinned commit"


def cmd_act(ctx: Ctx) -> int:
    # Author, edit (created only) and PR checks come before the lock.
    try:
        cmd = gate(ctx.env, ctx.limits)
    except Gated:
        return 0
    except Refused as r:
        ctx.report(f"refused: {r.reason}")
        return 0
    run_id = ctx.run_id()
    got = ctx.lock.acquire(run_id)
    if got != ACQUIRED:
        ctx.report("busy: the fleet is being reaped; retry later" if got == REAPING else "busy, retry")
        return 0
    try:
        fleet = ctx.backend.read_fleet()
        if fleet is not None:
            validate(fleet, ctx.limits)
        if cmd.verb != "down" and only_down(fleet, ctx.backend.mismatch(), ctx.clock()):
            line = "refused: fleet overdue or mismatched: only down runs"
        else:
            line = _execute(ctx, cmd, run_id, fleet)
    finally:
        ctx.lock.release(run_id)
    ctx.report(line)
    ctx.status()
    return 0


def cmd_reap(ctx: Ctx) -> int:
    run_id, b = ctx.run_id(), ctx.backend
    fleet = b.read_fleet()
    if fleet is not None:
        validate(fleet, ctx.limits)
    d = decide(fleet, b.lodestar_linodes(), b.mismatch(), ctx.clock(), ctx.limits)
    if d.action == EMERGENCY:
        ctx.lock.preempt(run_id)
    elif d.action == DOWN or d.age_reap:
        deadline = ctx.clock().timestamp() + REAPER_WAIT_SECONDS
        while ctx.lock.acquire(run_id) != ACQUIRED:
            if ctx.clock().timestamp() >= deadline:
                print("reaper: lock busy; trying again next hour")
                return 0
            ctx.sleep(30)
    else:
        print(f"reaper: nothing to do ({d.reason})")
        ctx.status()
        return 0
    try:
        if d.action in (DOWN, EMERGENCY):
            ctx.lock.fenced(run_id, b.destroy)
        if d.age_reap:
            ctx.lock.fenced(run_id, lambda: b.delete_linodes(d.age_reap))
    finally:
        ctx.lock.release(run_id)
    line = f"reaper: {d.action} ({d.reason}); Linodes over 44 h: {len(d.age_reap)}"
    if d.action == EMERGENCY:
        ctx.report(line)
    else:
        print(line)
    ctx.status()
    return 0


def cmd_dispatch_down(ctx: Ctx) -> int:
    run_id = ctx.run_id()
    ctx.lock.preempt(run_id)
    try:
        had = ctx.backend.read_fleet() is not None
        if had:
            ctx.lock.fenced(run_id, ctx.backend.destroy)
    finally:
        ctx.lock.release(run_id)
    ctx.report("owner's down: would destroy the fleet in state" if had else "owner's down: no fleet recorded")
    ctx.status()
    return 0


COMMANDS = {"gate": cmd_gate, "refuse": cmd_refuse, "act": cmd_act, "reap": cmd_reap, "dispatch-down": cmd_dispatch_down}


def main(argv=None, env=None, backend=None, store=None, transport=None, clock=None, sleep=None, repo=".") -> int:
    argv = sys.argv[1:] if argv is None else argv
    env = dict(os.environ) if env is None else env
    if len(argv) != 1 or argv[0] not in COMMANDS:
        print("usage: fleet.py " + "|".join(COMMANDS), file=sys.stderr)
        return 2
    if argv[0] not in ("gate", "refuse") and env.get("FLEET_LIVE") == "true":
        print("fleet: live mode is not built yet (OP-2); refusing", file=sys.stderr)
        return 2
    return COMMANDS[argv[0]](Ctx(env, backend, store, transport, clock, sleep, repo))


if __name__ == "__main__":
    sys.exit(main())
