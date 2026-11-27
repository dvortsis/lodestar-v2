# runner-records-r19 decisions

The runner's records PR after r18 (#1130). It records:
- the owner's 2026-11-26 decisions in the "Path to 100" session, as owner-decisions §2.85: performance first, a VPN on every test crawl, the AWS budget, D13 B and D1 to D12 of `docs/plan/2026-11-26-fastest-1.0-proposal.md`;
- the owner's 2026-11-27 answers to O1 to O4 of `docs/plan/2026-11-27-revised-path-to-100.md` (revision 6), as §2.86;
- the owner's 2026-11-27 answers to the runner's two questions (the AWS stop and the Legal Notice's safe-restart line), as §2.87;
- both plans, brought to `main` from their branches with only their Status lines changed, and the cloud-screen evidence the 11-27 plan cites (`docs/plan/evidence/2026-11-24-cloud-screens/`);
- build-plan r6: the 11-26 proposal is r6, and the 11-27 plan amends its crawler sequence (`docs/build-plan.md` header, §0.2 rung 4, §0.5 and §0.10);
- six new slice entries, the CR-COMB change, notes on six deferred slices, and the dashboard's crawler path.

The runner checked each owner message against the source session's own events. Everything else here is the runner's, under §2.82, §2.84 and §2.26 (performance and UX). Each call is flagged to the owner, who can veto it (§2.19). The refinements of earlier decisions are marked **(§2.19)**.

## What the owner decided
- **§2.85 (2026-11-26):** performance first and politeness second, with the safety rules unchanged; WireGuard with leak protection on every test crawl; an optional VPN in the product that works with any provider; the AWS Budget "Lodestar Path to 100" ($295 gross, alerts only); D13 B; D1 to D12 as recommended. Not approved: cloud results as evidence of record, and 44 h cloud instances.
- **§2.86 (2026-11-27):** O1 to O4 yes. Round 1 (CR-R1) replaces CR-AB-ADM, CR-AB-4, CR-CP1, CR-AB-LC, CR-AB-1 and CR-5 before CR-COMB. Per-switch attribution moves to 1.1. The round-1 report replaces Checkpoint 1.
- **§2.87 (2026-11-27):** the AWS stop is $295, as the AWS Budget measures it, replacing $117.50. The Legal Notice's safe-restart line is corrected, with a NOTICE_VERSION bump.

## The plans
- **`docs/plan/2026-11-26-fastest-1.0-proposal.md`** (from `claude/amazing-darwin-vzxlgw`, 6c8b7cf27): its Status line now cites §2.85, names it build-plan r6 and says its §4 crawler sequence is replaced by the 11-27 plan. The branch's other change, to `infra/dev-vm/pia-login.sh`, isn't part of this record and isn't brought in.
- **`docs/plan/2026-11-27-revised-path-to-100.md`** (from `claude/gifted-feynman-siwhjc`, 4ee28954e): its Status line now cites §2.86 and names CR-SPEC-R1 as the file that changes CR-SPEC for round 1.
- **The cloud-screen evidence:** one local path in `tbase.md` that named a session's working folder is withheld. Nothing else changed.

## The runner's calls (§2.19)
- **CR-0w (#1134) merged on 2026-11-27,** as the 11-27 plan's §9 item 3 asks. Its hold existed to keep the stage-1 images' 6 h start timing (runner-state, 2026-11-27T10:30Z). Those stages no longer run before the gate, so the hold is moot.
- **The harness is three slices, not one (§2.19).** The plan's X-cr-round1-harness (§5.2) is split into X-cr-round1-collect (collector, Compose, leak test, coordinator, run sheet: items 1, 3, 4, 8 and 11) and X-cr-round1-gate (analyzer, `gate-round`, memory fit, fixtures: items 2, 5, 6, 7, 9, 10 and 12), on disjoint files, beside X-dht-switch-identity (§5.1). They run at once, so the harness can be done within the 12 h gap after the comparator's window ends (about 2026-11-29T06:00Z).
- **CR-SPEC-R1 follows X-cr-round1-gate,** because its §6.1 evidence table is checked against the analyzer's functions before it's frozen (plan §6.1).
- **CR-AB-ADM, CR-AB-4 and CR-AB-LC no longer need cloud provisional runs.** They move to 1.1 (§2.86), so §2.82's "a provisional cloud run goes first" has nothing left to precede before the gate. The optional Aggressive cloud screen (plan §9 item 5) stays blocked while the test lanes' SSM SendCommand access is denied, an AWS-side block, and until the owner has checked for an AWS quarantine policy. It never delays the box.
- **The six deferred slices stay in `runner/slices.yaml`** with a note, and are `held` on the dashboard. `ext:owner-checkpoint-1` is marked superseded, not deleted: no slice before the gate depends on it now, and the 1.1 attribution slices keep it.
- **D7 and D11 also answer owner-queue items 26, 27 and 30 (runner's reading).** Item 27 (emulation accepted) and item 30 (parallel crawler slices that only add switch lines) close. Item 26 becomes the owner's own walkthrough.
- **D3 and D4 change rules the owner set,** and the owner approved them in a blanket yes ("yes to all recommendations"). D3 changes CR-SPEC's host design: parallel arms on separate VPN exits, swapped daily, and mover runs and parity checks don't void a VM segment. D4 makes the politeness part of G-v1-audits a bounded-load check for 1.0, with the full audit in 1.1. Both are flagged so the owner sees them by name.
- **The Legal Notice correction is a new slice, X-legal-safe-restart** (§2.87). It owns `docs/legal/legal-notice.md`, `crates/api/src/legal/notice.md`, `crates/api/src/legal.rs` (the NOTICE_VERSION constant), `crates/lodestar/tests/legal_facts.rs` and its decisions file. It also owns `crates/api/src/auth.rs`, only for the test that asserts the notice version. The user guide's own safe-restart lines are D-start's.

## New slices
- **X-dht-switch-identity** (Opus): a read-only gauge of the 12 crawler switches' effective values and `safe_restart` (plan §5.1).
- **X-cr-round1-collect** (Opus): round-1 collection, after CR-0w (plan §5.2).
- **X-cr-round1-gate** (Opus): round-1 judging, `gate-round` (plan §5.2).
- **CR-SPEC-R1** (Opus, docs): the round-1 CR-SPEC decisions file (plan §8, owner O3).
- **CR-R1** (Opus, gate): round 1 on `lodestar-host`, two valid days (plan §6, owner O1 to O4).
- **X-legal-safe-restart** (Opus): the Legal Notice line and the version bump (§2.87).

## Changes to existing slices
- **CR-COMB:** depends on CR-R1, `ext:owner-cr-spec` and `ext:experiment-host`. It defaults on the round-1 qualified candidate's switches by the defaults-only transfer (plan §6.5). Its owns are unchanged.
- **CR-AB-ADM, CR-AB-4, CR-CP1, CR-AB-LC, CR-AB-1 and CR-5:** deferred to 1.1 (per-switch attribution) by owner O1. Not run before the gate.

## Owner queue
- **Closed:** items 27 and 30 (§2.85, D7 and D11), and items 35 and 36 (D12).
- **Changed:** item 26 is now an action for the owner (D7).
- **Item 41:** the calls above, open to the owner's veto.
