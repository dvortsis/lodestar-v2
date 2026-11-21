# runner-records-r6 decisions (#1012)

The runner's records PR after r5. It records the runner's own calls from 2026-11-21. Each call is flagged to the owner, who can veto it (§2.19). The slices' own decisions files hold the detail.

## Dropped: X-settings-leave-guard-split (runner, §2.26)
- **The leave guard stays where it is.** The slice was meant to move the Settings leave guard out of the static graph that the wizard's FieldsStep and UpdatesStep load, and give back about 0.67 kB of "Wizard steps (on demand)".
- **What was measured** on `main` at a57eb43a:
  - "Wizard steps (on demand)" is 54,285 B against its 54,400 B limit.
  - Removing the guard outright saves 437 B. The rest of the 0.67 kB is draft code elsewhere in the form engine, which the wizard needs.
  - A static split, with the guard in its own module and passed to the form as a prop, saves 375 B on the wizard. It adds about 430 B to each of the six Settings first views that mount the form: Section, Logging, Insights, Integrations, Language and Notifications. All six go over their limits, which have 8–342 B of headroom.
  - A lazy split can't save enough. The guard's decision has to stay synchronous in the form, and only the confirm step could load on demand, which is under 100 B.
- **Why:** a split slows the screens users wait on most, in order to save bytes in a chunk that loads only when the wizard opens. The wizard keeps its 54.4 kB limit. No code changed.

## Calls in this round's slices
- **X-filter-budget-remeasure (#1009):** a case over the §20.8 budget is measured once more at the end, with a fresh calibration either side, and scaled by the faster one. The owner's 50 µs, the cases, the calibration and CI are unchanged, so the gate is no less strict. It now tells host noise from a real regression.
- **X-flake-utp-lossy-win (#1008):** the first-window deadline became a `FetchOptions` field so the lossy-link test can wait 20 s. Production keeps 4 s. Whether 4 s is right on lossy uTP links is queued as CR-UTP-FIRST-WINDOW (below).
- **X-api-repeated-params (#1010, on merge):** `/facets` refuses a repeated known parameter as `repeated_parameter`, like every other JSON endpoint.
- **CR-COL-6 (#1006):** the analyzer's `split_not_exercised_b_segments` check is confirmed as written. CR-AB-1 now depends on CR-COL-6.
- **X-filter-dicp-doc (#1011, on merge):** the filter's invisible-character table gains U+180B–180D and U+180F, so it equals Unicode's Default_Ignorable set. They don't join `newly_ignored`, because the filter already dropped them as combining marks before reading the table. No text reads differently. The slice's own decisions file shows this.
- **CR-1c (in progress):**
  - **Not kept on the screen's rule.** On 8 seeds with CR-LOOKUP-END on in both arms (Balanced), stored per minute rose 18.64 → 39.19 and stored per DHT query 0.0038 → 0.0048, but stored per slot-second fell 0.0101 → 0.0085, each on 8 of 8 seeds. The rule (build-plan §0.5) keeps a change only if both rise. The runner won't rewrite its own rule after seeing the data. CR-1c merges with its switch off and stays out of CR-AB-LC until the owner answers owner queue item 32, whether a change whose purpose is more slots should be judged per slot-second.
  - Lookup slots follow the preset chosen at start, and a new count needs a restart. The live rate and outstanding cap still follow live intensity changes and still bound the network.
  - The counts are Low 32, Balanced 80 and Aggressive 256, worked out from CR-LOOKUP-END's screen and clamped between today's 32 and the pool's 256. The switch is off by default.
  - The WK-4 duplicate-slot fix ships outside the switch.

## Cloud screens (runner, within the set limits; the owner asked to stay out of this loop, §2.72)
- **The sampler-switch screen passed as designed.** It ran for 15.7 h on one `c6a.xlarge` with the Balanced preset. It passed the a–e checks and a restart drill, cost about $2.70, and teardown was verified. It is not CR-AB-1 evidence: the Unraid A/B stays on hold at the owner's word.
- **Next:** screens of coordinated admission, uTP and route backoff, one lane at a time, each up to 16 h, under the $100 AWS cap. They use only switches the deployed stack already passes through. Adding the other four to the repository's fleet stack is X-fleet-switch-passthrough; updating a deployed stack stays the owner's.

## Queued from the screen and the reviews
- **X-fleet-switch-passthrough:** the fleet VM stack passes four crawler switches but not `LODESTAR_DHT_LOOKUP_END`, `_PRESET_SLOTS`, `_HINT_FIRST` or `_SAMPLER_COOLDOWN_SPLIT`. The runbook also gets the restart drill's environment line.
- **X-metrics-prefix-doc:** `docs/metrics.md` doesn't say that every exported series carries the `lodestar_` prefix.
- **X-sampler-frontier-starve:** a lead from the screen. With the split on, the frontier sat at 0 for about 3.3 h with every node cooling down, and samples/s fell to about half before recovering on its own.
- **CR-UTP-FIRST-WINDOW:** #1008's probe found about 1.25–1.8% of honest fetches over a 15%-loss uTP link miss the 4 s first window. A longer deadline would loosen a bound against slow peers, so any raise goes to the owner.
- **X-testvm-teardown-by-id,** from #994's review: teardown should check the lane's own volume by id, not only by tag.
- **For the owner:** the test-VM policy requires the `Project` tag on volumes but not `Purpose`, so a volume launched without `Purpose` would slip past the teardown check. Changing the policy is the owner's: owner queue item 31.
- **X-prefs-invisible-share** and **X-api-openapi-repeated:** small follow-ups from X-win-path-ignorables and #1010. The second waits for X-ui-guards, because it touches `web/`.
