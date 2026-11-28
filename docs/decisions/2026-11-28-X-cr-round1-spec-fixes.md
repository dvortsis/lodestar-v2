# X-cr-round1-spec-fixes decisions

The round-1 tools enforce what the fresh review of CR-SPEC-R1 (#1155) found, as the runner ruled on each finding (brief of 2026-11-28T00:46Z). `docs/decisions/2026-11-27-CR-SPEC-round1.md` (§5, §6, §7, §8) and `docs/soak/runsheets/host-round1-runsheet.md` cite this slice for each rule. Plan: `docs/plan/2026-11-27-revised-path-to-100.md` §6.2–§6.3 (owner O1–O4). Where this file and `docs/decisions/2026-11-27-X-cr-round1-gate.md` differ, this file wins.

## 1. The rehearsal's window (CR-SPEC-R1 §6; run sheet §5 step 6)
- **The rehearsal's collectors run `--rehearsal --warmup-minutes 30 --baseline-hours 3`.** It was `--baseline-hours 2` with rule 1.1's 60 min warm-up. collect.py already allows another warm-up under `--rehearsal` (its spec-kind check, collect.py:2004), so collect.py is unchanged.
- **Measurement days pass no `--warmup-minutes`,** so rule 1.1's 60 min applies. A test checks the argv.
- **The 4 h cap is enforced by `watch`.** `up` records each arm's `up_utc` and `collect` each collector's `collector_started_utc`. On a rehearsal day, `watch` stops every arm and writes the archive once 4 h have passed from the earliest of them, with a note. It wakes at the cap rather than up to a minute after.
- **`collect` prints the worst-case end against the cap:** the last collector's start, plus its 60 s launch allowance, the 30 min warm-up, the 3 h window and the 2 min overhang. It prints `warn` when that's past the cap. This is only an estimate: each collector's warm-up waits for its own process, so the cap in `watch` is what holds.

## 2. Bounded solo attempts (CR-SPEC-R1 §5)
- **An S attempt of a candidate is allowed only:**
  - (a) when the attempt just before it, in the declaration's order, gave that candidate a day with `solo_confirmation_required`; or
  - (b) when the candidate is the only arm left. Every other arm of that attempt must be named as dropped in its recorded end (the manifest's `attempt_ends` `reason`).
- **Each candidate gets at most one S attempt per round.**
- **Anything else adds `solo_not_permitted`.** The verdict's `solo_problems` says which rule failed: `solo_without_a_preceding_attempt`, `solo_not_after_a_drop` or `more_than_one_solo_attempt`.
- **A valid miss still stands beside `solo_not_permitted`** (CR-SPEC-R1 §5, "A valid miss stands"). The reason makes the candidate `not_judged` unless a judged `fail` exists, in which case it's `fail`. It isn't added to the reasons that override a miss, so a misused S can't erase one.
- **The reasons end-attempt takes** are comma-separated tokens:
  - `rehearsal_drop:<arm>`, only for an attempt that ran no day;
  - `product_failure:<arm>`;
  - `solo_confirmation:<arm>`;
  - `valid_miss:<arm>`;
  - `other:<slug>` for anything else the runner rules.

  The arm must be one of the attempt's arms. Free text is refused.
- **gate-round counts only two tokens as drops:**
  - `rehearsal_drop:<arm>`, when `days_run` is `[]`;
  - `product_failure:<arm>`, when a day archive of that attempt has a product event on that arm that wasn't reclassified as infrastructure.

  A claimed drop that the evidence doesn't show allows no S.
- **`check_declaration` is unchanged** (L2, L2b and L2c stay bounded by the shrinking chain). The S rules need the days' verdicts and the attempt ends, so they're judged in `judge_candidate`.
- **The tests cover the reviewer's probe:** L3 void, then S void, then S never passes B.

## 3. What a repeat may replace (CR-SPEC-R1 §5)
- **A repeat may replace any `not_judged` day, except:**
  - one with `solo_confirmation_required`, which goes to S;
  - one with `solo_confirmation_failed`, which is terminal;
  - one with a product reason: `product_failure`, `product_failure_under_diagnosis` or `peer_product_failure`.
  - Replacing one of these is `repeat_of_a_non_repeatable_day`, which replaces `repeat_without_infrastructure_fault`.
- **Refinement (runner to confirm, flagged per §2.19): a day with `unknown_event` is not repeatable either.** An unclassified collector close may be a product failure, and a repeat would let the day pass without the runner classing it. Once the runner classes the event, the day's reasons say what it is.
- **Replaced days stay in `every`,** so a failed solo or a valid miss in a replaced day still counts.
- **`RETRY_BUDGET` (2) and `repeat_of_a_judged_day` are unchanged.** Examples that can now be repeated: coverage under 98 %, `uplink_counter_reset`, a void segment.

## 4. The probe target is the WireGuard gateway (CR-SPEC-R1 §7; plan line 441)
- **`bring-up` checks each arm's `tunnel_target` inside its gluetun namespace** (`nsenter --net` with the host's `ip` and `ping`) and refuses a mismatch. The checks:
  - The target is none of the namespace's own addresses.
  - `ip route get` sends it through the tunnel interface (`wg0`, or the arm's `tunnel_interface`), and not as `local`.
  - It equals the tunnel's peer address, or every `via` on the interface's routes, where gluetun sets one.
- **Departure from the brief, which said to read the route or peer address "whichever gluetun exposes":** round 1's PIA config is a point-to-point WireGuard `/32` (`infra/pia-bridge/vm/pia-wireguard.sh`). gluetun sets that up with neither a peer address nor a `via`. So when neither exists, the gateway is the tunnel's first hop: the target must answer an echo sent with TTL 1 itself. An address further away answers with the gateway's `Time to live exceeded` and is refused.
- **Run state and the day archive record `tunnel_target_is_gateway: true` per arm,** plus how it was checked (`tunnel_gateway_check`: `peer_address`, `route_via` or `first_hop`), never the address. gate-round makes a day `not_judged` (`probe_target_unchecked`) for any declared arm whose archive lacks it.

## 5. Declaration checks present (CR-SPEC-R1 §6; run sheet 8b)
- **gate-round requires these checks present and ok in every declared arm's report of the day:**
  - `declaration_entry_found`;
  - `effective_switches_recorded`;
  - `declared_preset`, `declared_switches`, `declared_image_digest`;
  - `safe_restart_on`;
  - `preset_stayed_declared`, `switches_stayed_declared`.
- **The rule covers peers' reports too.** A peer that didn't run its declaration breaks the layout.
- **Anything missing or not ok is `declaration_checks_missing`.** For the candidate, a failed check already shows as `segment_void` or `segment_invalid`.
- **The synthetic fixture's reports now carry these checks,** and its archives the gateway flag. The golden `l3_pass_verdict.json` was regenerated for that reason only: only its input hashes changed.

## 6. Gluetun logs in the archive (CR-SPEC-R1 §8)
- **`archive` saves each arm's gluetun log, redacted, as `<arm>/gluetun.log`.** The log runs from the arm's `up_utc` (the day's first exit check if `up` didn't record one) to `shutdown_utc`. It's read with `docker logs --timestamps`, stdout and stderr in order.
- **What's redacted:**
  - Every IPv6 address is replaced by `[ipv6]`. A token counts only if it parses as IPv6, so times such as `10:00:00` are kept.
  - Every IPv4 address is replaced by `[ipv4]`.
  - Every WireGuard key (32 bytes, base64) is replaced by `[key]`.
- **The archive records each log's SHA-256,** and an unreadable log is noted once.
- **`manifest` lists each day's logs with their SHA-256 (`gluetun_logs`)** and refuses a day without them. `load_manifest` checks those hashes and lists them in the verdict's `inputs`, so the runner's event classification can be checked against them. The gate itself doesn't read the logs.
