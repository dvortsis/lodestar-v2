# Lodestar v2 — consolidated DHT crawler implementation plan

> Date: 2026-10-25 (America/Denver). Source inspection: `ad0ec5a84555b8b36e7ae94ed515b84ad6d46e02`.
> Owner authorized consolidation and CR-0 implementation. Later experiments remain proposals.
> Scope: incremental correctness, metadata conversion, fair recovery and measured tuning; no rewrite, database replacement, new worker framework or external dependencies.
> Updated 2026-10-26: §13 product source and regression tests are prepared in `work/soak-product`; full validation awaits collector completion. Crawler experiment gates remain unchanged.

> Current execution: [October 27 hardened-baseline ledger](2026-10-27-baseline-execution.md).
> The October 26 run was interrupted by disk exhaustion; historical launch/progress entries below are not current run status.

## 0. Authority and boundaries

Owner decisions take precedence over architecture, contract docs and build plan. Record refinements in `docs/decisions/`; architecture §16 is closed. Preserve Rust/Tokio single-owner actors, bounded channels, source DRR, coordinator leases/fencing, SQLite WAL single writer, compressed complete file trees, Tantivy, Svelte 5, consent, privacy, content filtering and owner budgets.

Freeze architectural scope. Reject prefix yield scoring, mandatory positive scrapes, privileged-port bans, live cooldown eviction, shared mutable sampler/pool maps, unconditional caches and global host tuning. BEP 51 targets control returned routing contacts, not sampled hashes. A positive Bloom filter is never authoritative proof a torrent is stored.

## 1. Verified integration anchors

| Component under `crates/` | Inspected behavior | Required change |
|---|---|---|
| `dht/node/sampler.rs` | 20k frontier, 200k cooldowns, 60s floor; full map can forget a cooldown after sending | Reserve before dispatch; bounded expiry and exact completion |
| `dht/node/actor.rs` | Ordinary lookups can stall after immediate failure/throttling; only scrapes have overall deadlines | Every request is scheduled or terminal; cleanup all pending RPCs |
| DHT/pool interface | Pool 256 logical waits/30s; actor 32 active/10k blind backlog; no cancellation or acknowledgement | Bounded acknowledged ownership and caller cancellation |
| `dht/krpc.rs` | Borrowed samples collapse absent/empty; encoder omits empty | Preserve presence without allocating hash vectors |
| KRPC transactions | u16 wraps; pending-ID exclusion and endpoint/family checks already exist | Wide session tokens; explicit reuse/restart assumptions |
| Sampling evidence | Responder endpoint discarded; repeat sightings suppressed upstream | CR-1 transient hints; defer T2 pending evidence contract |
| Transport | TCP/MSE wired; uTP library exists but app integration incomplete; lookup starts on family 0 | Honest baseline; one shared budget across configured families |
| `sources/pool.rs` | Socket DRR, separate lookup backlog/eviction, global interest bypass | Fairness at all three boundaries; separate fetch/lookup lifetimes |
| Partial imports | Ascending-ID LIMIT 50k on startup; snapshots lack eligibility timestamps | Runtime durable maintenance with writer outcomes |
| Endpoint policy | Subscribed IP lists, no general public destination restriction | Public guard before admission and dispatch; local fixture override |
| Workers | Coordinator leases exist; P4 remote sharding planned | No independent scheduler; targets do not partition responders |

Verify current HEAD before each slice. Prior soak evidence is in `docs/soak/2026-10-24-v0.5.0-metadata-throughput.md`; its binary commit was not established. Do not attribute every live failure to one source defect.

## 2. Sequence

