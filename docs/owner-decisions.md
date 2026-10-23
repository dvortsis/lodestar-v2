# Owner decisions: the primary rules

These are the instructions and decisions the owner gave or explicitly agreed to. They are the **primary rules** for the whole build, as the owner set on 2026-10-22: everything below them (the approved plan in `docs/architecture.md`, the contract docs, then `ui-design-skills` for gaps) is read under them.

Sources:
- **§1b** comes from the M0–M4 build sessions.
- **§1** comes from the "Modern DHT crawler architecture" design session (2026-10-20 → 2026-10-22). It was extracted from the owner's own messages and answers, with timestamps (UTC). It doesn't include assistant proposals the owner didn't accept. "Accepted proposal" means the owner approved it explicitly ("Yes", "go", "I'm good with that", "Your recommendations on everything", plan approval). The details are in the named section of `docs/architecture.md`.
- **§2** holds later decisions and resolves the open points.

When something here and a lower document disagree, this file wins. Raise the conflict and fix the lower document in the same PR.

## 1. Decisions from the design session (2026-10-20 to 2026-10-22)

Each rule cites the owner message or messages it comes from. Where a decision was later changed, only the final form is given, with what it superseded. "Accepted proposal" means the owner explicitly approved an assistant proposal ("Yes", "go", "I'm good with that", "Your recommendations on everything", plan approval). The details of such a proposal are in the named section of `docs/architecture.md`.

### 1.1 Product scope and goals

