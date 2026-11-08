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
import subprocess
import sys
import time
from datetime import datetime, timezone

import limits as limits_mod
from ancestry import head_of, is_ancestor
from comments import Channel, urllib_transport
from command import REASONS, Command, Refused
from gates import Gated, gate
from lock import ACQUIRED, REAPING, FleetLock, LockLost, MemoryStore
from reaper import DOWN, EMERGENCY, Decision, decide, only_down
from state import DryRunBackend, new_fleet, validate
from status import MARKER, render

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
        if _digits(pr) and repo_name and (transport or token):
            self.channel = Channel(transport or urllib_transport(token), repo_name, pr)

    def run_id(self) -> str:
        rid, att = self.env.get("GITHUB_RUN_ID", ""), self.env.get("GITHUB_RUN_ATTEMPT", "1")
        if not (_digits(rid) and _digits(att)):
            raise SystemExit("fleet: GITHUB_RUN_ID missing")
        return f"{rid}-{att}"

    def report(self, line: str, with_status: bool = False) -> None:
        print(line)
        if self.channel:
            body = f"`fleet.yml` (dry-run): {line}"
            if with_status:
                # A fresh, never-edited copy of the status, which the reader
                # helper trusts; the edited-in-place comment it drops.
                rendered = render(self.backend.read_fleet(), self.limits, self.clock(), dry_run=True)
                body += "\n\n" + rendered.replace(MARKER + "\n", "", 1)
            self.channel.post(body)

    def status(self) -> None:
        if self.channel:
            self.channel.upsert_status(render(self.backend.read_fleet(), self.limits, self.clock(), dry_run=True))


def _digits(s: str) -> bool:
    return s.isascii() and s.isdigit()


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


def _on_main_head(repo: str) -> bool:
    """The checked-out code is main's current head (a re-run checks out the old SHA)."""
    try:
        return head_of("HEAD", repo) == head_of("origin/main", repo)
    except (subprocess.CalledProcessError, ValueError):
        return False


def cmd_act(ctx: Ctx) -> int:
    # Author, edit (created only), PR and re-run checks come before the lock.
    try:
        cmd = gate(ctx.env, ctx.limits)
    except Gated:
        return 0
    except Refused as r:
        ctx.report(f"refused: {r.reason}")
        return 0
    if not _on_main_head(ctx.repo):
        ctx.report("refused: this run's code is not main's head")
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
    except LockLost:
        ctx.report("failed: the fleet lock was lost (pre-empted); nothing more was written")
        return 1
    except ValueError:
        ctx.report("failed: fleet.json is invalid; escalate")
        return 1
    finally:
        ctx.lock.release(run_id)
    ctx.report(line, with_status=True)
    ctx.status()
    return 0


def _decide(ctx: Ctx):
    b = ctx.backend
    fleet = b.read_fleet()
    if fleet is not None:
        validate(fleet, ctx.limits)
    return decide(fleet, b.lodestar_linodes(), b.mismatch(), ctx.clock(), ctx.limits)


def cmd_reap(ctx: Ctx) -> int:
    run_id, b = ctx.run_id(), ctx.backend
    try:
        d = _decide(ctx)
    except ValueError:
        ctx.report("reaper: fleet.json is invalid; escalate")
        return 1
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
        # Decide again under the lock: a command may have changed the fleet
        # while the reaper waited.
        d2 = _decide(ctx)
        if d.action == EMERGENCY and b.read_fleet() is not None:
            d2 = Decision(EMERGENCY, d.reason, d2.age_reap)
        if d2.action in (DOWN, EMERGENCY):
            ctx.lock.fenced(run_id, b.destroy)
        if d2.age_reap:
            ctx.lock.fenced(run_id, lambda: b.delete_linodes(d2.age_reap))
    except LockLost:
        ctx.report("reaper: the fleet lock was lost; trying again next hour")
        return 1
    except ValueError:
        ctx.report("reaper: fleet.json is invalid; escalate")
        return 1
    finally:
        ctx.lock.release(run_id)
    line = f"reaper: {d2.action} ({d2.reason}); Linodes over 44 h: {len(d2.age_reap)}"
    if d2.action == EMERGENCY:
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
    except LockLost:
        ctx.report("owner's down: the fleet lock was lost; run it again")
        return 1
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
    try:
        return COMMANDS[argv[0]](Ctx(env, backend, store, transport, clock, sleep, repo))
    except SystemExit:
        raise
    except Exception as e:  # noqa: BLE001 - a traceback could carry private values
        print(f"fleet: internal error ({type(e).__name__})", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
