# X-cr-round1-spec-fixes: the round-1 tools enforce what CR-SPEC-R1's review found

**From:** the runner, 2026-11-28T00:46Z. **Model:** Opus. **Branch:** `slice/X-cr-round1-spec-fixes`. **PR title:** `X-cr-round1-spec-fixes: Round-1 tools bound solo attempts, allow plan repeats, check the probe target`.

This replaces the earlier X-rehearsal-window brief; item 1 is that work.

**Why.** The fresh review of CR-SPEC-R1 (dvortsis/lodestar-v2 PR 1155) found places where the round-1 tools can't do what plan §6.2–§6.3 (`docs/plan/2026-11-27-revised-path-to-100.md`, lines 383–480, owner-approved O1–O4) requires. The runner ruled on each. CR-SPEC-R1 cites this slice for them.

**Deadlines.**
- Items 1, 4 and 6 are needed before the rehearsal, which starts after about 2026-11-28T18:00Z.
- Items 2, 3 and 5 are needed before day 1, at or after about 2026-11-29T06:00Z.
- Aim for one PR, ready by 2026-11-28T14:00Z. If that's tight, tell the runner by 11:00Z; it may split items 2, 3 and 5 out.

**The changes**
1. **The rehearsal window** (`infra/fleet/steps/vm/round1-coordinator.py`, around 616–617).
   - The rehearsal's collectors run with `--warmup-minutes 30 --baseline-hours 3`. Today it's `--baseline-hours 2` with comb's default 60 min warm-up (collect.py `SPEC_WARMUP_MINUTES`, around 1787–1789).
   - Measurement days keep the 60 min warm-up. Test that a non-rehearsal argv has no `--warmup-minutes`.
   - Compute the worst-case end from the coordinator's stagger. If it can pass 4 h from the first start, `watch` stops every arm at the cap. Test it.
   - Fix the comment "the 2 h rehearsal".
   - If collect.py refuses a shorter warm-up for a spec kind even with `--rehearsal`, ask the runner first. A collect.py change would be granted for the rehearsal only.
2. **Bounded solo attempts** (analyze_cr.py `check_declaration` around 1906–1921, and `judge_candidate`). Today any number of one-arm S attempts that are subsets of the chain are accepted, so S becomes an unbudgeted retry. The reviewer's probe showed B passing after L3 void → S void → S.
   - An S attempt is allowed in only two cases:
     - (a) its arm's candidate had `solo_confirmation_required` in the attempt just before it;
     - (b) its arm is the only arm left. Every other arm of the preceding attempt has dropped, either at the rehearsal or by a class 2 product failure. Plan §6.3 covers both: "S if both are gone", and "the other candidate continues in a fresh attempt with the remaining arms". The drop shows in that attempt's `attempt_ends` `reason`. Define the reason values it accepts, at least `rehearsal_drop` and `product_failure:<arm>`, and have the coordinator's `end-attempt` take them.
   - Each candidate gets at most one S attempt per round.
   - Anything else makes the candidate `not_judged` with a new reason, `solo_not_permitted`.
   - L2, L2b and L2c stay bounded by the shrinking chain, unchanged.
   - Add tests, including the reviewer's case.
3. **Plan repeats** (analyze_cr.py around 2420–2424, `repeat_without_infrastructure_fault`).
   - Plan §6.3 sends a coverage miss to "the retry rule", and the budget counts repeated days "whatever the cause". Today a day that is `not_judged` with no infrastructure event cannot be repeated, and the candidate stays `pending_retry` for good. Examples are coverage under 98 %, `uplink_counter_reset` and a void segment.
   - **Runner ruling:** a repeat may replace any `not_judged` day, except these:
     - (i) one with `solo_confirmation_required` (→ S);
     - (ii) one whose not_judged comes from a product failure, its own or a peer's (→ a fresh attempt, plan class 2).
   - Name the second reason `repeat_of_a_non_repeatable_day`, replacing `repeat_without_infrastructure_fault`.
   - The budget (`RETRY_BUDGET` 2) and `repeat_of_a_judged_day` are unchanged.
   - Add tests: a coverage miss repeated and then passing within the budget; a repeat of a product-failure day refused; a third repeat exhausting the budget.
4. **The probe target is the WireGuard gateway** (coordinator config check around 222–228, and `bring-up`).
   - The plan says the probe is "ICMP to its WireGuard gateway". Today `tunnel_target` is any IPv4 address, so an address inside the namespace would never show loss.
   - At bring-up, read the arm's WireGuard gateway from inside its namespace, the same way bring-up already reads egress. Use the tunnel interface's route or peer address, whichever gluetun exposes.
   - Refuse unless `tunnel_target` equals the gateway.
   - Record `tunnel_target_is_gateway: true` per arm in run state and the day archive. The repo and archive must never hold the address itself: no IPs in the repo.
   - gate-round makes a day `not_judged` (`probe_target_unchecked`) for any arm whose archive lacks it.
   - Add tests with a fake namespace reader.
5. **Declaration checks present** (analyze_cr.py gate-round, run sheet 8b).
   - Today, when the declaration entry isn't found, none of the `declared_*`, `effective_switches_recorded` or `*_stayed_declared` checks are emitted, and nothing fails.
   - gate-round requires `declaration_entry_found` ok and every named check present and ok. Anything missing makes the day `not_judged` (`declaration_checks_missing`).
   - Add a test.
6. **Gluetun logs in the archive** (coordinator `archive`, around 874–910, and `manifest`).
   - Handshake events (plan class 1, "a tunnel without a handshake for more than 180 s") rest on gluetun's logs. Today those aren't archived, so a classification can't be checked.
   - At `archive`, save each arm's gluetun log for the day, from its start to the shutdown, into the day folder as `<arm>/gluetun.log`.
   - Redact IPv4 and IPv6 addresses and the WireGuard keys. Test the redaction.
   - List each log with its SHA-256 in the manifest, so the runner's event classification can be checked against them.
7. **Decisions file:** `docs/decisions/2026-11-28-X-cr-round1-spec-fixes.md`. Give one short section per item, citing the CR-SPEC-R1 review.

**Owns:**
- `infra/fleet/steps/vm/round1-coordinator.py`;
- `docs/soak/tools/analyze_cr.py`;
- `docs/soak/tools/test_gate_round.py`;
- `docs/soak/tools/linux/test_collect_round1_coordinator.py`;
- `docs/soak/tools/linux/test_collect_round1_e2e.py`;
- `docs/soak/tools/fixtures/round1/**`;
- `infra/fleet/tests/test_round1_arms.py` (only if needed);
- the decisions file.

**Leave alone:**
- `docs/decisions/2026-11-27-CR-SPEC-round1.md` and the run sheet: the runner's PR 1155 owns both and cites your item names;
- `collect.py`: ask first;
- `runner/`: the runner adds your entry.

**Checks before `ready`:**
- `python3 -m unittest discover -s docs/soak/tools -p 'test_*.py'`;
- the linux tests;
- `infra/fleet` tests the way `run_tests.sh` runs them;
- the round-1 golden reproduces, or is regenerated with the reason stated;
- stage-1 reports unchanged;
- no fixed sleeps.

Follow `.claude/skills/steward/SKILL.md`. Send `ready <PR> <sha>` to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg). A fresh Opus review follows; false passes are the focus.

**Size:** about 400–600 lines, mostly tests.
