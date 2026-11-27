# runner-records-r18 decisions

The runner's records PR after r17 (#1105). It records:
- the owner's pause (2026-11-24T06:30Z), audit request and resume (2026-11-27), as owner-decisions §2.84, with the priority the runner set from them;
- the runner's post-merge audit of the 16 PRs a stand-in session merged during the pause (#1108 to #1123);
- the runner's calls since the resume, on #1124, #1127, #1126 and two held-fix PRs;
- calls from 2026-11-24 before the pause (#1104 and X-test-vm);
- held-finding ids S8-25 to S8-27, by id, severity and surface only;
- ten new slice entries, notes on three existing slices, and owner queue items 38 to 40.

Apart from §2.84, everything here is the runner's, under the owner's delegation (§2.82), §2.84 and §2.26 (performance and UX). Each call is flagged to the owner, who can veto it (§2.19). The refinements of earlier decisions are marked **(§2.19)**. The slices' own decisions files hold the detail.

## The pause and the resume (§2.84)
- **Paused at 2026-11-24T06:30Z** on the owner's word. The runner disabled its heartbeat and recorded the pause in runner-state.
- **A stand-in session ("Claude Duet") merged 16 PRs** while the runner was paused, from #1109 at 2026-11-24T09:17Z to #1122 at 2026-11-25T03:15Z.
- **Resumed on 2026-11-27.** The owner's words came through the path-to-100 session. The runner checked each against that session's own events. The stand-in stays paused.
- **§2.83's "Don't start any new slices" is superseded.** Build slots fill again, up to §2.81's four.
- **Priority:**
  1. the path to 100 (build-plan §0.5): cr-ab-comp-2 on the Unraid box, run by the owner; at least 12 h; CR-AB-ADM and CR-AB-4 back to back; Checkpoint 1; CR-AB-LC;
  2. the audit's fix slices (below);
  3. the stand-in's queue: X-restart-fuzz-v2, X-gs-followups-2 and the `docs/metrics.md` runner docs PR;
  4. tracking CR-0w.

## The audit of the stand-in's merges
The owner asked for it first (§2.84). Every merge had green CI, `main` stayed green, and repository hygiene was clean. A dash means the audit recorded no finding beyond the process notes.

| PR | Slice | Merge | Verdict |
|---|---|---|---|
| #1109 | NOS-2b | 32396391 | defects, none blocking |
| #1110 | ENR-11 | 4321071e | defects, none blocking |
| #1111 | IRC-1 | 7f079fe1 | sound |
| #1108 | GS-5 | 4689737c | — |
| #1114 | X-cmp-egress-ui | 69b90acd | — |
| #1113 | X-flake-cap-index-move | 2262b329 | — |
| #1115 | X-flake-search-keyboard-2 | 25238cff | — |
| #1112 | X-escalate-full-table | dde6436f | — |
| #1117 | ENR-G | 182d49fe | defects |
| #1116 | GS-4 | 760dbc17 | — |
| #1118 | X-split-resume-gate | 16fa8c8c | defects, none blocking |
| #1121 | X-gs-followups | 0c10e80f | — |
| #1120 | X-purge-rederive | fcfb45f5 | sound |
| #1119 | ENR-terms-api | 092d67e1 | sound |
| #1123 | X-web-followups-1124 | 25d4dbec | — |
| #1122 | X-split-spill-horizon | f3476188 | sound |

### Process
- **#1109 has no review record.**
- **Deltas after review were accepted without review** on #1117, #1120 and #1123. The runner read the deltas on #1115 and #1116 and found them fine.
- **From now on (§2.18, `runner/standards.md`):** a delta pushed after a review gets a runner delta check or a fresh review before merge.

### NOS-2b (#1109)
- **Important test gaps,** carried to NOS-3's notes:
  - the kill switch in `recv` is untested;
  - the frame-cap header test is missing;
  - an open session never re-checks `source_on`;
  - the trip-count baseline is untested.
- **Nits:** the pong flush; the routed connect switch is untested.
- The audit also found held finding S8-25 (below).

### ENR-11 (#1110)
- **Size:** the index and store budgets are unmeasured for version 3 claims. Follow-up **X-enr11-size-measure**.
- **Torznab caps** advertise `imdbid`, `tmdbid` and `tvdbid` unconditionally (`torznab.rs:198-199`). With partial ID coverage, an ID search then hides unidentified releases from Sonarr and Radarr. ENR-11's decisions file inferred a title-search fallback that doesn't cover this. Follow-up **X-torznab-id-caps**.
- **No slice yet** for the notice that ID fields are partial until the identify sweep is done. It needs `crates/api/src/search.rs`, which SRC-3b owns. Recorded in X-torznab-id-caps's notes for the runner to place.
- **For SRC-3b** (in its notes): documents built from claims get no identification terms.
- **Polish (backlog):**
  - no `query_props` ID fragment or fuzz seed;
  - `crates/store/tests/identify_sweep.rs` is owned but was never created (the tests are in `crates/search/tests/enrichment.rs`);
  - a quoted ID gives "needs a value";
  - typing `a` suggests `anidb:` and `anilist:` first.

### IRC-1 (#1111)
- **Important:** `fuzz/fuzz_targets/irc_announce.rs` has no seeds. Follow-up **X-irc-fuzz-seeds**: `template.def` plus line seeds, with the coverage recorded.
- **Important, carried to IRC-2's notes:**
  - bind the passkey host to `torrent_url_host`;
  - refuse loopback and private hosts;
  - `{authkey}`;
  - reqwest's `without_url`;
  - no redirects on the `.torrent` fetch;
  - the `url::Url` lifetime.
- **Nit:** 908 before `AUTHENTICATE` is untested.

### ENR-G (#1117)
- **New held finding S8-26** (below). It gates ENR-13b and any enrichment wiring.
- **Important (latent, public): a purge leaves served identification data in the index.** Follow-up **X-purge-index-shapes**: the served identification predicate must match the claims-shape predicate (`Claims::shapes_the_record`), and the purge must reindex.

### X-split-resume-gate (#1118)
- **New held finding S8-27** (below).
- **Correction (§2.19).** The decisions file says the slice clears §2.77's "no blanket gate carried across a restart" for the split. It doesn't on the upgrade path: a version-1 snapshot still resumes with its gate, so the first restart after upgrading can hold sampling once, for up to about 6 h. The file records that migration but doesn't flag it. The split is off by default, so the impact is low. CR-0w removes that gate.
- **Important, tracking (from before the stand-in):** CR-0w, CR-0w-soak, UX-start and D-start had no `runner/slices.yaml`, dashboard or build-plan entry. They were proposed with §2.77 (2026-11-15, `docs/decisions/2026-11-15-owner-no-start-wait.md`). So §2.77 is unmet for every user on first start and on every unproven restart. Their entries are added now.
- **Nits:**
  - `docs/metrics.md:676-677` and the `metrics.rs` help text are stale. They go to the `docs/metrics.md` runner docs PR.
  - The shutdown write, about 40 MiB, runs on the actor.

### The others
- **ENR-terms-api (#1119):** sound. Nit: the `adult:<kind>` tag reaches token callers.
- **X-purge-rederive (#1120):** sound. Nit: its flag to the owner should name the Sukebei-only Adult items that a purge un-hides.
- **X-split-spill-horizon (#1122):** sound. Nit: `wide(i)` in `horizon_tests.rs` can overflow `u32`.

## Calls since the resume (§2.19)
### X-soft-absent-void (#1124, merged 0baf8546)
- **A soft B-arm segment without `dht_soft_timeouts_total` is void** (`soft_timeout_counter_rendered`). This reverses #1102's "no warning" call, as r17 named it.
- **Polish:** the decisions file's "no opportunity" bullet should say that soft doesn't adopt route's zero-is-void rule.

### X-restart-fuzz-v2 (#1127, merged e01952de)
- **One call site outside the restart files was granted:** `crates/dht/src/node/proof.rs:849`. `restart::plan` now takes `Loaded` by value, so the spill section moves instead of being copied. That's about 32 MiB less at start.

### X-gs-followups-2 (#1126, merged 833dc1e1)
- **Size limits under §2.26:**
  - raised: Search collapse rows to 3.08 KB, Detail Related to 5.74 KB;
  - lowered: Detail drawer to 28.27 KB, Search results (lazy) to 43.76 KB.
- **A new query's rows start at row 1** in every view.
- **Focus after Show all moves to Related's heading.** The focus code lives in the lazy Related section, so the drawer's first view doesn't grow.
- **Shared lines in `web/.size-limit.mjs`** were granted beside UP-2 (#1106), in disjoint hunks.
- **Polish for the next slice that touches the detail drawer:** the drawer host's `data-ask` should also clear on a hash change.

### Size-limit lines in general
- **A slice may change its own entry lines in `web/.size-limit.mjs`** while another PR holds the file, when the hunks are disjoint.

### X-sec-S8-26 (#1129)
- **An additive shared edit in `crates/sources/**` was approved** while X-sec-S8-25 holds that crate.
- **Re-landed as #1129,** one commit with a neutral message, replacing #1128. `docs/security/v1/README.md` §5 says a fix's commits describe only what the code now does.
- **The general rule:** a held-fix PR whose history describes the old behaviour is re-landed as one neutral commit in a new PR. It's never rebased or force-pushed.

### X-sec-S8-25 (#1125)
- **The licence allowance is withdrawn.** The runner first allowed CDLA-Permissive-2.0 (for webpki-root-certs, Mozilla's root list) in `web/licenses/allow.json`, then withdrew it. A licence change is a legal call for the owner: owner queue item 40.
- **Follow-up X-tls-root-load-event:** a catalogued event that counts root certificates that fail to load from the OS store.

## Before the pause (2026-11-24)
### X-watch-env (#1104, merged 62a339a7)
- **rv1104 asked for changes:**
  - the `chown` recipe;
  - the troubleshooting codes `subfolder_create_failed`, `unreadable`, `subfolder_link` and `too_many_stuck`;
  - no `:ro`.
- The runner's delta check was done before the merge.
- **The load-time check is absolute-only (§2.19).** The disk rules apply when the watch applies the folder and on every poll (`docs/decisions/2026-11-24-X-watch-env.md`).
- **Follow-up X-watch-open-fstat** (low, not held: the path is operator-controlled). `Folder::open` has a parent-swap race.

### X-test-vm: the combo screen's first hour
- **66.2 stored a minute, with no stall.** Queries per second sat at the Balanced cap of 200: with all four switches on, the query budget binds.
- **A UX question for presets:** once the stall is gone, is the preset's query cap the next lever? It's an owner queue candidate, not a change now.
- **The spill fallback** comes about 96 h after suppression. X-split-spill-horizon covers it.
- **Between instances for all later blocks (§2.19).** Within-instance ABBA is confounded by the sampler's cycle. The runner extends r17's stage-3 between-instances design to every later block of the cloud's provisional runs.

### §2.77 and the owner's 21:45Z relay
- r17 flagged that X-split-resume-gate's hold conflicted with §2.77. #1118 removed the hold, except on the upgrade path (above).
- The owner's request relayed at 2026-11-23T21:45Z came only through X-test-vm. It isn't in owner-decisions, because r17 reads it as within §2.82.

## Held findings (ids only)
`docs/security/v1/README.md` §5's hold applies: no detail in the repository until a release with the fix is published.
- **S8-25:** important, plugin TLS on routed egress. Found by the audit of #1109. Fix **X-sec-S8-25**, #1125, in review.
- **S8-26:** important, latent, enrichment runtimes. Found by the audit of #1117. Fix **X-sec-S8-26**, merging as #1129. It gates ENR-13b and any enrichment wiring.
- **S8-27:** low, the DHT restart snapshot. Found by the audit of #1118. Fix **X-sec-S8-27**, building.

The three fix slices add their own `runner/slices.yaml` entries in their PRs.

## New slices
- **X-purge-index-shapes** (Opus): a purge takes served identification data out of the index.
- **X-enr11-size-measure** (Opus): measure the size budgets with identification answers in the corpus.
- **X-torznab-id-caps** (Opus): caps advertise ID parameters only when they can be answered.
- **X-irc-fuzz-seeds** (Opus): seeds for `irc_announce`, with the coverage recorded.
- **X-watch-open-fstat** (Opus): `Folder::open` checks the folder it opened, not the path.
- **X-tls-root-load-event** (Opus): the root-load event, after X-sec-S8-25.
- **CR-0w** (Opus, protocol review): remove the start wait (§2.77). After X-sec-S8-27, which holds the restart snapshot's files.
- **CR-0w-soak** (Opus): the analysis windows without a start suppression, then a soak sampling from minute one.
- **UX-start** (Sonnet): the first hour shows progress.
- **D-start** (Sonnet, docs): the user and packaging docs.

## Notes added to existing slices
- **IRC-2:** IRC-1's six carry-forwards.
- **NOS-3:** NOS-2b's test gaps and nits. Owns widen to `crates/sources/src/ws.rs` and its tests at launch, after X-sec-S8-25 merges.
- **SRC-3b:** documents built from claims get no identification terms.

## Owner queue
The runner-state ids are in brackets.
- **Item 38** (`advisories-S8-25-27`): should the runner open draft security advisories for S8-25 to S8-27?
- **Item 39** (`handover-delete`): may the runner delete `HANDOVER.md` from runner-state?
- **Item 40** (`license-cdla-permissive-2`): may the licence allowlist add CDLA-Permissive-2.0?
