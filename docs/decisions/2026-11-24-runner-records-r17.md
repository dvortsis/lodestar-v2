# runner-records-r17 decisions

The runner's records PR after r16 (#1097). It records:
- the runner's calls from 2026-11-23T09:45Z to 2026-11-24T02:15Z, on the API lane's PRs and the runner slots' PRs that merged in that time (#1096, #1099, #1082, #1098, #1100, #1102, #1101 and #1103);
- the calls in the cloud's stage 1 and its screens (X-test-vm);
- held-finding ids, by id, severity and surface only;
- eleven new slice entries, notes added to existing slices, and owner queue item 37.

There are no new owner decisions here. Everything is the runner's, under the owner's 2026-11-23 delegation (owner-decisions §2.82), §2.83 and §2.26 (performance and UX). Each call is flagged to the owner, who can veto it (§2.19). The refinements of earlier decisions or briefs are marked **(§2.19)**. The slices' own decisions files hold the detail. The owner's request relayed at 2026-11-23T21:45Z (below) isn't a new owner decision: its standing authority is §2.82's "Run as many tests in parallel if needed".

## Size: X-size-own-modules (#1096, merged 13f76fce)
- **No budget gets looser in real terms (§2.19).** The module-set measure gzips a route's chunks as one stream, so every route figure fell 15 to 30% with no code change. Where an old limit had deliberate headroom, the headroom is kept in proportion:
  - new limit = new measure + (old limit − old measure) × (new measure ÷ old measure), rounded up to 0.01 kB, never above the old limit;
  - **the four limits it sets:** Discover 35.35 kB, Logs 33.63 kB, Alerts 19.04 kB, Settings overview 31.80 kB;
  - the other seven route entries sat at their measure, so they fall to the new measure rounded up (Welcome at 12.08 kB, 0.01 kB under the formula, accepted).
- **§10.1's caps are unchanged.** They're in the per-file download measure, and the route entries enforce them at the same proportion in the stream measure.
- **Flagged (§2.19): the protected Alert and Welcome limits were lowered** (46 kB to 35.03 kB, and 16.5 kB to 12.08 kB). This follows the owner's rule that a limit may fall to its measured size rounded up. Neither was raised.
- **The measure fails closed:** a JS file that isn't in the chunk graph stops the size step instead of being sorted by name (rv1096).
- The merged tree passed `size:ci` on all 119 entries. The post-merge lane review's polish goes to **X-size-own-modules-2** (its notes list it). The PR's description is stale in places; that's cosmetic and not fixed.

