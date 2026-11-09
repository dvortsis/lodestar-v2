"""fleet.yml's entry point (OP-1, with live mode from OP-2; plan §2-§3).

    fleet.py gate            filter job: no secrets; writes allowed/reason
    fleet.py refuse          answers a refused /fleet command
    fleet.py act             runs one allowed command under the fleet lock
    fleet.py reap            the hourly reaper (exempt from OPERATOR_STOP)
    fleet.py dispatch-down   the owner's workflow_dispatch down: no input,
                             never through the parser, pre-empts the lock
    fleet.py pia-fence       just before deliver-pia: this run still holds
                             the lock act kept, and the fleet allows it (OP-2b)
    fleet.py pia-release     after deliver-pia, always: the conditional release

Everything comes from the environment the workflow sets; comment text only
through COMMENT_BODY. Logs carry fixed strings and counts, never a value from
a secret, fleet.json's slots or an inventory ID. Dry-run uses an in-memory
backend and lock; FLEET_LIVE=true switches to apply.py's live backend and the
S3 lock (OP-2). In live mode `run <step>` refuses until OP-3 adds the steps,
except `renew-vpn`, which chains into deliver-pia like `up` (OP-2b).
"""

from __future__ import annotations

import os
import subprocess
import sys
import time
from datetime import datetime, timezone

import limits as limits_mod
import ancestry
from ancestry import head_of, is_ancestor
from comments import Channel, urllib_transport
from command import REASONS, Command, Refused
from gates import Gated, gate
from apply import ApplyFailed
from lock import ACQUIRED, REAPING, FleetLock, LockLost, MemoryStore
from reaper import DOWN, EMERGENCY, NONE, decide, only_down
from state import DryRunBackend, FleetUnknown, LedgerUnreadable, new_fleet, validate
from status import MARKER, render

REAPER_WAIT_SECONDS = 300


class Ctx:
    def __init__(self, env, backend, store, transport, clock, sleep, repo):
        self.env, self.repo = env, repo
        self.limits = limits_mod.load()
        self.live = env.get("FLEET_LIVE") == "true"
        self.clock = clock or (lambda: datetime.now(timezone.utc))
        self.sleep = sleep or time.sleep
        self._backend, self._store, self._lock = backend, store, None
        self.deliver = False  # act keeps the lock for deliver-pia (OP-2b)
        self.channel = None
        repo_name, pr, token = env.get("GITHUB_REPOSITORY", ""), env.get("CHANNEL_PR", ""), env.get("GITHUB_TOKEN", "")
        if _digits(pr) and repo_name and (transport or token):
            self.channel = Channel(transport or urllib_transport(token), repo_name, pr)

    def _connect(self) -> None:
        """Live credentials are minted on first use, so only after the gates (and,
        for up and run, the main-head check; for the kill paths, the re-run
        check) have passed."""
        if self._backend is None and self.live:
            import apply  # noqa: PLC0415 - live only; needs boto3 and credentials
            self._backend, self._store = apply.live_context(self.env, self.limits, self.clock, self.run_id())
        if self._backend is None:
            self._backend = DryRunBackend()
        if self._lock is None:
            store = self._store if self._store is not None else MemoryStore()
            self._lock = FleetLock(store, self.limits["lock_ttl_minutes"], self.clock)

    @property
    def backend(self):
        self._connect()
        return self._backend

    @property
    def lock(self) -> FleetLock:
        self._connect()
        return self._lock

    def run_id(self) -> str:
        rid, att = self.env.get("GITHUB_RUN_ID", ""), self.env.get("GITHUB_RUN_ATTEMPT", "1")
        if not (_digits(rid) and _digits(att)):
            raise SystemExit("fleet: GITHUB_RUN_ID missing")
        return f"{rid}-{att}"

    def fence(self, run_id: str):
        return lambda: self.lock.fence(run_id)

    def fleet(self):
        """(fleet or None, unknown): unknown when fleet.json can't be read."""
        try:
            fleet = self.backend.read_fleet()
        except FleetUnknown:
            return None, True
        if fleet is not None:
            validate(fleet, self.limits)
        return fleet, False

    def _render(self) -> str:
        try:
            fleet, unknown = self.fleet()
        except ValueError:
            fleet, unknown = None, True
        return render(fleet, self.limits, self.clock(), dry_run=not self.live, unknown=unknown)

    def report(self, line: str, with_status: bool = False) -> None:
        """Best-effort: a channel that fails (GitHub down, a denied token) never
        stops what the caller does next, above all the reaper's enforcement."""
        print(line)
        if not self.channel:
            return
        try:
            body = f"`fleet.yml` ({'live' if self.live else 'dry-run'}): {line}"
            if with_status:
                # A fresh, never-edited copy of the status, which the reader
                # helper trusts; the edited-in-place comment it drops.
                body += "\n\n" + self._render().replace(MARKER + "\n", "", 1)
            self.channel.post(body)
        except Exception:  # noqa: BLE001 - fixed line; the error may carry details
            print("fleet: couldn't post to the channel")

    def status(self) -> None:
        if not self.channel:
            return
        try:
            self.channel.upsert_status(self._render())
        except Exception:  # noqa: BLE001
            print("fleet: couldn't update the status comment")

    def release(self, run_id: str, who: str) -> None:
        """Release the lock, reporting (never raising) a failure: the lock then
        expires on its own, which fails closed."""
        try:
            self.lock.release(run_id)
        except Exception:  # noqa: BLE001
            self.report(f"{who}: couldn't release the fleet lock; it expires on its own; escalate")


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


