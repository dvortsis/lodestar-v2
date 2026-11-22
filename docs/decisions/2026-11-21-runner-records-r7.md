# runner-records-r7 decisions (#1020)

The runner's records PR after r6 (#1012). It records the runner's calls from later on 2026-11-21. Each call is flagged to the owner, who can veto it (§2.19). The slices' own decisions files hold the detail.

## Calls in merged slices

- **X-ui-guards (#1013):**
  - **Size reshuffle.** The weighted icons add 153 B to "Search results (lazy)", so its limit rose from 40.62 to 40.76 kB. In the same change, "Search cards" shrank by 338 B and "Search live banner" by 362 B, and their limits were lowered to 7.97 kB and 2.58 kB, each the measured size rounded up to 0.01 kB, so the freed bytes can't be quietly spent.
  - **The axe scan of the fixture screens stays in the nightly fixture job,** not in a required PR job. It waits for each screen to settle: no `aria-busy`, the step's own ready control present, and no error banner.
  - **One on-state exception to the bold-icon rule:** the facet exclude toggle. Its filled icon is X-facet-exclude-fill (below).
- **CR-1c (#1014):**
  - **Slot count and running lookups follow different presets.** With `dht.preset_slots` on, the lookup slots are sized from the preset chosen at start. A later change of slot count needs a restart. After a live change, no new lookup starts above the smaller of the allocated slots and the live preset's count, and the lookups already running settle there within their 25 s deadline. The live preset's rate and outstanding cap hold at once, so the network bound never waits for them. Without this, sampling stalled after a live change down to Low (the review's finding).
  - The switch is off by default. CR-1c stays out of CR-AB-LC until the owner answers owner queue item 32 (r6).
- **X-prefs-invisible-share (#1015): the broader repair is kept.** The repair runs on any saved shortcut map that fails its check. It drops the remaps whose key is refused or malformed, and any remap that collides with a default in effect, and keeps the rest. Before, the whole field was ignored. A map the repair can't fix (an unknown action id, more than 64 remaps, two remaps sharing a key) is still ignored whole, as before. Keeping the valid remaps is better for users, and a repaired map always passes the check.
- **X-fleet-switch-passthrough (#1016):** all 11 crawler switches pass through the fleet VM stack, with tests that tie both the compose file's list and the unset list to the collector's `SWITCH_ENV`, so a new switch can't be missed in either. Mapping the treatment keys (`lc`, `hint`, `split`) is X-fleet-stacked-arms (below).
- **X-testvm-teardown-by-id (#1017):**
  - Each lane reads its root volume id once the instance is online and keeps it in the session only. After `terminated`, it checks that volume by id.
  - The test user can't delete a surviving volume (no `ec2:DeleteVolume`), so the owner deletes it. Owner queue item 33 asks whether to grant that, scoped to the same tags as the existing terminate permission.

## Calls in open slices

- **X-rusqlite-041 (#1018):** the bump forces two generated files, `web/licenses/rust.json` and `fuzz/Cargo.lock`, so both join the slice's owns. Capping `SQLITE_LIMIT_SCHEMA` in the importer is split out as X-importer-schema-limit, so the bump stays minimal.
- **X-safety-toggles (#1019):**
  - **The danger style** marks any change that loosens child safety: a CS rule turned off or weakened, a floor entry removed, or a hold by a CS rule released or marked a false positive in the review queue. Everything else, strengthening included, keeps the ordinary style. What each rule, switch and flow does is unchanged (§2.39).
  - **Labels:** the alert switch reads "Active" (zh 启用中) and is on when the alert isn't paused. The source switch reads "Enabled" (已启用). The requests they send are unchanged.
  - **Undo keeps the ordinary style** for now: the screen learns what an Undo restores only from its preview. Styling it from the preview is X-safety-undo-danger (below).
  - **Several solid danger buttons on one list.** With several CS rules on, each row's "Turn off" is a solid danger button, because the button component has no outline danger variant. The runner accepts that (§2.26): the list is the one place the user loosens child safety, and the style says so.
  - **The "What each pack hides" door is removed.** Each pack row now says what it hides, including the references the door showed, so nothing is lost and the screen is shorter.
  - **Sizes,** each the measured size rounded up to 0.01 kB, after the rule texts moved to their own on-demand chunk (the review round's final numbers):
    - Settings FiltersScreen first view: 31.14 → 31.42 kB;
    - a new "Filters rule names (on demand)": 4.02 kB. It replaces "Filters pack descriptions (on demand)" (1.6 kB), because the door is gone;
    - Detail drawer: 28.14 → 28.15 kB, for 2 B of hash and gzip drift;
    - Pack panels lowered from 9 to 7.91 kB, and Packs list from 1.85 to 1.56 kB.
  - **The screen** waits at most 1.5 s for the rule names, then shows the codes.
- **X-api-openapi-repeated (in progress):** `/export` reuses #1010's check, so `ast` or `hashes` in a GET query gets its named error. `/search` and `/facets` gain 400 descriptions, each listing only codes their code paths return. The other query routes' 400 descriptions are split out as X-api-openapi-400s.

## Cloud screens (runner, within the set limits, §2.72)

The next screens are coordinated admission, route backoff, uTP and lookup end. They run two lanes at a time, one switch per `c6a.xlarge`, each up to 16 h, under the $100 AWS cap. Two, not the runbook's four, so the coordinator can watch each lane closely and a Linux lane stays free for a slice that needs one meanwhile (§2.74 asks for lanes as far as resources allow).

- They are works-as-designed screens, never CR-AB-1 evidence. The Unraid A/B stays on hold at the owner's word.
- Each lane confirms its switch is on and that its metrics move as its decisions file says, and runs the standard checks (rate and outstanding within the preset, no panics, memory flat after warm-up, a restart drill with data intact). Each lane is torn down by id.
- **uTP:** a missed first-window deadline fails only that peer (CR-UTP-FIRST-WINDOW). Its count is reported, but it doesn't fail the screen.

## The sampler frontier (X-sampler-frontier-starve, in progress)

- **The cause is outside the sampler.** One failed sample, a timeout or a bad reply, cools a node for 6 h. A node that answers is sampled again every 5 min, so ordinary loss walks the live BEP 51 nodes into 6 h cooldowns within about 2 h, and sampling falls back on fresh contacts, which are mostly dead. The simulator reproduces it, and the cooldown split makes no difference to it.
- **That slice lands the cause and the reproduction** as an ignored sim test, with no crawler change.
- **CR-SAMPLE-BACKOFF-ESCALATE (queued, runner call §2.26):** a short first backoff after one failed sample, and 6 h after a second in a row. It shortens a first cooldown, so it goes behind its own switch, off by default, re-checks CR-0d's restart-proof horizon, and becomes a default only through the A/B pipeline. The preset's rate and outstanding caps are unchanged, so the load on the network stays bounded as now.
- **X-sample-cooling-metric (queued):** export how many nodes are cooling and why, and the failed-sample count.

## Queued

- **X-importer-schema-limit,** from #1018's review.
- **X-api-openapi-400s,** split out of X-api-openapi-repeated.
- **X-testvm-delete-on-termination,** from #1017: launches set `DeleteOnTermination` on the root volume explicitly, rather than leaving it to the image.
- **X-facet-exclude-fill, X-grid-two-col-large and X-snapshot-stale-outputs,** from #1013:
  - the exclude toggle's filled icon;
  - the two-column layouts the breakpoint ban removed, restored at the §10.2 `large:` breakpoint;
  - Settings snapshots at 2560 px that passed without the Outputs nav item.
- **X-safety-undo-danger,** from #1019: Undo takes the danger style when its preview shows it loosens child safety.
- **CR-SAMPLE-BACKOFF-ESCALATE and X-sample-cooling-metric,** above.
- **X-fleet-stacked-arms,** from #1016: a `WINNERS` line runs earlier winners in both arms of an A/B block.
- **For the owner:** owner queue item 33 (`ec2:DeleteVolume` for test volumes).