## RX-1 (#1082, merged 6bec9388)
RX-1-fixes (a runner slot, r16) took the PR through five fix rounds, each with a general and a security review, plus two independent reviews by the API lane. Calls in them:
- **An unclosed `/` reads as plain words, not a parse error.** This reverses item P6 of the review list that RX-1-fixes' first push (100048ff) answered, where that push had kept an unclosed slash as an error. A slash delimits a pattern only where one can start and end (r16), so a stray slash in ordinary text never turns a search into an error.
- **A bare `"` ends a `/regex/` scan:** a quote before the closing `/` makes the text plain words. A phrase's whitespace is rewritten when it's rendered, so a slash inside one could close a pattern only after a round trip (a fuzz find).
- **A stopped regex has its own flag.** The facets response carries `stopped`, and so does each facet. It's separate from `partial`, which means a field still being backfilled. The UI ignores `stopped` until RX-2, which shows "Stopped after 2 s" with a ≈ count.
- **An idle lease reuses its verified set.** The delta review found an idle lease's facets fell to 0 after 2 s, because they verified again under a spent deadline (blocking). Passes over a lease now read the matches it kept, so later pages and facets agree with page 1 and verify nothing.
- **Export and Torznab fail instead of going short** on a lease whose regex stopped (`422 regex_stopped`, and Torznab's error document). Owns widened for both.
- **Live regex pacing:** a cut gets a fixed 1-minute wait; from the second cut in a row, cuts double like stops, up to 1 hour.
- **Also in the fixes:** a per-search back-off in `Seen`; the warn event `search.percolate_regex_stopped`; the exact error texts in RX-1's decisions file; the stem fallback reuses the exact pass's clauses; suggest's count offers skip regex queries; the test of a lease's reused matches sets a 2 s budget, which the search finishes even on a loaded runner, then waits it out.
- **X-regex-lazy-verify also covers percolation and promoted torrents.** A promoted torrent (SRC-2b) is queued and matched once against the saved searches, outside the watermark range; lazy verification applies to that pass too. Recorded in its notes.
- **`set_budget_on_this_thread` stays `#[doc(hidden)]`:** a test seam with no production caller, used by the percolator's tests in another crate, like `cancel_at_seal_on_this_thread`.
- **The lane's second review:** P2 to P6 were folded, and P1 was declined. Its CPU point (the live CPU cost per regex request, which RX-1's decisions file leaves to X-regex-permit) goes to X-regex-permit. The slice fixed its own `runner/slices.yaml` entry.
- **The merged-tree tests** on main 3288d133 with the PR's head: api, search and lodestar passed 1,446. Eight `cli::import` failures were the test machine's full disk; after clearing space, 25 of 25 passed.
- **CI:** a Playwright flake at `web/e2e/search-live.routes.test.ts:29` (banner not found; the web runtime was unchanged; 5 of 5 passed locally). The runner used the one re-run and posted a §2.18 note. The fix is **X-live-banner-flake**.

## X-unverified-cap-index (#1099, merged 3288d133)
- **Migration V16 goes to X-unverified-cap-index.** No other open PR held a migration. The runner widened its owns to `schema.rs`, `snapshots.rs` and one line of `tests/swarm.rs`, and asked for: the migration's time on a large store, a V15 to V16 count test, and the downgrade. Usenet writes stay off by default.
- **An exact kept count instead of counting past the cap (§2.19).** The brief asked for a per-network count "so counting runs only past the cap". At the cap every adding write is past it, so that would still count on every write there. Triggers keep the count exactly, so nothing counts at all.
- At merge: the review approved, 7 of 7 mutations were killed, and V16 took 0.8 s (5.3 s with the pre-migration snapshot) on a 2M-item store in the runner's merged-tree run, on a different machine from the slice's own 0.61 s (1.40 s with the snapshot).
- **Follow-up X-unverified-count-heal,** a gate before usenet writes default on. A count too high wouldn't correct itself, as `count(*)` did.

## IRC-0 (#1100, merged 1c94f71c)
- **Size accepted:** 4,586 insertions and 1 deletion across 13 files, most of it a new crate, with the reason in the PR. Owns widened to the `irc_line` row in `fuzz/README.md` and one `TARGETS` line in `crates/lodestar/src/logging/mod.rs`. Reviewed on Opus with a security review.
- **Refinements of the brief (§2.19):**
  - **SASL unavailable is retried with back-off, not a stop.** This covers a server that doesn't offer SASL PLAIN, and one that registers the client without it, including a welcome (001) that arrives mid-SASL. Networks drop the capability while services are split, so a later attempt can succeed. The client never registers without SASL.
  - **`ERROR` counts only at the back-off cap.** Three connections in a row that the server ends with `ERROR` before registering stop the client (`Stop::Banned`), but only while the wait before each was already the cap. The runner's round-1 call counted every `ERROR`; it revised that in round 2, because three `ERROR`s would trip on ordinary throttling.
  - **464 (wrong server password) stops for good as `AuthFailed`.** A retry can't fix it.
