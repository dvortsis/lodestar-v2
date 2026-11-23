# runner-records-r16 decisions

The runner's records PR after r15 (#1087). It records:
- the owner's instruction of 2026-11-23T06:09Z and the two balances (owner-decisions §2.83);
- the runner's calls from 2026-11-23T03:00Z to 09:00Z, in the cloud's stage 1, in running slices and on the API lane's PRs;
- new held-finding ids, by id, severity and surface only;
- eight new follow-up slices and owner queue item 36.

Apart from §2.83, everything here is the runner's, under the owner's 2026-11-23 delegation (§2.82) and §2.26 (performance and UX). Each call is flagged to the owner, who can veto it (§2.19). The slices' own decisions files hold the detail.

## The owner's instruction (§2.83)
- **Build slices:** the four running at 06:09Z (CR-SOFT-TIMEOUT, X-detail-depth-compare, X-index-banner-late and X-hit-resolution-nits) finished and merged (#1091, #1094, #1093 and #1095). Their slots stay empty.
- **Queued follow-ups wait,** including the new ones below. Their entries go in now, so nothing is lost.
- **The one new slice since then is RX-1-fixes** (below). The API lane needed it.
- **Spend:** the AWS stop is $117.50 of new spend from 2026-11-23T00:31Z (90% of $130.56). This replaces r15's provisional figure.

## The cloud's stage 1 (X-test-vm)
- **The sampler's table fills on the shipped defaults.** The base lane reached its 200k cap about 35 min after suppression; samples then fell to about 0, and stored per minute fell from 16 to 10. That matches the 2026-11-16 AWS crawl behind X-sampler-cooldown-split.
  - **Runner's call:** stage 1 continues unchanged. The split is stage 3's treatment under the stacked order (r5.1 A2), and changing the control mid-stage would break the design.
  - Each segment's report adds `table_filled_after_seconds` and the stall minutes. Effects are reported for stalled and unstalled time where the data allows.
  - **An optional provisional lane** (split on against off, ABBA, its own instance, not a run of record) is X-test-vm's call, within the spend stop and its instance limit. It launched at 06:15Z.
- **Block 1's instances carry `ExpiresAt` at launch + 44 h, not 24 h.** X-test-vm set the tag at launch and can't lower it. r15 had withdrawn the 44 h. The backstop:
  - the block's lanes terminate by launch + 23 h (2026-11-24 about 00:03Z), with teardown planned for about 20:30Z after copy-off;
  - X-test-vm terminates any instance still running at 2026-11-24T00:00Z;
  - block 2 and every other lane run on fresh instances with `ExpiresAt` at most 24 h.
  - **This is a deviation from the owner's 24 h rule in the tag only.** No instance lives past 24 h. Flagged to the owner. Owner queue item 35 is still open.
- **X-test-vm's Linux instance limit is now 8,** its own setting. Worst-case new spend for stage 1 and the optional lane is about $50 to $58, under the stop.
- **The AWS connector's sign-in lapsed at about 06:45Z** for every session. The owner signed in again at about 07:05Z. Teardowns that need the connector are tracked by X-test-vm.

## Calls in running slices
- **CR-SOFT-TIMEOUT (#1091, merged 60768bb0). The design became A-window; this amends r15's section.**
  - **What changed:** the slice showed that r15's design (a Slow query frees its lane, the 3/4 headroom rule) can't meet the runner's per-scenario dominance ruling. The runner withdrew that ruling.
  - **The merged design:** a Slow query keeps its lane and drops out of the K closest when the lookup picks whom to ask next. The leg stops waiting on it once K others have answered. There are at most `ALPHA` Slow queries per leg, no extra queries, and no cap changes. `SOFT_TIMEOUT` is 1 s, and the switch `dht.soft_timeout` is off by default.
  - **The rule it was held to:** no single wait is lengthened, and the caps are unchanged. Build-plan §0.5's standing rule holds on 8 of 8 seeds. p90 and p99 lookup duration are no worse than off.
  - **The CR-SIM screen** (stored per minute, off to on): with `lookup_end` off, 13.13 to 16.50; with it on, 18.64 to 26.21.
    - With `lookup_end` off, p90 and p99 sit at the 25 s deadline in both arms.
    - With it on, p90 is 24.1 s against 25.6 s, and p99 25.7 s against 26.0 s.
  - **Also in #1091:**
    - a detached Slow query releases the request's shared in-flight count, a liveness fix;
    - a `testing`-feature seam in `dht`, for tests only;
    - one line in `infra/fleet/steps/vm/block-lib.sh` (`CRAWLER_SWITCHES` gains the switch), approved in owns.
  - **r15's description is superseded:** both mechanisms, the 2·`ALPHA` in flight and the 3/4 rule.
  - The design that frees a lane is parked as **X-soft-lane-free**.
- **X-host-ab-runsheet (#1090, merged a6ed7bc8). Three runner calls, all protective:**
  - A backup missed during a block is started by hand at the block's end. That changes no setting.
  - A missed parity check is **not** started by hand; the owner is told.
  - Block 2 starts at block 1's start + 36 h ± 1 h.
  - The mover schedule is the owner's setting: owner queue item 36.
- **X-index-banner-late (#1093, merged 14071a21):**
  - while rows are painted, the banner's line holds and the banner never shrinks or grows;
  - a /search/state failure clears the remembered decision;
  - every fix went in the PR, with no follow-ups.
- **X-detail-depth-compare (#1094, merged 3abc5427):**
  - the Compare route's limit is 31.79 kB, its measured size;
  - the "Differences only" switch is a native checkbox with `role="switch"`; `Toggle.svelte` would have cost 33 kB;
  - its screenshots were checked in review instead of being committed with the PR.
- **X-retry-typed-flake (#1092, merged a818dd34):** the test waits for the live editor before typing (option A). The product fix, a static query bar that shows the query, is **X-querybar-static-q**.
- **X-hit-resolution-nits (#1095, merged 37dafb32):** the API sends a stored resolution only when it's one of `Resolution::ALL`, and null otherwise. So `openapi.json`'s closed enum holds after an MSI rollback or a snapshot restored from a newer build.

## The API lane
- **RX-1 Q10 (on #1082): option (c), re-split the 1,536 B.** The index is at most 560 B and the store at most 976 B per torrent; the total is unchanged. Owner-decisions §2.46 gives (a) to (c) to the runner.
  - The 96-byte prefix is kept. Both gates and architecture §3's line changed in #1082.
  - At 8c4ee3bb the index measured 549.8 B and the store 948.6 B.
  - `.github/workflows/ci.yml`'s size-budget comment now says 560 B and 976 B (it said 512 B).
- **The lane's `max_open_prs` stays 2,** a preset limit. The runner answers its questions within one wake, as `runner:` comments on its PRs.
- **RX-1's reviews found four security issues and ten general items** (2026-11-23T06:30Z and 06:54Z on #1082). Their requirements are on the PR, with no exploit detail. Runner calls in them:
  - **One regex budget per search.** At most 4 regex clauses per query, refused with code `query_syntax` and reason `regex_too_many`, with the message "Use at most 4 /regex/ terms in one search."
  - One deadline is shared by all of a query's regex clauses, its compile, its facet variants and recovery. Facet counts computed under a stop are marked partial.
  - **A stopped verify never advances a saved search's watermark.** The row stays queued for the next batch.
    - A new warn event, `search.percolate_regex_stopped`, carries only the saved search's id and fires at most once per saved search per hour.
    - RX-2 shows the state in the UI.
  - **Leases keep only the verified match set,** charged at its real size. So later pages agree with page 1 and never verify again.
  - A closing `/` needs whitespace, `)` or the end of input after it. Otherwise the text is plain words.
  - Verifying only what the rest of the query keeps is **X-regex-lazy-verify**, required before G-v07-scale. G-v07-scale tests realistic name lengths.
- **RX-1 changes hands.** The lane stopped RX-1 at its per-slice spending limit (about $30, at 08:30Z) and handed the fixes back.
  - The runner kept the lane's limit; the lane didn't continue.
  - **A runner slot, RX-1-fixes (Opus), took `slice/RX-1` over** under §2.83's exception. The lane released the branch.
  - PR #1082 keeps its title, so the dashboard still reads it as RX-1. It merges after a fresh general and security review.
- **X-setup-token-boot (#1083, merged a698fde6):** the runner approved the files outside its owns, retroactively. They're listed in its decisions file.
  - The lane's own review came after the merge, and its points became **X-setup-token-nits**.
  - Runner call: the Continue check logs under its own label, `setup_token`, not as a failed setup.
  - **Headroom note for any slice that touches `web/src/routes/+layout.ts`:** at a698fde6 the Search route has 19 B left and Welcome has 3 B.

## Held findings (ids only)
The detail of each is held outside the repository until it is fixed. Ids are never reused.
- **S8-09:** low, pre-existing. The DHT lookup's bootstrap-router reply path.
- **S8-10:** blocking. Search regex memory (#1082).
- **S8-11:** important. The regex CPU budget (#1082).
- **S8-12:** important. Saved searches and alerts with regex (#1082).
- **S8-13:** low. Stored rule text (#1082).

S8-10 to S8-13 are being fixed in RX-1-fixes.

## New slices
- **X-soft-lane-free:** parked. CR-SOFT-TIMEOUT's lane-freeing design, behind its own switch and screened under the distribution rule.
- **X-trigram-switch:** a `search.trigram` setting to turn the trigram index off. API lane.
- **X-regex-lazy-verify:** before G-v07-scale. API lane.
- **X-setup-token-nits:**
  - the `setup_token` log label;
  - `location.hash` kept in `next`;
  - an injected boot deadline in the test;
  - `Retry-After`'s date form.

  API lane.
- **X-querybar-static-q:** the static query bar shows the query. It must save the bytes it adds to the Search route.
- **X-shell-nav-cls:** the shell's nav links shift at boot (about 9e-5, seen on main too).
- **X-fleet-baab-tails:** rv1089's optional findings (goldens, a BAAB copy-off test, the Linux lane doc).

## Owner queue
- **Item 36:** may the experiment-host operator move Unraid's mover schedule for a host stage, then restore it? The runner recommends yes.