def _down(ctx: Ctx, run_id: str, fleet, fence=None) -> bool:
    """Destroy, then, once nothing tagged is left, book the spend, tombstone
    fleet.json and clear the mismatch flag. False if anything remains."""
    fence = fence or ctx.fence(run_id)
    if not ctx.backend.destroy(fence):
        return False
    ctx.backend.finish_down(fleet, fence)
    return True


def _tolerant(ctx: Ctx, run_id: str):
    """The fence for paths that have already pre-empted (the reaping marker): a
    lock read that fails (S3) doesn't stop the sweep; losing the lock still does."""
    fence = ctx.fence(run_id)

    def check():
        try:
            fence()
        except LockLost:
            raise
        except Exception:  # noqa: BLE001
            pass
    return check


def _execute(ctx: Ctx, cmd: Command, run_id: str, fleet) -> str:
    b, now, would = ctx.backend, ctx.clock(), "" if ctx.live else "would "
    if cmd.verb == "status":
        return "status: updated the status comment"
    if cmd.verb == "up":
        why = limits_mod.check_up(ctx.limits, cmd.preset, b, now)
        if why:
            return f"refused: up {cmd.preset}: {why}"
        commit = head_of("origin/main", ctx.repo)
        new = b.create(new_fleet(ctx.limits, cmd.preset, commit, now), ctx.fence(run_id))
        roles = ", ".join(new["roles"])
        cost = limits_mod.projected_usd(ctx.limits, cmd.preset)
        ctx.deliver = True  # deliver-pia follows, under this run's lock
        return f"up {cmd.preset}: {would}create {roles}, pinned to main; expires {new['expires_at']}; at most ${cost:.2f}"
    if cmd.verb == "down":
        if not _down(ctx, run_id, fleet):
            return "down: tagged resources remain after destroy; escalate"
        return f"down: {would}destroy the fleet" if fleet else "down: no fleet recorded; nothing tagged is left"
    # run <step> <vm> [<commit>]
    if fleet is None:
        return f"refused: run {cmd.step}: no fleet"
    if not is_ancestor(fleet["commit"], "origin/main", ctx.repo):
        return f"refused: run {cmd.step}: the fleet's pinned commit is not an ancestor of main"
    if cmd.commit is not None and not is_ancestor(cmd.commit, "origin/main", ctx.repo):
        return f"refused: run {cmd.step}: the commit is not an ancestor of main"
    if cmd.step == "renew-vpn":
        # The WireGuard step runs only inside deliver-pia (plan §3): this run keeps
        # the lock, and the chained jobs fence, deliver and release it.
        ctx.deliver = True
        return f"run renew-vpn {cmd.vm}: {would}deliver the PIA login again (deliver-pia, under this run's lock)"
    if ctx.live:
        return f"refused: run {cmd.step}: step not built (OP-3)"
    return f"run {cmd.step} {cmd.vm}: would run the step at the fleet's pinned commit"


