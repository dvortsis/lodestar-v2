# runner-records-r20 decisions

The runner's records PR after r19 (#1144). It records:
- the runner's rulings since r19 that no merged PR's own decisions file holds;
- ten new slice entries, and owns, deps or notes on fifteen existing ones;
- the `runner/slices.yaml` and dashboard changes that owner-decisions §2.85's D1 to D13 still needed after #1144;
- the crawler path on the dashboard: comparator, harness, CR-SPEC-R1, the tool fixes, the rehearsal, round 1, CR-COMB, G-v06-crawler.

Everything here is the runner's, under §2.82, §2.84 and §2.26 (performance and UX), or its reading of §2.85 and §2.86. Each call is listed under "Flags for the owner (§2.19)" at the end, and the owner can veto any of them.

## Already recorded in a merged PR's decisions file
These rulings are cited here, not repeated:
- **`docs/decisions/2026-11-27-X-cr-round1-gate.md` (#1147):**
  - a fresh attempt after a layout change doesn't count against the retry budget, and layouts only shrink;
  - `attempt_ends` lists only the days an attempt actually ran;
  - a malformed declaration is an error (exit 2), not a verdict.
- **`docs/decisions/2026-11-27-X-cr-round1-collect.md` (#1150, and #1151's additions):**
  - a leak-test failure before the day's collectors start is a day note, and the arm is blocked until a passing re-test;
  - `LODESTAR_INTENSITY` passes through, with the second `test_steps.py` grant;
  - the comparator is bound after its window;
  - the pool map uses the VM's kernel names, and a backing device sits in exactly one pool;
  - the declaration is append-only.
- **`docs/decisions/2026-11-27-CR-SPEC-round1.md` (#1155):**
  - the §1 list of refinements: the S bound, the repeat exceptions, the rehearsal window, dropping C when `drop-c` is still `not_judged` at the cap, L2b and L2c, coverage counted window-wide, a diagnostic arm dropped on its first product failure, and the rest of that list;
  - the §14 rulings on the draft's 25 mismatches;
  - the 1.1 items, pool names among them.
- **`docs/decisions/2026-11-27-X-legal-safe-restart.md` (#1149):**
  - D-start restores the safe-restart section as user guide 3.8;
  - X-prowlarr-register's planned notice version moves up one;
  - `operator-guidance.md` is at version 6, which covers X-event-log-followups' item;
  - the `tests/legal.rs` and `dto/auth.rs` grants.
- **`docs/decisions/2026-11-27-D-start.md` (#1148):** 3.8 and 3.9, the `docs/user/README.md` grant, and the hand-off that X-start-wait-strings now carries.
- **`docs/decisions/2026-11-27-X-import-drain-memory.md` (#1153):**
  - the title set is in-process only and rebuilt at every start (content safety);
  - the checksum stays, with `crc32fast` granted;
  - the `docs/metrics.md` and search-bench grants.
- **`docs/decisions/2026-11-27-X-import-hot-stamp.md` (#1154):** the invariant, the writer audit and the order test.

## Rulings recorded here
- **X-healthz-cold-start (in flight):**
  - The have-filter fills on a background thread, and intake waits for the fill (the runner's default in its brief, §2.26). It's sized from the exact count, since the id range follows a shared sequence and can be several times larger. The id-range bound is kept for X-healthz-count-async.
  - **The start-up `count(*)` stays in `prepare()` for now.** At 10M a cold start fell from about 5.3 s to 0.9 s with it kept. Moving it to the fill thread makes `stats.stored_torrents` null while the fill runs, which changes a documented contract (`docs/metrics.md`, the API and the web schema). So it's a follow-up of its own, X-healthz-count-async, not part of this PR.
  - Granted 2026-11-28: `crates/lodestar/src/cluster/serve.rs` (its have step waits for the fill), `crates/pipeline/src/log_events.rs` (`pipeline.have_fill_failed`), and `bench/examples/cold-start.rs` with its `bench/Cargo.toml` and `bench/README.md` lines.
  - **A fill that keeps failing no longer fails the start.** `/healthz` answers ok while crawling waits, and an Error event is raised at most every 60 s. Before, a failing fill in `prepare()` failed the start.
- **UX-start (#1152, open):**
  - `/welcome` opens the event stream for its live counts, a refinement of item W9.
  - Granted: new entries in `docs/ui-glossary.md`; `crates/api/src/dto/ops.rs` for the `/stats` example only (`sampling_blocked` null, CR-0w's hand-off), with the regenerated `openapi.json` and `schema.d.ts`.
  - Search results (lazy) rises to 44,224 B (§2.26). Trimming saved nothing.
- **X-facet-exclude-fill (#1145, open):** `TermGroup.test.ts` is granted, and `web/.size-limit.mjs` for its Facet rail (lazy) entry only (§2.26).
- **X-dht-switch-identity (#1146):** `crates/lodestar/tests/dht_switch.rs` was granted on 2026-11-27 for its end-to-end `/metrics` test.
- **D-start (#1148):**
  - **The safe-restart section comes back as user guide 3.8, and "Check your understanding" becomes 3.9.** The Legal Notice's "user guide, section 3.8" pointer is the owner's wording (§2.87), so the guide moves to match it, not the notice. `docs/decisions/2026-11-27-X-legal-safe-restart.md` points here for this call.
  - `docs/user/revisions.md`, `docs/user/index.md` and `docs/user/glossary.md` were granted with `docs/user/README.md`.
- **`web/.size-limit.mjs` is a shared edit between UX-start and X-facet-exclude-fill.** Each touches only its own entry, and whichever merges second brings `main` in, as X-prowlarr-register's `services.rs` note does.
- **X-irc-fuzz-seeds (#1133):** `crates/sources-irc/tests/fuzz_seeds.rs` was granted on 2026-11-27 (the seed writer and its staleness check). r19 missed the owns line.
- **X-leaktest-vpn-held-down is launched before the rehearsal.** Round 1 runs `leak-test.sh` on every arm before and after every day, so a harness flake costs a day (CR-SPEC-R1 §4). It owns its 2026-11-28 decisions file, and the cloud runbook's §3.2 only if the failure's description changes.
- **X-cr-round1-spec-fixes replaces the X-rehearsal-window brief,** which never launched and never had an entry. Its items come from the review of #1155.
  - CR-SPEC-R1 §6 lets the rehearsal start only after it merges.
  - So CR-R1 now depends on it, and on X-cr-round1-collect-fixes (#1151).
- **The Prowlarr notice moves to version 7.**
  - X-event-log-notice took version 5 and X-legal-safe-restart took 6.
  - `ext:owner-notice-v5` becomes `ext:owner-notice-v7`, still met. Runner's reading: the owner approved the wording (§2.55), not a number, so only the version it ships under moves.
  - X-prowlarr-register's title and note say 7, and it now depends on X-legal-safe-restart.
- **X-event-log-followups' operator-guidance item is done by #1149.** The slice drops it.
- **The pool-name check is a 1.1 item** (CR-SPEC-R1 §11, §14). A pool named outside `[a-z0-9_]{1,32}` can never match a note, so the rule fails safe until then. It's entered as X-cr-round1-1p1 and held. CR-SPEC-R1's other 1.1 items join it when 1.1 is planned.
- **No entry for X-import-drain-memory.** Its own PR added it.

## §2.85's D1 to D13
**Already done by #1144:**
- D2's note on `ext:experiment-host`;
- D3, through CR-SPEC-R1;
- D6, `limits.extra_shared` 2;
- D7, D11 and D12's owner-queue items;
- D10's VM size, recorded in §2.85.

**Added here:**
- **D1, the scope freeze:**
  - G-v1's note says 1.0 is frozen as plan 2026-11-26 §3.
  - The plan leaves the exact list to the runner, "for a yes" from the owner. It isn't drawn here, and nothing is held for D1 until the owner says yes to it.
- **D2, the host of record:**
  - G-v06-crawler, G-v1-crawler and G-v1-soak-linux name `lodestar-host` in their notes, and the soak's gate text and `ext:unraid-soak-host` comment say so too.
  - G-v1-crawler's note records the plan's one RC run serving three gates (§6.1 week 5). `runner/README.md` step 7 now names it as the one exception to "two never share a run".
  - G-v1-vs-bitmagnet's side-by-side runs are booked by the runner in the same hours, swap included. The RC's one run doesn't replace them.
- **D4, politeness:**
  - D4 adds a bounded-load check of the crawler's DHT politeness for 1.0 and moves the crawler's full politeness audit to 1.1. G-v1-audits' title, note and gate text say so.
  - **M8-POL stays a 1.0 audit of every plugin that ships:** manifest caps, the per-host limiter and back-off. D1's list may narrow it only to the plugins that ship.
  - The slice that carries the bounded-load check is named with D1's list.
- **D5, the 50M import:**
  - As the owner wrote it: the 50M import moves to 1.0.x unless the host has spare time in weeks 2–4.
  - G-v1 no longer depends on G-import-50M or `ext:owner-50m-approval`, and G-import-50M's note says 1.0.x. **If the host has that time and the import runs then, both go back into G-v1's deps;** otherwise they stay out. G-v1's note and the dashboard say the same.
  - On the roadmap, the 50M step is marked 1.0.x and its arrow into M8 is gone.
  - `docs/build-plan.md` §0's gate count (now twelve G-v1-* gates) and P7's 50M line follow.
- **D8, the optional VPN:**
  - X-vpn-bind (Opus, security review).
  - D-vpn-guide (docs), which rewrites guide 3.3 as "Use a VPN (optional)".
- **D9:** no change now. It sets 1.1's politeness margin.
- **D11, the owns rule:**
  - The `slices.yaml` header and `runner/README.md` step 7 now say that crawler slices may run in parallel when their only overlap is adding switch lines to shared files.
  - The older notes on X-sampler-cooldown-split and CR-SOFT-TIMEOUT that call queue item 30 open are left as history.
- **D13 B:**
  - G-v1-vs-bitmagnet is a new 1.0 gate, side by side on `lodestar-host`. G-v1 depends on it.
  - It has a gates entry, and it's added to the roadmap's 1.0 step.

## New slices
- **X-cr-round1-collect-fixes** (Opus, merged as #1151): X-cr-round1-collect's owns, plus the granted analyzer files.
- **X-cr-round1-spec-fixes** (Opus, in flight): the owns in its brief.
- **X-cr-round1-1p1** (Opus, held for 1.1): the pool-name check.
- **X-import-hot-stamp** (Opus, merged as #1154).
- **X-healthz-cold-start** (Opus, in flight).
- **X-healthz-count-async** (Opus, after X-healthz-cold-start): it touches `docs/metrics.md`, the API DTO and `openapi.json`, the web schema and the readers of `Monitor::torrents()`.
- **X-start-wait-strings** (Sonnet, after UX-start and D-start):
  - the two restart-witness event messages and the Dockerfile comment;
  - the wizard's `wizard_how_cold` and its tests only if UX-start's PR left them, since UX-start owns the wizard.
- **X-vpn-bind** (Opus), **D-vpn-guide** (Sonnet, docs) and **G-v1-vs-bitmagnet** (gate), all from §2.85.

## Owner queue
- **Item 42:** the calls below, open to the owner's veto.

## Flags for the owner (§2.19)
Runner refinements recorded in other files, flagged again here so the list is complete:
1. A fresh attempt after a layout change doesn't count against the retry budget (X-cr-round1-gate).
2. A leak-test failure before the day's collectors start is a day note, not an infrastructure event (X-cr-round1-collect; CR-SPEC-R1 §4).
3. The comparator is bound after its window, which is weaker than an hourly check (X-cr-round1-collect; CR-SPEC-R1 §10).
4. CR-SPEC-R1's §1 list:
   - the S bound and the repeat exceptions;
   - the rehearsal's 30 min warm-up and 3 h window, ended by the first decisive `drop-c`;
   - C dropped when still unjudged at the cap;
   - L2b permitted, and L2c added;
   - coverage counted window-wide;
   - "not measured" failing as a valid miss;
   - a diagnostic arm dropped on its first product failure;
   - an uplink counter decrease making the day `not_judged`;
   - the stricter CR-ADM and uTP checks;
   - the uplink hash read only at each collector's launch;
   - and the rest of that list.
5. The title-suggestion set is in-process only and rebuilt at every start (X-import-drain-memory).

Recorded here:

6. `/welcome` opens the event stream for its live counts (UX-start, item W9).
7. A fill that keeps failing no longer fails the start. `/healthz` answers ok while crawling waits, with an Error event at most every 60 s (X-healthz-cold-start).
8. The start-up count stays in `prepare()`, and the nullable `stored_torrents` contract change waits for X-healthz-count-async.
9. Size raises under §2.26: Search results (lazy) to 44,224 B (UX-start), and the Facet rail (lazy) entry, 18.86 KB on `main`, for X-facet-exclude-fill. #1145's head doesn't change it yet, so the runner records the new figure when it lands.
10. The Prowlarr notice ships under version 7 on the owner's version-5 approval of its wording, with the condition renamed.
11. D5 as written: the 50M import moves to 1.0.x, and goes back into G-v1's deps only if the host has spare time in weeks 2–4 and it runs then.
12. D4 leaves M8-POL a 1.0 audit of every shipped plugin, narrowed by D1's list only to the plugins that ship. The bounded-load check's slice is named with that list, and the list goes to the owner before anything is held.
13. D11's wording: "only adding switch lines to shared files" is the test for running crawler slices in parallel.
14. The 1.0 RC's one 24 h run serves G-v1-crawler, G-v1-soak-linux and the RC side of G-v1-vs-bitmagnet (plan §6.1 week 5), an exception to `runner/README.md`'s "two never share a run".
15. Intake waits for the have-filter fill (X-healthz-cold-start, §2.26).
16. D-start restores guide 3.8 and renumbers "Check your understanding" to 3.9, to match the owner's Legal Notice pointer.
