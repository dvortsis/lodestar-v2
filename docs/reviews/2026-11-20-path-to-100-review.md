# Review of the path-to-100 plan

Review date 2026-11-20 UTC. Reviewed: [the path-to-100 plan](https://github.com/dvortsis/lodestar-v2/blob/29abb73f82d31c1ab2e8ea26c12ddaaeaa70ddf0/docs/reviews/2026-11-17-path-to-100-plan.md) and the 2026-11-16 documents it consolidates on `review/dht-crawler-survey-20261116` at `29abb73f`: the throughput strategy review, the Antigravity counter-review, the research extension, the additional-methods note and the crawler survey. Also reviewed: the other 2026-11-16 work on the crawler rate, which is the 13 h AWS crawl on `main` `aa185384` and the runner's `X-sampler-cooldown-split` lead in `runner-state`.

Evidence: the [CR-0 final report](../soak/evidence/2026-10-30-cr0-final/cr0-final.md) and its `samples-hourly.jsonl`, the [comparator decision](../decisions/2026-11-08-cr-ab-comp-1-accepted.md), the CR-ADM, CR-1a-H and CR-1a-S decision records, and `crates/dht/src/node/{lookup,actor,mod,sampler}.rs` on `main` `a3f8b6a9`. No code was changed and no crawl was run for this review.

**The plan's method is right, but it treats the limiting stage as unknown when CR-0 already measures it.** Lookup service is saturated. Each admitted lookup holds one of the 32 slots for about 24 s and costs up to about 59 DHT queries. The metadata fetch stage meanwhile uses under 5% of its sockets. The path to 100/min is to **make lookups shorter and cheaper, then run more of them, and keep discovery flowing through the worst hour**. Admission, freshness and transport work help, but at today's numbers they come second. Keep the live order the owner set. Start building the lookup changes now rather than after Checkpoint 1.

## 1. The capacity ledger can be filled today

The plan's model (§3: `L`, `W`, `y`, `q`, `Q`) is the right frame. Its Step 0 treats the parameters as still to be measured, but CR-0's 24 hours already give them:

| Quantity | CR-0 value | Source |
|---|---|---|
| `L`, mean occupied peer-request slots | **30.3 of 32**; 30 or more in 88% of the 76 hourly snapshots; never queued (`dht_peer_queued` 0) | `samples-hourly.jsonl`, `dht_peer_active` |
| Admitted lookups/s | **1.286** (111,177 in 86,440 s); completions match (111,147) | report table |
| `W`, mean slot residence (Little's law, `L / λ`) | **≈ 23.6 s**, against a 25 s `LOOKUP_DEADLINE` | derived |
| `y`, stored per admitted lookup | **0.160** (17,783 / 111,177) | report table |
| Check: `60 × L × y / W` | **12.3/min**; measured 12.34 | derived |
| `q`, wire queries per admitted lookup | **≤ 59**, probably 40–55 (6.58M queries also include sampling and maintenance) | report table |
| DHT queries used | 76/s of the Balanced 200/s; outstanding median 144 of 1024 | report table, gauges |
| Metadata fetch | 4.71 attempts/s; mean 17.9 of 400 sockets busy, so **≈ 3.8 s per attempt** | report table, `fetch_active` |
| Pool backlog | `fetch_pending` pinned at about 200,000; 98% of harvest events evicted before service (event ratio) | gauges, report |

The plan says the peer-request gauge "was not uniformly full" (§3). That reading comes from the analyzer's equality test, which flagged exactly 32 in 435 samples. At a mean of 30.3, with no queue and 95.5M capacity refusals, the stage is saturated. A slot caught between release and the next grant reads as 31, so the equality test misses it.

**What 120/min needs at the measured yield (`y` = 0.16):** 12.5 admitted lookups/s. With 32 slots, that means `W` ≤ 2.6 s. With about 150 queries/s left for lookups after sampling and maintenance, it means `q` ≤ 12. Today `W` ≈ 24 s and `q` ≈ 40–55. **Neither 32 slots nor today's lookup can get there.** Lookups must get about 5–9× shorter and about 4× cheaper, or `y` must rise, or both.

Caveats: these are hourly point samples (n = 76) from a Windows host, before CR-ADM. `W` can be read directly from `dht_peer_lookup_seconds_total` / `dht_peer_lookups_answered_total`, exported since CR-1a-H, but neither CR-0's report nor the 2026-11-16 AWS report kept them.

## 2. What works in the plan (keep it)

- **The result is defined before anything is optimized.** The plan uses the CR-SPEC numerator, reports strict exceedance of 100 separately, and sets 120/min as a stated engineering target. It refuses to restart the latency clock.
- **The capacity model.** It has separate lookup, packet and metadata bounds, and keeps logical fetch tasks apart from transport attempts. Every stage gets checked against these.
- **The counter-review's corrections, which I rechecked and agree with:**
  - `State::Skipped` is behaviourally the same as `Failed`;
  - the 67-minute FIFO figure is a scenario, not a measured p95;
  - 98.33% (unaccepted), not 99.47%;
  - continuations already run before sampling in `maintain`.
- **No multiplying of unrelated percentages,** and a defined failure path in §6.
- **The persistent SQLite scheduler is deprioritized.** Its ten reproduced counterexamples are correctness blockers, not speed.
- **Diagnostics before freezing a build.** The plan should narrow these to what the ledger needs (§5 step 0).

## 3. Blind spots

1. **There is no lever to end a lookup once it has peers.**
   - **How a lookup ends today:** only when the 8 closest live candidates have all answered (`Lookup::is_done`), with 3 queries in flight, a 4 s per-query timeout and a 64-query cap. About 38% of DHT queries time out.
   - **What a crawler needs:** peers, not announce tokens.
   - **Simulator evidence that the tail is most of `W`:** in the streaming screen, first peers arrive after 0.34–0.39 s, against 5.5 s lookups (14–16× sooner) on `healthy` and `admission-pressure`. That record also notes that "the slot isn't freed, so streaming brings fetches forward without letting more lookups run".
   - **What to do:** end a request, and free its slot, once it holds enough values. That cuts `W` and `q` together and changes no rate, cap or politeness rule.
   - **Gap in the documents:** neither the plan nor any 2026-11-16 document proposes it. Soft timeouts and routing quality appear, but they make convergence faster, not unnecessary.
2. **Responder hints were judged on a screen that couldn't let them work.**
   - **Why the screen couldn't succeed:** by its contract (CR-1a-H), a hint is one extra query followed by the full iterative lookup in the same 25 s slot, so `W` can't fall. 94% of hints expired while their hashes waited behind the saturated slots.
   - **Why they still matter:** a BEP 51 responder stores the hashes it samples. A hinted `get_peers` is the one route to `q` ≈ 1–2 and `W` ≈ one round trip.
   - **What the plan should test:** dispatch the hint at capture (CR-2's T1 tier), and end the request as soon as the hint returns values.
   - **The per-IP guard (60 s) can stay as it is.** Use one hash per responder. While sampling there are roughly 1,500 or more distinct responders a minute. At 120/min only about 750 lookups a minute are needed in total, and hints need cover only part of that.
3. **The slot count is a real limit.** The plan says not to prescribe raising 32 (§3). At a measured `W` of about 24 s and slots full, raising `L` is necessary unless `W` falls below about 3 s, and a converging Kademlia lookup won't do that.
   - CR-1c (slots sized to the preset) is queued after Checkpoint 1.
   - Build it with item 1. Raise `L` after `q` falls, so that the Balanced 200/s budget is what binds, never the outstanding cap.
4. **The 2026-11-16 AWS crawl and the sampler lead aren't in the plan.**
   - **What the AWS run showed** (8.16/min): the sampler's 200,000-entry table fills about 35 minutes after suppression ends. Sample timeouts get the full 6 h cooldown (`SAMPLE_TIMEOUT_BACKOFF` = `MAX_INTERVAL`), so timed-out nodes fill much of the table. Sampling then idles for about 5.5 h, and stored falls from about 12 to 6–8/min.
   - **Why it matters later:** at 12.5 lookups/s, a 200,000-key backlog lasts about 4.5 h, and old keys convert worse. Stalls would break "no hour below 70".
   - **What to do:** `X-sampler-cooldown-split` is required for the worst hour, though not for the mean. A time-bucketed filter for timeout cooldowns would keep them in a few MB, and a false positive only skips a node; it never shortens a cooldown. It also keeps responders coming for item 2.
5. **The fetch-stage illustration overstates socket pressure.** The plan assumes 8 s per fetch task. CR-0 shows about 3.8 s, so 38 attempts/s needs about 145 of 400 sockets. Conversion has room; it isn't the next limit.
6. **The simulator isn't calibrated to the live lookup.**
   - **The gap:** its lookups take 5.5–16 s and its baselines reach 30–76 stored/min. Live lookups hold their slot about 24 s and store 12/min.
   - **What follows:** "inconclusive" results for lookup-side changes (hints, streaming, prefixes) are weak evidence either way.
   - **Before using it to reject a lookup change:** match its timeout share (about 38%) and its slot residence.

## 4. Concerns

1. **The timeline puts implementation behind two experiments that can't move `W` or `q`.**
   - **The cost:** Step 1 keeps CR-AB-ADM → CR-AB-4 → Checkpoint 1 before new work. That is about 68 host-hours per treatment plus retries, so roughly two weeks.
   - **The likely outcome:** CR-ADM's best screen (+30%) projects to about 16/min, so Checkpoint 1 will report far short. Only then would Step 2 and Step 3 begin.
   - **The fix:** building doesn't need the host. Build the lookup-cost changes alongside the live runs.
2. **Runs lose the data that decides this.** The AWS report's file went with the instance, and no report kept lookup seconds or queries by purpose. Every run, cloud lanes included, should keep `W`, `q` and `y`.
3. **The plan's Step 2 is real but second-order at these numbers.** Freshness raises `y`, but with 98% of harvest events never served, serving more matters more now. Of CR-2, take only the T1 hint tier now.
4. **The packet budget is one shared bucket.** Sampling (up to 100 per tick), verification pings, refresh and lookups all draw from the same 200/s. As lookups get cheaper and more numerous, the two sides compete. Give lookup continuations a bounded reserved share and keep sampling live.
5. **Two things stay unknown until measured.**
   - How often a live hinted `get_peers` returns peers. The simulator's 15–42% is optimistic.
   - How much `y` falls when a lookup stops early with fewer peers.

   For both, decide on output per slot-second and per query, not on first-peer latency.

## 5. Recommended path

0. **Now, one small slice: metrics the ledger needs.**
   - **New metrics:** a histogram of slot residence by end reason, queries by purpose, time to first values, values per lookup and hint outcome.
   - **The collector keeps:** these, plus the existing `dht_peer_lookup_seconds_total` and `dht_peer_lookups_answered_total`.
   - **Timing:** land it before CR-AB-ADM's build is frozen. If it isn't ready, run CR-AB-ADM anyway; the existing lookup-seconds pair still gives `W`.
1. **Host: keep the owner's order.** CR-AB-ADM, then CR-AB-4, then Checkpoint 1, which is the owner's 2026-11-02 choice. Checkpoint 1's report then carries the measured ledger and items 2–5 as the option.
2. **Build now: a cheap-lookup switch.**
   - **One mechanism behind one switch:** end a `get_peers` request when it holds enough values, or its closest set has answered, whichever comes first. Start a request on admission instead of the next tick.
   - **Unchanged:** the 25 s hard deadline, the rate and the outstanding cap.
   - **Then:** a bounded soft timeout as a separate step.
3. **Build now: hint first, finish on values.**
   - Dispatch at capture, one hash per responder, with the guard unchanged.
   - It shares the switch family with CR-1a-H, but is a new contract, so it needs a new decisions file.
4. **CR-1c: slots sized to the preset,** once `q` has fallen, bounded by the 200/s Balanced budget.
5. **`X-sampler-cooldown-split`,** so supply and hint responders last through the worst hour.
6. **Then the conversion work:** CR-4's outcome, fetch phase timing and freshness, to raise `y`.

**How to decide each step:**
- **Keep a change** only if both `y·L/W` and `y·Q/q` rise.
- **Cloud lanes** can confirm the mechanism (`W` and `q` move) under the runner's screen rules; they never stand as evidence of a gain.
- **Gains** come only from the host A/B and CR-COMB.

**An illustration, not a forecast:**
- **Without hints:** `W` 6 s, `q` 15, `L` 64 and `y` 0.16 give a lookup bound of 102/min and a packet bound of 96/min. That is right at the line.
- **With hints on half the lookups:** say `q` ≈ 2 and `W` ≈ 1.5 s for those, at the same `y`, with the rest as above. Then `q` averages about 8.5, the packet bound is about 170/min and the lookup bound at `L` 64 is about 160/min.

So the path is plausible, not proven. Early termination, hints and the slot sizing must each deliver, and `y` mustn't fall much.

## 6. Who decides what

Nothing here changes an owner number. The gate, the Balanced 200 queries/s, the 1024 outstanding cap, BEP 51 intervals, the sampler's per-IP limits and the hint guard all stay. The slot count, the lookup end condition and the timeouts are code constants. Changing them is the runner's call under `docs/owner-decisions.md` §2.26, recorded in a decisions file and flagged under §2.19. Building alongside the live runs is the runner's ladder call. Checkpoint 1's inputs stay as the owner set them.

## 7. How this review was checked

I read the plan once against the data. Then I challenged my own first draft, and corrected these:

- **The packet bound.** My first draft said shorter lookups plus more slots stay inside 200/s. At `q` ≈ 50 they don't, which is why cutting `q` comes before raising `L`.
- **The hint guard.** My first draft proposed widening it. The build plan's run rules keep per-IP limits fixed, and one hash per responder needs no change.
- **The live order.** I first proposed putting the lookup work ahead of CR-AB-4. Checkpoint 1's inputs are the owner's choice, so the live order stays and only the build order changes.
- **What isn't proven.** `W` is derived, not read; `q` is an upper-bound estimate; neither hint usefulness nor `y` under early termination has been measured live.

With those corrections, I agree with every recommendation above as the most likely path. The performance claims remain hypotheses, and only the host runs can settle them.
