# Lodestar v2 build plan: v0.5.1 → 1.0, with helper-agent orchestration

> First written 2026-10-22, after M3. It plans the order of work and who does it: the main session or a helper agent, per the delegation policy in `CLAUDE.md`.
> **§0 (2026-11-12, r4) is the current plan.** It replaces the 2026-11-02 r3 status, ladder, owner queue and next actions, which the build had outrun; the track tables, the crawler program and the gate definitions keep their r3 text. It rests on the owner's answers in `docs/owner-decisions.md` §2.25–§2.60, on the runner's choices in `docs/decisions/2026-10-30-master-plan.md`, `docs/decisions/2026-11-02-master-plan-r2.md` and `docs/decisions/2026-11-02-master-plan-r3.md`, and on `docs/decisions/2026-11-12-master-plan-r4.md` and `docs/decisions/2026-11-13-master-plan-r4-reviews.md` (r4.1).
> **Picking this up from another session or agent?** Start at [`docs/handoff.md`](handoff.md).

## 0. Status and plan

### 0.1 Where things stand
Status as of `main` at `f47369b8` (committed 2026-11-12T22:53Z). This table is a snapshot and goes stale within hours: `main` takes 40 to 60 merges a day. **The live view is the roadmap dashboard** (`tools/status-connector`, reading `runner/slices.yaml`, `runner/dashboard.yaml` and GitHub) and the `runner-state` branch (`state.json`); reconcile both against `main`'s merges before acting ([handoff §2](handoff.md#2-where-the-truth-lives)).

