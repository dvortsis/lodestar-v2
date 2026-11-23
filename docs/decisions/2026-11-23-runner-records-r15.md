# runner-records-r15 decisions (#1087)

The runner's records PR after r14 (#1084). It records:
- the runner's calls from 2026-11-23T01:00Z to 03:00Z for #1081, #1085 and #1086;
- the cloud's stage 1 as it started;
- the calls at the step 0 of CR-SOFT-TIMEOUT and X-fleet-baab;
- the new slice X-fleet-baab.

There are no new owner decisions: everything here is the runner's, under the owner's 2026-11-23 delegation (owner-decisions §2.82) and §2.26 (performance and UX). Each call is flagged to the owner, who can veto it (§2.19). The slices' own decisions files hold the detail.

## The cloud's stage 1 (X-test-vm, 2026-11-23)
- **Started 2026-11-23T01:02Z** on pinned main `0b736fd6`, with three lanes: CR-AB-ADM, CR-AB-4's uTP arm, and an A/A lane of the shipped defaults. Each lane has its own instance and IP, as r14 planned.
- **Spend.**
  - Billing shows credits applied but not the remaining balance, and §2.82 asks for the balance first. **So the stop is §2.74's $100 cap until X-test-vm confirms a balance from billing.** Then it becomes 90% of the lower of the confirmed balance and the owner's stated $133.72, at most $120.35 of new AWS spend from 00:31Z.
  - X-test-vm's worst-case estimate for stage 1 is about $41, under either stop.
- **X-test-vm's calls, accepted by the runner:**
  - **2 h segments,** the shortest `start-block.sh` takes (2 or 3 h). These provisional cloud runs aren't CR-SPEC's runs of record: CR-SPEC's 6 h segments (§2.34, §2.50) still bind the experiment host's runs, which give the gain.
  - **Block 2 (BAAB) runs from a recorded local patch** to the fleet's arm order, because the fleet can only run ABBA. X-fleet-baab replaces the patch.
  - **The Linux instance limit goes from 4 to 7**, X-test-vm's own setting, within the spend stop.
  - **The route backoff arm runs only if** the uTP lane's first A leg shows IPv6 no-route (decisions/2026-11-02-CR-4.md).
- **A/A on the fleet.** analyze_cr derives its arms from the recorded switches, so an A/A lane, whose arms are identical, is refused. X-test-vm fits it by hand with analyze_cr's own `fit_effect` on the design labels until X-fleet-baab adds the mode.
- **gluetun is recreated** by `start-block.sh`'s leak test, so the VPN exit can change between blocks. X-test-vm records whether it stayed the same.
- **Expected end:** block 1 ends about 2026-11-23T19:40Z, and block 2 about 18 h after it starts. Results reach the runner as numbers only.
- **Correction: the instance expiry stays at most 24 h (the runner withdraws its acceptance of launch + 44 h).**
  - The owner approved Linux test instances on AWS on a question that said "24 h expiry tags" (decisions/2026-11-12-X-test-vm.md, 2026-11-15T05:27:38Z), and `infra/dev-vm/linux-aws.md` says `ExpiresAt` is at most 24 h ahead. An instance lifetime isn't a performance or UX number (§2.26), so the runner can't loosen it. The fleet's 44 h (`cloud-fleet-runbook.md`) is for the owner's Linode fleet, not test-VM instances.
  - **So each block runs on its own instance**, launched for that block, with `ExpiresAt` at most 24 h ahead. Block 1's instances get their tag set back to launch + 24 h, and each block ends inside it (about 18 h). Block 2 starts on fresh instances. Each block is balanced within its own instance and IP, so the effect estimate still never compares instances; only the block-to-block spread grows.
  - **Owner queue item 35** asks whether a cloud stage may keep one instance for up to 44 h, as the fleet does. Until the owner answers, the 24 h rule holds.

