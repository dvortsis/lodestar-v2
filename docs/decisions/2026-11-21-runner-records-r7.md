# runner-records-r7 decisions (#PR)

The runner's records PR after r6 (#1012). It records the runner's calls from later on 2026-11-21. Each call is flagged to the owner, who can veto it (§2.19). The slices' own decisions files hold the detail.

## Calls in merged slices
- **X-ui-guards (#1013):**
  - **Size reshuffle.** "Search results (lazy)" rose to 40.76 kB (+153 B) for the weighted icons. In the same change, "Search cards" fell to 7.97 kB (−338 B) and "Search live banner" to 2.58 kB (−362 B), both set to their measured size rounded up to 0.01 kB, so the freed bytes can't be quietly spent.
  - **The axe scan of the fixture screens stays in the nightly fixture job,** not in a required PR job. It waits for each screen to settle: no `aria-busy`, the step's own ready control present, and no error banner.
  - **One on-state exception to the bold-icon rule:** the facet exclude toggle. Its filled icon is X-facet-exclude-fill (below).
- **CR-1c (#1014):**
  - **Slot count and running lookups follow different presets.** With `dht.preset_slots` on, the lookup slots are sized from the preset chosen at start. A later change needs a restart. Running lookups follow the live preset at once: they never exceed the smaller of the allocated slots and the live preset's count. Without this, sampling stalled after a live change down to Low (the review's finding).
  - The switch is off by default. CR-1c stays out of CR-AB-LC until the owner answers owner queue item 32 (r6).
- **X-prefs-invisible-share (#1015): the broader repair is kept.** A saved shortcut map that fails its check, for any reason, keeps its valid remaps. Only the remaps whose key is refused or malformed are dropped, along with any remap that collides with a default in effect. Before, the whole field was ignored. Keeping the rest is better for users, and the result always passes the check.
- **X-fleet-switch-passthrough (#1016):** all 11 crawler switches pass through the fleet VM stack, from one list, with tests. Mapping the treatment keys (`lc`, `hint`, `split`) is X-fleet-stacked-arms (below).
- **X-testvm-teardown-by-id (#1017):**
  - Each lane reads its root volume id once the instance is online and keeps it in the session only. After `terminated`, it checks that volume by id.
  - The test user can't delete a surviving volume (no `ec2:DeleteVolume`), so the owner deletes it. Owner queue item 33 asks whether to grant that, scoped like the existing terminate permission.

## Calls in open slices
- **X-rusqlite-041 (#1018):** the bump forces two generated files, `web/licenses/rust.json` and `fuzz/Cargo.lock`, so both join the slice's owns. Capping `SQLITE_LIMIT_SCHEMA` in the importer is split out as X-importer-schema-limit, so the bump stays minimal.
- **X-safety-toggles (#1019):**
  - **The danger style** marks any change that loosens child safety: a CS rule turned off or weakened, or a CS-1 floor entry removed. Everything else, strengthening included, keeps the ordinary style. What each rule, switch and flow does is unchanged (§2.39).
  - **Labels:** the alert switch reads "Active" (zh 启用中) and is on when the alert isn't paused. The source switch reads "Enabled" (已启用). The requests they send are unchanged.
  - **Sizes,** each the measured size rounded up to 0.01 kB, after the rule texts moved to their own on-demand chunk:
    - Settings FiltersScreen first view: 31.14 → 31.41 kB;
    - a new "Filters rule names (on demand)": 4.0 kB. It replaces "Filters pack descriptions (on demand)" (1.6 kB), because each pack row now says what it hides;
    - Detail drawer: 28.14 → 28.15 kB, for 2 B of hash and gzip drift;
    - Pack panels lowered from 9 to 7.88 kB, and Packs list from 1.85 to 1.56 kB.
  - **The screen** waits at most 1.5 s for the rule names, then shows the codes.
- **X-api-openapi-repeated (in progress):** `/export` reuses #1010's check, so `ast` or `hashes` in a GET query gets its named error. `/search` and `/facets` gain 400 descriptions, each listing only codes their code paths return. The other query routes' 400 descriptions are split out as X-api-openapi-400s.

## Cloud screens (runner, within the set limits, §2.72)
The next screens are coordinated admission, route backoff, uTP and lookup end. They run two lanes at a time, one switch per `c6a.xlarge`, each up to 16 h, under the $100 AWS cap.
- They are works-as-designed screens, never CR-AB-1 evidence. The Unraid A/B stays on hold at the owner's word.
- Each lane confirms its switch is on and that its metrics move as its decisions file says, and runs the standard checks (rate and outstanding within the preset, no panics, memory flat after warm-up, a restart drill with data intact). Each lane is torn down by id.
- **uTP:** a missed first-window deadline fails only that peer (CR-UTP-FIRST-WINDOW). Its count is reported, but it doesn't fail the screen.

## Queued
- **X-importer-schema-limit,** from #1018's review.
- **X-api-openapi-400s,** split out of X-api-openapi-repeated.
- **X-testvm-delete-on-termination,** from #1017: launches set `DeleteOnTermination` on the root volume explicitly, rather than leaving it to the image.
- **X-facet-exclude-fill, X-grid-two-col-large and X-snapshot-stale-outputs,** from #1013:
  - the exclude toggle's filled icon;
  - the two-column layouts the breakpoint ban removed, restored at the §10.2 `large:` breakpoint;
  - Settings snapshots at 2560 px that passed without the Outputs nav item.
- **X-fleet-stacked-arms,** from #1016: a `WINNERS` line runs earlier winners in both arms of an A/B block.
- **For the owner:** owner queue item 33 (`ec2:DeleteVolume` for test volumes).