**Slices:** 588 in `runner/slices.yaml`. The dashboard counts 436 merged, 19 in progress, 123 not started, 3 closed and 7 epics (all expanded). Four of the 123 are in fact finished and only need marking (CR-AB-COMP, X-tokio-1541-v052, WK-10d-store, WK-10; `docs/decisions/2026-11-12-master-plan-r4.md`), so about 120 slices are left to start. Slice PRs are open all the time (the live list is GitHub's open pull requests; at the snapshot there were 21), plus #632, the owner's own branch, which the runner leaves alone.

| Area | State | What's left |
|---|---|---|
| **M0–M5, v0.5** | Done. G-v05 passed 10-24, G-m5 passed 11-01 (#352) | Backlog only (Track B) |
| **v0.5.1** | **Published 2026-11-07** from `12cd25ca`. Soak r5 on rc.4 passed (0.91 MiB/h, 95 % CI [0.47, 1.35], bar 2.0); the gluetun leg and the Windows MSI check passed (`docs/decisions/2026-11-07-v051-release-verdict.md`) | The owner replaces the published release text with the runner's draft |
| **v0.5.2** | `release/v0.5.2` cut at `045d4a38` (`docs/decisions/2026-11-07-P-v052-cut.md`). Three candidates assembled on the branch: rc.1, rc.2 (adds #579) and **rc.3 = `833a0d25`** (rc.2 plus tokio 1.54.1 and the notice's "applies to" line, #740, §2.58), with release notes checked against the branch (#751). The owner's long runs were moved to rc.2 (`docs/releases/v0.5.2-rc2-delta.md` on the branch). **No `v0.5.2` tag exists, and `main` records no soak verdict for it** | The gate in §0.4 on the current candidate, the owner's tag, the notes brought to `main` |
| **v0.6 Sources** | Done: 37 of 38 slices (SRC-10b dropped). **SRC-G passed** (`docs/decisions/2026-11-07-SRC-G.md`) | ENR-G; X-consent-gate-lift (building); SRC-3b moved to v0.7 |
| **v0.6 crawler** | Every experiment built behind its switch, all off by default (7 of 7). The bitmagnet comparator ran and was accepted: **317.8 stored/min** over 24 h. Lodestar's accepted baseline is **12.3/min** (CR-0) | Every live A/B run (none has run), Checkpoint 1, CR-1c, CR-5, CR-COMB, G-v06-crawler (§0.5) |
| **v0.6 remote workers** | 18 of 24 on the roadmap step: the `lodestar worker` command, replication, the sharded DHT, fetch on workers and the spool are merged. In review: WK-11b (#779), WK-18 (#797) | WK-12 → WK-13 → WK-16 → WK-19; then the chaos gate WK-G (it needs WK-11b, WK-13 and WK-16, not the guide) |
| **v0.6 companions** | 20 of 23: catalogue, signature and checksum checks, supervision, credentials, Settings → Companions. CMP-D in review (#782); X-cmp-egress-api and CMP-12 building | X-cmp-egress-ui, D-companions-guide, then CMP-G |
| **M6 packaging** | 16 of 17. Windows updates, the release manifest, the settings CLI, the install smoke and the user guide are merged | **G-m6**, which waits only on the owner's manual installs (`ext:owner-m6-manual`). Two Windows rollback slices are follow-ups, not part of the gate: X-win-upd-manual-rollback (#795) and X-win-upd-first-rollback |
| **Imports** | Tools done (IMP-0..5), SB-6 and X-search-files-3 merged, the smoke stages ran. Only a synthetic figure exists: 100k at ~13,900/s | G-import-1M, G-import-10M, G-rebuild-10M, G-import-50M, all on the owner's import host; then CR-3 |
| **v0.7 / M7** | 15 of 47: GS-0, GS-1, UP-0, UP-1, UN-0..2, ENR-9, ENR-10, ENR-12, TZ-0, OUT-0, OUT-2 and others. In review: ENR-14 (#764), ENR-13 (#793). Building: GS-2 | §0.2 rung 5 |
| **v0.8** | 0 of 24 merged; NOS-0 (#789), NOS-2a (#791) and LEM-0 (#787) in review | §0.2 rung 7 |
| **M8 hardening** | 3 of 30 on the roadmap step (M8-NET-E2E, M8-THEME, M8-WIN-1). In review: M8-REL-1 (#783), M8-PERF-1a (#780), M8-SEC-0 (#786), M8-DOC-2 (#785) | §0.2 rung 8 |
| **1.0 release candidate and gates** | 0 of 13 | §0.2 rung 9 |

**Code facts** (they replace the 2026-11-02 list): the store is at **V14**; search is at `SCHEMA_VERSION` **7** (`crates/search/src/schema.rs`), and v0.5.2 ships both, so a v0.5.1 user rebuilds once, v5 → v7. The workspace has 25 crates, about 565k lines of Rust including tests, and 38 fuzz targets. There is no `todo!()`; every `unimplemented!()` is in a test double; Rust has 3 TODOs. All 24 theme families ship in light and dark. Packaging is real for Docker, Unraid, Windows (WiX MSI and portable) and Linux. The workspace version in `Cargo.toml` on `main` is still `0.5.0`: each release branch sets its own version, and `release.yml` checks the tag against it.

**What the code defers on purpose** (each is stated in the code or the guide, none is a stub):
- Discover's "rising" and "recently resolved" groups answer unavailable (`crates/api/src/discover.rs`): they need swarm history and a resolution timestamp.
- The StashDB and ThePornDB plug-ins are on hold and never registered as a running source (`crates/sources-plugins/src/stashdb.rs`, `tpdb.rs`).
- Worker version, uptime, egress, shards, throughput and quarantine are left out of the API until WK-13.
- `docs/operations.md` lists as not built: the Community Applications listing, incoming peers on 6881/tcp, a Settings screen for the web address, a Settings screen for worker join tokens, and Windows updates that install themselves.
- Windows artifacts ship unsigned until the owner provides a certificate (`ext:owner-signing-cert`).
- The `search-latency`, `filter-budget` and `reporting-overhead` CI jobs are not part of `ci-ok`, so a failure in them doesn't block a merge.

The runner checks each of these against `runner/slices.yaml` and adds a slice where none covers it (`docs/decisions/2026-11-12-master-plan-r4.md`).

### 0.2 Order of work and tracks
**Concurrency:** from 2026-11-13T00:00Z the cap is **four build slices and four cloud-testing slices** (owner, 2026-11-12T22:30Z, §2.64; over-cap sessions finish only their open PR, §2.65). Before that it was eight in total plus four shared by the cloud tester and the runner (§2.59, §2.60), with owner windows of more sessions on 2026-11-12 (§2.61, §2.63). Opus, helper and docs slices all count. Sessions already running finish their slices; nothing new launches while the cap is reached. The live list is `slots` in `runner-state`, and the decisions behind it are recorded as §2.61 to §2.65. Filling every slot isn't required: a slot stays empty when no ready work fits, when a slice's `owns` would overlap a running one, or when starting it doesn't make sense for the roadmap. The `runner/slices.yaml` limits (16 Opus + 16 helpers, §2.15) are the ceiling once the owner lifts that. The cap is in `runner/slices.yaml` as `limits.phase_total` so the launch procedure enforces it (`runner/README.md` step 7). **The ladder decides between ready slices;** a slice being ready in the graph doesn't jump the ladder, with one exception, the next rule.

**Above the ladder: cloud testing first** (owner, 2026-11-12 about 18:56Z, §2.62: "Have Agent Runner prioritize anything needed for cloud testing"). Cloud-testing PRs are reviewed and merged first, and cloud-testing fix slices take the next free build slot ahead of features. The cloud-testing lanes use the four cloud-testing slots (§2.64).

**Also first, by the owner (2026-11-13, §2.66, in runner PR #809):** the first-run wizard's raw config keys and the missing Sign out (UI review B1 and B2, one slice, X-wizard-labels-signout; launched 2026-11-13T03:15Z) go ahead of the larger queue. Both are small, and every new owner meets them.

**What decides the date of 1.0.** Building isn't the constraint: about 140 slices remain, and the build lands 40 to 60 PRs a day. Three things are:
1. **The crawler gate.** 12.3/min measured against 100/min. The comparator shows the target is reachable on this host. The experiment calendar is 8 to 9 measured days, about three weeks with start-up and retries (§0.7). The comparator's day is done; no A/B run has started.
2. **One Unraid box serves four queues:** crawler runs, import pilots, the 1.0 Linux soak and the 1.0 crawler re-measure. Every booking is exclusive.
3. **Owner actions** (§0.3).

So: **the host is never idle, and build slots never wait for it.**

**The ladder** (r4; the owner approved it on 2026-11-12 by approving this plan, recorded in `docs/decisions/2026-11-12-master-plan-r4.md` until the runner adds it to `docs/owner-decisions.md`; fill free slots in this order; the owner can reorder it in one line). Rungs 2 to 8 run in parallel wherever their slices are ready; the order decides only between ready slices competing for a slot.
1. **Land what's open.** The open PRs go through review and the landing lane before new launches. #632 (`feat/lodestar-secure-pia-bridge`) is the owner's branch: the runner leaves it alone, and the owner decides whether it lands, goes to a slice or is closed. A cloud review the owner ran on it raised 11 findings, not yet verified, four of which could stop live fleet runs; they are with the owner.
2. **Ship v0.5.2** (§0.4): the gate on the current candidate, then the owner's tag. v0.5.1 carries tokio 1.54.0, which was yanked; §2.58 puts the fix in v0.5.2, so this rung doesn't wait behind the crawler runs for the host. §2.58 also has the runner prepare a v0.5.1.1 if v0.5.2 slips by more than a few days.
3. **The v0.6 build side, to its three gates:**
   - workers: WK-11b and WK-18 (in review); WK-12 (the next migration) → WK-13 → WK-16 → **WK-G**; WK-19, the guide chapter, after WK-16 and WK-18;
   - companions: X-cmp-egress-api → X-cmp-egress-ui; CMP-12; CMP-D, D-companions-guide → **CMP-G**;
   - M6: **G-m6** waits only on the owner's manual installs; the two Windows rollback slices follow up and don't hold it;
   - small: ENR-G (ready), X-consent-gate-lift (building).
4. **The crawler's live evidence** (Track C, on the owner's host): CR-AB-ADM → CR-AB-4 → CR-CP1 (Checkpoint 1) → CR-AB-1 → CR-5 → CR-COMB → **G-v06-crawler**. CR-1c is a build slice that needs only CR-AB-ADM's result, so it is built alongside CR-AB-4 and Checkpoint 1. **The v0.6 tag follows P4 in §3 until the owner answers:** the crawler gate passes (or the owner re-decides at Checkpoint 1) and the cluster chaos and transport tests stay green; each feature's own gate (SRC-G, passed; WK-G; CMP-G) decides whether that feature ships in v0.6. **Question for the owner:** does the tag wait on the crawler gate only (P4), or also on WK-G, CMP-G and G-m6? §2.25 and §2.27 say only that the crawler gate "gates the v0.6 tag".
5. **v0.7 / M7.** It doesn't wait for the v0.6 tag:
   - grouping and regex: GS-2 → GS-3, GS-4, RX-1; GS-3 → GS-5; RX-1 → ENR-11 → OUT-1 → OUT-3;
   - enrichment: ENR-14 → the twelve plug-ins (ENR-tmdb, -tvdb, -anilist, -anidb, -mb, -ol, -igdb, -discogs, -steam, -srrdb, -predb, -debrid), which run side by side; ENR-13;
   - upload: UP-2, X-watch-env, and X-watch-stall-retry (a stalled copy into the watch folder is failed for good on Unix; from the 2026-11-12 codebase review). The three own different files and can run side by side;
   - uTP: X-utp-spurious-rto after X-utp-backoff (#788); it also makes a connection time out when the peer's packets acknowledge nothing (same review);
   - unverified items and usenet: SRC-3b (after GS-3 and ENR-11; a migration) → UN-3 → UN-4;
   - IRC: NOS-2a → IRC-0 → IRC-1 → IRC-2, IRC-3;
   - small: X-unverified-cap-index (ready), X-percolate-promoted-hold (building);
   - then G-v07-scale and **G-v07**.
6. **Import pilots** (Track I), in the gaps between crawler runs: G-import-1M → G-import-10M and G-rebuild-10M → G-import-50M → the owner approves the numbers → CR-3 and the full library.
7. **v0.8:** the parsers and transports first, since they wait on no gate (NOS-0, NOS-2a, LEM-0, ADH-0, then NOS-1, NOS-2b, NOS-3, LEM-1); plug-ins and companions on workers (PL-0 to PL-8, CW-0 to CW-3) after WK-G and CMP-G; PL-7b after the owner approves the per-worker consent wording; then DOC-v08 and **G-v08**.
8. **M8 hardening:** what waits on no gate starts now (M8-SEC-0, then the security reviews as each surface settles; M8-REL-1, M8-REL-2; M8-PERF-1, M8-PERF-3; M8-DOC-2); after WK-G: M8-CHAOS-1 to M8-CHAOS-4, then M8-PERF-2 and M8-CI, which need M8-CHAOS-1; after G-m6, G-v07 and G-v08: M8-POL, M8-PURGE, M8-DR, the other M8-DOC slices and the last security reviews. Each slice's exact `deps` are in `runner/slices.yaml`.
9. **The 1.0 release candidate:** R-v1-cut, the owner tags the rc, the eleven G-v1-* gates run on it, then **G-v1** and the owner tags v1.0.0.
10. **Backlog and polish** (Track B) in whatever slots remain. The cloud operator slices and their gate OP-G run in the cloud-testing slots (§2.64) and, where cloud testing needs them, ahead of the ladder (§2.62), once the owner finishes the operator setup.

After 1.0: the Unraid Community Apps submission and package installers (owner), then v1.1 I2P, v1.2 eD2k and v1.3 IPFS (§3 below, architecture §14).

**Track status at r4.** The tables below keep their r3 text as the record of how each track was planned. Where they stand now:
- **Track R:** complete through R4. R5, the v0.5.2 cut, is done; its gate and tag are rung 2.
- **Track C:** C0a to C3 done. C4 onwards is rung 4.
- **Track I:** I1, I1b and I2 done. I3 onwards is rung 6.
- **Track S:** complete, SRC-G passed.
- **Tracks W, K and M6:** no longer held. Their slices are in `runner/slices.yaml` (plans `docs/plan/2026-11-06-v06-workers.md`, `-v06-companions.md`, `-m6-gap-audit.md`); what's left is rung 3.
- **Track M7** (its heading still says "ready to expand"; it was expanded on 2026-11-05): SB-6 and X-search-files-5 merged; the plan is `docs/plan/2026-11-05-v07-m7.md`; what's left is rung 5.
- **Track B:** every item its list names as running or in review has merged; the live backlog is in `runner/slices.yaml`.
- **v0.8 and M8** were planned after r3: `docs/plan/2026-11-06-v08.md` and `docs/plan/2026-11-06-m8-hardening.md`.

**Fixes from the 2026-11-12 and 11-13 reviews (r4.1).** Three reviews of `main` ran at the owner's request: a whole-codebase review, a UI/UX review in four parts with later checks, and a bug review of the untrusted-input code. Findings that are blocking or important and concern security or content safety are held out of this repository until a release with the fix is published (`docs/security/v1/README.md` §5, owner §2.47): this plan names each by id, severity and surface only. The runner gives the detail to the owner for draft advisories, and the code-review folder stays off `main`, and the UI review's folder stays on its branch (`claude/codebase-review-bugs-x9ff7p`), until the runner decides what, if anything, holds no held detail and comes to `main`. The runner owns the slice ids and where each fix sits.

**Track Q: code-review fixes.** The code review found 203 findings (16 blocking, 100 important, 87 polish); 153 are in no slice, decision or open PR. Grouped by surface:

| Group | Findings (id, severity, surface) | Plan |
|---|---|---|
| **Content safety** | FILTER-01, FILTER-02, FILTER-03 (blocking, child-safety core); UPDATE-01, STORE-01, UPDATE-05 (blocking and important, restore paths); APIFEAT-01 (blocking, re-graded to an owner decision; rules); SRC-02 (important, sources) | Fixes held under `docs/security/v1/README.md` §5. The restore-path fix enforces rules the repository already records (`docs/decisions/2026-11-12-X-win-upd-manual-rollback.md` follow-up 1; SRC-14b's purge rule, #400), so it is a runner call (§2.33), flagged for veto (§0.3). FILTER-02's fix goes ahead as a slice (§2.33). SRC-02 is on the veto list. FILTER-01, APIFEAT-01 and FILTER-03 wait on the owner's choice between fixes (§0.3, 13 to 15) |
| **Hostile input and availability** | PIPE-01, DHT-01, SEARCH-01, STORE-02, PIPE-02 (blocking and important; fetch pool, DHT, search indexer, store writer) | Fixes held under §5. Until the owner answers (§0.3, 19), the runner places them as it places any new fix slice (§2.33), so they don't wait |
| **Wrong results and lost state** | LOGSET-01 (a saved logging setting reverts at restart); SEARCH-02 (sorted `sig:` searches); CLUSTER-01 (worker lease reconciliation; the runner's slice X-lease-reconcile-every-session, before WK-G, not yet in `runner/slices.yaml`); CMP-01 (important, companions, security: id only; fix before the first pin); INTEG-01, LOGSET-03, APIFEAT-04, APIFEAT-03 (no notification or digest is ever sent, though Settings shows them as live); WEBCORE-01 ("Matched in N files" never exceeds 20) | New slices. The notifications gap also needs a decision on what Settings says until they are built |
| **Supply chain and release** | Three open RustSec advisories in `Cargo.lock`; no advisory check in CI (INFRA-09: no slice adds one; M8-SEC-5 reviews dependencies by reading only). INFRA-01: the image publish has no `release` environment gate (below). INFRA-02, INFRA-03, INFRA-04, INFRA-08 (release workflows); UPDATE-02, UPDATE-03, UPDATE-04, STORE-03 (updater and store) | New slices. INFRA-01 is checked before the v0.5.2 tag (rung 2): the tag publishes `:0.5.2`, `:0.5` and `:latest` as soon as the smoke and arm64 build pass, before the `release` approval, and the workflow that runs is the one at the tagged commit, so gating it for this tag needs a release PR to `release/v0.5.2` first. INFRA-03 matters for any patch on an older line once a newer version is out |
| **Test net** | WIRE-05, TEST-03 (no fuzz or property test for peer-wire); TEST-01 (three real-server Playwright specs match no CI project and never run); TEST-04 (CI fuzzing is 60 s a target from an empty corpus); TEST-02, DHT-03, LOGSET-04, WEBCORE-05 (assertions after a fixed sleep, a blocking finding under CLAUDE.md) | New slices; TEST-01 first, since it hides regressions |
| **The rest** | The remaining findings in no slice; the review's table lists each with a "tracked?" column | The runner triages them into slices: blocking and important before 1.0, the rest as Track B |

Already done or in flight: CLUSTER-06 points 1 and 2 (#797, merged; points 3 and 4 have no slice), the Roll back half of UPDATE-06 (#795, merged), INFRA-14 (#781, merged), CLUSTER-01's first fix (#779, merged), CLUSTER-04 (WK-12, WK-13, WK-G), TEST-06's 10M measurements (M8-PERF-1 to 3). Track Q's blocking and important findings feed G-v1-security and the eight M8 security reviews; none may be open at the release candidate unless held under `docs/security/v1/README.md` §5. The runner extends G-v1-security's scope to them in `runner/slices.yaml`.

**Track U: UI/UX fixes and the UI plan.** Steps 1 and 2, the drawer rule and the other answers in step 3 are the owner's and the runner's (§2.66, #809). The order of steps 3 to 6 is this plan's recommendation for the runner to place; the review's own "do these first" list (page frame, adaptive row, toggle rule, label formatter, Settings navigation) groups the same work by cause:
1. **B1 and B2, first** (owner): X-wizard-labels-signout. The wizard shows raw config keys as labels (`intensity`, `archive_raw`); nothing signs a user out (`LogoutButton.svelte` is imported nowhere).
2. **The guards**, in runner PR #809: X-sources-restructure (Settings → Sources, before the next plug-in row, owner decision); X-ui-guards (Phosphor icon weights migrated and banned, the dead `sm:` to `2xl:` classes banned, an axe run on fixture data in CI, since the mock's short lists probably hid B3); X-insights-funnel-fixture (B4 is a fixture gap: the fixture seeds no funnel events, so Fetch reads 0 against the funnel's 413).
3. **I1 and I3 together**, one slice, per §2.66 items 1 to 3: the drawer beside the results from 1366 px, rows that adapt to their container, "Group versions" and "Identified as" as decided there.
4. **B3** (`/insights` fails the scrollable-region axe check at every width) is fixed in or before X-ui-guards, whose fixture-backed axe run covers `/insights` and would otherwise be red; X-ui-guards may only touch icon imports and dead classes in `web/src/lib/**`, so B3 is its own small slice or the runner widens that slice. Then **L1** (a page opened while the server is down stays on "Error 500" after it returns: say "Couldn't reach Lodestar" and retry) and **L2** (when the session ends with an unsaved setting, the Save dialog never says so and the change is lost after sign-in: keep the draft across sign-in).
5. **The rest of the review** (I2, I4 to I28, L3 to L5, polish), grouped by cause by the runner: one page frame and header, one label formatter, one control height set, Insights narrative and chart choices, Settings search and badges, and the 407-against-409 count gap.
6. **Part 4's opportunities, in its order**, once the owner answers its proposals (§0.3, 17): a checklist in place of the nine-step wizard, zero-result rescue, a plain health sentence, "What's this?" popovers, per-screen help links, simpler row and rail defaults, one noun for alerts, "Open in torrent client".
7. **The UI plan for unbuilt modules** (part 2, section 5) feeds the briefs of GS-3, GS-4, GS-5, RX-1, UP-2, UN-3, UN-4, ENR-11, ENR-13b (the follow-up the runner plans, not yet sliced), OUT-1, OUT-3, WK-16, PL-7a, PL-7b, CW-0 to CW-3, IRC-2, NOS-3, LEM-1, ADH-0 and M8-DR. ADH-0 is ready to launch now, so its brief must say first that it needs no new switch.
8. **The 1.0 design gate** (M8-DR, G-v1-design): design review with all skills on fixture data at eight widths including 1440, with the review's scripts kept as repeatable checks.

Not yet checked by anyone: a screen reader, keyboard-only use of the wizard, real touch devices, a language longer than English, overlay motion timings and dialog radius, chunk-load failure, the first hour on a live install, index rebuild and paused states, print, the Fetch counter on a live crawl, performance budgets, and real Updates, Companions and Workers state. The UI review's `03-not-done.md` says how to run each; each needs an owner action or a slice.

Each track below lists its steps in order. "Owner" marks a step only the owner can take; `ext:` names match the `external` section of `runner/slices.yaml`.

**Track R: release**

| Step | Who | Waits on | Exit |
|---|---|---|---|
| R1 Unraid soak r3 verdict: **FAIL (memory)**, 1.88 MiB/h, 95 % CI [1.43, 2.33] over hours 12–24, so the upper bound misses the ≤ 2 bar. Coverage, shutdown and evidence were clean (`docs/decisions/2026-11-03-soak-r3-verdict.md`) | runner (analysis), owner (evidence hand-back) | the baseline end, 11-02 22:16Z | `analyze` report: no stall, no interruption, ≥ 24 h covered, memory slope upper 95 % CI ≤ 2 MiB/h on `anon` private memory over hours 12–24, peak reported |
| R1-fix Memory fix **X-mmap-cache-prune**: M1-exp (prune versus default) first, then prune tantivy's mmap cache after each commit. M2 (commit cadence) only if M1-exp leaves growth | R-mem-triage session (existing) | R1 | M1-exp passes, then a fresh Opus review, green CI and merged to `main` |
| R2 gluetun leg (≥ 2 h measured, part of the v0.5.1 gate) | owner runs it on rc.4 after R1″; runner analyses | R1″ | collector run with `--leg gluetun --require-gluetun`. **Passed** (2026-11-06, runner; `docs/decisions/2026-11-07-v051-release-verdict.md`) |
| R3 **rc.3 = rc.2 + #357 + #402 + the memory fix**, owner's decision 2026-11-03 (§2.31, replacing §2.29's rc.3 = rc.2 + #357). #357 (F-filter-load-fail-closed) is on `release/v0.5.1` as #377 (merged `0acc5a37`). Cherry-pick checks and delta mapping as in §2.29: filter load, stand-ins, the §20.8 budget. #402 requires a re-soak, so r3's evidence no longer covers rc.3 and R1′ runs a fresh one. None of #357, #402 or the fix touches `crates/store`, so r1's upgrade evidence still counts under §2.28 (the fix must keep it so) | runner; owner tags rc.3 | R1-fix | rc.3 artifacts signed and hashed; delta report |
| R3′ **R-rc3-cut**: #402 and the memory fix onto `release/v0.5.1` after #377 (merged `0acc5a37`) | R-rc3-backport session (existing) | R1-fix | release PR green, fresh Opus review, merged; the owner tags `v0.5.1-rc.3` |
| R1′ Soak r4 on rc.3 | owner | R3′ | **FAIL** (memory): 29.35 MiB/h, CI [28.75, 29.96] (`docs/decisions/2026-11-05-soak-r4-verdict.md`) |
| R3″ **R-rc4-cut**: X-thp-off (#434), X-mem-gauges (#433, #442) and X-mimalloc-override (#448), plus the runner's addition X-log-url-redact (#432), onto `release/v0.5.1`; and **LEGAL-1r**, the Legal Notice at setup and login on the release branch (owner §2.40) | R-rc4-cut and LEGAL-1r sessions | #448 merged | both release PRs green, fresh Opus reviews, merged; the owner tags `v0.5.1-rc.4` |
| R1″ Soak r5 on rc.4: Linux main leg, frozen image, 6 h suppression + 30 min warm-up + 24 h baseline; Appdata Backup disabled (owner §2.40) | owner | R3″ | `analyze` report with gate evidence yes and memory slope upper 95 % CI ≤ 2 MiB/h over hours 12–24. **PASS** 2026-11-06: 0.91 MiB/h, CI [0.47, 1.35] |
| R-win Short Windows MSI service smoke on rc.4 (≥ 1 h: start, stop, clean restart, non-zero stored rate, and the Legal Notice at setup). The memory fixes are Linux-only or platform-independent, so a full re-soak isn't needed. **Passed** (2026-11-06, runner verdict: stored above 0 after the first-run hold-back; the one refused start was an extra folder ACE, not rc.4) | owner | R3″ | pass |
| R3‴ **rc.5 = rc.4 + Legal Notice v3, text only** (#540, slice R-rc5-notice; the owner chose it directly, §2.48). `release/v0.5.1` is at `a0eb0a9087c2474ef4285416403eacbf8a475111` | R-rc5-notice session | R3″ | release PR green, fresh Opus review, merged. **Done** (`a0eb0a90`). No `v0.5.1-rc.5` tag: the release is tagged straight as `v0.5.1` on `12cd25ca` (R3b), because #558 changed docs only (runner, 2026-11-07) |
| R-legal **Legal documents (§2.48):** before any version is tagged, check `docs/legal/legal-notice.md` and `docs/legal/operator-guidance.md` against that build, keep both on one release baseline with the notice's "applies to" line, decide `NOTICE_VERSION` by X-legal-docs' substance test, and record the check in the release decisions file | runner | the release candidate to be tagged | check recorded in the release decisions file. **Done** for v0.5.1: one docs fix, R-legal-status (#558) |
| R3b Tag `v0.5.1` on `12cd25ca38f8eac1a930316623dec742c1ba585d` (rc.5, #540, plus the operator-guidance fix #558; never on `a0eb0a90`), approve the release environment, publish | **owner** | R1″, R2, R-win, R3‴, R-legal | GitHub release published; `:latest` moves |
| R4 Bring `docs/releases/v0.5.1.md` to `main`, mark `docs/releases/v0.5.0.md` as never released, update the README's release status | docs slice | R3b | — |
| R5 **v0.5.2 cut from `main`** (recommended, §0.4) | runner | R3b | G-v052 |
| On failure at R1/R2 | fix PR on `main`, cherry-picked to `release/v0.5.1`, the next rc, a fresh frozen soak. Runs are never stitched | — | — |

**The stand-in gap in rc.3 and rc.4.** rc.3 is rc.2 + #357 + #402 + the memory fix (§2.31), with nothing else from `main`; rc.4 adds only the §2.40 changes, not #367. #357 alone holds what an unreadable rule's stand-in matches, and nothing in rc.3 undoes those holds: F-filter-standin-undo (#367) re-judges them, and rc.3 doesn't write the `filter.stand_ins` record a later upgrade needs to find them. The delta report states this gap. rc.3 and rc.4 are built without #367, so it ships in v0.5.2 from `main`, unless the owner says otherwise.

**Track C: crawler to the v0.6 gate.** Details, evidence and the checkpoint are in §0.5.

| Step | Who | Waits on |
|---|---|---|
| C0a **CR-COL-3**: the Linux collector and analyzer keep and judge the gate's evidence. Today `collect.py` keeps only the CR-0 series, so `searchable_new_total`, the latency histogram and its dropped counter, and CR-4's family-labelled, connect-timeout, no-route and uTP series are discarded; only `fetch_attempts_total` and `fetch_success_total` survive with their `transport` label (reproduced by the 2026-11-02 review). Retain bounded, validated labels and histogram buckets including `+Inf`; compute searchable rate, hourly minima, latency coverage and per-transport/per-family breakdowns; reject missing evidence and counter resets; fixtures that pass and fail; a short end-to-end rehearsal | Opus | — |
| C0b **CR-SPEC**: a frozen measurement specification, written before any data is collected (§0.5 "Measurement specification") | Opus (docs) | — |
| C1 CR-4 merged (#365, 2026-11-02) ✔ | Opus | — |
| C2 Experiment host ready (one node, one public IP, no imports, not the Windows PC that ran CR-0): **the Unraid box, DHT port not forwarded** (§2.51), and **booked exclusively per run** (§0.7 calendar) | **owner** | `ext:experiment-host` |
| C3 Pinned bitmagnet comparator, 24 h, fresh data, its own day | owner runs, runner analyses | C0a, C0b, C2 |
| C4 Live ABBA A/B, 6 h measured × ≥ 2 reps per arm: CR-ADM, then CR-4 uTP; route backoff only if a baseline shows `fetch_no_route_total{family="ipv6"} > 0` (CR-4's own gate), otherwise recorded as skipped | owner runs, runner analyses | C0a, C0b, C1, C3 |
| C5 **Checkpoint 1**: projection to the gate, reported to the owner | runner → **owner** | C3, C4 |
| C6 A/B runs for CR-1a-S, CR-1a-H, CR-1b; build CR-1c (cap sized to the intensity preset) | Opus + owner host | C5 |
| C7 CR-5: profile, then either tune or record a reasoned skip in its decisions file. Either outcome completes it, so CR-COMB can depend on it | Opus | C6 |
| C8 CR-COMB: winning switches default on, 24 h acceptance on the gate metric (G-v06-crawler) | Opus + owner host | C6, C7 |

**Track I: imports to scale.** Gates in §0.6.

| Step | Who | Waits on |
|---|---|---|
| I1 10k and 100k smoke stages (runner) | Opus | — |
| I1b **SB-6's population contract** (Track M7): each later producer of an SB-6 field ships a resumable backfill, so fields exist and are filled | Opus | SB-6 |
| I2 X-search-files-3: use the file index's `size` field (added by SB-6; it's an index format change) so cold file-search p95 stays ≤ 150 ms at 10M (`docs/decisions/2026-10-31-X-search-files-2.md`) | Opus | SB-6 |
| I3 G-import-1M on the owner's data (Bitmagnet dump restored per IMP-0 first) | owner host, runner analyses | `ext:import-host` |
| I4 G-import-10M: steady-state v6 import at 10M | owner host | I2, I3, SB-6 (which waits on SRC-1 → SRC-2) |
| I4b **G-rebuild-10M**: the v5 → v6 blue/green rebuild at 10M. Build and checkpoint a 10M v5 corpus and index with a pinned pre-SB-6 binary, upgrade a copy to SB-6, and measure the rebuild, serving during it, catch-up, the disk high-water mark and an interrupted resume. An initial v6 indexing pass doesn't count as blue/green evidence | owner host, runner analyses | I3, SB-6 |
| I5 G-import-50M: measured numbers go to the owner, naming which SB-6 fields were populated | owner host | I4, I4b |
| I6 Full corpus (~40–50M) once the owner approves the 50M numbers (`ext:owner-50m-approval`); then CR-3 (durable partial upgrades, the next free migration), the wizard subset (WIZ-*), IMP-05..08 | Opus | I5 |
| Parked | B-import-server-2 (import through the running server's writer) adds an API that takes a source path or credentials, which is new attack surface. **Online** import targets (1,000/s, writer-share fairness, search latency under import) belong to it, not to the pilots | **owner** |

**Track S: v0.6 Sources.** The plan is `docs/plan/2026-11-01-v06-sources.md` §6. Its critical path is SRC-0 ✔ → **SRC-1** → SRC-2 → **SRC-7** → SRC-10* → SRC-G.
- **Now:** SRC-4 (consent framework), SRC-5 (egress and kill-switch), SRC-9 (Internet Archive JSON parser). All are Opus with `/security-review`.
- **SRC-1:** V14 (`docs/decisions/2026-11-02-master-plan-r2.md`; `migration_order` records it). The owner allowed migrations on `main` before the v0.5.1 tag (§2.28), so `ext:owner-migrations-on-main` holds and SRC-1 may land. CR-3, which once reserved V14, takes V15 after the 50M gate.
- **SRC-7:** Q2 and Q15 are answered (§2.28), so it waits only on SRC-2, SRC-4, SRC-5 and SRC-6.
- **SRC-3 depends on SB-6** (Track M7) instead of the old `ext:m7-schema-bump`.
- **P-v06-rulepacks:** plan the other half of E-v06-sources (curated rule packs, ThePornDB/StashDB enrichment) the same way P-v06-sources-plan did.
- Verification fetches of unverified keys stay behind `sources.verify_unverified` (off) until the crawler acceptance soak (C8) ends.

**Track W: remote workers (E-v06-workers; r3 text, written while the epic was held; it was unheld on 2026-11-06).** The expansion starts from what's built: the library covers P4 steps 1–2. Still to build, in order:
1. the `lodestar worker` subcommand and the coordinator side in `crates/lodestar`, wired to `ClusterServer`/`WorkerSession`;
2. have-filter replication (today the worker ignores the message);
3. the `sharded(keyspace)` DHT;
4. fetch and prepare on workers, plus the spool;
5. the chaos harness and the §19.8 gates;
6. Settings → Workers and per-worker Insights (Sonnet).

Any worker table goes through the migration queue (§0.7). Interface changes from the crawler program keep `crates/cluster`'s chaos and transport tests green.

**Track K: companions (E-v06-companions; r3 text, unheld on 2026-11-06 and now mostly built).** At r3 there was no code. Expand per P4: signed catalogue, checksum and signature checks, supervised child processes, encrypted credentials (Opus), then compose and CA-template generation, detection adapters and the Companions UI (Sonnet).

**Track M6 (E-M6; r3 text, unheld on 2026-11-06).** Most of P3 is built (§0.1). The expansion is a **gap audit** against P3 and §14, then slices for what's missing. Known items:
- FUSE and network-filesystem detection: verify whether it exists;
- the worker template: label it "requires v0.6" more prominently, or keep it out of the CA submission until Track W step 1 lands;
- the user guide refreshed for v0.5.1 (the first hours are slow because of the 6 h sampling suppression; `docs/decisions/2026-10-31-owner-delegation.md`);
- the Unraid Community Apps submission (**owner**), after 1.0 (§2.37); not part of M6's exit.

**Track M7 (E-M7, ready to expand).** Already merged: the integrations crate and adapters, notifiers, Send UI. To expand:
- **SB-6, the schema-bump convergence slice (Opus, first).** Search rebuilds once per release (§1.3). SB-6 is the bump 5 → 6; X-search-files-5 adds 6 → 7 (the torrent-level file size terms and B13's title-identity key) before the v0.5.2 cut, so a v0.5.1 user rebuilds once, v5 → v7 (runner, 2026-11-05). It fixes, in one change: SRC-3's `verified:`/`source:`/`network:` fields; `payload_sig` and the grouping fields; the regex trigram field; file category and `pieces_root`; and `size` in the file index, which X-search-files-3 needs. It waits only on SRC-2 (so the unverified data model is settled), not on E-M7's expansion. Later M7 slices fill these fields; apart from v7, they don't bump again. It lands before the 10M import pilot.
  - **Population contract (r3).** A new field is empty on every record indexed before its producer ships: `crates/search/src/rebuild.rs` opens an existing current-version index without rebuilding, and the live indexer visits only new ids and `reindex_log` entries. So SB-6 also defines, and every producer slice (SRC-3, grouping, `payload_sig`, trigram, file category) ships: a per-field **population generation** recorded durably; a **resumable backfill** that walks existing records through `reindex_log` in bounded batches and survives restarts, deletions and filter changes made while it runs; completion criteria; and **truthful availability** while it runs (a query on a partly populated field says so, and a prefilter never treats an unpopulated field as "no match"), built on the existing `missing_fields()` / `unavailable` reporting in `rebuild.rs`. Fields SB-6 can compute from the store (file `size`, `pieces_root`, file category) are filled by SB-6's own blue/green rebuild, which X-search-files-3 relies on. Each producer's acceptance tests SB-6 → import → producer on an existing database, including a restart mid-backfill.
  - **Cost.** Producers that land after the 50M import rewrite the corpus once each. Either they land before the 50M import, or their backfill time, disk and RSS are measured at 10M and budgeted for 50M. The 50M approval names which fields were populated; a later producer re-checks the affected budgets when its backfill runs.
- The Torznab server; `.torrent` upload and folder watch (upload needs an **owner** exception to the JSON-only middleware); grouping (MinHash, collapse-by-group, "same file"); the regex prefilter and time-budgeted verify; the enrichment harness and plugins; the usenet tier and IRC announces; the Stremio addon and bitmagnet export.

**Track B: backlog** (fills spare slots; each is small):
- S-blocklists (running);
- F-filter-standin-undo (#367);
- openapi-example-size (#368, in review);
- X-strip-format-chars (strip every Unicode Cf character in feed titles and digests, from #366's review), running; #367 merges first and this slice brings `main` in (both touch `crates/api/src/digest.rs`);
- i9-stage2 (print the `a..<b` range form), which waits until the release after the one that ships its parser;
- X-settings-overview: replace the placeholder Settings Overview with needs-attention and changed-from-default (§10.10.8);
- X-overflow-links: the overflow menu sends Import and Backup to unregistered screens ("unavailable") and lacks Logs and the Syntax guide;
- Logs event labels (~200 events × 2 messages × 2 locales);
- FX-4's off-ramp `.css` spacing onto the scale;
- a polish slice for `carry_forward` (118 items, over the 15 threshold in `runner/README.md` step 9);
- **X-required-web-checks**: before the owner makes web and E2E checks required (§2.28), add an always-reporting aggregate check per workflow, with explicit applicability (which paths, including backend changes that need E2E) and failure propagation. Today the jobs are named `check + test + build + size` (`web.yml`) and `playwright (core, matrix, sample, motion, keyboard)` (`e2e.yml`), and both workflows are path-filtered, which `runner/README.md` says would stall unrelated PRs if required. Take the exact check names from a real run, show a web-changing PR and a docs-only PR both reporting, and keep snapshot regeneration separate from comparison;
- **D-egress-note**: the user guide says plainly that blocklist list fetches ignore `HTTPS_PROXY` and use the container's own route (a whole-container VPN still applies), per §2.28's acceptance of #362. The in-app copy under Settings → Blocklists is **X-egress-copy**, a follow-up web slice after S-blocklists (#370), which isn't widened.

### 0.3 Owner queue
What only the owner can do, ordered by what each unblocks. The live list, with the smaller items, is `owner_queue` in `runner/dashboard.yaml`. The runner keeps working on everything else meanwhile.

| # | Action | Unblocks |
|---|---|---|
| 1 | Book the next crawler run on the experiment host (CR-AB-ADM), and keep the runs back to back (`ext:experiment-host`) | Track C and the v0.6 tag |
| 2 | Do the M6 manual Unraid and MSI installs and approve the rc's signing (`ext:owner-m6-manual`) | G-m6 |
| 3 | Tag and publish v0.5.2 once its gate passes | A current release; `ext:release-after-parser` (i9-stage2) |
| 4 | Book each import pilot on the import host and restore the Bitmagnet dump (`ext:import-host`) | The 1M, 10M and 50M gates |
| 5 | **Checkpoint 1**: re-decide the crawler gate if the projection falls short (`ext:owner-checkpoint-1`) | CR-AB-1, CR-5, CR-COMB |
| 6 | Register the dedicated benchmark runner (`ext:bench-runner`) | G-v1-10m |
| 7 | Approve the per-worker consent wording (`ext:owner-v08-q3`) | PL-7b |
| 8 | Complete the cloud operator setup (`ext:owner-operator-setup`) | OP-G |
| 9 | Name the Windows host for the 1.0 soak and free the Unraid box for the 1.0 Linux soak (`ext:windows-soak-host`, `ext:unraid-soak-host`) | G-v1-windows, G-v1-soak-linux |
| 10 | Approve the 50M numbers (`ext:owner-50m-approval`) | The full library and CR-3 |
| 11 | Provide a code-signing certificate (`ext:owner-signing-cert`) | Signed Windows artifacts |
| 12 | Tag the 1.0 release candidate, then v1.0.0 (`ext:owner-v1-rc`, `ext:v1-tagged`) | The release |

**Any time:** add `web-ok` and `e2e-ok` to the main ruleset's required checks (§2.37); replace the published v0.5.1 release text; add the Cloudflare secrets so the status connector deploys (#742); delete the branch `wip/CMP-0-catalogue`; say whether a whole-index Wikidata backfill counts as "bulk" (`docs/decisions/2026-11-12-runner-notes.md`, ENR-14); the runner calls open to a veto (§2.19) listed in the dashboard's owner queue.

**Only if the Nightly channel is offered:** the nightly signing key (§2.52) and the host for nightly update manifests (`ext:owner-pk-nightly-host`).

**After 1.0:** the Unraid Community Apps submission (§2.37) and winget and other package installers (§2.52).

**Questions the reviews and the plan raise (2026-11-13).** The owner's answers to the UI review are recorded (§2.66, runner PR #809); what is still open:

| # | Question | From |
|---|---|---|
| 13 | FILTER-01 (content safety): the review offers two fixes; which one? The detail is held (`docs/security/v1/README.md` §5) and the runner gives it to you directly | Code review |
| 14 | APIFEAT-01 (rules): which of the review's two options for how your own rules rank against packs and lists? Detail held as above | Code review |
| 15 | FILTER-03 (content safety): which of the review's two fixes? Detail held as above | Code review |
| 16 | INFRA-01: should the image publish wait for the same `release` environment approval as the binaries? For the v0.5.2 tag it needs a release PR to `release/v0.5.2` before the tag | Code review |
| 17 | Eight proposals that change §10 or the glossary (UI review part 4, section 8): name the app's intended reader in §10.0; reorder Search around words and clicks, with new default facets and row facts; a plain health summary ahead of the Insights story; three required setup steps and a checklist in place of nine inline steps; lead the network step with safety, not port forwarding; accept GB as typed, or a units preference; "alert" as the single noun; a first-use prompt, or default off, for single-key actions that launch another program | UI review part 4 |
| 18 | Does the v0.6 tag wait on the crawler gate only (P4), or also on WK-G, CMP-G and G-m6? The plan follows P4 until you answer | Plan r4 |
| 19 | Do Track Q's fixes for the hostile-input group go ahead of ladder rungs 2 and 3? Until you answer, the runner places them as it places any new fix slice (§2.33), so they don't wait; the question is only whether they go ahead of those rungs | This plan |
| 20 | Open draft security advisories for the held findings, then delete the review branch that holds their detail (the runner gives you both directly) | Code review |
| 21 | Approve or change the proposed replacement sentence about the torrent cache in the Legal Notice (§2), which gates X-consent-gate-lift-external, not yet in `runner/slices.yaml` (`docs/decisions/2026-11-12-X-consent-gate-lift.md`) | Runner PR #807 |

**Runner calls open to your veto (§2.19):** every restore path carries recorded holds and deletions, Windows included (UPDATE-01, UPDATE-05, STORE-01; the rule is already recorded in `docs/decisions/2026-11-12-X-win-upd-manual-rollback.md` follow-up 1); SRC-02, the recorded decision in `docs/decisions/2026-11-02-SRC-2.md`, to be settled before X-consent-gate-lift-external (not yet in `runner/slices.yaml`), the Account r2 or the first Restricted or Overlay plug-in lets more sources run; and TEST-05, the detail drawer's size budget (28 KB in the gate against 15 KB in the architecture, a §2.26 call).

**A choice worth making:** the import pilots share the Unraid box with the crawler runs (§2.52). Running them elsewhere would remove the largest calendar conflict. It changes an owner decision, so it's the owner's to make; until then imports fill the gaps between crawler runs (§0.7).

Every question the earlier plans raised was answered, except those in rows 13 to 21 above and the older open points below: §2.28 and §2.37 (Sources, rule packs, required checks, the server-import surface, the upload exception), and the v0.7, companions, workers, M6, v0.8 and M8 questions recorded as met conditions in `runner/dashboard.yaml`. `docs/owner-decisions.md` §3 ("Points still open") predates these; the owner may move answered items into §2.

### 0.4 v0.5.1 gate (and v0.5.2)
**v0.5.1** (G-v051), from the 2026-10-30 plan. **Passed: v0.5.1 was published on 2026-11-07** (`docs/decisions/2026-11-07-v051-release-verdict.md`).
- ✔ Release branch at a fixed SHA (`c3f6341a`); `v0.5.1-rc.N` candidates with hashes (rc.2 `6be95525`).
- ✔ Delta from the soaked `744b84d6` mapped to targeted tests.
- ✔ L1–L7 and F-* merged; the CR-0 soak accepted (`docs/decisions/2026-10-30-CR-0-acceptance.md`).
- ✔ Every workflow green on rc.2, no regenerated baselines.
- ✔ Windows ≥ 6 h service-mode run of the rc MSI, plus a portable smoke test.
- ✔ **24 h Linux Docker soak on the owner's Unraid box** with CR-COL: the binding memory gate (upper 95 % CI of the `anon` private-memory slope over hours 12–24 ≤ 2 MiB/h; report the slope with its CI and the peak). Attempt r3 on rc.2 failed the memory gate (1.88 MiB/h, CI [1.43, 2.33]); attempt r4 on rc.3 failed too (29.35 MiB/h, CI [28.75, 29.96]); **attempt r5 on rc.4 passed** (0.91 MiB/h, CI [0.47, 1.35], `docs/decisions/2026-11-06-soak-r5-verdict.md`). **The upgrade test** (the Oct 22 volume, V3 → V12) passed only at r1's start, and r1 itself isn't gate evidence (it stopped at 19.6 h on the old working-set rule; partial slope 1.85 MiB/h, CI [0.92, 2.77]). Per owner-decisions §2.28, r1's clean upgrade counts if rc.2's migration code matches r1's build (the runner checks); otherwise rc.2 gets a short upgrade smoke. Its stop rule is the old working-set rule, which `docs/decisions/2026-10-31-X-soak-docker-memrule.md` showed could stop a healthy run; a stop on that rule alone is reviewed against `anon` before it counts as a failure.
- ✔ ≥ 2 h gluetun leg.
- ✔ rc.4 = rc.3 + the memory fixes + LEGAL-1r (§2.40), plus the runner-added redaction fix, with its delta mapped (Track R step R3″), then soak r5 on rc.4 (R1″, **passed** 2026-11-06) and a short Windows MSI smoke (R-win, **passed** 2026-11-06).
- ✔ Updates ship off (no nightly key yet; the release key exists).
- ✔ The owner tags, approves the release environment and publishes.

**v0.5.2 status at r4.** `release/v0.5.2` is cut from `main` at `045d4a38` with the feature manifest frozen (`docs/plan/2026-11-06-v052-feature-manifest.md`, delta in its §9; `docs/decisions/2026-11-07-P-v052-cut.md`). The branch holds three candidates: rc.1, rc.2 and rc.3 = `833a0d25`, which is rc.2 plus tokio 1.54.1 and the notice's "applies to" line (#740, §2.58); the release notes on the branch name rc.3 and are checked against it (#751). No `v0.5.2-rc.N` or `v0.5.2` tag exists as a git ref. The owner's long runs were moved to rc.2: the 24 h Docker soak on an upgraded folder with every pack on, the ≥ 6 h Windows run on a fresh folder, and the V12 → V14 upgrade smoke and rollback check (`docs/releases/v0.5.2-rc2-delta.md` on the branch). `main` holds no verdict for any of them. Still to do: the runner states which candidate the gate runs on and what evidence exists; the long runs and their verdict in a release decisions file; the legal-documents check (§2.48); the owner tags; the notes come to `main`, where `docs/releases/v0.5.2.md` is still the first draft.

**v0.5.2 as planned (recommended, runner, flagged in the r2 decisions file).** `main` already carries fixes v0.5.1 lacks (B-update-5..6, the M5 exit fixes and later work) and diverges fast. Cut `release/v0.5.2` from `main` soon after v0.5.1 ships rather than cherry-picking long chains. **Freeze the cut first:** a feature manifest that lists every behaviour enabled in the build with its own acceptance, and every partly landed feature (Sources, M7) as inactive or excluded. The gate is v0.5.1's: the 24 h Linux Docker soak stays the binding memory gate, plus a ≥ 6 h Windows service run, and the delta rule adds re-soaks for crawler lifecycle, sampler or endpoint-policy changes. **A cut from `main` ships one-way migrations** (V13, and V14 if SRC-1 has merged), so the gate adds a V12 → V13+ upgrade smoke on a copy of a v0.5.1 data folder, pinned by its schema version and the snapshot file the upgrade writes (`<db>.pre-v13.db`), and a rollback-to-snapshot check: `lodestar rollback` to that file, then the v0.5.1 binary starts on it and serves. Crawler switches stay off in v0.5.2 unless they've passed C8.

### 0.5 Crawler program
**Evidence so far.**
- CR-0 24 h final (`docs/soak/evidence/2026-10-30-cr0-final/cr0-final.md`): 12.3 stored/min. Fetch success is about 4.4 %. Of 389,644 fetch failures, **74 % timed out**, 15 % were unreachable and 11 % refused. The DHT actor rejected 95.6M peer requests for capacity.
- rc.2 runs: Windows service mode 17.0/min; Unraid r1's 14 h preliminary ~11/min.
- CR-SIM screens (IPv4 only, approximate routing, no churn, so a screen never replaces a live run):
  - CR-ADM: Better on 8/8 seeds in all four scenarios, +26 % to +50 %, capacity refusals to 0.
  - CR-1a-H: inconclusive; ~94 % of hints expire unused.
  - CR-1a-S: +7.5 % in `healthy` only; fetches start 4.6–16× sooner.
  - CR-1b: inconclusive; worse 8/8 on one secondary under admission pressure. Expected to stay off.
  - CR-4: can't be screened (the simulator has no transport layer).

**Reading.** The gate needs about **8×**. The failure mix points at H3 (reachability and transport: TCP timeouts are what uTP-only and NATed peers look like) and H2 (admission churn), more than H1 (lookup latency). So the order is: CR-ADM and CR-4 first, then the H1 candidates.

**Before any live run** (r3):
- **CR-COL-3 (tooling).** The Linux collector and analyzer must keep and judge the gate's own evidence (Track C step C0a). Without it a completed run permanently lacks its primary outcome and the checks that prove a treatment was active.
- **CR-SPEC (measurement specification).** A decisions file, frozen before collection and not changed after any data is seen. §2.25 makes the exact measurement rules refinements **the owner confirms before the first acceptance analysis**, so CR-SPEC is flagged to the owner and confirmed **before any data exists**: the first booked run (the comparator, CR-AB-COMP) and CR-COMB both depend on `ext:owner-cr-spec`. CR-COL-3 implements its rules, so it follows CR-SPEC. It fixes:
  - **the measured window:** it starts at a stated boundary after suppression ends plus a stated warmup, for every run including the 24 h acceptance soak on a fresh directory (whose 6 h first-start suppression is otherwise inside or outside the mean by accident); hours are consecutive 60-minute intervals counted from that start (not wall-clock aligned, not rolling);
  - **carry-over between arms:** a fresh data directory per arm, or a stated wash-out, and how CR-0d's `dht.safe_restart` is set across arm switches;
  - **the cohort:** which events count towards an arm (by first sighting inside the window), and how latency samples that straddle a window edge are treated;
  - **latency coverage:** the maximum share of dropped latency samples (`discovery_to_searchable_dropped_total`) for which a p95 is reported at all; below that coverage the p95 is "not measured", never a pass. Dropped samples are counted as above the threshold when bounding the p95. Unfinished work at the window's end is reported, not dropped;
  - **mechanism checks** per treatment (for example uTP connects and `dht_utp_in_total` for CR-4), so an arm whose switch didn't take effect is void, not negative;
  - **the analysis:** effect size with a confidence interval per treatment, and what counts as a win.

**Order** (Track C table, §0.2):
1. CR-4 is merged (#365).
2. Run the pinned **bitmagnet comparator early**, before tuning, on the same host and IP, on its own day, from fresh data. It shows what 100/min means there.
3. Live ABBA runs per CR-SPEC (6 h measured, ≥ 2 repetitions per arm, treatment boundaries and host activity logged, per-family counters): **CR-ADM**, then **CR-4 uTP** (`dht.utp`), then route backoff (`dht.route_backoff`) **only if a baseline shows `fetch_no_route_total{family="ipv6"} > 0`**, as CR-4's decision requires; it expects no measurable effect, and a skipped arm is recorded, not run. Measure each alone.
4. **Checkpoint 1** (owner's choice, 2026-11-02). After the CR-ADM and CR-4 results, the runner projects the reachable single-node rate from the measured gains and the comparator, against all three thresholds (the 24 h mean, the worst hour and the p95 discovery-to-searchable). **It's a projection with stated uncertainty, not a measured ceiling:** isolated gains aren't necessarily additive or multiplicative, and 6 h arms don't bound a 24 h worst hour. The failure breakdown justifies testing transport first; it doesn't say how many timeouts uTP recovers. Only CR-COMB's 24 h soak measures the gate.
   - If the projection reaches ≥ 100/min: continue with step 5.
   - If it doesn't: the runner reports the projection and its uncertainty, the dominant limiter (from the failure and rejection breakdowns), the comparator's rate and the options to the owner **before** CR-5. The owner re-decides. **The runner never loosens the gate.** Sources, workers and M7 continue meanwhile; only the v0.6 tag waits.
5. A/B for CR-1a-S, CR-1a-H, CR-1b; build CR-1c (a lookup cap sized to the intensity preset) after the CR-ADM result.
6. CR-5 (profile-driven tuning, including the verified-known cache): profile first, then tune only on profiler evidence, or record a reasoned skip. Either way it completes, and CR-COMB waits for it.
7. **CR-COMB:** combine the winners, make them default on, and run the 24 h acceptance soak on the gate metric. If the host, its public IP or its network changed since the comparator ran, re-run the comparator first.

**Still unscheduled:** CR-2 (fairness tiers) and CR-3 (durable partial upgrades; Track I step 6). A dual-stack default DHT bind (`[::]`, from CR-0f's follow-ups) and BEP 33 piggyback are CR-4 follow-ups, taken only if CR-4's per-family counters show IPv6 coverage matters.

**Rules for every run:**
- on the experiment host (the Unraid box, §2.51), never overlapping a soak on it;
- the same network setup for every run (DHT UDP port not forwarded, Lodestar UPnP off, router unchanged, same public IP); after a change the runner re-runs the comparator before the next run (§2.51; stricter than CR-SPEC rule 6.8);
- no imports on that host during a run;
- the comparator on separate days, never back to back with a Lodestar run, version pinned;
- Defender exclusion recorded on Windows;
- packets/s, bytes, connects/s, open sockets and router/NAT health recorded;
- BEP 51 intervals, per-IP limits and the §18 politeness caps are never tuned away.

### 0.6 Gates added by this plan
**Crawler (gates the v0.6 tag):**
- **What counts:** unique infohashes fetched and verified from the network in this run, committed and searchable. Imports, recovery, rediscoveries, filtered or quarantined items and failures are reported separately. Rows a rule hides still count as searchable (`docs/decisions/2026-10-30-CR-gate-metrics.md`).
- **Where:** one node, one public IP, a fresh data directory with no imports, the Balanced preset.
- **Target:** ≥ 100 per minute sustained, comparable to a pinned bitmagnet run. The stretch target is 1,000/min, which remote workers serve.
- **Thresholds** (decided 2026-10-31 under `docs/owner-decisions.md` §2.26): 24 h mean ≥ 100/min, no hour below 70/min, p95 discovery-to-searchable ≤ 10 min. That figure runs from the first sighting of the infohash and includes the metadata fetch; it doesn't replace §1.6's "new torrent → searchable < 5 s", which starts once the metadata is stored.
- **How it's measured:** per CR-SPEC (§0.5), frozen before collection and confirmed by the owner: the window's start boundary, 60-minute intervals from that start for the worst hour, the cohort, and the latency-coverage rule under which a p95 counts.
- Politeness and network guardrails are never relaxed to reach it. Checkpoint 1 (§0.5) is where a shortfall goes to the owner.

**Import scale:**
- **The ladder is offline-only:** `lodestar import` runs with the server stopped (the CLI refuses to write while it runs). Online import (through the running server's writer, with writer-share fairness and search latency under import) belongs to B-import-server-2, which is parked, so the pilots don't claim it.
- Runner smoke stages at 10k and 100k, then the owner's ladder: **1M → 10M**, each measured against the budgets in `docs/owner-decisions.md` §1.6, including about 2.3 KB per torrent all-in. Any excess, with what drives it, goes to the owner before the next stage.
- Then a measured **50M gate**, whose numbers go to the owner before the full corpus (~40–50M) is imported.
- Measured at every stage, never estimated (`docs/decisions/2026-10-30-master-plan.md` "Imports"): bytes per record, source copies and PostgreSQL restore space, backups, the old and new index during a rebuild, the have-filter, `filter_hits` and the review queue, relevance (the existing `relevance` judged fixture as a regression check only: it builds its own synthetic corpus, so it says nothing about ranking on pilot data; NDCG on real data waits for P7's judgment set), backup and restore time, cold start, RSS, search p95 (including cold file search), Insights overhead. A backup precedes each stage; removal by job provenance is tested.
- **Import speed** (runner, `docs/decisions/2026-10-31-IMP-4b.md`): 2,000/s offline is the pilots' pass line. It's measured end to end, to the job's completion with every record durably written **and** indexed, and reported in two parts, write time and index drain, so the synthetic insert figure (13,900/s) can't stand in for it. The 1,000/s online line is B-import-server-2's acceptance, not the pilots'.
- **The 10M stage has two parts:** steady-state import into a v6 index (G-import-10M), and the v5 → v6 blue/green rebuild from a pinned pre-SB-6 corpus (G-rebuild-10M). Only the second is rebuild evidence.
- **The 50M approval names the populated feature set.** Passing while trigram, grouping or signature fields are empty doesn't approve their future disk, rebuild and RSS cost; each producer re-checks the affected budgets when its backfill runs (Track M7, SB-6).
- After the gate: CR-3, then a subset of the wizard, then the other import families (Rats, dhtc, MongoDB, …; `docs/plan/2026-10-29-dht-import-wizards.md`).

**Other gates, for reference:**

| Gate | Where it's defined | Status at r4 |
|---|---|---|
| G-v051 | §0.4 | **passed**; v0.5.1 published 2026-11-07 |
| G-v052 | §0.4, manifest §6 | cut done; rc, soaks and tag open |
| SRC-G (v0.6 sources) | plan `2026-11-01-v06-sources.md` §4.2 | **passed** (`docs/decisions/2026-11-07-SRC-G.md`) |
| G-v06-crawler | above | open: 12.3/min against 100/min |
| G-import (1M, 10M, rebuild 10M, 50M) | above | open: no pilot has run |
| WK-G, the §19.8 chaos gates (workers, v0.6) | architecture §19.8, plan `2026-11-06-v06-workers.md` | open |
| CMP-G (companions, v0.6) | plan `2026-11-06-v06-companions.md` §4.3 | open |
| G-m6 | architecture §14, plan `2026-11-06-m6-gap-audit.md` | open; waits on the owner's manual installs |
| G-v07-scale, G-v07 | architecture §14 and §18.9, plan `2026-11-05-v07-m7.md` | open |
| G-v08 | architecture §19.8 and §18.9, plan `2026-11-06-v08.md` | open |
| §20.8 filter budget (50 µs) | architecture §20.8, owner §2.20 | held; the Neutralize case is reported, not held, at ~55 µs on Intel 8573C runners. Checked on the reference CPU in G-v1-audits |
| G-v1 and the eleven G-v1-* gates (CI on the rc, relevance, 10M performance and size, crawler re-measured, Windows soak, Linux soak, chaos, audits, security, design, UI) | architecture §17, plan `2026-11-06-m8-hardening.md` | open; relevance has a synthetic NDCG@10 of 0.9186 |

### 0.7 Dependencies and serial points
```mermaid
flowchart LR
  R[v0.5.1 soak + tag] --> EP[Unhold E-M6, workers, companions]
  R --> V52[v0.5.2 cut]
  CR4[CR-4 merged] --> AB[A/B: CR-ADM, CR-4]
  TOOL[CR-COL-3 + CR-SPEC] --> COMP
  HOST[(owner: experiment host, booked per run)] --> COMP[bitmagnet comparator]
  COMP --> AB
  COMP --> CP1{Checkpoint 1}
  AB --> CP1
  CP1 -->|on track| AB2[A/B: CR-1a, CR-1b; CR-1c; CR-5] --> COMB[CR-COMB 24 h] --> G6C[G-v06-crawler]
  CP1 -->|short| OWN[(owner re-decides)]
  SRC1[SRC-1 V14] --> SRC2[SRC-2] --> SRC7[SRC-7]
  SRC7 --> SRC10[SRC-10a..d, 11, 12, 13] --> SRCG[SRC-G]
  SRC2 --> SB6[SB-6 schema bump] --> SRC3[SRC-3] --> SRCG
  SB6 --> I10[G-import-10M]
  SB6 --> RB10[G-rebuild-10M v5 to v6]
  I1 --> RB10
  RB10 --> I50
  I1[G-import-1M] --> I10 --> I50[G-import-50M] --> CR3[CR-3, wizard, import families]
  EP --> W[Workers 1-6] -.ships in.-> V06
  G6C --> V06((v0.6 tag))
  SRCG -.ships in.-> V06
  EP --> K[Companions] -.ships in.-> V06
```

**Things that run one at a time:**
- **Migrations:** one migration PR open at a time (`ext:migration-slot`), written only by an Opus slice that owns the store schema. V12 → V13 → **V14 SRC-1** are merged. Four slices wait for a number: WK-12 (v0.6), SRC-3b (v0.7), PL-8 (v0.8) and CR-3 (after the 50M gate). `runner-state` `migration_order` and several plans still reserve **V15 for CR-3**, which lands last. r4 recommends the order **WK-12, SRC-3b, PL-8, CR-3**, each taking the next free number when it reaches the slot, which means releasing CR-3's reservation; the numbers are the runner's to assign, and until it does, V15 stays CR-3's. The runner records the queue in `runner-state` `migration_order`.
- **Search schema:** a released user rebuilds the search index once per release. Normally that's one `SCHEMA_VERSION` bump per milestone. M7 has two, SB-6 (v6) and X-search-files-5 (v7), both before the v0.5.2 cut, so users rebuild once, v5 → v7 (runner, 2026-11-05; the rebuild always goes straight to the current version). A slice that needs a new indexed field asks the milestone's open bump to carry it instead of bumping again.
- **The owner's machines:** the Unraid box finishes the v0.5.1 soak first. After that, the experiment host runs A/B arms, comparator days and acceptance soaks, and the import host runs pilots. If they're the same machine, the runs never overlap; the order is soak → comparator → A/B → pilots between A/B blocks. **Bookings are exclusive and per run:** `ext:experiment-host` and `ext:import-host` hold for one booked run, not once for all, so two gates can't share the host because their `owns` happen to be disjoint. **At r4** the v0.5.1 soak and the comparator are done; the order from here is: the v0.5.2 soak first, if it hasn't run (ladder rung 2); then crawler A/B blocks, with an import pilot in each gap between them; and the 1.0 Linux soak and crawler re-measure last, on the release candidate.
- **The experiment calendar** (the slice cap can't shorten this serial path):

  | Run | Measured time |
  |---|---|
  | bitmagnet comparator | 1 day |
  | CR-ADM, CR-4 uTP (ABBA, 2 reps × 2 arms × 6 h each) | 2 days |
  | route backoff, only if the baseline shows IPv6 no-route events | 0–1 day |
  | CR-1a-S, CR-1a-H, CR-1b, CR-1c | 4 days |
  | CR-COMB 24 h acceptance | 1 day |
  | **Total measured** | **8–9 days** |

  Add each arm's start-up: a fresh data directory means the 6 h sampling suppression plus warmup before measurement starts, unless CR-SPEC adopts a crash-safe restart between arms. Add a retry budget of about 30 % for interruptions and repeats. **Plan for roughly three weeks of exclusive host time**, and keep the v0.5.x soaks and the import pilots off those days. The runner keeps the booking calendar in `runner-state` and states the next booking in every daily summary.
- **CI capacity:** each push starts about 37 jobs that every PR queues behind. One push per round of fixes.
- **Release branches:** fixes for a release land on `main` first and are cherry-picked; nothing is developed on a release branch.
- **Hub files** (§1.3 table): unchanged.

### 0.8 Anticipated complications

| Complication | Why it matters | Mitigation |
|---|---|---|
| The crawler gap may be structural | One IP and BEP 51's per-node intervals may cap discovery well below 100/min, whatever we tune | The early comparator shows what's possible on the same IP; Checkpoint 1 brings it to the owner before CR-5; workers (more IPs) serve the stretch |
| uTP can't be simulated | CR-SIM has no transport layer | Live A/B only; `dht.utp` stays off until it wins; the Settings field stays labelled "not active yet" so a saved value can't decide an A/B arm |
| The owner's machines are a bottleneck | Soak, A/B arms, comparator days and pilots all need them, and each needs exclusive use | §0.7 calendar; the runner states the next machine booking in every daily summary |
| A host plugin interrupts a run | r2 was stopped by CA Docker Auto Update; Appdata Backup can stop containers | Host checklist before every run: exclude the containers from auto-update and backup plugins and record the next scheduled plugin runs |
| Search rebuild at scale | A blue/green rebuild needs about 2× index disk and hours at 50M | SB-6 lands before the 10M pilot; G-rebuild-10M measures a real v5 → v6 rebuild at 10M |
| New fields stay empty on old records | A field added by SB-6 isn't filled on records indexed before its producer ships, so queries and prefilters would be wrong | SB-6's population contract: per-field generation, resumable backfill, truthful availability; backfill cost measured at 10M and named in the 50M approval |
| Experiment evidence is lost | The Linux collector drops the gate's own series | CR-COL-3 and a rehearsal before any booked run |
| A measurement decided after the data | Window boundaries, coverage and cohort rules chosen late can bias the gate | CR-SPEC, frozen before collection |
| Two agents build at once | A stale runner timestamp doesn't prove the runner stopped; the watchdog restarts it after 4 h | The explicit handover in [handoff §5](handoff.md#5-path-b-a-single-agent-without-the-runner): `HANDOVER.md` on `runner-state`, honoured by both paths |
| Migration contention | Sources, CR-3 and workers each want a migration | §0.7 queue, one at a time |
| Unverified items in the id sequence | They grow the hot-column arrays | SRC-2 measures RSS at 1M unverified ids; the Q11 cap (1M dead hashes) |
| Triggers on the crawler's hot path | V13's triggers add up to 4 index probes per insert | No new trigger on `torrents` inserts for Sources; unverified rows reuse V13's trigger pattern (P-v06-sources-plan, Q16); measure insert cost at each import stage |
| The filter budget on common runners | The Neutralize case projects ~55 µs on Intel 8573C against 50 µs | Reported, not held (`docs/decisions/2026-10-30-F-filters-neutral-perf.md`); revisit if rule packs add Neutralize terms |
| File search at 10M | Cold p95 projected over 150 ms | SB-6 adds the field, X-search-files-3 uses it, both before the 10M pilot |
| `main` outruns the release branch | Cherry-pick chains get long and risky | v0.5.2 soon after v0.5.1 |
| Promises the binary can't keep (r3; the `worker` command and the Settings Overview have since been built) | The Unraid worker template calls a missing `worker` command; Settings Overview is a placeholder; menu links lead to "unavailable" | Track M6 gap audit, Track B items |
| Stale runner state | `state.json` shows merged slices as building; at r3 its `migration_order` stopped at V11 (it now runs to V15) | A successor reconciles against `main` first ([handoff](handoff.md)); the runner trims `state.json` and moves history to `log/<id>.md` (`runner/README.md` step 10) |
| Usage runs out mid-slice | A session stops with a half-done branch | Small PRs, one decisions file per slice, the handoff; a branch with no PR is restarted from its brief, not resumed blind |
| Toolchain debt | MSRV 1.89 and one temporarily allowed clippy lint, `chunks_exact_to_as_chunks` (`Cargo.toml`) | Remove the allow when CI's clippy reaches 1.99 (runner follow-up) |
| Owner answers arrive late | Owner actions (host, keys, tags) gate whole tracks | §2.28 delegates decisions to the runner's recommendation; ask once for actions, and fill the slots with the ladder's other rungs meanwhile |

### 0.9 Next actions
In ladder order, as slots allow (§0.2; no two with overlapping `owns`):
1. **Land the open PRs** (the live list is GitHub's open pull requests). #632 waits for the owner (§0.2 rung 1).
2. **Bring the records up to date** (runner): the slices missing from `runner/slices.yaml`, the gates and notes in `runner/dashboard.yaml` that still describe 2026-11-08, the owner decisions made after §2.60, and `state.json`'s slice statuses. The list is in `docs/decisions/2026-11-12-master-plan-r4.md`.
3. **v0.5.2:** state which candidate the gate runs on (rc.3 = `833a0d25` is the newest) and what evidence exists, check INFRA-01 (the image publish on a version tag; the workflow that runs is the one at the tagged commit), finish the gate in §0.4, give the owner the go for the tag.
4. **Launch what's ready** (at 2026-11-12T22:55Z, not yet in a slot): ENR-G, X-unverified-cap-index, X-win-upd-first-rollback, ADH-0, UP-2, X-watch-env, X-watch-stall-retry (once the runner adds it).
5. **Assign the next migration numbers** (§0.7): WK-12 first, since WK-13, WK-16, WK-19 and WK-G all wait behind it; then SRC-3b, PL-8 and CR-3.
6. **As each merges:** GS-2 → GS-3, GS-4, RX-1; ENR-14 → the twelve enrichment plug-ins; NOS-2a → IRC-0; WK-12 → WK-13; X-cmp-egress-api → X-cmp-egress-ui.
7. **The host calendar:** the v0.5.2 soak if it's still owed, then the owner books CR-AB-ADM; the runner states the next booking in every daily summary and proposes an import pilot for each gap.
8. **Gates as their slices finish:** WK-G, CMP-G, then G-v07-scale and G-v07, then G-v08.
9. **M8** alongside all of this, the security reviews first; then R-v1-cut and the 1.0 gates.
10. **The reviews' fixes** (r4.1): B1 and B2 (launched 2026-11-13, owner); then the guards and the Sources restructure in #809; the runner turns Track Q and the UI review into slices (neither X-watch-stall-retry nor X-utp-spurious-rto is in `runner/slices.yaml` yet), sends the held findings' detail to the owner for draft advisories, asks the owner the questions in §0.3 (13 to 21), and decides what comes to `main` from the review branches.

### 0.10 History of this section
- **2026-10-30:** the post-CR-0 plan: lanes L (land the owner's work), F (release blockers), C (crawler candidates behind switches), I (imports), S (Settings reconcile), D (docs), and the v0.5.1 gate. All lanes finished by 11-01. Its text is in git history (`git log -p docs/build-plan.md`) and its reasoning in `docs/decisions/2026-10-30-master-plan.md`.
- **2026-11-02:** the r2 rewrite (`docs/decisions/2026-11-02-master-plan-r2.md`), then §2.28's answers (#371), then r3 after an external review (`docs/decisions/2026-11-02-master-plan-r3.md`).
- **2026-11-12:** r4 (`docs/decisions/2026-11-12-master-plan-r4.md`). The status (§0.1), the ladder (§0.2), the owner queue (§0.3), the gate table (§0.6) and the next actions (§0.9) are rewritten from `main`, the dashboard and `runner-state`; §0.4 and §0.7 are brought up to date. The r3 text of those sections is in git history.
- **2026-11-13:** r4.1 (`docs/decisions/2026-11-13-master-plan-r4-reviews.md`): the header, the "not yet recorded" notes for §2.62 to §2.64 (now recorded), §0.2 (the owner's B1 and B2 priority and the review tracks Q and U), §0.3 (nine questions for the owner, the veto list and the closing sentence), §0.9 (item 3 and a new item 10) and §1.2's cap line.

---

## 1. Orchestration model

### 1.1 Who does what (CLAUDE.md policy, made operational)

**Main session (Opus 5.5, high effort): the integrator.** It owns:
- **Rust: search.** Leases, rebuild, facets, suggest, percolator, regex prefilter.
- **Rust: store.** Migrations and on-disk formats.
- **Rust: API.** Contracts, auth and keys, SSE, insights queries.
- **Rust: protocols and untrusted input.** The cluster and mTLS, uTP, Torznab query mapping, `.torrent` import, and hand-written feed, XML and IRC parsers.
- **Rust: other security-critical code.** Companion signature checks, credential storage and the SSRF guard.
- **Security-sensitive UI state machines.** Setup token, 18+ confirmation, §20.11 password and effect warnings, CSX-5.
- **Every review and merge.**

**Sonnet helpers** take everything well specified:
- web foundations and screens per §10, and the theme compiler;
- formatters, written to a main-session spec and reviewed by the main session line by line;
- the typed client and URL state, reviewed by the main session;
- test harnesses, CI workflows, packaging and docs;
- download-client and notification adapters behind a trait the main session defines;
- plugin glue, manifests and recorded-fixture tests, after the main session lands plugin #1 of each family;
- serde-derived JSON clients;
- simple CRUD endpoints that follow an established pattern.

**Haiku helpers** do mechanical work only: generated theme files, message-key extraction and renames, formatting sweeps.

**What's never delegated:** parsers of untrusted input, on-disk formats, concurrency-heavy code, security and content-safety logic.

To protect the critical path, **the main session writes almost no Svelte.** Its time goes to Rust and to reviews.

A non-Claude agent picking up the work maps this to "use your strongest model for core, parser, store, concurrency and security work, and never hand those to a weaker one" ([handoff §5](handoff.md#5-path-b-a-single-agent-without-the-runner)).

### 1.2 How a helper runs
- **Launch:** the runner starts each slice in its own cloud session (`runner/README.md`). Inside a session, a helper is an Agent with `isolation: "worktree"`, `run_in_background: true` and the model above, branched from `main`.
- **Brief:** every slice gets one, using the template in §7. The runner keeps them on the `runner-state` branch under `briefs/`.
- **Cap:** from 2026-11-13T00:00Z, four build slices and four cloud-testing slices at a time (§0.2; owner §2.64), and filling them all isn't required. `runner/slices.yaml` `limits` (16 core + 16 helpers, §2.15) are the ceiling once the owner lifts it. Review bandwidth is the bottleneck, not how many agents can be spawned.
- **Review gate** (for every result):
  - read the whole diff;
  - run `npm run check`, `npx vitest run`, `npx size-limit` and the affected Playwright specs;
  - run `cargo fmt --check`, `clippy -D warnings` and `test` wherever Rust is touched;
  - run `/code-review high` on diffs over 400 lines;
  - run `/security-review` on anything touching auth, credentials, filters, the cluster, companions or source plugins;
  - redo weak output on Opus rather than patching it piecemeal.
- **Unanchored second read** for the riskiest PRs (leases and cursors, rebuild and swap, percolator, mTLS join and fencing, uTP, companion verification, the filters API, plugin consent). A fresh Opus review agent gets only the diff and the spec sections, not the author's conclusions.
- **Workflow scripts are optional.** Any wave can run as a `Workflow` script instead of hand-launched agents, but only if the owner explicitly says "use a workflow".

### 1.3 Seams that make parallel work safe
1. **Contract-first API.** Typed DTOs with utoipa produce `/openapi.json`; a bin writes `web/src/lib/api/openapi.json` and `openapi-typescript` turns it into `schema.d.ts`. CI fails if either is stale. New endpoints can land first as typed structs plus a `501` stub with an example. Helpers use only the generated types; a missing field is a request to the main session, never a local type.
2. **Fixture backend.** `lodestar serve --fixture` builds a deterministic data directory; screens and E2E run against it.
3. **Token contract.** `docs/design-tokens.md` fixes every CSS custom-property name.
4. **Nullable fields.** "Never measured" is `null`, so the UI's "—" state is built and tested before a source exists.
5. **Points that must run one at a time:**

| Shared thing | Rule |
|---|---|
| `crates/store/src/schema.rs` migrations | One migration PR at a time (`ext:migration-slot`), Opus only, in the queue in §0.7 (V14 → SRC-1 next) |
| `crates/search` `SCHEMA_VERSION` | One rebuild per release, through the blue/green rebuild. M5's was v5 (facets). M7: **SB-6** (v6), which carries SRC-3's `verified:`/`source:`/`network:` fields, `payload_sig`, the trigram field and file category/`pieces_root`, and **X-search-files-5** (v7), the torrent-level file size bucket terms and B13's title-identity key, both before the v0.5.2 cut; users rebuild once |
| `crates/api/src/lib.rs`, `crates/lodestar/src/services.rs` | Each slice adds one line; merged in sequence |
| `openapi.json` / `schema.d.ts` | Regenerated after bringing in `main`, never merged by hand |
| `web/package.json` and the lockfile | A new dependency needs main-session approval |
| `.github/workflows/*` | `ci.yml` (Rust, with `ci-ok`), `web.yml`, `e2e.yml`, `release.yml`, `freshness.yml`; scheduled jobs live inside `ci.yml` and `e2e.yml` (there is no `nightly.yml`). One owner at a time |
| Messages | One message file per feature (`features/*/messages/{en,zh}.json`), merged by a script |

---

## 2. Sequencing and critical path
```
P0 seams ─► P1 v0.5 MVP ─► P2 M5 ──────────────► v0.5.1 ─► v0.5.2
   (done)       (done)       (done 11-01)            │
                                                     ├─► P3 M6 (mostly built; gap audit)
                                                     ├─► P4 v0.6: Sources ∥ workers ∥ companions ∥ crawler gate
                                                     └─► P5 M7/v0.7 (SB-6 first) ─► P6 v0.8 ─► P7 M8 → 1.0
```
- **Critical path to v0.6:** the crawler gate (§0.5), which needs the owner's experiment host and may need the owner's re-decision at Checkpoint 1; in parallel, the Sources path SRC-1 → SRC-2 → SRC-7 → SRC-10* → SRC-G.
- **Critical path to 1.0:** M7's SB-6 and the import scale gates feed P7's 10M/50M performance gates.

---

## 3. Phases and slices
Each item is one PR of about 1,500 changed lines or fewer. **[M]** = main session or an Opus slice, **[S]** = Sonnet, **[H]** = Haiku.

### Done: P0–P2 (M0–M5)
- **P0 seams:** router split and error envelope (B0), typed DTOs and OpenAPI (B1), embedded SPA (B3), settings registry (B2), web foundations (W0–W9), the contract docs.
- **P1 v0.5 MVP:** blue/green rebuild (B5b), searcher leases (B5a), the M5 migration (B8, V10), SSE (B4), facets (B6, search schema v5), the fixture server (B16), primitives, charts, formatters, app shell, search UI, detail drawer, Docker. G-v05 passed 2026-10-24.
- **P2 M5:** Insights (B11a–c), suggest (B7), API keys and feed tokens (B12), percolator and alerts (B9), result cache (B5c), siblings/compare/discover (B13), stats and crawler control (B14), export and jobs (B10b), filters API (B15), settings API and form engine, every Settings screen, the wizard, update engine and screens, logging core and screens, Filters & Safety, keyboard map, Playwright matrix and Lighthouse, zh catalogue. **G-m5 passed 2026-11-01** (`docs/decisions/2026-11-01-G-m5.md`).
- The standing rule from P1, "the client never computes counts, cohorts or percentages", is in `runner/standards.md` §4.
- Every slice's decisions are in `docs/decisions/`; the slice graph and its history are in `runner/slices.yaml` and on `runner-state`.

### P3: M6 packaging (mostly built; Track M6)
Built: the clap CLI with `backup`, `rollback`, `vacuum`, `reindex`, `stats` and more; the Windows service entrypoint, port warnings and runtime work; the WiX MSI (service, firewall rules, opt-in Defender exclusion) and portable zip; Unraid templates (main, gluetun, worker); the systemd unit, Linux tarballs and the `release.yml` matrix; smoke jobs; the user guide in `docs/user/`.
To audit and finish (E-M6 expansion): FUSE and network-filesystem detection; the worker template's v0.6 labelling; the user guide's v0.5.1 refresh; anything else the gap audit finds.
**Owner actions:** the Unraid CA submission, after 1.0 (§2.37). The code-signing certificate is deferred (§2.23): Windows builds ship unsigned, and the guide covers the SmartScreen warning.

### P4: v0.6
**Remote workers v1 (§19), Opus, about 6 PRs.** Built: cluster CA, mTLS and one-time join tokens; the postcard-over-WebSocket protocol, heartbeats and fencing (library only). To build: wiring and the `worker` command, have-filter replication, `sharded(keyspace)` DHT, fetch and prepare on workers plus the spool, the chaos harness and §19.8 gates (Track W). **[S]** Settings → Workers and per-worker Insights.

**Companion manager (Track K):**
- **[M]** the signed catalogue, checksum and signature checks, supervised child processes and encrypted credentials.
- **[S]** compose and CA-template generation, detection adapters and the Companions UI.

**Sources (Track S):** sliced into SRC-0..17 and SRC-G by `docs/plan/2026-11-01-v06-sources.md`: the unverified tier and claims (§18.5), the consent framework, egress and kill-switch, Prowlarr/Jackett as plugin #1, Nyaa, AnimeTosho, Internet Archive, Academic and official lists, download clients as a source, BEP 48 full scrape, torrent-cache enrichment, the Sources API and UI, the Jobs view. Parsers are Opus; plugin glue is Sonnet after SRC-7 sets the pattern.

**Filters:** **[M]** curated rule packs; **[S]** ThePornDB/StashDB enrichment following the plugin pattern. To be planned by P-v06-rulepacks.

**v0.6 tag gate (§0.6, owner §2.25):**
- the single-node crawler gate passes, or the owner re-decides at Checkpoint 1;
- the interface changes from the crawler program keep `crates/cluster`'s chaos and transport tests green.

Feature work on workers, companions and sources continues in parallel; only the tag waits. What ships in v0.6 is §14's scope; each feature's own gate (SRC-G for Sources, §19.8 for workers) decides whether it's ready to ship in it.

### P5: M7 / v0.7
- **[M] SB-6 first** (§0.2 Track M7): the search schema bump to v6, rebuilt blue/green; X-search-files-5's v7 follows before the v0.5.2 cut, and users rebuild once.
- **[M]** The Torznab server; `.torrent` upload (owner exception needed) and folder watch; grouping (MinHash, collapse-by-group), `payload_sig`/`sig:` and "same file"; the regex trigram prefilter and time-budgeted verify.
- **Already built:** the integrations trait and adapters (qBittorrent, Transmission, Deluge, rTorrent, aria2), the notifiers (webhook, ntfy, Discord, email, RSS), secret storage and the SSRF guard, the Send UI. uTP moved into the crawler program (CR-4).
- **Enrichment plugins** (srrDB, PreDB, Wikidata, TMDB/TVDB, AniDB/AniList, MusicBrainz, OpenLibrary, IGDB, Discogs, Steam, debrid): **[M]** the HTTP, egress, rate-limit and cache harness plus the first plugin (reusing SRC-5's egress); **[S]** the rest, one module each; every response parser reviewed by Opus.
- **[M]** The usenet tier, IRC announces and PreDB's IRC feed.
- **[S]** Stremio addon and bitmagnet export.

### P6: v0.8
- **[M]** Placement for singleton, account and overlay plugins on workers; companions on workers. (No Anna's Archive or LibGen importer: owner-decisions §2.38.)
- **[S]** Nostr NIP-35 and Lemmy plugin glue (parsers are Opus work).

### P7: M8 hardening → 1.0
- **[M]**
  - The relevance judgment set and `relevance` bin, with an NDCG@10 gate.
  - 10M performance and size gates on a dedicated runner, plus the owner-approved 50M import-scale numbers (§0.6).
  - The crawler gate re-measured on the release candidate.
  - The Windows soak test.
  - A full-system security review.
  - Plugin politeness and purge audits.
  - The chaos gates green.
- **[S]**
  - CI wiring for those gates.
  - Docs: `query-language.md`, `relevance.md`, `themes.md`, the user guide, a README with the brand lockups.
  - Playwright and axe across all 48 themes on a schedule.
- Release 1.0 when every §17 gate is green.

### After 1.0 (outline)
- v1.1: network tiers and I2P (i2pd)
- v1.2: eD2k/Kad and aMule
- v1.3: IPFS and IPNI
- v1.x: the micro-piece prober (consent-gated), embedded WireGuard, saved-search auto-send

Each gets its own detailed plan when it's reached. All of the protocol work is Opus work.

---

## 4. Where helpers add value, and where they don't
- **High value.** Many disjoint files with tight specs: Svelte screens, theme files, packaging and CI, adapters and plugin glue after an Opus pattern, Playwright matrices and docs.
- **Little or negative value.** Splitting search, the percolator, leases and rebuild, the cluster, uTP or schema work across agents. These have tight invariants and shared files, and reviewing split work would cost more than writing it. The same goes for two helpers on one screen, or a helper inventing API shapes.

## 5. Standing risks
The current risks are in §0.8. These standing mitigations still apply:

| Risk | Mitigation |
|---|---|
| Helpers drift from §10 | Briefs cite exact § and skill paths; the hex and arbitrary-value ban; snapshots and axe in Riverstone light/dark on every PR (all 48 themes on a schedule); `design-review` per screen |
| API contract drift | Generated OpenAPI → TS types, a CI drift check, `501` stubs with examples |
| Conflicts in shared files | Per-feature messages and screens, one line per slice in hub files, no barrel files |
| Bundle creep | size-limit per route chunk in every slice's checks; fixed by code splitting, never by raising limits without a recorded §2.26 decision |
| Review backlog lowers quality | The helper cap; the main track pauses for reviews; weak output redone on Opus |

## 6. Verification
- **Every PR:**
  - Rust: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`;
  - web: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`, and `npx playwright test` for anything with an e2e spec;
  - `git fetch origin main` before pushing (clones are shallow).
- **Milestone exits:** as listed in each phase and in §0.6, mirroring architecture §14 and §17.

## 7. Helper brief template
```
Goal: <one sentence>.   Model: opus|sonnet|haiku.   Branch: slice/<id> from main.
Read first: CLAUDE.md; runner/standards.md; docs/architecture.md §<n>; the plan or decisions files for this area;
  ui-design-skills/skills/<x>/SKILL.md and docs/design-tokens.md (UI only); web/src/lib/api/schema.d.ts (web only)
Own (create/edit): <paths>.   Do not edit: hub files (§1.3), package.json, anything not listed.
Inputs: <endpoints / fixtures>.   Out of scope: <list>.
Done when: <the slice's checks>, plus CLAUDE.md's "Before pushing" checks; no hex/arbitrary values;
  strings only in features/<x>/messages; ≲ 1,500 changed lines; decisions in docs/decisions/<date>-<id>.md.
Report: files changed, hub edits needed, missing contract fields, deviations from spec and why.
```

## 8. Automation: the runner
`runner/README.md` describes a long-lived Opus **runner session**:
- It starts each slice in a new cloud session as soon as the slice's dependencies are merged.
- It has a fresh Opus reviewer check every PR.
- It merges PRs that are green and have no blocking findings (owner, 2026-10-22; only the runner merges, §2.22).
- It keeps going around the clock, with a watchdog that restarts it.

The slice graph is in `runner/slices.yaml`; live state is on the `runner-state` branch. If no runner can run, [`docs/handoff.md`](handoff.md) describes how a single agent continues safely.