| Slice | Deliverable | Gate |
|---|---|---|
| CR-0a | Exact sampler, conservative restart, codec/wire-token correctness | First correctness slice |
| CR-0b | Public endpoint policy, socket diagnostics and observability | Before baseline |
| CR-0c | Lookup progress/deadlines, acknowledged admission, cancellation, shared family resources and actor fairness | Before baseline |
| Baseline | 24h repaired TCP/MSE soak, random targets, 60s floor | CR-0 tests pass |
| CR-1a | Responder-assisted peer lookup and streaming | Reliable lifecycle and baseline |
| CR-1b | Reversible 256-prefix target experiment | Measure CR-1a first |
| CR-2 | T0/T1/T3 fairness; T2 deferred pending sound evidence contract | Stable interfaces |
| CR-3 | Durable fair partial upgrades | Stable lease/outcome interfaces |
| CR-4 | Separately measured uTP and optional BEP 33 piggyback | No prerequisite for TCP correctness |
| CR-5 | Conditional verified-known cache/other tuning | Profiler evidence |
| CR-6 | Documentation, acceptance, release review and soak | Applicable slices |
| P4/v0.6 | Worker coordination and safe responder handover | Existing roadmap |

CR-0 means CR-0a/b/c, not the later optimizations. Keep reviewable commits/PRs. Do not renumber existing roadmap phases or block packaging on experiments. This task does not merge, deploy or alter a running soak.

