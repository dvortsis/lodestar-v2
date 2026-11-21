# runner-records-r8 decisions (#PR)

The runner's records PR after r7 (#1020). It records the runner's calls from the afternoon of 2026-11-21. Each call is flagged to the owner, who can veto it (§2.19). The slices' own decisions files hold the detail.

## Calls in merged slices
- **X-labels (#1023):**
  - **Release groups pass through as the index holds them.** The index stores tag terms lower-cased, so the facet rail shows "rarbg", not "RARBG". No client formatter can recover the case. Fixing it is X-facet-group-case (below). Compare already gets the parser's case and shows it.
  - **Per-field label modules** (`format/label-*.ts`), not one barrel, so each screen loads only the names it shows.
  - **Two lazy passes were measured and reverted.**
    - *Builder:* loading the value names on demand grew the Builder to 8,722 B and five other entries by 20–100 B, because the bundler regrouped the chunks. Search loads the same module before anyone opens the Builder, so the lazy load saved no user anything.
    - *Insights stage pages:* also measured and reverted.
  - **Size limits,** each the measured size rounded up to 0.01 kB:

    | Entry | Limit |
    |---|---|
    | Builder | 8.43 kB |
    | Facet rail | 18.83 kB |
    | Insights stage pages | 40.85 kB |
    | Detail sections | 38.82 kB |
    | Detail drawer | 28.31 kB |
    | `/t/[hash]` | 22.13 kB |
    | Search results | 40.83 kB |
    | Search cards | 8.01 kB |

    The final round's extra bytes are behaviour the review asked for: the facet "Find" box matches the code as well as the label, and flags read "Yes"/"No" in the Builder as they do in the rail.
  - **Wording:** type names use the glossary's singular names everywhere, and a share reads "82% video", in lower case.
  - **The Discover CSV keeps plugin ids.** It's the server's export, and its `key` column is the plugin id. Names there are X-insights-source-names (below).
- **X-api-openapi-repeated (#1021):**
  - **The 400 lists carry only the codes each route and method can reach.** `logs.rs`, `logs/stream.rs`, `search.rs` and `ops.rs` joined the slice's owns.
  - **`route_layer` replaces `layer`** on `/export`, `/search` and `/facets`. The review expected a 400 in place of a 405. The slice showed that the auth layer's own method fallback already gave 405, so the change is defensive, and a test pins the 405.
  - The other `Query` routes' 400 lists are X-api-openapi-400s (r7).
- **X-sampler-frontier-starve (#1022):**
  - **The cause:** one failed sample cools a live node for 6 h. It lands as option 1: the written cause plus the ignored `sampler_starve` simulation test, with no crawler change.
  - **The fix** is CR-SAMPLE-BACKOFF-ESCALATE (below), behind its own switch, with X-sample-cooling-metric to show it.
- **X-importer-schema-limit (#1024):**
  - **The cap is 256 schema objects,** about 14 times magneticod's largest schema.
  - **It bounds parsing for a well-formed schema.** For a schema SQLite finds corrupt, the existing statement deadline bounds it, as before the cap.
  - **The runner kept the fix to wording,** with no pre-scan and no test file for the corrupt case: nothing is worse than before, and the slice's decisions file says what each bound covers.
  - **A file over the cap gets its own message,** saying it isn't a database Lodestar can import, because the generic advice to import a copy wouldn't help.

## Calls in running slices
- **X-safety-undo-danger, rescoped.** The slice found that Undo's server preview was always all zeros, marked as an estimate, so no preview field could tell a loosening apart. The runner chose to fix the server in the same slice:
  - **Undo's preview is computed** by comparing the kept previous rule set with the current one. If the comparison can't be made, it falls back to today's estimate, and the Undo still works.
  - **`Assessment.loosens_child_safety`, a new boolean:**
    - It's true when a change turns off or weakens a CS rule, removes a floor entry, or releases a CS hold or marks it a false positive. That's #1019's danger rule.
    - Undo's confirm takes the danger style from it, so an Undo that turns off a CS rule that caught nothing in 30 days still shows danger, like that rule's own "Turn off".
    - The flow, the warning text, the password step and what Undo does are unchanged (§2.39).
  - **The new field changes `openapi.json` and `schema.d.ts`.** The slice regenerates them only after #1021 merged, so two running slices never owned those files at once.
  - **Size limits,** each the measured size rounded up to 0.01 kB:

    | Entry | Limit | Why |
    |---|---|---|
    | Filters dialogs | 35.97 kB | +67 B, the derivation |
    | Settings InsightsScreen | 45.31 kB | hash and gzip drift |
    | Settings NotificationsScreen | 45.52 kB | hash and gzip drift |
    | Filters pack panels | 7.92 kB | hash and gzip drift |
- **CR-SAMPLE-BACKOFF-ESCALATE:**
  - **The switch has a fixed name:** config key `dht.sample_backoff_escalate`, environment variable `LODESTAR_DHT_SAMPLE_BACKOFF_ESCALATE`, off by default.
  - **With it on,** a node's first failed sample in a row gets a short cooldown, between the node's own sample interval and 1 h, chosen from the simulation. A second failure in a row gets the existing 6 h.
  - **It is the one deliberate exception** to X-sampler-cooldown-split's rule that a cooldown is never shortened. It's a performance change (§2.26), and turning it on by default goes through the A/B process.
  - **Unchanged:** the rate and outstanding caps.
  - **Re-checks it carries:** the restart proof (CR-0d), the simulation's restart lower-bound model, and the failure streak, which a restart resets.
- **X-page-frame:** most page headings and outer padding live inside the feature screens, not the route files, so the slice's owns widen to the header and outer-wrapper lines of those screens. Nothing inside a screen changes. `features/filters/**` stays out while X-safety-undo-danger runs there; if the Filters page still doesn't line up, that's a follow-up.

## New slices
- **X-facet-group-case:** release-group facet values show their display case. The facet still matches on the lower-cased term. The display form comes from data the index already holds; if that needs a schema version bump, it rides the next one.
- **X-insights-source-names:**
  - The Insights by-source rows carry the plugin's display name from the server, as the provenance rows already do.
  - The client drops its own 287 B name table.
  - The insights CSV keeps its `key` column (the plugin id) and adds a `name` column after it, so existing readers of the CSV keep working.
- **X-narrow-states, note only:** Logs already shows local times in the chosen zone since #1023, so that part of I28 is done.
