# X-gluetun-leak-cmp (runner brief, 2026-11-13)

Follow-up to #800 (X-gluetun-leak, merged 1b26cea4). Same session, in a tester slot (§2.64 4 + 4). Small, one PR.

## Branch and owns
- Branch `slice/X-gluetun-leak-cmp` from current `origin/main`. PR title `X-gluetun-leak-cmp: leak-test cleanup and wording follow-ups`.
- Owns: `infra/fleet/steps/vm/leak-test.sh`, `infra/fleet/tests/test_steps.py`, `docs/decisions/2026-11-12-X-gluetun-leak.md` (append a short "Follow-ups (X-gluetun-leak-cmp)" section; no new decisions file unless a decision changes), `docs/operations/cloud-fleet-runbook.md` §3.2, the one sentence in `docs/user/03-keep-lodestar-safe.md` (use the `user-docs` skill), and the comment in `packaging/companions/gluetun-post-rules.txt` (comment only, no rule change).

## Do (from reviews/rv-800-87be769b.md and rv-800-e46cb683.md)
1. O1 of rv-800-87be769b: set `host_set=1` / `guard_set=1` before each `iptables -I`, so a signal in the insert window can't leave a copy. Add the signal-path test (O5): a stub `compose` that blocks, the test sends TERM, then asserts 0 copies of both guards. Waits are on conditions with a deadline of at least 10 s, never fixed sleeps.
2. O2: `drop_all` treats only `-C` exit 1 as absent and returns 1 on any other exit. Report `recreate_guard_held` when `guard_set` is still on, not as a credentials problem. Add a test for a transient `-C` error.
3. O3: `cleanup` reports a host guard it can't remove (exit 1 from the EXIT trap), with a test. If you judge that too strict, say instead in the decisions file that the removal is best effort; the runner prefers reporting.
4. O4 docs:
   - add the IPv4-only reason;
   - correct "cleared at the start" to the same-endpoint scope, or remove by tag (`--comment lodestar-leak-`), which the runner prefers;
   - add the host-stack path for a user's own gluetun restart to the cmp notes;
   - reword the user-guide sentence so it's true, for example "Nothing Lodestar or gluetun sends leaves outside the VPN";
   - runbook §3.2: move `container_mac` to its own bullet, and say `post_rules_before_egress` without `recreate_guard_held` is a stop too.
5. O6: call `post_rules_on` instead of the inlined copy.
6. The multicast comment in `gluetun-post-rules.txt` (O3 of rv-800-e46cb683): comment only. Any rule change needs a new runner call first.
7. Re-check the post-rules after phase B's reconnect (the session's own note). A missing rule fails the run, as before egress.
8. Optional, if small: the old O5 (`git cat-file -e <pin>:packaging/companions/gluetun-post-rules.txt`) and old O7 (`-Q in`).

## Not in this slice
- The template items (compose `depends_on: gluetun: condition: service_healthy` / Unraid autostart wait, `VPN_INTERFACE` per VPN type, the post-rules file delivery, the gluetun Overview overclaim, `HEALTH_SMALL_CHECK_TYPE=dns` in troubleshooting) go to a separate runner slice, X-gluetun-templates. Don't edit `packaging/` templates or docs/user beyond the one sentence.

## Don't
- **Don't touch either test VM.** X-test-vm's Main and Lane K windows run until about 15:01Z; nobody touches the VMs until then. No cloud calls, no terraform, never read AWS_* variables. Unit tests only.
- Don't record IPs, firewall details or provider account data.
- No owner-set number changes.

## Checks before push
- `python3 -m pytest infra/fleet/tests/test_steps.py -q`, `shellcheck infra/fleet/steps/vm/leak-test.sh`, plus whatever CI runs for `infra/fleet`. Push once, then follow the steward skill and send the runner `ready <pr> <full sha>`.
- After merge, the VM re-run of the leak test on the new head goes through X-test-vm, once its windows end. Say in the PR which head needs it.