Independent product work during the baseline wait is tracked in [§13](#13-product-work-during-the-soak). It does not advance CR-1 or change the baseline configuration.

## 3. CR-0a: sampler and protocol

One authoritative state per canonical endpoint: absent, reserved/in-flight with a request token, or cooling with deadline/generation. Frontier capacity is separate.

- Reclaim expired obligations, then reserve immediately before dispatch; states never exceed the initial 200k cap. Full capacity defers samples without losing eligible frontier items or spinning.
- Matching local send failure aborts a reservation. Successful sends remain owned until matched response/error/timeout. Stale completions cannot alter another request. Only one sample per endpoint is outstanding.
- Success wait is `max(remote_interval, 60s)` within the BEP 51 0–21600 domain. Invalid/missing data follows explicit conservative failure policy. No premature eviction/shortening.
- Expiry records include generation checks. Bound heap growth and expiry work per turn; no full-map scan per packet. Instrument state/frontier/heap/deferred/stale counts without endpoint labels.
- Saturation suppresses new samples, not ordinary replies, routing or independent peer work. Safe saturation is observable backpressure, not a correctness failure.

### Restart choice for CR-0

Use conservative startup sampling suppression for the maximum remote interval plus query timeout. This needs no new log, wall-clock assumptions or per-packet durable writes and survives process crashes. Cost: up to six hours without BEP 51 sampling after each production start; passive discovery and peer lookup continue. Start the measured baseline after suppression; report it separately.

Only explicit isolated/fresh-history library or fixture callers may override this delay. Do not silently disable it in application wiring. Future persistence is separate work requiring pre-send durability, latest dispatch validity, outstanding-request recovery, atomic snapshot/log sequencing, partial-tail handling and clock policy. Async disk I/O alone does not establish these guarantees.

### Protocol

- Preserve borrowed `Samples` with explicit presence. Encode empty samples on BEP 51 replies only. Test missing/empty/nonempty/invalid and property round trips.
- Use wide wire tokens with random session start and no session reuse; exhaustion stops new dispatch instead of wrapping. Preserve endpoint/family/node-ID checks. Random restart identity is probabilistic protection, not a guarantee against every delayed packet.
- Tie sample reservation to the real pending transaction. Late successful duplicates and timed-out replies must not release new work.

## 4. CR-0b: endpoint policy and diagnostics

Reject non-public/private/reserved/loopback/link-local/multicast/unspecified destinations and port zero for public queries and metadata dials. Normalize mapped IPv4. Explicit local workflow/fixture override is required. Filter before consuming candidate capacity and recheck at final network dispatch with subscribed-IP rules. No privileged-port ban. Do not indiscriminately apply outbound discovery policy to inbound protocol service.

Report requested/granted socket receive/send buffers and option/readback failures through aggregate diagnostics/typed events. Missing kernel-drop counters remain unknown. Preserve private-source routing, source consent, pause, filtering and metadata-only behavior. No sysctl/NOTRACK changes.

## 5. CR-0c: lookup lifecycle and dispatch

### Ownership

Requests carry hash, identity, bounded lifetime and cancellation/completion capability. Full queue/actor capacity is backpressure, not an attempt. Bound all handoff queues and accepted work; remove the unowned 10k FIFO from application use.

Actor acknowledgement establishes ownership; only accepted work consumes an attempt. Caller cancellation/drop reaches queued work, all family legs and pending RPCs. Completion is reliable and separate from lossy discovery emission. Source/actor shutdown closes ownership. Stale completion cannot affect a new attempt for the same hash. No unbounded task per discovery.

CR-0 returns terminal peer batches; streaming is CR-1a. Pool timeout/eviction, successful fetch and alternate peer arrival cancel still-owned requests. CR-0 uses receiver-drop cancellation: the caller releases its reservation locally once; the actor observes closure and independently reclaims queued work, family legs and RPC accounting under its own hard cap. No terminal delivery is promised to a dropped receiver. Non-cancelled completion remains reliable. This resolves the earlier conflicting acknowledgement/completion sketches; see the CR-0 decision note.

### Progress and families

Every lookup, including maintenance, has a deadline. Zero outstanding RPCs with fresh candidates is allowed only with a scheduled continuation. Throttling/WouldBlock retries later; permanent send failure advances without losing the continuation. All terminal paths clean pending RPC/address accounting, including scrape deadlines.

One logical request owns all configured families, one deadline/attempt and a bounded deduplicated peer set. Family 0 is not necessarily IPv4. Completion waits for all legs or a logical terminal cause. Initially retain 32 logical peer requests, existing global caps and shared per-request RPC concurrency rather than doubling resources on dual stack. Bound total query work consistently and terminate on exhaustion. No contradictory per-leg/aggregate constants.

Ensure admitted lookup continuations and routing get bounded dispatch opportunities before sampling. All query paths, including response-driven continuations, obey the same hard caps. Packet-rate and outstanding-slot budgets are separate. Prove liveness under saturation; do not adopt unsupported fixed lane percentages. Later share policies accumulate fractional credits across ticks and lend unused capacity.

## 6. Baseline and decisions

After CR-0 tests, run a 24h TCP/MSE baseline on the intended deployment with unchanged random targets/60s floor. Record SHA/configuration/OS/Docker/VPN/routes, grants, suppression end, warm-up and interruptions. Running-deployment changes are outside local code verification.

Measure actor admitted/queued/active/scheduled/terminal requests, pool attempts/evictions, first-peer/completion latency, raw and unique sample evidence, verified searchable yield per hour/byte/RPC, cold-cohort failures/delay, writer work, RSS and available drops. Include failures and cohort lag, not only latency among successes.

Use matched budgets/comparable windows, repeated or alternating variants where practical, and measured variance. Predeclare primary metric, practical threshold, observation window and guardrails per experiment. Universal 10% noise and 15% improvement claims are unsupported. Inconclusive variants stay off. A small timeout delta does not prove absence of fragmentation.

## 7. CR-1: hints/streaming, then prefixes

CR-1a retains responder endpoint and monotonic hint age only while pending. The DHT responder is not a peer-wire endpoint. Direct get_peers obeys global and per-responder caps, TTL and logical deadline. Choose consistent endpoint/IP rate units; fallback must not bypass the same responder guard. Preserve actual XOR distance. Expired/throttled hints fall back without delaying the lookup. Never evict an unexpired rate restriction as idle.

Streaming separates `PeersFound` from reliable terminal completion and tracks lookup activity independently from fetch state. Filter/deduplicate bounded peer batches; start fetching without releasing lookup ownership. Test terminal-channel saturation, cancellation races, stale generations, source failure and exactly-once cleanup.

CR-1b tests 256 eight-bit target prefixes with random suffixes and independent family cursors. Keep random rollback and existing refill watermark initially. Attribute routing novelty to target, hashes to responder/call. No complete-coverage claim. Test wrap/pause/cycles. Measure separately from CR-1a and any cooldown-floor change.

## 8. CR-2: fairness, optional corroboration

Preserve inter-source DRR. T0 is interactive interest (not bulk import), T1 has a fresh untrusted peer hint, T3 is cold. T2 remains deferred until independent evidence delivery is defined.

Guarantee service opportunities at lookup admission, retention before useful work and eligible socket dispatch. Borrow unused shares. Bound interactive bursts without indefinite starvation. Measure eligible admissions/resource time/age, not completion shares for requests with differing duration. No socket entitlement for a key without peers.

T2 must preserve exact hash identity, >=64-bit keyed reporter tags, epoch/expiry, candidate generation and bounded channel/drop behavior. No actor/pool shared mutable map, bare/XOR key mixing in the canonical Bloom filter, or claims that exact pool checks recover evidence dropped upstream. Self-generated lookup results are not independent discovery. Promotion preserves provenance/lease/cohort.

## 9. CR-3: durable partial upgrades

Replace lowest-50k startup replay with a runtime due-work producer and durable eligibility. Fetch snapshots alone cannot express it; add only minimal indexed state and measure storage growth.

- Explicit upgrade origin survives lease/pool/preparation/persistence/restart. Bypass only the stored-hash shortcut; preserve consent/private routing, filters, claims and fencing.
- Small work-conserving maintenance service floor, not idle-only admission.
- Separate admission, actual network attempt, terminal failure, eviction/defer and commit. Queue rejection/eviction is not a failed network attempt. Retry cadence is policy.
- Writer atomically upgrades canonical data, preserves provenance/corrections, clears partial/retry state, appends reindex work and records success. Rollback records no success.
- Discovery conservation remains independent of maintenance counters. Peer-found/fetched are distinct; admission is not attempted. Concurrent sightings merge provenance/peers without turning maintenance into new discovery.
- Test 100,001 partial rows with first 50k unavailable, crash boundaries, due recovery, saturation, eviction, import/delete/quarantine races and bounded growth. Enrichment backfill remains independent.

## 10. CR-4/5 and P4

CR-4: uTP wiring is a separate measured slice. Optional BEP 33 piggyback checks 256-byte blooms, missing-field compatibility and packet budgets; estimates remain provisional until verification. Zero is not rejection or independent corroboration. IPv6 route backoff requires route-level evidence, not peer timeouts.

CR-5: profile touches before adding a bounded verified-known cache. SQLite confirms identity; retain fallback/invalidation for sources, retention, deletion and quarantine. One policy change per experiment.

P4: extend existing leases. Target partitions do not partition responders. Stable responder ownership requires epochs and worker-enforced lease expiry; coordinator fencing cannot stop a partitioned sender. Handover preserves obligations or suppresses through the last permitted send plus outstanding-response/cooldown horizon. Periodic summaries alone are insufficient. Shared-egress workers share relevant budgets. Do not freeze shard count/hash design now.

## 11. Deterministic gates and CR-6 acceptance

| Boundary | Failure tests |
|---|---|
| Sampler | duplicate offer pending/cooling, capacity edge/full, bounded expiry bursts, stale token/generation, send failure, timeout/late response, intervals, startup suppression, ordinary replies while blocked |
| Codec/wire | absent/empty/nonempty/invalid fields, borrowed round trips/properties, mismatched endpoint/family, late successful duplicates, exhaustion/restart assumptions |
| Endpoint | v4/v6/mapped, reserved/private/port-zero, pre-admission/final deny, explicit fixture override |
| Lifecycle | all sends fail/throttle, scheduled progress/deadline, rejection without attempt, cancel before/after admission, duplicate terminal, full channels, shutdown, bounded queues/RPC cleanup |
| Families/fairness | absent/failing family, shared deadline/concurrency, aggregate completion/cancel, lookup and maintenance progress during sample saturation |
| Streaming | batch before finish, cancel race, full channel, stale generation, concurrent fetch, exactly-once release |
| Upgrade | separate maintenance/discovery accounting, rollback, lease restoration, concurrent sightings/filter/deletion, durable fair retry |

Use artificial clocks and condition waits with >=10s deadlines. A cap is not a leak test: drain all requests and verify zero remaining ownership. Throttled fresh work must be scheduled or terminal, not necessarily already in flight. Exact counter assertions require explicit operation sequences.

Run fmt, relevant unit/integration/property tests and clippy; workspace gates before push. Fuzzing, Windows/Linux/Docker, VPN/Unraid checks and a matched 24h post-change soak are release gates with honest executed/not-executed status. Preserve 10M-torrent/300M-file search/ingest budgets, ~2.3KB/torrent, owner RSS and Insights overhead limits. Small fixtures do not prove full-scale gates.

Update architecture §§5/6/7/18.4/19.5 only where implemented, build-plan alongside P0–P4, metrics/events and decision notes. No default IP/hash labels. Stop experiments on correctness/security/privacy regression, lost cold progress, unbounded memory or demonstrated budget violation. Keep correctness fixes on rollback. Fresh protocol/security review and CI precede merge.

## 12. Execution record

### CR-0 implementation validation (2026-10-25)

- Owner authorized consolidation and CR-0 on 2026-10-25. Branch `fix/dht-cr0`, source `ad0ec5a8`. Prior untracked review/soak files are not implementation changes.
- Implemented CR-0a/b/c: reserved sampler ownership with bounded expiry, six-hour-plus-timeout restart suppression, absent/empty samples, non-reused eight-byte session transactions, shared public endpoint policy, socket readback diagnostics, reliable acknowledged/cancellable peer requests, lookup deadlines/continuations, and shared dual-stack limits. Maintenance precedes packet service; socket receive selection remains unbiased between families.
- Updated architecture/build-plan/metrics and [the CR-0 decision record](../decisions/2026-10-25-CR-0.md). The new internal policy crate and diagnostic event are registered in the application's logging targets/catalog fixture.
- Validation passed on Windows: `cargo test --workspace --offline -j 4 --quiet --no-fail-fast` (2,105 passed, zero failed, 16 ignored, including doctests); `cargo fmt --all --check`; `git diff --check`; and strict all-target Clippy for `lodestar-dht`, `lodestar-network-policy`, `lodestar-peer-wire`, `lodestar-sources`, and `lodestar-pipeline`. The initial workspace run caught the missing logging target/catalog registrations; both were corrected before the successful rerun.
- `cargo clippy --workspace --all-targets --offline -j 4 -- -D warnings` fails on existing Windows-only dead-code diagnostics for `from` and `closed` in `crates/cluster/tests/transport.rs:469,476` (present at the source HEAD and untouched here). The full workspace lint gate is therefore not green. The application build also reports the existing missing `web/build` stub-page warning; this Rust validation is not a packaged-UI validation.
- Not executed: fuzzing, Linux/Docker/VPN/Unraid validation, scale gates or the post-suppression 24h soak. No deployment or live speedup is claimed; later slices remain unimplemented.

### Subsequent support work and soak launch (2026-10-26)

- CR-0 binary commit: `43d054df962098812023fc5d95df139813a429df`. The owner authorized stopping the Docker crawler and starting the isolated native Windows soak. Sampling suppression ended at 13:48:47 MDT on 2026-10-26. At the 14:04:54 MDT status sample, the run was healthy and in warmup; the measured baseline had not started. This is launch/progress evidence, not soak acceptance.
- Support work is complete in local commit `80d7e198` on `work/cr0-soak-tools`: a read-only aggregate analyzer, five lifecycle regressions and Windows-only lint cleanup. Its recorded validation passed 2,110 Rust tests (16 ignored), 16 Python tests, workspace all-target Clippy, formatting and diff checks. This supersedes the lint limitation above for the support checkout only; it does not change the running binary. The packaged-UI, Linux, fuzzing, scale and soak gates remain outstanding.
- The support checkout is `target/worktrees/cr0-soak-tools`; its validation record is `docs/soak/2026-10-26-support-tools.md` in that checkout. Live run state is in `target/soak/cr0-2026-10-25-43d054df/status.json`. Use actual collector completion rather than the estimated finish time to release deferred validation work.

## 13. Product work during the soak

The owner selected product fixes and a small setup completion screen rather than a full onboarding wizard. The four slices below are prepared in the isolated product worktree; see the [execution record](2026-10-26-soak-product-execution.md) for changed files and executed/deferred checks. Their identifiers are local task labels, not new crawler phases. No database migration, new dependency, backend API change or crawler tuning is included.

| Task | Priority / order | Deliverable | Status |
|---|---|---|---|
| PW-1 | 1 | Detail-cache visibility and stale-response protection | Prepared; regression execution deferred |
| PW-2 | 2 | Setup completion screen with Search and Settings actions | Prepared; browser/build gates deferred |
| PW-3 | 3 | Browser Back focus restoration and Discover All browsing | Prepared; regression execution deferred |
| PW-4 | 4 | Accurate build, shutdown and Unraid logging instructions | Source/doc checks passed; package smoke deferred; existing material-lint failures recorded |

### PW-1: detail-cache visibility

**Problem:** cached detail and search-hit metadata can remain visible after content visibility changes. Cache presence is not authorization to display metadata.

- Revalidate `/torrents/{hash}` on every detail open before exposing cached identifying metadata. Gate associated search-hit metadata on the same successful validation.
- Invalidate caches and clear/revalidate mounted detail on content-settings events, event resynchronization and session changes. Preserve existing bounded cache sizes and expiry rules.
- Cancel in-flight model and prefetch requests. Use generation checks so late responses cannot restore cleared data even if cancellation races with completion. Purge affected entries on hidden/not-found/authentication failures; failed revalidation must not reveal cached content.
- Keep the change within the detail model/cache/prefetch lifecycle and existing settings/session event plumbing. Broader Search/Discover visibility redesign is outside this slice.

**Acceptance:** regressions cover close/reopen, visibility changes in another tab, visibility resync, logout, request failure and delayed responses after invalidation or navigation to another hash. No cached title or detail is displayed before successful revalidation, and stale requests cannot repopulate invalidated state.

### PW-2: setup completion screen

Replace the placeholder `web/src/routes/welcome/+page.svelte` with a short setup-success message, primary **Go to Search** action to `/search`, and secondary **Review settings** action to `/settings`. Reuse existing components, tokens and navigation conventions; provide English and Chinese messages. Use existing authentication checks to handle refresh/direct visits without falsely showing setup success to a signed-out or setup-required visitor.

**Acceptance:** successful setup reaches a usable page; both actions navigate correctly; keyboard order, visible focus and page focus work on desktop/mobile. Refresh and direct visits follow authentication state. The screen performs no settings writes and changes no defaults or consent. A full onboarding wizard is outside this task.

### PW-3: search and detail navigation

- Keep the detail host lazily loaded, then mounted for the Search page's lifetime so it observes drawer closure on browser Back. Restore focus to the connected invoking result, or the search input if that result disappeared; do not steal focus from a control the user already focused. Preserve cleanup on route exit and direct full-page detail behavior.
- Add frontend URL parameter `browse=1` to Discover's **All → See all** destination. Empty-query browsing uses the existing match-all search AST; explicitly typed queries take precedence, and parse errors never fall back to match-all. Typed-category links keep their existing queries.
- Load results immediately for explicit browsing. Include browse state in request identity and keep results, facets, pagination and sorting consistent. Preserve density; Clear/reset removes browse mode and restores the normal empty Search landing page. Browser Back/Forward restores URL-driven state.

**Acceptance:** browser regressions cover drawer Back/Forward, explicit close, removed invokers, route exit, direct detail visits and focus restoration. Search tests cover All and typed-category links, browse pagination/filtering/sorting, query precedence, invalid queries, Clear/reset and history navigation. The only new external interface is the frontend URL parameter; search HTTP contracts remain unchanged.

### PW-4: installation and shutdown documentation

- In `docs/operations.md` and `packaging/linux/README.md`, document frontend installation/build (`npm ci`, then `npm run build` in `web/`, with the declared Node prerequisite) before Rust builds that embed the UI. Preserve platform-specific Rust flags and make the missing-frontend stub limitation explicit.
- Add `--stop-timeout=60` to the standalone Docker example in `packaging/docker/README.md`, matching the existing Compose/template grace period.
- Correct the portable Windows README and launcher guidance: press Ctrl+C, wait for shutdown to finish, then close the window. No new console-event handling is included.
- Correct the Unraid guide's claimed Log level field: document an optional custom `LODESTAR_LOG` variable and its default instead of adding a template field. Keep setup facts and related guide references consistent with the actual templates.

**Acceptance:** commands match package scripts and templates, documentation lint/sync checks pass, and shutdown instructions match implemented behavior. Follow the project's user-documentation skills when editing those materials. Any packaged-UI smoke test uses a fresh fixture after the soak, never the live data or ports.

### Execution boundaries and validation

- Start product implementation from support commit `80d7e198` in a separate worktree/branch, outside the live evidence directory. Keep one reviewable commit per slice; split PW-3 further if useful. Check current source and concurrent work before implementation. This documentation update does not start implementation or publish changes.
- During warmup and the measured baseline, prepare source, tests and documentation with lightweight checks. Defer dependency installation, heavy builds, browser suites, fuzzing and benchmarks on this host until the collector has actually finished. Record deferred checks explicitly; prepared code is not validated completion.
- After the soak, run targeted cache/navigation/auth regressions, then required frontend checks, unit tests, build, size budgets and applicable browser suites. Include keyboard, reduced-motion and desktop/mobile coverage for affected flows, using existing themes and synthetic fixtures. Run documentation lint/sync checks for PW-4 and Rust gates if implementation expands into Rust. Do not weaken the repository's pre-push requirements.
- Keep the active executable, collector, configuration, data, network settings and ports unchanged. Never clean the root `target/` tree: it contains live evidence and the support checkout. Use separate fixture data and ports for later runtime checks.
- Keep the original Docker crawler stopped throughout the soak. Its restart restriction remains at least six hours plus four seconds after the actual native crawler stop; no automatic restart is part of these tasks.
- Record each slice's changed files, executed/deferred checks and remaining limitations before marking it complete. CR-1 and later crawler experiments retain their existing gates regardless of product-task progress.

References: [BEP 5](https://www.bittorrent.org/beps/bep_0005.html), [BEP 33](https://www.bittorrent.org/beps/bep_0033.html), [BEP 42](https://www.bittorrent.org/beps/bep_0042.html), [BEP 51](https://www.bittorrent.org/beps/bep_0051.html), project authority documents and 2026-10-24 soak/research reports.


## 14. Authorized unblocked follow-ups (2026-10-26)

The owner authorized completing the unblocked follow-ups proposed during the soak. Source and regression cases are prepared on `work/soak-followups`, based on product-preparation commit `07421c3c`, in `target/worktrees/soak-followups`:

- Snapshot retention: a separate V12 deletion journal survives temporary quarantine expiry/release and failed cleanup. This is an explicit extension beyond §13's frontend-only scope; no live schema is migrated.
- Search: a bounded retained file-ID cache and cancellation during cold compilation, including facet recompilation.
- UI: measured drawer fit, fullscreen background focus isolation, measured Insights jump offsets, narrow-row fact priority, heading/landmark corrections, complete short-list sizing, and explicit unfinished-destination states.
- A dependency-ordered post-soak validation and release checklist.

See [execution and validation sequence](2026-10-26-soak-followups.md) and the retention/search/UI decision notes linked there. These changes have source-level checks only; full Rust, frontend, browser, scale and packaged-UI acceptance remain pending. The §13 live-run isolation and actual-collector-completion gate remain unchanged. No push, merge, deployment or crawler restart is included.

## 15. Additional authorized findings (2026-10-26)

The owner subsequently authorized fixing the remaining partial-import name/archive defects, updater persistence ordering, and the 27 material lint failures/two warnings. These are prepared above the §14 commits in the same isolated worktree. The documentation gates now pass; new Rust regression execution, including the Unix-only updater test, remains deferred. See the [additional execution record](2026-10-26-soak-final-findings.md) for scope, decisions and post-soak commands. CR-1 and the live-run restrictions are unchanged.