- **Round 2 also:** a NickServ service target that's validated; registered on the welcome; U+2028 and U+2029 and Unicode format characters filtered from the nick, with a table test; a welcome mid-SASL is internal, never passed on.
- **For IRC-1:** strip formatting, control and format characters from `Events` text before anything is stored, logged or shown, and budget about 4.5 MB of event-queue text per client.
- **The post-merge lane review:** `line::unescape_tag` can yield CR or LF in a tag value, though the docs say never. Its fix, X-irc-tag-crlf, is IRC-1's first commit, with 908 then 904 mapped to `SaslUnavailable`. IRC-2 uses SASL by default, and the `NickServ@services.<network>` form where NickServ is used. Both are in their notes.

## NOS-1 (#1101, merged 79c4d52c)
- **Synthetic events, not real relay events (§2.19).** The tests sign their own events with k256's `SigningKey`, beside BIP-340's 19 vectors. The runner asked for it as a refinement note in the PR's description.
- **The long digit runs in `crates/sources-feeds/tests/nostr_sig.rs` are accepted.** They're BIP-340's test vectors, copied verbatim. The repository's rule against 12-digit numbers is about account-like numbers.
- **`web/licenses/rust.json` outside its owns is accepted.** `licenses.py` regenerates it with the lockfiles. **From now on, a slice that changes dependencies lists it in owns** (added to NOS-2b's).
- NOS-1 lands before NOS-2b, which shares the lockfiles.
- **For NOS-3** (in its notes): dedupe an event id only after `parse_signed` returns Ok; count `BadSignature` per relay and back off from or drop that relay; never trust the id in a relay's `Message::Ok`; call `nostr_sig::parse_signed` in `fuzz/fuzz_targets/nostr.rs`; avoid hashing the id twice in `parse_signed`, or record it as deliberate.

## X-soft-key (#1102, merged 766b396a)
- Launched 2026-11-23T23:26Z in build slot 1 for X-test-vm, under §2.83's exception. Its runner calls are in its own decisions file (`docs/decisions/2026-11-23-X-soft-key.md`): owns widened to `scrub.py` and the stage-3 README, and what a sampling minute is. They're recorded there and only referenced here.
- **Follow-up X-soft-absent-void (§2.19).** A soft block on an image without #1091 runs A/A, but `effect` still decides. It voids (or warns on) a B-arm soft segment that lacks `dht_soft_timeouts_total`. This reverses #1102's recorded "no warning" call.
- Nits from the review: that decisions file's heading lacks "(#1102)" (it's #1102); `test_block.py`'s winners-length test hits the repeat rule, not the length limit (in X-soft-absent-void).

## ENR-debrid (#1103, merged 24b823fe)
- **Dropped (a reasoned skip): no provider qualifies** under plan Q4 (owner-decisions §2.47). The slice shipped its check and no code. Its stricter bar ("clearly allow", unclear doesn't qualify) is flagged in its own file.
- The runner marks it dropped in `runner/slices.yaml` (its notes, in SRC-10b's form; it isn't a dep of G-v07), `runner/dashboard.yaml` (`state_override: closed`) and the v0.7 plan's row.
- The lane's own reviewer said merge but missed the Adult-off wording. The runner's request for changes stood, and the fix merged with it.
- **A relaunch for TorBox** is possible within v0.7 if a person reads its current terms in a browser and finds they clearly allow a third-party app to check hashes with the user's own key. Owner queue item 37 asks, optionally; nothing waits on it. Silent or ambiguous terms are a grey case for the owner (§2.28).

## The cloud's stage 1 and screens (X-test-vm)
### The collector's clock check: X-collector-ntp (#1098, merged 9ea2e855)
- **The blocker:** analyze_cr judged every stage-1 segment invalid on D1a (`ntp_synchronized`). The collector's image has neither `chronyc` nor `timedatectl`, and its fallback failed.
- **The runner launched X-collector-ntp** (an Opus build slot) under §2.83's exception:
  - the raw `adjtimex(2)` syscall first, read-only (`modes = 0`);
  - no new privilege: no CAP_SYS_TIME, no looser seccomp profile, no chrony socket;
  - analyze_cr's D1a rule unchanged.
- **The cause** (X-test-vm's probe in the collector image): the image's libc has no `ntp_adjtime`, so the call failed in Python before reaching the kernel. It wasn't seccomp. The raw syscalls work. The review had asked for the hedged cause and a diagnosis section, and the merge waited for this probe.
- **The pickup rule:**
  - new blocks pick the fix up from a rules commit at or after the merge;
  - a running block may swap the collector between segments only with exactly main's file at or after the merge, its sha256 matched to that commit's, and the swap recorded in the driver log and the copy-off record;
  - no segment is split across collectors;
  - stage 1 only, the one exception to "the collector comes from the rules commit".
  - `judge_block` checks only `image_digest`, so a swap doesn't break a block.
- **Re-judging D1a (§2.19, the cloud's provisional runs only):** the runner decided both that new blocks use the fixed collector from their next start-block, and that segments already run are re-judged on the host's logged `chronyc tracking` lines (leap Normal, offsets of a few µs) where the driver log holds that host evidence. Nothing is stitched. This refines CR-SPEC rule 6.5 (§2.34), which judges the clock from the collector's own record; it doesn't apply to the experiment host's runs of record.
- **Also in #1098, at the runner's request:** `fetch_first_window_timeouts_total` is kept in snapshots. No other counter was missing.

### Verdicts and readings
- **esc2** (the escalate screen): checks (a) to (f) passed, and the restart drill passed. With escalate off, a restart keeps the cooldowns, so stage-1 segments 2 to 4 of block 1 start stalled. Torn down, about 12.85 instance-hours, about $2.20.
- **The escalate gap, confirmed by the runner's code reading** (`sampler.rs` on 0db3b28a): with the split off and the table full, the sampler's `next()` returns nothing before it reaches the frontier, so a struck node never gets its 300 s second sample. CR-SAMPLE-BACKOFF-ESCALATE's "a full table included" holds only with the split on. **Follow-up X-escalate-full-table.** It waits under §2.83, because escalate isn't a stage-1 treatment.
- **base:** (a), (b), (d), (e) and (f) passed. **(c) is a no: the full-table stall.** The sampler cycled through about 35 min of sampling, then about 6 h locked. The table held 49% timeouts, 33% interval and 18% unsupported. L1 didn't show, and the drill passed.
- **utp2:** (a) to (d) and (f) passed. For (e), anonymous memory peaked at 391 MiB, rising 15.6 MiB/h over segment 1's last 4 h and 49 MiB/h in segment 2's burst; it's watched. Both segments were invalid for D1a (before #1098).
- base, utp2 and esc2 were torn down, verified, for about $6.80.
- **The route gate is met** (IPv6 `no_route` 3), so X-test-vm runs the route instance.
- **Block 1 (all segments stalled):**
  - segment 1: s1adm and s1aa had a full table all window (carry-in share 0.986, net per minute about 0); s1utp stored 10.51 a minute, with 4.73% fetch success;
  - segment 2: s1adm went from 9.62 to about 19.9 stored a minute (ADM on, 0 capacity rejections). That's the only lead against the A/A drift (s1aa 9.67 to 4.6). s1utp's 10.51 to 5.20 is inside the noise;
  - segment 3: s1adm 20.8 (9,000 grants, 0 refusals or capacity rejections, so mechanism check 5.1 passes), s1utp 12.38, s1aa 10.69 (4.79 in segment 2).

### Stalled windows
- **Stage 1 continues unchanged:** no mid-block change and no stitching.
- **A stalled segment reports:**
  - its sampling and locked minutes;
  - the time until the table filled;
  - net per minute over sampling minutes only.
- **Under 30 sampling minutes reads "no signal",** never a no.
- **base's (c) is recorded as a no:** the stall.

### Covariates, and the split first
- **The design concern:** the sampler's cycle confounds the arms. It runs about 35 min of sampling, then about 6 h locked, and the cooldowns restored at start swing between restarts (199,999 to 12,203). The A/A floor swings 2.2×.
- **Covariates:** each segment reports these as descriptive covariates, with no regression at this sample size:
  - the cooldowns restored at start;
  - sampling and locked minutes;
  - the time until the table filled.

  Leads are read only between segments with matching start states.
- **The split goes first.** The split lane (stage 3's treatment, already running) is the priority read. The runner decides after the split's segment 2. If the split keeps sampling through the window with net per minute above base, the split moves ahead: new paired stage-1 blocks run with the split on in both arms after block 1 ends, and block 1 then reads as screens only. **This is flagged (§2.19), for the cloud's provisional runs only:** it puts the split ahead of the stacked stage order (r5.1 A2, §2.79 and §2.80), where the split is stage 3's treatment.
- **L1 memory:** the 16 h soft and hint screens give the unbroken anonymous-memory slope.
- **The split's lead** (one segment, not an effect estimate): segment 2 with the split on stored 19.8 a minute against 11.0 off. Sampler-full time was 0 s against 5,495 s. Anonymous memory rose 120 MiB, with the filter's fixed 32 MiB. **The split ends the full-table stall on the real network.**
- **Spill growth:** spill slots grew about 360k an hour, which reaches the 7.86M fallback after about 20 h of continuous sampling. **Follow-up X-split-spill-horizon** covers what happens at the fallback and how spilled entries age out. It matters for a 24 h CR-COMB run and long-lived installs.

### Stage 3's split A/B runs between instances (2026-11-24, §2.26, §2.19)
- **The measurement:** after a clean restart from a spilled state, the split held sampling (reason `resumed`) for 5 h 25 min on the real network. That matches the up-to-6 h cost in the split's decision. An A/B that switches arms on one instance restarts the node at each switch, so a split-on segment would spend most of its window held.
- **Runner's call:** stage 3's split A/B moves to a between-instances design (option c):
  - paired instances, one per arm, started together;
  - the same instance type, region and image;
  - the arm-to-instance assignment swapped across pairs;
  - within X-test-vm's fleet limit of 10 and the 24 h instance cap (`ExpiresAt`).
- **The hold conflicts with owner-decisions §2.77** for anyone who turns the split on. §2.77 says: "No sampling wait after any start, and no blanket gate carried across a restart." The split is off by default, so no shipped default breaks it today. Flagged to the owner.
- **Why the design is flagged:** the estimate now compares instances, which the within-instance ABBA and BAAB design avoided (r15). Pairing and the swap across pairs keep that spread out of the arm effect. These are provisional cloud runs; the experiment host's runs of record keep CR-SPEC's design.
- **Follow-up X-split-resume-gate** (not launched now under §2.83): shorten or remove the resume hold once cooldowns have spilled, so a user with the split on doesn't lose up to 6 h of sampling after every restart. §2.77 requires it before the split can ship on by default, and, in the runner's reading, before the split is offered as a user setting at all. The split's product note says so until then. Flagged to the owner.

### Screens and spend
- **2026-11-23T15:40Z:** two 16 h screens, soft (#1091's first real-network run) and hint (stage 3), about $2.70 each. 7 of 8 Linux instances were live; the worst-case commitment was about $64 of the $117.50 stop.
- **The owner, relayed from X-test-vm's session at 2026-11-23T21:45Z,** asked for as many tests as needed, preferring to find solutions and issues quickly. It's a request within §2.82's standing "Run as many tests in parallel if needed", not a new decision.
  - X-test-vm raised its Linux instance limit from 8 to 10, its own setting.
  - It launched two on-only screens: `combo` (ADM, split, lookup end and preset slots) and `allon` (all 12 switches). They have `ExpiresAt` at launch + 16 h, about $2.70 each, and aren't runs of record.
  - An ADM and hint on-only screen was approved for X-soft-key's free instance slot.
- **Block 1 is torn down.** Block 2 (BAAB) has segments ending 2026-11-24 about 05:28Z, 08:29Z, 11:30Z and 14:31Z, with teardown about 18:52Z.
- **Spend:** of new AWS spend since 2026-11-23T00:31Z, about $22 was new in this period and about $50 is committed, against the $117.50 stop (§2.83).

## Held findings (ids only)
`docs/security/v1/README.md` §5's hold covers blocking and important findings: their detail stays outside the repository until a release with the fix is published, and merged but unreleased still counts as unfixed. Low findings are listed by id and surface as a matter of course. Ids are never reused.
- **S8-10 to S8-13** (r16): fixed on main by RX-1 (#1082); S8-10 to S8-12 stay held until a release.
- **S8-14:** important, regex search. Fixed on main, held until a release.
- **S8-15:** low, regex search (#1082). Closed.
- **S8-16:** low, search. Open: **X-sec-S8-16**.
- **S8-17:** low, regex search (#1082). Closed.
- **S8-18:** low, api. Open: **X-sec-S8-18**.
- **S8-19:** low, live queries (#1082). Closed.
- **S8-20:** important, live regex queries. Fixed on main, held until a release.
- **S8-21 and S8-22:** low, the IRC client. Fixed before merge in #1100.
- **S8-23:** low, the IRC client. Documented in #1100; IRC-1 handles it.
- **S8-24:** low, Nostr signatures. Closed in code by #1101.

## New slices
- **X-escalate-full-table** (Opus): escalation reaches a struck node on a full table with the split off; a sim test; its decisions file corrects CR-SAMPLE-BACKOFF-ESCALATE's line. Waits under §2.83.
- **X-split-spill-horizon** (Opus): the spill filter's fallback, and ageing or eviction of spilled entries.
- **X-split-resume-gate** (Opus): shorten or remove the resume hold after a spill; §2.77 requires it before the split ships on by default.
- **X-collector-ntp** (Opus): merged as #1098. Its entry is added now.
- **X-soft-absent-void** (Opus, §2.19): void or warn on a soft B-arm segment without the soft-timeout counter.
- **X-unverified-count-heal** (Opus): a way to correct the kept usenet count if it ever drifts; a gate before usenet writes default on.
- **X-live-banner-flake** (Opus, web e2e): the test already waits for the registration call; the session finds the race first (likely candidates: the page's `/events` stream not yet open, or the returned id not yet applied) and never adds the existing wait again.
- **X-regex-permit** (Opus): the live CPU cost per regex request, as RX-1's decisions file names it. API lane.
- **X-ast-regex-roundtrip** (Opus): N3 from the #1082 review (pre-existing). API lane.
- **X-sec-S8-16** (Opus): S8-16 fix (low, search; detail held). API lane.
- **X-sec-S8-18** (Opus): S8-18 fix (low, api; detail held). API lane.

**X-irc-tag-crlf** is IRC-1's first commit, not a slice of its own.

## Notes added to existing slices
- **IRC-1:** X-irc-tag-crlf, the 908-then-904 mapping, stripping `Events` text, and the per-client event budget. Owns gain `line.rs`, `client.rs` and its test.
- **IRC-2:** SASL by default and the `NickServ@services` form.
- **NOS-2b:** owns gain `web/licenses/rust.json`.
- **NOS-3:** the NOS-1 points above. Owns gain `nostr_sig.rs` and `fuzz/fuzz_targets/nostr.rs`.
- **X-size-own-modules-2:** #1096's post-merge polish.
- **X-regex-lazy-verify:** percolation and promoted torrents.
- **RX-2:** the separate `stopped` flag.
- **ENR-debrid:** dropped.

## Owner queue
- **Item 37 (optional):** will a person read TorBox's current terms in a browser before v0.7 closes? If they clearly allow third-party hash checks with the user's own key, ENR-debrid can relaunch for TorBox within v0.7. Silent or ambiguous terms come back to the owner under §2.28.