def _on_main_head(repo: str) -> bool:
    """The checked-out code is main's current head on GitHub (a re-run checks
    out the old SHA). Fails closed if GitHub can't be asked.

    Elsewhere, `origin/main` in the ancestor checks means GITHUB_SHA, because
    actions/checkout points it there; once this check passes that is main's
    head, and either way it is a commit on main, so those checks stay sound.
    """
    remote = ancestry.remote_main_head(repo)
    if remote is None:
        return False
    try:
        return head_of("HEAD", repo) == remote
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
    # Paths that create or change the fleet run only main's current code. A down
    # never waits on that: slightly older main code is the safe direction for a kill.
    if cmd.verb in ("up", "run") and not _on_main_head(ctx.repo):
        ctx.report("refused: this run's code is not main's head")
        return 0
    run_id = ctx.run_id()
    got = ctx.lock.acquire(run_id)
    if got != ACQUIRED:
        ctx.report("busy: the fleet is being reaped; retry later" if got == REAPING else "busy, retry")
        return 0
    try:
        fleet, unknown = ctx.fleet()
        if cmd.verb != "down" and unknown:
            line = "refused: fleet state unreadable: only down runs"
        elif cmd.verb != "down" and only_down(fleet, ctx.backend.mismatch(), ctx.clock()):
            line = "refused: fleet overdue or mismatched: only down runs"
        else:
            line = _execute(ctx, cmd, run_id, fleet)
    except LockLost:
        ctx.report("failed: the fleet lock was lost (pre-empted); stopped before its next fleet write")
        return 1
    except ApplyFailed:
        ctx.report("failed: terraform didn't finish; the fleet is recorded, so run down (output is in the evidence bucket)")
        return 1
    except LedgerUnreadable:
        ctx.report("failed: the spend ledger is unreadable, so up is refused; escalate")
        return 1
    except ValueError:
        ctx.report("failed: fleet.json is invalid; escalate")
        return 1
    finally:
        if not ctx.deliver:  # else pia-release releases it, after deliver-pia
            ctx.release(run_id, "act")
    if ctx.deliver:
        _write_outputs(ctx.env, deliver="true")
    ctx.report(line, with_status=True)
    ctx.status()
    return 0


def _acquire(ctx: Ctx, run_id: str) -> bool:
    deadline = ctx.clock().timestamp() + REAPER_WAIT_SECONDS
    while ctx.lock.acquire(run_id) != ACQUIRED:
        if ctx.clock().timestamp() >= deadline:
            return False
        ctx.sleep(30)
    return True


def _first_attempt(ctx: Ctx, who: str) -> bool:
    """Kill paths refuse only a re-run (which replays an old event with old code),
    never main having moved on: they must not skip an hour of lifetime enforcement."""
    if ctx.env.get("GITHUB_RUN_ATTEMPT") != "1":
        ctx.report(f"{who}: refused, a re-run; escalate")
        return False
    return True


