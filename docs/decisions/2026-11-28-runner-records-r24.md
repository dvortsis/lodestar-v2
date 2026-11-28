# runner-records-r24 decisions (#1187)

This file records the owner's answers of 2026-11-28 (owner-decisions §2.89 to §2.93), the D1 rewiring of `runner/slices.yaml` and `runner/dashboard.yaml`, and the runner's calls since r23 (#1170). The rulings are the runner's, under §2.26 and its reading of §2.85 to §2.93, except where they apply an owner answer. A ruling that only grants owns or records a slice isn't flagged. The owner can veto any flagged call.

## The owner's answers
- **§2.89, the night rule.** No CPU load tests at night; soaks may run unsupervised. The runner reads "night" as 06:00Z–15:00Z (05:00Z–14:00Z from 2027-03-14T09:00Z to 2027-11-07T08:00Z). The test VM shares the owner's uplink ("Yes, same internet connection"), so the X-test-vm Aggressive screen is barred at night and during every measured window: the comparator, the rehearsal, CR-R1 days 1 and 2, and the drills.
- **§2.90, the Windows VPN binding: "Yes".** One small Windows-only `unsafe` `setsockopt` (`IP_UNICAST_IF` / `IPV6_UNICAST_IF`) in `crates/network-policy`, like the store exception (§2.35). The new slice X-vpn-bind-win needs a fresh Opus review and a `/security-review`.
- **§2.91, six slots: "Yes, increase to 6 slots".** `limits.phase_total` goes from 4 to 6 in `runner/slices.yaml`, with a comment. The runner's speed-up commitments don't change the bar:
  - the review starts when the PR opens, beside CI;
  - worst cases are measured before a ruling;
  - X-ci-test-shard splits the slow test jobs;
  - M8-DR is split, with M8-DR-a reviewing the final screens early.
- **§2.92, the ongoing review.** It runs every 6 h. Its rules are on `runner-state` in `reviews/throughput.md`. It also updates the dashboard's plan and estimates, as the owner asked at 19:45Z.
- **§2.93, D1 approved, and v0.5.2 isn't published.** The D1 list is below. There is no public v0.5.2, and the next public release is 1.0. `release/v0.5.2` stays untagged, and the "Allow workers" issue on that branch is moot.
- **Owner queue item 44 is done.** The owner updated the test-VM stack to the reviewed template through a change set: only the policy was modified, with no replacement. The item leaves the queue.
- **The comparator, cr-ab-comp-2, is done** (owner-run on `lodestar-host`, confirmed in the runner session at about 19:45Z). The analyzer marked its report valid. CR-AB-COMP shows as done through `state_override`, and the crawler list, the roadmap's comparator step, the `experiment-host` condition and the crawler note carry the result.
  - **Window:** 2026-11-27T17:00:06Z to 2026-11-28T18:00:52Z, 1,440.51 measured minutes, ended by `baseline_window_complete`.
  - **Result:** 456,140 torrents stored, a mean of 316.65 a minute. The best hour was 387.60 a minute (hour 0), the worst 281.46 (hour 10). The count rose every hour.
  - **Checks:** all 19 passed, including NTP sync, scrape gaps, no host activity, an unchanged public IP and a digest-pinned image. The peak clock drift was 0.077 s.
  - **Next:** the next Lodestar start is the rehearsal, then CR-R1 day 1. Day 1 starts no earlier than 2026-11-29T06:00:52Z, the window's end plus 12 h.

## The D1 rewiring, node by node
**Pattern.** This follows r19's CR moves under §2.86. Each moved slice stays in `runner/slices.yaml` with a note ("moved to 1.1 by the owner's D1 … runner records r24") and is `held` on the dashboard. Its dependants on the 1.0 path stop waiting on it. The moved slices are grouped in a new roadmap step, "Moved to 1.1 (your D1)", after the 1.0 step, as "Per-switch attribution (1.1)" follows the crawler gate.

**Moved to 1.1 (held, with a note):**
- the v0.7 tails: OUT-1, OUT-3, UN-3, UN-4, IRC-2, IRC-3, SRC-3b, ENR-13b and the gate G-v07-scale;
- every unmerged v0.8 slice: PL-0, PL-1, PL-2, PL-3, PL-4, PL-5, PL-6, PL-7a, PL-7b, PL-8, CW-0, CW-1, CW-2, CW-3, NOS-3, LEM-1, ADH-0, DOC-v08 and the gate G-v08.
- The merged v0.8 parsers (NOS-0, NOS-1, NOS-2a, NOS-2b and LEM-0) ship switched off. They stay in the roadmap's v0.8 step, now labelled "v0.8 parsers (the rest in 1.1)".

**Kept for 1.0:** WK-12, WK-13, WK-16, WK-19 and WK-G; Track Q; all of M8; RX-2. Track U is fill work: its roadmap step says that what isn't merged at the cut moves to 1.1.

**Changed nodes in `runner/slices.yaml`:**
- **G-v07:** its deps drop OUT-1, OUT-3, IRC-2, IRC-3, UN-3, UN-4 and G-v07-scale. Its note says its checks fold into G-v1-audits. It's `closed` on the dashboard (`state_override`), since no separate v0.7 gate runs.
- **G-v1-audits:** the title adds "and G-v07's v0.7 safety checks". The deps add G-v07's remaining 23 deps: the shipped outputs, upload, enrichment, usenet storage and IRC slices, all merged. A note names the checks.
- **M8-POL:** E-v06-sources, SRC-G, RP-G and ENR-G. It no longer waits on E-M7, E-v08, G-v07 or G-v08. Every plugin that ships in 1.0 is merged.
- **M8-PURGE:** the same deps, plus WK-G, for the purge by worker (WK-12, WK-13).
- **M8-DR:** M8-DR-a, E-M6, RX-2, WK-G and X-vpn-bind-ui, the screens that still change for 1.0. It no longer waits on E-M7, E-v08, G-m6, G-v07 or G-v08. Its title becomes "after M8-DR-a: every screen it didn't cover, then a delta on those it did".
- **M8-SEC-2:** M8-SEC-0, RX-2 and WK-G (the regex query bar, the workers API and its screen).
- **M8-SEC-3:** M8-SEC-0, E-v06-sources, SRC-G, RP-G and ENR-G. Its Nostr and Lemmy scope is the merged parsers.
- **M8-SEC-4:** M8-SEC-0, RP-G and ENR-G.
- **M8-SEC-5:** M8-SEC-0, E-M6 and M8-PERF-3. G-m6 now runs on the RC.
- **M8-SEC-6:** M8-SEC-0, E-v06-workers, E-v06-companions, WK-G and CMP-G.
- **M8-SEC-7:** M8-SEC-0. Its integrations are all merged.
- **M8-SEC-8:** M8-SEC-0, E-M6 and WK-12, the store migration.
- **M8-DOC-1:** M8-REL-2 and RX-2.
- **M8-DOC-3a:** E-M6, RX-2, WK-19, D-vpn-guide (which owns guide 3.3) and G-v06-crawler (the new crawler defaults).
- **R-v1-cut:** drops E-M7, E-v08, G-v07, G-v08 and G-m6. It adds RX-2, WK-19, G-v06-crawler and Track Q's fix slices, the same ids G-v1-security lists. It keeps the M8 slices, E-M6, E-v06-workers, E-v06-companions, WK-G and CMP-G.
- **G-m6:** adds R-v1-cut and `ext:owner-v1-rc`, because its manual installs are on the 1.0 RC.
- **G-v1:** adds G-m6.
- **E-M8 (epic):** drops E-v08.
- **M8-REL-2:** adds X-ci-test-shard, since both edit `ci.yml`.
- **X-tokio-1541-v052:** `closed` on the dashboard, with a note. No v0.5.2 tag means this pre-tag PR isn't needed. P-v052-cut and X-consent-gate-052 were already merged.

**The dashboard:**
- The `held` list adds the 28 moved ids.
- `state_override` sets G-v07 and X-tokio-1541-v052 to `closed`, and CR-AB-COMP to `done`.
- The roadmap:
  - The v0.7 step loses the moved ids.
  - The v0.8 step keeps only the merged parsers.
  - The new step `v11` follows `v1`.
  - The arrow v08 → m8 becomes v06 → m8, and tracku → m8 is removed.
  - The `v052` step is "v0.5.2 cut, not released", with `override: closed` and a plain line saying it's superseded. This is the first step marked superseded. `closed` is the state the connector already uses for superseded slices.
- The gates:
  - G-v07's row takes its status from G-v1-audits and says so.
  - G-v08's row is `held`.
  - G-v1-audits' row names the v0.7 checks.
- The `ph-v10` phase and the v0.7 and v0.8 workstream texts say what moved.
- The conditions:
  - `migration-slot` is `met: true`: V17 is assigned to WK-12, and the later numbers are queued in `runner-state`.
  - `ext:owner-m6-manual` is reworded: the owner's manual Unraid and Windows MSI installs, done on the 1.0 release candidate.
- The owner queue:
  - Item 44 leaves.
  - Item 46 lists the flags below.
  - Item 47 is the manual installs on the RC.
- `notes.now`:
  - "Four build sessions" becomes "Six build sessions and two test sessions".
  - A new "Estimates" entry is added.
  - The crawler entry carries the comparator's result.
  - The v0.5.1 entry no longer promises rule packs in v0.5.2.

## New slice entries
- **Merged since r23, never entered:**
  - X-filter-budget-drift (#1175);
  - X-drawer-detach-flake (#1177);
  - X-hidden-count-wide-shift (#1179);
  - X-smoke-torrent-flake (#1180);
  - X-all-themes-settle (#1182);
  - X-hidden-count-grouped (#1183);
  - X-detail-actions-timeout (#1184);
  - X-vpn-hold-macos (#1185).
- **To build:**
  - X-ci-test-shard (Opus; `ci.yml`, with `runner_guard.py` and its test now that #1186 is merged);
  - X-hidden-count-drawer-wrap (Sonnet, fill work: with the drawer open, the grouped count alone wraps the toolbar at a box of 528–840 px);
  - X-vpn-bind-win (Opus, §2.90, after X-vpn-bind-ui);
  - M8-DR-a (Opus, docs: the early design review of the screens that are already final);
  - three flake fixes, each fixed once: X-flake-dht-spill (the spilled-cooldown proof test), X-flake-url-scrub (`url.rs` `scrub_text`'s speed test on macOS) and X-flake-core-rejudge (`core_rejudge.rs`, a range that never reads).
- **Already present:** X-row-rail, X-vpn-bind-ui, X-healthz-count-async (#1181) and M8-PERF-3 were entered earlier. X-row-rail, WK-12 and M8-PERF-3 gain notes for the runner's calls below.
- Each new id has a placement rule.

## Doc fixes carried from reviews
- **#1181:**
  - `docs/decisions/2026-11-28-X-healthz-count-async.md` gains its PR number in the heading.
  - Its suite count goes from 266 to 268 tests.
  - The two orphaned sentences go back to the `stored` bullet, which is split from the analyzer's bullet.
  - The chunk measures 3,432 B (3,407 B before the review round).
- **Also for #1181:**
  - `web/.size-limit.mjs`' pipeline comment says 3432 B.
  - `docs/metrics.md`' have-filter row says only capacity and bytes jump, while `have_entries` climbs during the scan.
  - The `log_events.rs` doc comment for the `scan` stage says "read of the store" (doc comment only; `cargo fmt --check` passes).
- **#1183:**
  - The decisions heading gains "(#1183)".
  - The count text for "≈ 123.5M" is 266 px.
  - The cap bullet ends "holds." and says it supersedes the 1,024 px cap in the wide-shift file's line 9.
- **#1185:** the heading gains "(#1185)", and line 8's Linux check says the pin fails at `bind_device` (ENODEV), on the same refused path as macOS's.
- **#1186** merged while this PR was being written, so its decisions heading is left for the runner's next records PR.

## Runner refinements since r23 (§2.19)
1. **X-row-rail's size grants (§2.26):**
   - The step-0 limits are measured plus at most 64 B, with Search results (lazy) at most 45.0 kB and Facet rail (lazy) at most 20.2 kB.
   - The 18:31Z grants: Search cards 8.17 → about 8.95 kB (+735 B), the table 6.50 → about 6.75 kB, the Alert route 35.39 → about 35.45 kB, and a shared chunk 15.74 → about 15.76 kB, each at measured plus at most 64 B. Search results is 44.60 kB and Facet rail 20.14 kB.
   - A shared ResultRow chunk is used only if it saves at least 300 B.
   - The owns grants are the row files, its messages, `badges.ts`, one fixture field, and three `DiscoverPage.test.ts` seeder assertions under ruling C (one two-tier row; Discover keeps its backfill marker through a small prop).
2. **#1183's grouped-note threshold is 72rem, not the ruled 71rem.** The longest English grouped line measures 1,102 px, so 71rem would leave 34 px.
3. **The toolbar caps from 128rem:**
   - 256 steps (1,024 px) in #1179: the controls end at the box, so the worst English line fits one row with at least 48 px left;
   - then 288 steps (1,152 px) in #1183, so a wide window can show the grouped note.
   - Both keep the UI review's L3 finding answered.
4. **#1184: vitest `testTimeout` and `hookTimeout` are both 20 s,** above the repo's 10 s condition waits. 49 files relied on the 5 s default.
5. **WK-12:**
   - `worker` is a `u32` (CHECK ≥ 1), where §18.5 says `u16`.
   - Test-only writer-wiring asserts are allowed in `writer.rs`.
   - Unattributed remote rows are purged with a primary-key cursor (`purge_unattributed_remote_step(after, limit)`), a refinement of the runner's ruling 4.
6. **M8-PERF-3 (#1186):**
   - The push trigger is narrowed to release lines (`release/v` plus a version), because `release/**` matched 12 backport branches.
   - The runner is registered by hand by default, so no credential is stored.
   - The setup page states mitigation (b) accurately: it doesn't bind sessions.
   - The registration token is entered at a prompt in the container and unset before the run.
   - The ruleset patterns `release/v*.*` and `v*.*.*` are for the owner to confirm on the ruleset screen.
   - The runner recommends making `runner-guard` a required check.
   - Until `ci.yml`'s test matrix is literal (X-ci-test-shard), the runner security-reviews any PR that touches `ci.yml`, `.github/scripts/`, `docs/operations/imports/pilots/smoke/`, a new top-level Python file or `CHAIN_BEFORE_SHA256`.
7. **#1175's merge commit title is truncated.** The runner copied it from a cut-off list instead of fetching it. `main` can't be amended. The runner now fetches the exact title before every merge.
8. **The D1 judgement calls:**
   - **G-v07 is closed, not held.** Its checks run inside G-v1-audits on the RC, which takes G-v07's remaining deps. The gates row for G-v07 passes with G-v1-audits. The 1.1 items bring their own safety checks to 1.1's gate.
   - **G-m6 moves after the cut.** Its manual installs are on the 1.0 RC, so a pre-cut dependency on it would deadlock the cut. M8-DR, M8-SEC-5, M8-SEC-8, M8-DOC-3a and R-v1-cut drop it; their need for packaging is E-M6 and the merged PK-* slices. G-v1 waits for G-m6 instead.
   - **R-v1-cut waits on Track Q's fix slices** (the ids G-v1-security already lists), as its title ("every blocking and important finding fixed") says. It also waits on G-v06-crawler, RX-2 and WK-19 directly.
   - **The kept slices are wired only where a review or a doc needs them:**
     - M8-PURGE, M8-SEC-2, M8-SEC-6 and M8-DR on WK-G;
     - M8-SEC-8 on WK-12;
     - M8-SEC-2, M8-DOC-1, M8-DR and M8-DOC-3a on RX-2;
     - M8-DOC-3a on WK-19, D-vpn-guide and G-v06-crawler;
     - M8-DR on X-vpn-bind-ui.
   - **M8-SEC-8 doesn't wait for X-vpn-bind-win.** That slice has its own `/security-review`, and G-v1-security's delta review covers whatever lands after M8-SEC-8.
   - **Track U doesn't gate M8.** The roadmap arrow tracku → m8 goes, and M8-DR doesn't wait for Track U.
   - **E-M8 keeps E-M7** (its kept parts ship) and drops E-v08.
   - **v0.6 and v0.7 stay milestones and gates, not public releases** (§2.93, runner's reading).
   - **The superseded v0.5.2 step uses `override: closed`.** X-tokio-1541-v052 is closed.
   - **Eight merged slices gain entries,** where the brief listed fewer, because the runner's notes asked r24 to add them.
