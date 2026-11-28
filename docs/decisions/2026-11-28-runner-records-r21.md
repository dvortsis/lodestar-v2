# Runner records r21 (2026-11-28)

This file records the runner's calls since r20 (#1156) that no merged PR's own decisions file holds. It also aligns CR-SPEC-R1's text with the tools #1158 merged, before the rehearsal. Each call is the runner's, under §2.26 and its reading of §2.85–§2.87. Every one is listed under "Flags for the owner (§2.19)" at the end, and the owner can veto any of them.

## Already recorded in a merged PR's decisions file
- **`docs/decisions/2026-11-28-X-leaktest-vpn-held-down.md` (#1157):**
  - the cause: gluetun's startup health check fails with the endpoint blocked and restarts the VPN about every 6 s;
  - holding gluetun with `docker pause` on phase-B retries, for the probes that test kernel state;
  - `dns_blocked_ifdown` never held, and run last;
  - once one probe needs a held retry, the later kernel-state probes start held (`hold_from_first`);
  - held `ipv4_blocked` sent to an address looked up while the tunnel was up;
  - an attempt that never tried to send never counting as clean, live or held, including an unroutable address;
  - the fail-closed paths.
- **`docs/decisions/2026-11-28-X-cr-round1-spec-fixes.md` (#1158):**
  - the gateway check for PIA's /32 configs: not a local address, routed through `wg0`, equal to the peer or `via` where one exists, otherwise answering a TTL-1 echo itself;
  - a day with `unknown_event` can't be repeated;
  - a peer's missing declaration check takes the day;
  - `handshake_evidence_missing`;
  - `rehearsal_drop:<arm>` counts only on the round's first attempt;
  - a failed gateway re-check clears the earlier record.

## Rulings recorded here
- **CR-SPEC-R1 follows the tools' tokens.** §5 and the run sheet now write the drop as `rehearsal_drop:<arm>`, one token per dropped arm, and list every `end-attempt` form. The code was right and the spec's bare `rehearsal_drop` was not; no rule changes.
- **X-healthz-cold-start (#1159, open):**
  - Remote workers get no lease until the have-filter is filled, so they don't fetch stored keys during the fill. `crates/pipeline/src/remote.rs` is granted for that change only.
  - Its owns also take the files the new event needs: the logs catalog golden and the web event names and messages.
- **CMP-G is launched** (the companion gate tests). The Track Q fixes stay with the owner-started reviewer session (§2.71), so the runner doesn't launch them. CMP-G owns its decisions file.
- **The 10M import re-run waits.** It runs on the box only after round 1's day 2, so it doesn't contend with the comparator, the rehearsal or the round-1 days.

## Flags for the owner (§2.19)
1. The leak test's held retries (#1157): a hold doesn't change what a kernel-state probe proves, and gluetun's own DNS forwarder is still tested live.
2. The leak test's "never tried to send is never clean" guard, which also closes a gap on `main` for live attempts (#1157).
3. The gateway check's first-hop test, where no peer address or `via` can be read (#1158).
4. Three stricter round-1 rules beyond the brief: `unknown_event` days can't be repeated, a peer's missing declaration check takes the day, and `handshake_evidence_missing` (#1158).
5. Remote workers wait for the have-filter fill (X-healthz-cold-start).
6. The 10M import re-run is held until round 1's day 2 has ended.