## Calls in running slices
- **CR-SOFT-TIMEOUT (step 0, go about 2026-11-23T02:45Z). The design:**
  - **Both mechanisms.** A query unanswered after the soft timeout becomes Slow. A Slow query frees its `ALPHA` lane, so the next candidate is queried, and it no longer blocks the closest-set end.
  - **At most `ALPHA` Slow queries per leg.** So a leg has at most 2·`ALPHA` queries outstanding. A Slow query still counts against the global outstanding cap, the rate and the 64-query cap until it answers or reaches `query_timeout`, so none of those rises.
    - This is the runner's refinement. Without it, a leg whose candidates are all slow could keep opening lanes until `query_timeout`. It's flagged because it's next to the owner's bounds against slow peers, though it loosens none of them.
    - **It amends the brief's "never raises `ALPHA`":** the constant is unchanged, but a leg's in flight can reach 2·`ALPHA`.
  - **Lookups stay within 3/4 of the outstanding cap** (added after this PR's review). With CR-1c on, Low's 32 slots × 2·`ALPHA` × 2 families would be 384 in flight against Low's cap of 256. Lookups could then fill the cap and crowd out sampling, which stops at 4/5 of it, and the 16 maintenance lookups. CR-1c's rule keeps slots × `ALPHA` × 2 families within 3/4 of the cap (decisions/2026-11-21-CR-1c.md). So a Slow query frees its lane only while the lookups' total in flight, Slow queries included, stays within 3/4 of the live preset's cap; otherwise it blocks its lane as with the switch off. A paused-clock test at Low, with CR-1c on and every slot slow, shows sampling and maintenance still send, and a mutation without the check fails it.
  - **Late replies settle once.** A Slow query that answers after its leg ended is held as `Purpose::Detached`, settles once, and adds nothing to a finished lookup.
  - **`SOFT_TIMEOUT` is 1 s, checked on the tick,** and a 2 s variant is screened too. The choice follows the standing rule (stored per slot-second and per query both rise), then the ratio of late replies to soft timeouts. If neither value passes, it merges with the switch off.
  - **Two counters:** `dht_soft_timeouts_total` and `dht_soft_timeout_late_replies_total`. They're additive in `docs/metrics.md`, with entries in `crates/lodestar/src/metrics.rs` and its parity test. No dashboard edit.
  - **Owns refined:** `crates/lodestar/src/metrics.rs` (the two entries and the parity test), `docs/metrics.md` (the two rows) and `crates/dht/src/node/preset_slots_tests.rs` (the Low test, if it lives there).
- **Two API-lane slices wait for CR-SOFT-TIMEOUT:** X-sampler-streak-metrics and X-capacity-rejection-source. They share `crates/lodestar/src/metrics.rs` and `docs/metrics.md` with it (X-capacity-rejection-source also shares `actor.rs` and `mod.rs`). Their after-entries are in runner-state.
- **X-detail-attributes no longer waits:** #1081 merged.
- **X-skip-results-flake (#1085), review round 1:**
  - **Every barrier is a barrier.** A pointer press, a key the page handled (`defaultPrevented`), and Enter or Space on a native control all clear the buffer. The letters typed after Ctrl+K or Cmd+K before the palette opens aren't replayed as shortcuts.
  - The import chain gets a `.catch`, so a failed chunk stops the buffering instead of holding keys for good.
  - The list of native controls is copied into `Shell.svelte` rather than exported, because exporting it moved a shared chunk and cost Insights overview 3 B.
- **X-fleet-baab (step 0, go about 2026-11-23T02:53Z):**
  - **BAAB** is `MODE=baab` in `block.env`. ABBA stays the absence of the line, so old blocks read as ABBA and new ABBA files are byte-identical.
  - **Every reader uses one function.** `block-lib.sh` has `arm_of`. `scrub.py` carries its own `arm_of`, because copy-off mounts that file alone. A test checks that the shell, the scrubber and analyze_cr agree.
  - **analyze_cr:**
    - `--design abba|baab`, one per `--block`. With no `--design`, the output is byte-identical, held by a golden.
    - It refuses a block whose switches don't match its design (`order_differs_from_design`).
    - `--aa` needs `--design`. It refuses a block whose arms' switches differ (`aa_switches_differ`). An A/A report says `noise_floor`, never win, loss or no effect, and checkpoint 1 and the gate refuse it.
  - **A/A on the fleet** (the runner's call). It's `"purpose": "aa"` with ABBA or BAAB: `PURPOSE=aa` in `block.env`, with the treatment's switches off in both arms and earlier winners on in both. The scrubbed evidence carries `"purpose": "aa"` only for A/A blocks. An A/A block measures the noise floor of the stage's control, never a treatment.
  - **The scrubber's redaction doesn't change.** An ABBA golden from main's `scrub.py` shows the output is otherwise byte-identical.
  - **Owns refined:** plus `infra/fleet/steps/vm/vm-status.sh` (a `block_mode_baab` line) and `infra/fleet/steps/rules.py` (its mode and purpose values and `args()` only, validation as strict as now), and new golden files under `docs/soak/tools/fixtures/`.
  - **Left out:** any expiry or instance-lifetime change, any cloud call, and any change to the redaction rules.
  - **Never beside X-fleet-sudo-narrow,** which also edits `infra/fleet/steps/**`. Idle draft #920 also edits `cloud-fleet-runbook.md`.

## Calls in merged slices
- **X-detail-depth (#1081):**
  - The review fixes were checked by the runner: the "Set up a download client…" menu item, Swarm's empty and "never scraped" states, ArrowDown before load, the back link's coarse-pointer target, and the null `from.url` guard.
  - **All 62 `/t` and drawer baselines were kept.** Every `/t` shot moves, and dropping the theme set would turn the nightly red.
  - `search-results-adaptive.routes.test.ts` waits for the Technical section before the drawer shot, so no skeleton lands in the baseline.
  - X-detail-depth-compare (PR 2) launches later in a fresh session, once X-shell-home hands back `web/.size-limit.mjs`. GS-4 waits for it.
- **X-narrow-states-2 (#1086):**
  - **The phone stepper's exception to standards §4.** Below 640 px, a step that isn't current shows its icon and number, with its name and status for screen readers only. It holds on four conditions, each checked at 320 px:
    - (a) the icons differ by shape and reach 3:1;
    - (b) the accessible name has number, name and status;
    - (c) from 640 px every step shows icon plus text;
    - (d) the current step's line isn't cut at 320 px.
    The runner checked them rendered, including dark themes (9.19:1 and 14.2:1).
  - **Owns corrected:** the alert detail list is `alerts/AlertDetailPage.svelte` (P9's file), not `AlertsListPage.svelte`.
  - **The alert list's 5 px scroll at 320 px:** a visually-hidden span escaped the row's clipping. The cells now clip, and the e2e runs at 320 and 390 px with 16 and 20 px root fonts.
  - **The mark row kept its ellipsis.** The runner rejected the round that removed it, and the bytes came from dropping an inherited `color` and a redundant `margin: 0`.
  - **No size raises.** Alert is at 46.00 / 46 kB and Welcome at 16.50 / 16.5 kB, both with no margin. The next alert-detail or wizard slice pays with removals; its brief says so.
  - **What ran prettier over `docs/decisions/` in #1080:** the session ran `prettier --write src e2e ../docs/decisions` by hand. **No guard:** the repo's own scripts never reach `docs/`, and the restore was caught in review. `runner/standards.md` §4 now says to format only with `npm run format` in `web/`.

## A note for every UI slice
A bare word "divider" anywhere in a scanned source file, a code comment included, makes Tailwind emit DaisyUI's `.divider` into the global CSS (+162 B, over its limit, which takes no raise). Use another word. `runner/standards.md` §4 now carries the note.

## New slice
- **X-fleet-baab** (testing tooling, Opus, a fresh Opus review): BAAB blocks and an A/A mode in the fleet tooling and analyze_cr (above). It runs in slot 3 now and is needed before the experiment host's block 2. It never runs beside X-host-ab-runsheet or X-fleet-sudo-narrow; the runner orders them.

## Follow-ups dropped
- **X-test-vm-tighten,** proposed at 00:59Z for BAAB and a per-stage expiry setting: BAAB became X-fleet-baab, and the expiry is the owner's (item 35).