def cmd_reap(ctx: Ctx) -> int:
    if not _first_attempt(ctx, "reaper"):  # before any credential
        return 0
    run_id, b = ctx.run_id(), ctx.backend
    try:
        fleet, unknown = ctx.fleet()
        flag = b.mismatch_state()
        d = decide(fleet, b.lodestar_linodes(), flag == "true", ctx.clock(), ctx.limits)
    except ValueError:
        ctx.report("reaper: fleet.json is invalid; escalate")
        return 1
    if flag == "unreadable":
        # Only a readable true destroys. Commands stay down-only (fail closed),
        # and lifetime enforcement below goes on.
        ctx.report("reaper: the mismatch flag is unreadable; commands are down-only; escalate")
    # The emergency path doesn't wait for the lock; everything else, the
    # firewall audit included, runs under it.
    marker = d.action == EMERGENCY
    if marker:
        ctx.lock.preempt(run_id)
    elif not _acquire(ctx, run_id):
        ctx.report("reaper: lock busy; trying again next hour")
        return 0
    left = emptied = False
    problems = None
    try:
        if unknown:
            try:
                b.sweep(ctx.fence(run_id))  # stale job rules still go
            except LockLost:
                raise
            except Exception:  # noqa: BLE001
                pass
            ctx.report("reaper: fleet.json unreadable; firewall audit skipped; escalate")
        else:
            # The audit's own failure must never stop the lifetime checks below:
            # it escalates, and an audit that can't run is never a mismatch.
            try:
                problems = b.audit(ctx.fence(run_id))  # sweeps stale job rules first
            except LockLost:
                raise
            except Exception:  # noqa: BLE001
                problems = None
                ctx.report("reaper: the firewall audit couldn't run; escalate")
        failed = getattr(b, "lookup_failures", 0)
        if failed:
            ctx.report(f"reaper: {failed} job-rule lookups failed, so those rules were swept; escalate")
        if problems:  # only the audit sets it, so never with fleet.json unreadable
            b.set_mismatch(problems)
            ctx.lock.preempt(run_id)  # a mismatch pre-empts as the overdue case does
            marker = True
        # Decide again with what's true now (a command may have run while we waited).
        fleet, unknown = ctx.fleet()
        flag = b.mismatch_state()
        # A mismatch found just now goes down this hour even if the flag's re-read fails.
        d = decide(fleet, b.lodestar_linodes(), flag == "true" or bool(problems), ctx.clock(), ctx.limits)
        fence = _tolerant(ctx, run_id) if marker else ctx.fence(run_id)
        if d.action in (DOWN, EMERGENCY):
            emptied = _down(ctx, run_id, fleet, fence)
            left = not emptied
        elif d.age_reap:
            ctx.lock.fenced(run_id, lambda: b.delete_linodes(d.age_reap))
        elif (flag == "true" or problems) and not fleet and not unknown:
            emptied = _down(ctx, run_id, None, fence)  # a flag with nothing recorded: sweep and clear it
            left = not emptied
    except LockLost:
        ctx.report("reaper: the fleet lock was lost; trying again next hour")
        return 1
    except LedgerUnreadable:
        emptied = True
        ctx.report("reaper: fleet down and tombstoned, but the spend ledger is unreadable; escalate")
    except ValueError:
        ctx.report("reaper: fleet.json is invalid; escalate")
        return 1
    except Exception as e:  # noqa: BLE001 - the marker stays; a person looks
        ctx.report(f"reaper: unexpected error ({type(e).__name__}); escalate")
        return 1
    finally:
        # The reaping marker clears only on a confirmed empty inventory; otherwise
        # it stays until its own expiry, and down-only holds meanwhile.
        if not marker or emptied:
            ctx.release(run_id, "reaper")
    line = f"reaper: {d.action} ({d.reason}); Linodes over 44 h: {len(d.age_reap)}"
    if left:
        line += "; tagged resources remain; escalate"
    if d.action == EMERGENCY or left:
        ctx.report(line)
    else:
        print(line)
    ctx.status()
    return 0