1. **Build a modern DHT crawler and torrent-metadata search engine from scratch.** It replaces the owner's bitmagnet + Lodestar stack (`dvortsis/lodestar`), whose "performance and search functionality was sub-optimal". It should have "all the bells and whistles". *(17:12:42)*
2. **Search should be "lightning-fast advanced search"** that lets the owner "pinpoint exactly what I'm looking for, when dealing with ambiguously named torrents, by file type, date, time, number of trackers, etc." *(17:12:42)*
3. **"Grab as much metadata from the torrent as possible."** *(17:12:42)*
4. **Priorities, in the owner's order: "performance, database size, and first-class UX".** *(17:12:42)*
5. **The architecture plan, after two owner-requested review passes, was approved as the baseline.** The owner first asked to "call out any areas of improvement, both UX and performance, blind spots, misses, and areas of concern" and then said "Review your work one last time". The approved plan is now `docs/architecture.md`. *(17:25:42, 17:30:47, approved 17:32:36)*
6. **Lodestar is the central directory for all sources.** *(23:34:22, accepted 23:36:40)*
7. **Insights dashboard: a reporting dashboard "telling the complete story of the process".** This is the accepted Insights proposal *(21:01:47, "Yes, let's do it." 21:03:34; PR #1 merged)*:
   - "Crawler" becomes "Insights".
   - The story runs Discover → Fetch → Understand → Index → Serve.
   - It uses cohort-based funnels.
   - Recent cohorts are labelled "incomplete".
   - It reads only pre-aggregated rollups.
8. **Remote clients pull resources from multiple machines to increase crawler and processing threads.** This is the accepted remote-workers design in §19: one coordinator holds the database, index and UI, and workers run `lodestar worker --join`. *(02:49:08 on 10-21; PR #18 merged)*
9. **Adult is a media category alongside movies, TV shows and the rest.** *(01:39:40 on 10-22; implemented in PR #28, merged)*

### 1.2 Stack and language

1. **Rust single binary:** crawler, SQLite storage, Tantivy search, API and embedded web UI. *(Q1, 17:16:06)*
2. **The frontend is Svelte 5, prioritising the smallest and fastest UI.** *(Q2, 17:37:04; reconfirmed in [5] at 17:56:22)*
   - This superseded the assistant's twice-made recommendation of React 19 + Vite / Tremor.
3. **External metadata enrichment comes as opt-in plugins.** The system is offline by default. *(Q1, 17:16:06)*
4. **bitmagnet import is a one-time importer, as an M2 add-on.** *(Q2, 17:37:04)*
   - This superseded "Start fresh" from Q1 at 17:16:06.

### 1.3 Repo, process, branching and PRs

1. **A new repo, `github.com/dvortsis/lodestar-v2`.** It is **public**, with default branch `main`. The owner created it ("Claude can't create repos on this account"). *(Q1 17:16:06, [13] 19:58:53, [14] 20:01:00)*
   - The assistant's PowerShell had proposed a private repo. The owner created a public one, and did not change it after the assistant warned that the plan and brand pack would be public.
2. **Branching:** one feature branch per milestone or coherent slice, merged to `main` by pull request. PRs stay at roughly ≤ 1,500 changed lines. Docs-only trivial changes may go straight to `main`, as `CLAUDE.md` puts it. *(accepted via "Your recommendations on everything.", 20:05:30)*
3. **Quality gates:** fmt, clippy and tests pass before every push. Every parser gets fuzz and property tests. Benchmarks gate the size and speed budgets once they exist. *(20:05:30)*
4. **PR monitoring:** once a PR is open, Claude watches it, fixes CI failures and answers review comments. *(20:05:30)*
5. **Fresh sessions rather than compaction.** The plan and `CLAUDE.md` carry the decisions. *(20:05:30, answering [17] at 20:04:49)*
6. **Finish the remaining work.** At 03:46:25 the owner issued `/goal  finish whatever is left to do`. The assistant took this as authority to merge #28 and #29 and to carry on with the M4 filter slices.
7. **Workflow for design changes:** design additions went in as docs PRs that the owner approved. Examples:
   - Insights → #1
   - sources → #9 ("I'm good with that. Open the PR", 23:36:40)
   - content filtering → #27 ("go", 02:45:15)

### 1.4 Models and delegation

The owner asked "Which model/ effort level is best for this build?" ([15], 20:03:25) and "Can you automatically select the appropriate model for the job?" ([16], 20:03:58). He then accepted the recommendations with "Your recommendations on everything." ([18], 20:05:30):

1. **This main session runs Opus 5.5 at high effort for core work.** That covers M0–M2: torrent-data parsing, storage, the DHT crawler, the peer protocol, search and ranking. It also does all reviews.
2. **Sonnet 5.5 helpers** do:
   - UI components and screens (M5);
   - theme JSON and the theme compiler;
   - packaging (M6);
   - CI workflows and docs;
   - test scaffolding that follows an existing pattern.
3. **Haiku 5.5 helpers** do mechanical jobs only: generating the theme files from the spec, renames and formatting.
4. **Never delegated:** protocol and untrusted-input parsers, storage formats and concurrency.
5. **Every helper's output is reviewed and tested in the main session before merge.** Weak output is redone on Opus.
6. **Two helpers that edit the same files never run at the same time.**

### 1.5 UI: design authority, framework, style, fonts, palette, themes, brand

1. **Design authority:** "Use ui-design-skills as the source of truth for anything it applies to. Reassess any pertinent parts of the plan. Plan out the UI if not already done so in detail." *(17:38:41)* The repo is `https://github.com/dvortsis/skills/tree/main/ui-design-skills`. *(17:50:09)*
2. **Framework vs skills:** "Keeping Svelte 5 and prioritizing Smallest and fastest UI. Keep skills' concepts and spirit." *(17:56:22)*
   - The skills apply by concept and spirit where they assume React/Tremor.
   - The owner's explicit choices override the skill defaults.
3. **Style:** "I Confirm adopting minimalist-visual-style". *(19:33:30)*
4. **Fonts:** "Atkinson Hyperlegible Next + Atkinson Hyperlegible Mono as the system fonts". *(19:49:36)*
5. **Palette:** Riverstone is the "default/ official" palette. *(18:18:00)*

   | Mode | background | surface | text | primary | accent |
   |---|---|---|---|---|---|
   | Light | `#F3F6F5` | `#FFFFFF` | `#243C44` | `#527F91` | `#829F8A` |
   | Dark | `#141F25` | `#22313A` | `#E5EEED` | `#85AFC0` | `#A0C3A7` |

   - **Categorical colours:** `#527F91`, `#829F8A`, `#C4A765`, `#BF8173`, `#8C9BA7`, and a plum in place of seafoam `#92A9A0` (next point).
6. **Plum replaces seafoam:** "replace seafoam #92A9A0 with a muted plum such as #9C8AA5". *(19:33:30)*
   - The value in use is **`#9381B6`**, chosen by the assistant for colour-blind distinguishability.
   - The assistant offered to switch to exactly `#9C8AA5`; the owner later confirmed `#9381B6` (§2.2).
7. **Themes:** "a large collection of themes in both light and dark for the user to select". *(18:18:00)* Confirmed later as 24 families × light/dark, including Rosewood (§2.3).
8. **Brand:** "use the attached brand assets in the build, where applicable" (`Lodestar_Final_Assets.zip`). *(19:33:30)*
   - The assets live in `brand/source/` as plain files, not LFS, because the proxy blocks LFS. The assistant reported this at 20:01 and the owner did not object.

### 1.6 Performance budgets

1. **Performance and database size are the top priorities**, together with first-class UX. *(17:12:42)*
2. **Smallest and fastest UI is the top UI priority.** *(17:56:22)*
3. **The approved plan's budgets apply** (§3 of the 17:32:36 plan; the current figures are in `docs/architecture.md` §3 and §10.1):
   - p95 targets on a synthetic 10M-torrent / 300M-file dataset (4 cores, NVMe), enforced as CI benchmark gates:

     | Operation | p95 |
     |---|---|
     | Typeahead | < 15 ms |
     | Keyword search, top 50 | < 30 ms |
     | Facets | < 100 ms |
     | Filter-only browse | < 50 ms |
     | File-level query | < 150 ms |
     | Regex | < 500 ms |
     | New torrent → searchable | < 5 s |
     | Web UI first paint (LAN) | < 1 s, JS < 200 KB gzipped |

   - Size: about 2.3 KB per torrent all-in by default.
   - Steady-state RSS under 1 GB at 10M torrents.
   - A low-resource mode.
4. **The reporting layer must not hurt performance.** *(question at 20:59:35)* The Insights budgets accepted at 21:03:34:
   - search p95 rises by at most 5% and ingest throughput drops by at most 2% with the dashboard open, enforced by benchmarks;
   - dashboard queries take ≤ 50 ms whatever the index size;
   - rollup tables grow by ≤ 50 MB a year;
   - the Insights page loads lazily.
5. **Remote workers** (accepted design, PR #18):
   - the coordinator ingests ≥ 1,000 new verified torrents per second;
   - a worker uses ≤ 256 MB of RAM and sends about ≤ 3 KB per torrent.
6. **Filter engine:** the planned target is under 50 µs per torrent at 10k rules. This is in PR #27, approved by "go".

### 1.7 Deployment

1. **Lodestar runs in a Docker container, as an Unraid container, or installed on Windows.** *(17:12:42)*
2. **Companions** *(23:34:22; accepted with the assistant's platform adjustment, 23:36:40)*:
   - If a router is needed (for example i2pd), it is installed directly via the app.
   - If a download client is needed, the app downloads it and sets it up automatically.
   - **Accepted adjustment:** fully automatic installs on Windows and bare-metal Linux, from a signed, pinned catalogue, with confirmation per install.
   - **On Docker and Unraid,** installs are guided rather than self-installing: a generated compose service or a pre-filled Unraid template, then automatic detection and configuration.
   - Companion traffic follows the VPN settings.
   - A missing companion only disables the plugins that need it.

### 1.8 Content filtering and safety

All of this is final as of PR #27, approved by "go" at 02:45:15, plus the later answers.

1. **The blocklist is easily editable and as smart as possible**, to prevent false positives while effectively blocking "the specific thing that is intended to block". *(01:39:40)*
2. **The child-safety core is always present,** and its rules are editable so false positives can be corrected. *(01:44:34)*
   - It was refined by the owner's "more control over which specific rules go into layer 1". *(01:53:58)*
3. **Locked floor = "Minimal floor": CS-1a/1b and CS-8a only.** *(Q7, 02:27:43)*
   - These hold only torrents the owner confirmed or deliberately imported, plus their exact reposts.
   - They are always on and can't be switched off, but remain editable, for example to remove a mistaken entry.
4. **Every core rule is on by default;** the user can switch off every one that isn't locked. *(02:37:10)*
   - This superseded the earlier "move out of the core with re-auth + confirmation" design, and "shouldn't be removable" for non-floor rules.
5. **CS-1 is a block against specific torrents the user tagged themselves.** The owner asked this at 02:15:01 and the assistant confirmed. Its sources are:
   - the owner's confirmations in the review queue;
   - "Report as child-abuse material";
   - hash lists the owner deliberately imports.

   No third-party list is built in, and there is no crowdsourcing. There is a manual "report to a hotline" link. *(context 01:57–02:22; approved with PR #27)*
6. **CSX-5 is an editable override list that "overrides all the filters".** *(02:24:40)* "Patterns override everything" *(02:27:43)*.
   - Keywords override "anything in the core rules that isn't part of the locked floor". *(02:37:10)*
   - Final form: CSX-5 entries, patterns and keywords included, override every filter **except the locked floor**.
   - The owner's example is **"jailbait", which is a popular legal adult film series**. *(02:37:10)*
7. **"I want a way to see the impact … before making a change."** *(02:27:43)*
   - A mandatory impact preview comes before any filtering change, in §20.10.
   - The assistant added a per-keyword "Override" (default) vs "Neutralize term" strength, with both shown side by side in the preview. The owner did not object, and it was approved via "go".
8. **No audit log for filtering changes:** "I don't like recording a change in the audit log as we're dealing with torrents." *(02:41:15)*
   - Instead, there is a plain-language warning about the change's effect, modelled on the assistant's "jailbait" warning, and "The user must enter their password to make the changes."
   - This superseded the assistant's earlier audit log, typed confirmation and reason, and "each reveal is logged".
   - Per §20.11: the password is required for every weakening change, every time, with no grace period. Strengthening changes and previews need no password.
9. **"no audit log for exceptions either, but also warning and password."** *(02:43:19)*
   - Exceptions keep no author, reason or history.
   - Tests created from false positives are stored hashed, so no torrent names are kept.
10. **Popular blocklists:** "Allow the user to easily connect to popular blacklists and have the app keep them updated." *(03:07:05)*
11. **Subscribed blocklists never feed the locked floor.** *("I agree with the default.", 03:15:35)*
    - The sole exception is a vetted child-abuse hash list that the owner explicitly marks as part of the floor, with a warning and the password.
    - Once marked, it follows the publisher's removals as well as additions.
12. **Adult is a media category alongside Movies, TV and the rest.** *(01:39:40; PR #27 design approved via "go"; implemented in PR #28)*
    - "Show adult content" is off by default and turned on with an 18+ confirmation.
    - This replaced the "restricted adult tier" of PR #9; adult sources use the normal consent levels.
    - Detection is cautious, so mainstream titles stay out of Adult.

### 1.9 Sources and plugins

1. **The owner wants non-web discovery sources considered**, per the Gemini list. *(22:52:29)*
   - These were adopted into the §18 roadmap, PR #9, accepted at 23:36:40 and merged:
     - tracker announce and PEX;
     - BEP 48 full scrape;
     - Nostr NIP-35;
     - PreDB/IRC;
     - and others.
2. **"I do want the I2P content, as you can find some rare content there."** *(23:05:20)*
   - The owner also asked for other "similarly less publicly available, even risky" sources. *(23:05:20)*
   - He asked about "high-value sketchy sources" *(23:29:49)* and "live connections that can be crawled" *(23:31:23)*.
   - The resulting additions, accepted with "I'm good with that. Open the PR" *(23:36:40)*:
     - network tiers: I2P (v1.1), eD2k/Kad (v1.2), IPFS (v1.3);
     - the `usenet` tier;
     - live sources.
3. **IPFS DHT,** including seeing the files contained, is an off-by-default IPFS tier. *(23:15:34; "Yes." 23:21:50)*
4. **A full roadmap of sources is ordered "by greatest width and breath of rare files/ user base/ ease of incorporating".** *(23:21:50)*
5. **"Each source should be a plugin and can be activated/ deactivated."** *(23:21:50)*
6. **Per-source crawler priority and number of agents** (the owner asked whether this makes sense, 23:21:50). The accepted form, in PR #9:
   - a priority (Low/Normal/High/Max) plus a concurrency share inside one global budget, with per-source politeness caps;
   - "agents" are counted in each plugin's own unit;
   - optional auto-balance.
7. **Safety rules that came with the accepted sources PR #9:**
   - every source is off by default, metadata only, and passes the child-safety filtering;
   - consent levels apply to risky tiers;
   - private results stay local;
   - "no profiles of people": a peer's address is never stored with what it shares.

### 1.10 Workers

1. **Remote clients on multiple machines increase crawler and processing threads.** *(02:49:08 on 10-21)*
   - The accepted design (§19, PR #18, merged) uses a coordinator plus outbound-connecting workers.
   - Workers take on DHT crawling, metadata fetching and CPU-heavy processing.
   - Leases make sure nothing is fetched or counted twice.
   - Workers join with one-time tokens and get per-worker certificates.
   - The UI gets a Settings → Workers page.

### 1.11 Everything else

1. **Tagline / repo description:** "Every node is a new discovery". This came from the assistant's `gh repo create` description, which the owner pasted back at 19:52:51. It is the tagline in `CLAUDE.md`. The owner never stated it himself.
2. **No-model-identifiers rule:** "Never put model identifiers in commits, PRs or code" comes from the harness/system prompt, recorded in `CLAUDE.md`, **not** from an owner message in this session.
3. **No-LFS rule:** "Don't reintroduce LFS" is an environment constraint the assistant reported at 20:01; the owner accepted it by continuing. It was not an owner instruction.

## 1b. Decisions from the build sessions (M0–M4, 2026-10-20 to 2026-10-22)
Extracted the same way from the owner's messages and answers in the "M0 - M2 startup", "M1 results review", "M3 setup" and "M4 continuation" sessions. Times are UTC.

1. **Merging and review.**
   - "Go ahead and review/ merge. You handle the repo maintenance, review, and development work…" *(M0–M2, 10-20 22:28:05)*
   - "yes, don't wait for me to review in the future." *(M3, 10-21 18:21:24)*

   §2.4 below adds the fresh-Opus-reviewer requirement and governs.
2. **Sub-agents:** use "as many sub-agents as add value without decreasing efficiency" *(asked twice)*.
3. **Opus sub-agents may build core slices** in separate worktrees or sessions. Every result is reviewed in the main session before merge, and Sonnet stays on docs, UI and other pattern work. "Yes, Opus forks + my review" *(M4, 10-22 03:17:13)*.
   - This is the owner-approved exception to "never delegate" (§1.4.4): core work may go to **Opus** sub-agents, never to Sonnet or Haiku.
   - The build runner's Opus slice sessions use it.
4. **PR size:** larger PRs are accepted when splitting them would hurt review. M4 was split as "Two PRs: 1–3, then 4–6", where the option text stated "Each PR is over 1,500 lines" *(M4, 10-22 03:17:13)*. The ≲ 1,500-line guideline stands; an exception needs a stated reason.
5. **Hot columns are safe in-memory arrays** (atomics, snapshotted to disk), not mmap: "Safe RAM arrays" *(M4, 10-21 16:43:07)*. This replaces §7's mmap design, because the workspace forbids `unsafe`.
6. **The HTTP client for tracker scrapes and plugins is reqwest + rustls** *(M4, 10-21 16:43:07)*.
7. **The title_stem plurals tier is a fallback only:** "Yes, let's use it for fallback." *(M3, 10-22 03:43:36)* This replaces always-on stemming.
8. **The M0 sign-off:** "Yes, proceed as you advise" *(M0–M2, 10-20 21:14:59)*.
   - The per-torrent budget split: store ≤ 1.0 KB, search index ≤ 0.5 KB, file-tree blobs ≤ 0.6 KB.
   - Each milestone adds the tables it first writes.
   - Fuzz targets arrive with M1.
9. **Milestone order** as run: "go ahead and start m2" *(10-21 01:58:49)*. The M3 order (parser and classify first, then the worker boundary and file index, then enrichment) was accepted with "go" *(10-22 04:44:12)*.

## 2. Later decisions and resolved points (owner, 2026-10-22, build-runner session)
1. **Design authority.** The design decided in the design session is the primary design strategy; `ui-design-skills` fills the gaps it leaves.
   - The session's own UI rule (§1.5.1, "use ui-design-skills as the source of truth for anything it applies to") was carried out by rewriting the §10 UI plan to apply the skills. That plan is therefore primary, and the skills decide only what it and the owner's choices leave open.
   - Precedence: this file → `docs/architecture.md` → contract docs (`design-tokens.md`, `metrics.md`, `ui-glossary.md`) → `ui-design-skills` for gaps (in its own precedence order) → slice briefs.
2. **Plum `#9381B6`** is confirmed as Riverstone's categorical slot 6. This closes the "such as #9C8AA5" question; seafoam stays retired.
3. **Theme collection: 24 families × light and dark (48 themes),** including **Rosewood** as the 24th family. Riverstone is the default and official palette.
4. **Merging.** The build runner may merge PRs with green CI and no blocking findings from a fresh reviewer. It may use GitHub auto-merge as a fallback, and may create as many branches as needed.
5. **The build runs continuously and in parallel** (`docs/build-plan.md`, `runner/`). The owner leaves to the runner the choice of which features run in parallel. The runner watches and coordinates the M4 sessions.
6. **Usage:** the owner has a high-usage plan and wants throughput. Concurrency is limited by quality (dependencies, file ownership, review), not cost.
7. **The locked floor from config files: add by file, remove only in the app.**
   - Config files (`blocklist.files`, a subscription marked `floor = true`) may only **add** to the locked floor.
   - Anything that would remove or weaken a floor entry is refused at start-up and has to be done in the app, with the effect warning and the password (§1.8.8). That covers deleting a hash from the file, and a floor-marked list's upstream removals, which wait for confirmation there.
8. **Rule packs follow §20.6:** off until chosen, and pre-checked in the first-run wizard. Packs are never on by default for installs that skip the wizard.
9. **Blocklist subscriptions are the user's choice:**
   - **Off by default.**
   - The user chooses whether to add lists, and which ones, in the first-run wizard and in Settings.
   - The app shows a **warning matched to how strict the chosen lists are**, for example a list that would hide a large share of the DHT.
   - The owner didn't address the refresh mechanics (24 h refresh; holding a version that is empty, shrinks by more than half or grows tenfold); they stay as designed in §20.13.
10. **Riverstone stays exactly as chosen.** The §10.18 claim about colour-blind distinctness was wrong and is corrected. The non-colour cues (2 px gaps, fixed order, labels, the "High-legibility data" patterns option) carry the difference.
11. **Users control what is logged** (owner, 2026-10-22, settings session: "Allow user control over what is actually logged").
    - Every log event and every kind of data in a line can be switched by the user. By default there are no torrent specifics (architecture §12.1).
    - Asked whether the items otherwise never logged should be controllable too, the owner chose **"Allow both"**:
      - **Peer activity** (a peer address together with a torrent) and **Filter change details** (which rule, term or exception changed) may be logged;
      - each is **opt-in, off by default, time-limited and shown with a warning**;
      - they're written only to the local log file and live log.
    - This narrows "No profiles of people" (§1.9.7) and the no-audit-log decisions (§1.8.8, §1.8.9) for the log only, while the user has opted in. Outside that, they stand unchanged.
    - **Secrets are never logged.** The names of sealed (quarantined or dropped) items stay unloggable as well; they weren't part of the question, and revealing one keeps §20.11's warning and password.
12. **Lodestar's licence is AGPL-3.0-or-later** (owner, 2026-10-22, settings session). The owner said "This app will stay open source" and asked for the most appropriate licence. The owner chose the recommended **AGPL-3.0** over GPL-3.0, MPL-2.0 and MIT/Apache-2.0.
    - **Why:** Lodestar is a self-hosted network service. The AGPL makes anyone who runs a modified version for others over a network offer them its source, so every fork stays open; plain GPL doesn't cover hosted forks. The same choice is made by comparable self-hosted apps (Immich, Mastodon, Nextcloud, Grafana).
    - **Applied:**
      - `LICENSE` holds the full text;
      - `license = "AGPL-3.0-or-later"` is set in `[workspace.package]` (every crate inherits it), in `fuzz/` and in `web/package.json`;
      - the README has a licence section.
    - **The About screen's "Source code for this version" link** is the offer of source the AGPL requires (architecture §10.10.13).
    - **Brand assets** (the name as a product name, the logos, the mascot, the scenes and the icons) are outside the AGPL: all rights reserved, usable unmodified to refer to Lodestar (`brand/LICENSE.md`).
    - **Dependency allowlist** *(detail added by the settings session to carry out this decision, not the owner's words; architecture §16)*. It's enforced by CI, with B2c's licence generation, and applies to shipped components only:
      - **Allowed:** MIT, MIT-0, Apache-2.0, BSD-2/3-Clause, ISC, Zlib, 0BSD, BSL-1.0, BlueOak-1.0.0, Unicode-3.0, MPL-2.0, CC0-1.0, OFL-1.1 (fonts, shipped as separate files) and the GPL-3.0/AGPL-3.0 family.
      - Packages with no licence field go in a reviewed overrides file. The list is generated per shipped target, so platform-only crates drop out (architecture §10.10.13).
      - **Refused:** GPL-2.0-only, SSPL, non-commercial and proprietary licences.
      - Companions such as qBittorrent run as separate programs and aren't affected.
13. **Easy auto-update, optional at each release level** (owner, 2026-10-22, settings session: "Can we build in easy auto capability and make it optional at various release levels?", meaning auto-update). Architecture §12.2 has the details. This supersedes "Lodestar never updates itself" and the check-only design.
    - **Off until the user chooses it.** Checking contacts GitHub, and the system is offline by default (§1.2.3). The first-run wizard asks.
    - **The choices:**
      - **Check and notify**, or **Install automatically** at one of three levels: **Fixes only** (recommended), **Fixes and features**, or **All releases**;
      - **Always install security fixes**;
      - a channel: **Stable**, **Beta** or **Nightly**.
    - **Never automatic:** releases that need manual steps.
    - **Every update** is signed and verified, waits for an install window, is health-checked afterwards, and is rolled back automatically on failure.
    - **"No telemetry, ever"** still holds: checking sends nothing about the installation.
    - **Signing keys** (created by the owner, 2026-10-22):
      - a **release key** (`02910813433E81E0`), stored as the `RELEASE_SIGNING_KEY` secret of the GitHub `release` environment;
      - an offline, password-protected **recovery key** (`67922DDF005FD310`) that only signs key changes.

      The public keys are in `packaging/keys/`.
14. **User documentation is written for people who can install an app from the Unraid app store but may not know how to set it up** (owner, 2026-10-22, settings session: "add rules to write documentation for moderately-tech-savvy users, rather than users who actually know what they're doing. That means explaining things more, create/ provide diagrams and screenshots when possible").
    - The rules are in the project skill `user-docs` (`.claude/skills/user-docs/SKILL.md`), referenced from `CLAUDE.md` and `runner/standards.md` §4b.
    - **The rules:**
      - explain why before how, and define every term on first use;
      - one action per step, with a check after each change;
      - per-platform sections;
      - warnings, with the undo, before risky steps;
      - "If something went wrong" on every guide;
      - screenshots for every Lodestar screen a guide uses, captured automatically from fixture data;
      - Mermaid diagrams for anything that flows or connects, each with a text equivalent.
    - **The audience, refined the same day:** someone who can install an app from the Unraid app store but may not know how to set it up. They may be unfamiliar with Docker's architecture, with Windows' security features and limits, and with good security and safety practice. *"This user should be guided, in a plain matter-of-fact way, without being too wordy."*
      - Every choice shows its recommended, safe answer.
      - Unsafe options are named plainly, with the alternative.
      - Background stays brief, collapsed or linked.
    - **"Use documentation-skills to guide your documentation process"** (owner, the same day). The owner's new `dvortsis/skills/documentation-skills` collection guides the process:
      - `github-docs` for the guide (its "capable installer" is this audience);
      - `material-composer` for other materials.

      `user-docs` became a thin Lodestar layer on top of them. It sets the folder (`docs/user/`), the facts, the screenshots, Lodestar's safe choices, and their gates.
    - **Precedence,** as with `ui-design-skills`: these decisions, then `user-docs`, then the skills.
    - **Brand:** a `brand/brand.json` profile fixes the fonts the skills would otherwise misdetect.
    - **Scope:** the README, `docs/user/`, `docs/operations.md`, release notes and in-app help. Developer and contract docs are exempt.

15. **Runner concurrency:** up to 16 Opus code slices and 16 helper slices at once (raised from 4 core / 8 helpers, then doubled the same day). Slices still need disjoint owned paths, and every PR still gets a fresh Opus review before merge. The daily launch cap rose by 50%, from 40 to 60 slice launches a day (owner, 2026-10-23: "raise the cap by 50%"); it stays a backstop against runaway loops, not a budget.
16. **Adult-relabel quarantines:** when a user's Is-adult correction or class rule makes the Adult-only child-safety core quarantine a torrent, the sealed name is still purged after 30 days, but the entry then becomes a tombstone (hash and rule only), so the torrent is never fetched again. It can still be released deliberately, with the effect warning and the password.
17. **Deleted data is overwritten:** SQLite's `secure_delete` is on, so deleted rows (sealed quarantine names, deleted torrents) are zeroed rather than left in free pages that a copy of the data folder could recover.
18. **A red check that isn't the PR's doesn't block merge** (owner, 2026-10-22, build-runner session: "Yes, I agree to 1 and 2"). A check that's also red on `main`, or a known flake with a fix PR open, in code the PR doesn't touch, doesn't block a merge. The runner says so on the PR, naming the check, why it isn't the PR's, and the fix PR if there is one. Every other check must pass, and the fresh review still applies.
19. **The runner decides refinements of an owner decision** (same message). The runner may decide anything that refines an owner decision without reversing or loosening it: how a gate is measured, file layouts, defaults the owner didn't set. It records each one in `docs/decisions/` and flags it in its next update to the owner, who can veto it. Anything that reverses or loosens an owner decision, weakens a security or content-safety decision, or changes a number the owner set still goes to the owner first. The same day the owner also allowed changing the filter budget gate (§1.6.6) if something is more efficient for the stack: "You can modify the filter budget/ gate if there is something more efficient to our tech stack." Such a change is recorded and flagged the same way.
20. **The filter budget holds on typical hardware** (owner, 2026-10-23, build-runner session: "Go with whatever makes for the best user experience"). The 50 µs per torrent (§1.6.6) should hold on typical hardware rather than the fastest, so the reference is the EPYC 7763, the most common hosted runner. Until the engine meets it there, #97's calibrated gate uses the fastest runner seen as a stopgap; a follow-up performance slice moves the reference when the engine meets it.
21. **Open questions decided by the runner on the owner's instruction** (owner, 2026-10-23, build-runner session: "Make the decisions, consider best possible UX and performance be the deciding factor"). Like §2.19, each is flagged to the owner, who can veto it. Two of the eight questions were already settled: the Adult-hold question (§2.16) and #66's budget sign-off (superseded by #97 and §2.20).
    - **Every filter rule shape is held to the budget.** NEAR, negation, fuzzy, prefix and `title:` rules count, not only the shapes the cheap check can reject: a realistic mixed rule set is held at 50 µs, each expensive shape's cost at 10% of rules is reported, and every shape stays near-linear on hostile names. If a shape can't meet the budget at 10% of rules without changing what it matches, matching stays exact and the Filters UI warns about the rule set's cost instead.
    - **Migration snapshots follow the purge and expire.** `<db>.pre-vN.db` snapshots get the same expiry purge as the live store, with `secure_delete` on, so a sealed name never outlives §2.16's 30 days there. They're deleted after 90 days, which ends rollback to them, so nothing deleted since (§2.17) outlives 90 days either.
    - **NEAR with multi-word terms:** multi-word terms inside NEAR now match more widely than before. This is noted in the NEAR slice's decisions file only; there are no installs yet, so no re-preview of stored overrides is built.
    - **Release signing uses separate keys by role:**
      - The **release key** signs stable and beta manifests (both built from `v*` tags). It lives in the GitHub `release` environment, which only `v*` tags may use, with the owner as required reviewer, so the owner approves each release.
      - A **nightly key**, in its own environment, signs nightly manifests. Installs trust it only on the Nightly channel.
      - A **freshness key**, in its own environment and used only by the scheduled weekly re-sign, signs short statements that keep an already-signed manifest current (channel, manifest hash, timestamp, expiry). It can't sign new content, so a leak allows at most keeping an old manifest looking fresh, never installing anything new. Stable manifests therefore never lapse waiting for a weekly approval.
      - The nightly and freshness keys are reachable by workflows on `main`; a leak affects Nightly installs only, or freezes updates, and is fixed by a recovery-signed rotation.
      - The owner creates the nightly and freshness key pairs and sets up the environments and secrets.
    - **Recovery key:** an accepted risk. The offline, password-protected key isn't rotated; if it's lost or compromised, the remedy is a new build installed by hand. No rotation statement is designed.
    - **Merging stays with the runner.** Sessions don't merge their own PRs. Branch protection was later added by the owner's own decision, §2.22.
22. **Only the runner merges; sessions drive their own PRs** (owner, 2026-10-23: "Yes, I approve this change"). The owner asked for less risk, better throughput and minimal handholding.
    - **Merging:** only the build runner merges, enables auto-merge or writes to `main`. A hook (`.claude/hooks/merge-guard.sh`) refuses those calls in every other session.
    - **Branch protection:** `main` requires a PR and the `ci-ok` check, with no bypass. `ci-ok` covers the correctness jobs, so §2.18 no longer waves a red `ci-ok` job through: the runner re-runs it once if it's a flake, or fixes `main` first if it's red there. The performance budgets, `web.yml`, `e2e.yml` and `release.yml` stay outside `ci-ok`, under §2.18.
    - **Registration:** every session that writes code on the repository is a runner slice or registered with the runner, including sessions the owner starts.
    - **The `steward` skill** (`.claude/skills/steward/SKILL.md`): slice sessions self-review with a fresh Opus reviewer before opening their PR, fix their own red CI and merge conflicts, and message the runner when they are ready or blocked.
    - **The runner's final review is unchanged.** The runner also brings `main` into a stale PR before merging it, keeps `main` green by reverting or fixing a breaking merge first, and archives merged sessions.
    - **A watchdog** restarts a runner that has gone quiet.
    - **No merge queue:** GitHub offers merge queues only on repositories owned by an organization, and this one belongs to a personal account. The freshness rule in `runner/README.md` stands in for it.
23. **Code signing waits** (owner, 2026-10-23: "we're going to wait on the code-signing certificate since I'm not selling this commercially").
    - **No certificate for now:** Windows builds (MSI, portable zip, binaries) ship without an Authenticode signature until the owner gets one. The `owner-signing-cert` dependency blocks nothing.
    - **Update trust is unchanged:** the Ed25519-signed manifest and its SHA-256 and size pins (architecture §12.2) are what the update engine trusts, so the update path loses nothing it relies on. The engine checks Authenticode only on artifacts that carry a signature.
    - **The user guide** explains the SmartScreen warning plainly. It also shows how to check a download against the release's signed checksums before choosing "Run anyway" (§2.14).
24. **Multi-session best practices are standing rules** (owner, 2026-10-23, "M4 completion strategy" session: "Let's add all the best practices for multi-session development to Claude's global rules to ensure a more efficient experience going forward"). This came out of M4's last PR (#48): it chased `main` for 36 hours while `main` took 63 merges a day, picked up other slices' flaky tests, and its session sat on a permission prompt the owner couldn't see. The owner first approved a one-off merge pause until #48 lands, then made the lessons permanent.

    The practices, as applied (the owner approved them as a set; the wording and thresholds below are refinements under §2.19, not the owner's words):
    - **A landing lane:** the runner holds merges that overlap a reviewed PR that keeps needing `main`, until it lands. It extends §2.22's freshness rule (`runner/README.md` step 4).
    - **Small PRs:** slices that would be large are split before launch, so each PR lands between other merges.
    - **Shared CI and branches:** one push per round of fixes, `main` brought in only when needed, pushes held during a pause, and one session per branch.
    - **Tests wait for conditions, never fixed sleeps.** A flake is fixed once, where it is, not re-run onto the next PR.
    - **Sessions don't stall unseen:** long-running sessions run in Auto, the coordination tools are pre-approved in `.claude/settings.json`, and the runner notifies the owner at once when a session waits on a permission prompt.
    - The rules are in `CLAUDE.md` ("Multi-session work"), `runner/README.md`, `runner/standards.md` and the `steward` skill. The thresholds were picked under §2.19, so the owner can veto them, and are recorded in `docs/decisions/2026-10-23-multi-session.md`.

## 3. Points still open (not decided by the owner)
- **Name-pattern blocklists.** The owner said "build those" *(M4, 10-22 03:27:05)*; they were then moved into the §20 rule engine by the approved design PR #27. Assumed satisfied by the rule engine unless the owner says otherwise.
- **The libtorrent interop harness.** A commit adding it was rejected *(M0–M2, 10-20 23:41:23)*, possibly as an interruption rather than a refusal. It isn't built.
- **First-load budget.** The approved plan said JS < 200 KB gzipped; the later UI plan (§10.1) uses ≤ 100 KB total first load (JS ≤ 45 KB). The stricter §10.1 figures apply until the owner says otherwise.
- **Repository visibility.** The repo is public. The owner created it that way and hasn't commented since.
