# runner-records-r15 decisions

The runner's records PR after r14 (#1084). It records:
- the runner's calls from 2026-11-23T01:00Z to 03:00Z for #1081, #1085 and #1086;
- the cloud's stage 1 as it started;
- the calls at the step 0 of CR-SOFT-TIMEOUT and X-fleet-baab;
- the new slice X-fleet-baab.

There are no new owner decisions: everything here is the runner's, under the owner's 2026-11-23 delegation (owner-decisions §2.82) and §2.26 (performance and UX). Each call is flagged to the owner, who can veto it (§2.19). The slices' own decisions files hold the detail.

## The cloud's stage 1 (X-test-vm, 2026-11-23)
- **Started 2026-11-23T01:02Z** on pinned main `0b736fd6`, with three lanes: CR-AB-ADM, CR-AB-4's uTP arm, and an A/A lane of the shipped defaults. Each lane has its own instance and IP, as r14 planned.
- **Spend.**
  - Billing shows credits applied but not the remaining balance. So the stop stays at 90% of the owner's stated figure: $120.35 of new AWS spend from 00:31Z.
  - X-test-vm's worst-case estimate for stage 1 is about $41, which fits under §2.74's $100 cap as well, so the reading of "credit" doesn't change what runs.
- **X-test-vm's calls, accepted by the runner:**
  - **2 h legs:** ABBA, then BAAB, about 38 h per lane. `start-block.sh` takes 2 or 3 h segments, inside the 48 h guard.
  - **The instance expiry for this stage is launch + 44 h.** That loosens the runbook's 24 h rule for Linux instances for this stage only, because a stage that ends early wastes its first block. The 48 h guard is unchanged. Any later stage's expiry is the runner's call for that stage, recorded with it; nothing is built in (X-fleet-baab leaves it out).
  - **Block 2 (BAAB) runs from a recorded local patch** to the fleet's arm order, because the fleet can only run ABBA. X-fleet-baab replaces the patch.
  - **The Linux instance limit goes from 4 to 7**, X-test-vm's own setting, within the spend stop.
  - **The route backoff arm runs only if** the uTP lane's first A leg shows IPv6 no-route (decisions/2026-11-02-CR-4.md).
- **A/A on the fleet.** analyze_cr derives its arms from the recorded switches, so an A/A lane, whose arms are identical, is refused. X-test-vm fits it by hand with analyze_cr's own `fit_effect` on the design labels until X-fleet-baab adds the mode.
- **gluetun is recreated** by `start-block.sh`'s leak test, so the VPN exit can change between blocks. X-test-vm records whether it stayed the same.
- **Expected end:** about 2026-11-24T14:00Z to 17:00Z. Results reach the runner as numbers only.

## Calls in running slices
- **CR-SOFT-TIMEOUT (step 0, go 2026-11-23T02:4xZ). The design:**
  - **Both mechanisms.** A query unanswered after the soft timeout becomes Slow. A Slow query frees its `ALPHA` lane, so the next candidate is queried, and it no longer blocks the closest-set end.
  - **At most `ALPHA` Slow queries per leg.** So a leg has at most 2·`ALPHA` queries outstanding. A Slow query still counts against the global outstanding cap, the rate and the 64-query cap until it answers or reaches `query_timeout`, so none of those rises.
    - This is the runner's refinement. Without it, a leg whose candidates are all slow could keep opening lanes until `query_timeout`. It's flagged because it's next to the owner's bounds against slow peers, though it loosens none of them.
  - **Late replies settle once.** A Slow query that answers after its leg ended is held as `Purpose::Detached`, settles once, and adds nothing to a finished lookup.
  - **`SOFT_TIMEOUT` is 1 s, checked on the tick,** and a 2 s variant is screened too. The choice follows the standing rule (stored per slot-second and per query both rise), then the ratio of late replies to soft timeouts. If neither value passes, it merges with the switch off.
  - **Two counters:** `dht_soft_timeouts_total` and `dht_soft_timeout_late_replies_total`. They're additive in `docs/metrics.md`, with entries in `crates/lodestar/src/metrics.rs` and its parity test. No dashboard edit.
  - **Owns refined:** `crates/lodestar/src/metrics.rs` (the two entries and the parity test) and `docs/metrics.md` (the two rows).
- **Two API-lane slices wait for CR-SOFT-TIMEOUT:** X-sampler-streak-metrics and X-capacity-rejection-source. They share `crates/lodestar/src/metrics.rs` and `docs/metrics.md` with it (X-capacity-rejection-source also shares `actor.rs` and `mod.rs`). Their after-entries are in runner-state.
- **X-detail-attributes no longer waits:** #1081 merged.
- **X-skip-results-flake (#1085), review round 1:**
  - **Every barrier is a barrier.** A pointer press, a key the page handled (`defaultPrevented`), and Enter or Space on a native control all clear the buffer. The letters typed after Ctrl+K or Cmd+K before the palette opens aren't replayed as shortcuts.
  - The import chain gets a `.catch`, so a failed chunk stops the buffering instead of holding keys for good.
  - The list of native controls is copied into `Shell.svelte` rather than exported, because exporting it moved a shared chunk and cost Insights overview 3 B.
- **X-fleet-baab (step 0, go 2026-11-23T02:5xZ):**
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
  - **What ran prettier over `docs/decisions/` in #1080:** the session ran `prettier --write src e2e ../docs/decisions` by hand. **No guard:** the repo's own scripts never reach `docs/`, and the restore was caught in review. Briefs for UI slices now say to format only through `npm run format` or `npm run check`.

## A note for every UI slice
A bare word "divider" anywhere in a scanned source file, a code comment included, makes Tailwind emit DaisyUI's `.divider` into the global CSS (+162 B, over its limit, which takes no raise). Use another word. UI briefs carry the note.

## New slice
- **X-fleet-baab** (testing tooling, Opus, a fresh Opus review): BAAB blocks and an A/A mode in the fleet tooling and analyze_cr (above). It runs in slot 3 now and is needed before the experiment host's block 2. It never runs beside X-host-ab-runsheet; the runner orders the two.

## Follow-ups dropped
- **X-test-vm-tighten,** proposed at 00:59Z for BAAB and a per-stage expiry setting: BAAB became X-fleet-baab, and a stage's expiry stays a runner call recorded per stage.