def cmd_dispatch_down(ctx: Ctx) -> int:
    if not _first_attempt(ctx, "owner's down"):  # before any credential
        return 0
    run_id = ctx.run_id()
    ctx.lock.preempt(run_id)
    empty = False
    try:
        try:
            fleet, _ = ctx.fleet()
        except ValueError:
            fleet = None  # malformed: still destroy and sweep
        empty = _down(ctx, run_id, fleet, _tolerant(ctx, run_id))
    except LockLost:
        ctx.report("owner's down: the fleet lock was lost; dispatch a new run")
        return 1
    except LedgerUnreadable:
        empty = True
        ctx.report("owner's down: done, but the spend ledger is unreadable; escalate")
    except Exception as e:  # noqa: BLE001 - the marker stays; a person looks
        ctx.report(f"owner's down: unexpected error ({type(e).__name__}); escalate")
        return 1
    finally:
        if empty:  # the marker clears only once nothing tagged is left
            ctx.release(run_id, "owner's down")
    if not empty:
        ctx.report("owner's down: tagged resources remain after destroy; escalate")
    else:
        would = "" if ctx.live else "would "
        ctx.report(f"owner's down: {would}destroy the fleet" if fleet else "owner's down: no fleet recorded; nothing tagged is left")
    ctx.status()
    return 0


def _delivery_command(ctx: Ctx) -> bool:
    """The comment again, through the same gates: only `up` and `run renew-vpn`
    chain into deliver-pia."""
    try:
        cmd = gate(ctx.env, ctx.limits)
    except (Gated, Refused):
        return False
    return cmd.verb == "up" or (cmd.verb == "run" and cmd.step == "renew-vpn")


def cmd_pia_fence(ctx: Ctx) -> int:
    """Just before deliver-pia (which can't reach the control bucket): this run
    still holds the fleet lock, and the fleet is readable, recorded and not
    overdue or mismatched. Fails the job otherwise, so deliver-pia is skipped."""
    if ctx.env.get("GITHUB_RUN_ATTEMPT") != "1" or not _delivery_command(ctx):
        print("pia-fence: refused")
        return 1
    if not ctx.live:
        print("pia-fence: would check that this run still holds the fleet lock")
        return 0
    run_id = ctx.run_id()
    try:
        ctx.lock.fence(run_id)
        fleet, unknown = ctx.fleet()
        if unknown or fleet is None or only_down(fleet, ctx.backend.mismatch(), ctx.clock()):
            ctx.report("deliver-pia: refused: the fleet is unreadable, gone, overdue or mismatched")
            return 1
    except LockLost:
        ctx.report("deliver-pia: refused: this run no longer holds the fleet lock; nothing delivered")
        return 1
    except ValueError:
        ctx.report("deliver-pia: refused: fleet.json is invalid; escalate")
        return 1
    print("pia-fence: this run holds the fleet lock")
    return 0


def cmd_pia_release(ctx: Ctx) -> int:
    """After deliver-pia, whatever it did: a conditional delete that succeeds
    only while the lock still holds this run's ID."""
    if ctx.env.get("GITHUB_RUN_ATTEMPT") != "1":
        print("pia-release: refused")
        return 1
    if not ctx.live:
        print("pia-release: would release this run's fleet lock")
        return 0
    run_id = ctx.run_id()
    try:
        released = ctx.lock.release(run_id)
    except Exception:  # noqa: BLE001 - the lock then expires on its own
        ctx.report("pia-release: couldn't release the fleet lock; it expires on its own; escalate")
        return 1
    print("pia-release: released" if released else "pia-release: not this run's lock any more; nothing to release")
    ctx.status()
    return 0


COMMANDS = {"gate": cmd_gate, "refuse": cmd_refuse, "act": cmd_act, "reap": cmd_reap, "dispatch-down": cmd_dispatch_down,
            "pia-fence": cmd_pia_fence, "pia-release": cmd_pia_release}


def main(argv=None, env=None, backend=None, store=None, transport=None, clock=None, sleep=None, repo=".") -> int:
    argv = sys.argv[1:] if argv is None else argv
    env = dict(os.environ) if env is None else env
    if len(argv) != 1 or argv[0] not in COMMANDS:
        print("usage: fleet.py " + "|".join(COMMANDS), file=sys.stderr)
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
