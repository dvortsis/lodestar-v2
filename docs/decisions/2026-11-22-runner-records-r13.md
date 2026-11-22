# runner-records-r13 decisions (#1074)

The runner's records PR after r12 (#1068). It records the runner's calls on 2026-11-22Z for #1069 to #1072, the four Track U slices running now, and the slices they leave. Each call is flagged to the owner, who can veto it (§2.19). The slices' own decisions files hold the detail.

## Calls in merged slices
- **X-size-limit-one-build (#1069):**
  - A plain `npx size-limit` now stops with an error naming `npm run size:ci`, unless `build/_app/version.json` says `0000000`, the measurement build's version.
  - **The config doesn't rebuild.** A size check must not replace `build/`, which other steps may be using.
  - This PR updates the docs that still named `npx size-limit` as the check: `docs/handoff.md`, `docs/architecture.md` §17, `infra/dev-vm/linux-lane.md`, `runner/standards.md`, and the search and shell READMEs.
  - Older records, briefs and review files keep their wording.
  - **`CLAUDE.md` is left for the owner.** Its "Before pushing" list still names `npx size-limit`, which now stops with an error. The runner proposes `npm run size:ci` in its place and doesn't edit the file.
- **X-vm-status-mode (#1070):**
  - vm-status reads the mode with `load_block_env`, the same reader arm-switch and copy-off use, in a subshell. It emits one `block_mode_<abba|on|unknown>` count.
  - **A correction to `2026-11-22-X-vm-status-mode.md`:** a NUL byte doesn't make a record read `unknown`. Bash's `read` drops it, so `MO\0DE=on` reads as `MODE=on`, and both the reader and status say `on`. #1070's test pins that case as `on`. The file's other `unknown` cases stand.
- **X-pool-past-cap-room (#1071), option (d), a runner call under §2.26:**
  - **The cause is order, not room.** A claim held back by its network's share left the partial queue and rejoined at the back, up to eight times in one seed.
  - **The fix:** it keeps its place, and the head is the oldest claim whose network's share has room for it. No bound moves.
  - **Results over 640 seeds:**
    - at the 1 MB test budget, no claim waits past the bound, and the worst wait is 19.94 s;
    - at the worker budget, the worst is 6.68 s;
    - the 64-seed run asserts that nothing waits past the bound.
  - It also fixes a lost wake-up in `settle` that was there before this slice.
  - **Declined:**
    - (a) and (b): they cut coverage, to the worker's metadata limits, and (b) also lowers the main node's `MAX_METADATA_MIB` from 47 to 36;
    - (a'): it loosens the floor against slow peers, and only the owner may loosen such a bound.
- **X-fuzz-advisories (#1072):**
  - `fuzz/Cargo.lock` moves ctutils from 0.4.2 to 0.4.8 (RUSTSEC-2026-0385).
  - CI's advisories job checks `fuzz/Cargo.lock` too.
  - This PR updates `deny.toml`'s header to say so.
  - **Two nits wait for the next edit of `ci_changes.py`:** re-wrap the docstring at line 35, and say "the lockfiles" at lines 139–140.

## Calls in running slices
- **One owner per file.** The owner's rule is that running slices' owns never overlap. Owner question n 30 asks for a narrow exception (additive lines in shared crawler files) and is still open; it wouldn't cover this file anyway. So `web/.size-limit.mjs` has one owner among the running slices: X-search-toolbar.
  - X-insights-story's new overview entry moves to X-insights-story-stages. The overview has no size entry on main today, so PR 1 lands unbudgeted, with its measured size in the PR, and that size is PR 2's baseline.
  - X-settings-find sends a measured size for its on-demand chunk, if no entry covers it, and the runner adds that entry after X-search-toolbar lands.
- **X-search-toolbar:**
  - **One view switcher (List, Compact, Table, Cards)** replaces the density control and the view toggle. This refines §10.2 and §10.4, and old `density=` and `view=` URLs map to it. §10.10's per-account Appearance density preference stays, and sets the switcher's default.
  - **The index-growing banner stays undismissible,** because it says results are incomplete. Under 640 px it's one compact line.
  - **Size calls:**
    - **No raises.** Search cards stays at 8.01 kB: the raw name on cards measured 8.11 kB, so the slice finds the 97 B inside the chunk, or drops the raw name on cards only and keeps it in the other views;
    - the Results toolbar also stays at 4.6 kB (it measured 5.13 kB). The slice built a lighter version: a native `<select>` for View on a phone, and a native popover for Select and Save, with no menu library.
    - The rules it moves into the lazy `toolbar.css` need a layout-shift check.
  - **Owns gain** `results/results.css` (two rules only), `web/.size-limit.mjs` (the Search cards and Results toolbar entries only, with no raise) and its decisions file.
  - I15's Resolution column moves to X-hit-resolution (below). The slice keeps the Raw name column.
- **X-insights-story, split into two PRs:**
  - PR 1, the overview: U25, I25, I9, P14, P16, U26, P10's cohort copy and 03-n.
  - PR 2, X-insights-story-stages: 03-q, on its own branch. The Insights stage pages entry has about 10 B of headroom.
  - **The health sentence never claims health without data.**
  - The strip and the KPI row stay separate (§10.9.1, §10.9.2), and the layout follows §10.9.6.
  - **U26 (Undo on intensity)** is an inline `role=status` line, because the toast queue has no action slot.
  - **The DOM follows the wide-screen order,** so keyboard order matches what wide screens show (WCAG 2.4.3). The grid reorders narrow screens.
  - **Owns gain** the Insights messages, its two e2e specs and its decisions file. The stage pages move to the new slice.
- **X-units-typed** (/security-review before "ready"):
  - **What's stored is canonical.** Anything persisted is `to_query()` output, with bytes or IEC units only. `to_query_typed()` output is never stored: it serves the query bar, the URL, `/parse`'s and natural phrasing's `canonical`, and display.
  - **A saved search's `q` in its view is `to_query_typed(stored AST)`.** So an old saved search whose raw text says `GB`, stored as a GiB AST, opens in search as the same set its alert matches.
  - **Did-you-mean stays on the stored reading.** It may drift by 7% on a size clause, with no visible effect.
  - The chip echo mirrors the typed printer on the exact byte value, so `1500MB` echoes as "1.5 GB".
  - **Printer tweaks** (bytes for exclusive bounds on multiples of 1,000, and bytes when they need fewer digits than IEC) are approved if they keep the set and nothing keys storage on the text. Golden files may change where the meaning doesn't.
  - **Owns gain** `crates/api/src/saved.rs`, `crates/search/src/lib.rs` (a re-export), `crates/search/tests/query_props.rs`, `fuzz/fuzz_targets/query.rs` and its decisions file.
- **X-settings-find:**
  - **A secret field's value is never shown in results or held in the index.** Results show the same masked state the form shows.
  - **I18:** the slice hides only what it saw do nothing on a real install, or what the registry marks inert.
  - **The index is built at runtime,** in a lazy chunk, on first focus of the box or first palette open. Matching is by word and prefix for Latin text, and by substring for Chinese.
  - **`/`** goes through the keys registry, scoped to the page. One key never does two things on a page.
  - **Navigation badges** come only from data already loaded.
  - **No direct-change palette commands.**
  - **Owns gain:**
    - `security/SecurityScreen.svelte` (P17 only) and the per-screen icon files (P3 only);
    - the Settings messages (`settings/messages/en.json` and `zh.json`), where its new `settings_find_*` keys go. `web/messages/` is generated, so nothing is committed there;
    - two e2e specs and its decisions file.

## Cloud testing
- **The escalation lane** (escalation on, cooldown split off) stalled about 36 minutes after suppression ended.
  - The cooling table reached its 200,000 cap, and the frontier stayed pinned.
  - Escalation re-asks after 300 s, but the record keeps its slot for 6 h. So escalation alone doesn't prevent the stall that `2026-11-20-X-sampler-cooldown-split.md` describes.
  - **Runner calls:**
    - verdict b is "no";
    - verdict a is read from the first 36 minutes only, and labelled so;
    - escalation is screened only together with the cooldown split from now on.
- **The route lane hasn't exercised IPv6:** there were no IPv6 fetch attempts.
- **Capacity rejections of about 4.5 M an hour** on three lanes are a lead only, and nothing acts on them yet.
- **No new sampler-counter slice is needed.** `dht_sample_failures_total` and `dht_sample_cooling_reason` were added by X-sample-cooling-metric, after the lanes' build.

## New slices
- **X-insights-story-stages:** X-insights-story's 03-q, the stage pages reconciled with the overview. The same session builds it after PR 1 merges. It also adds the Insights overview size entry, may edit the `[stage]` route, and never runs beside another slice that owns `web/.size-limit.mjs`.
- **X-hit-resolution (core):** search results carry the release's resolution (null when none), and the table view gets I15's Resolution column. `SearchHit` has no such field today. It runs after X-units-typed and X-search-toolbar, which own the files it touches.
