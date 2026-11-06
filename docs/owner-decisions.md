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
9. **Adult is a media category alongside movies, TV shows and the rest.** *(implemented in PR #28, merged)*

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
   - content filtering → #27

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
6. **Filter engine:** the planned target is under 50 µs per torrent at 10k rules.

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

1. **The blocklist is easily editable and as smart as possible**, to prevent false positives while effectively blocking the specific material it is intended to block.
2. **The child-safety core is always present,** and its rules are editable so false positives can be corrected.
3. **Locked floor = "Minimal floor": CS-1a/1b and CS-8a only.**
   - These hold only torrents the owner confirmed or deliberately imported, plus their exact reposts.
   - They are always on and can't be switched off, but remain editable, for example to remove a mistaken entry.
4. **Every core rule is on by default;** the user can switch off every one that isn't locked.
   - Pattern-based core rules are individually switchable because heuristic matches carry false-positive risks, including ambiguous wording and legal adult role-play marketing (§20.5.1).
5. **CS-1 is a block against specific torrents the user tagged themselves.** Its sources are:
   - the owner's confirmations in the review queue;
   - "Report as child-abuse material";
   - hash lists the owner deliberately imports.

   No third-party list is built in, and there is no crowdsourcing. There is a manual "report to a hotline" link.
6. **CSX-5 is an editable override list:** CSX-5 entries, patterns and keywords included, override every filter **except the locked floor**.
   - A motivating example for false-positive overrides is **"jailbait", which is a popular legal adult film series**.
7. **Mandatory impact preview before any filtering change** (§20.10).
   - Keyword entries choose between **"Override"** (default) and **"Neutralize term"** strength, with both shown side by side in the preview.
8. **No audit log for filtering changes:** changes are not recorded in an audit log because Lodestar deals with torrents.
   - Instead, there is a plain-language warning about the change's effect, and the user must enter their password to make the changes.
   - Per §20.11: the password is required for every weakening change, every time, with no grace period. Strengthening changes and previews need no password.
9. **No audit log for exceptions either, with effect warning and password.**
   - Exceptions keep no author, reason or history.
   - Tests created from false positives are stored hashed, so no torrent names are kept.
10. **Popular blocklists:** users can easily connect to popular blocklists and have the app keep them updated.
11. **Subscribed blocklists never feed the locked floor.**
    - The sole exception is a vetted child-abuse hash list that the owner explicitly marks as part of the floor, with a warning and the password (§2.7).
12. **Adult is a media category alongside Movies, TV and the rest.**
    - "Show adult content" is off by default and turned on with an 18+ confirmation.
    - Adult sources use the normal consent levels.
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
8. **Rule packs follow §20.6:** off until chosen, and pre-checked in the first-run wizard. Packs are never on by default for installs that skip the wizard. *Superseded by §2.39 (2026-11-04): packs are on by default and the user opts out.*
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
25. **After the CR-0 soak: history, release, crawler target, import scale, runner** (owner, 2026-10-30, build-runner session, answering the runner's questions while approving the master-plan update). The plan is `docs/build-plan.md` §0. The details the runner chose are in `docs/decisions/2026-10-30-master-plan.md`.
    - **The rewritten history replaces `main`.** The owner force-pushes it, since Claude can't and shouldn't. The owner's local branches (`fix/dht-cr0`, `work/soak-*`) already sit on it. The runner moves the paused slice branches over as patches.
      - **Done 2026-10-30:** the owner backed up every ref, force-pushed `058d9937` over `2d5e761d` with a lease, and restored the `main` ruleset. The owner confirmed the rewritten history as intended, including its two tree differences from the old `main`: wording in this file's §1.8 (the owner-message quotes and timestamps removed) and in `docs/architecture.md` (among them §20.8's guardrail test now reads "can't be deleted" rather than "can't be deleted or disabled", matching §1.8.4's switchable non-floor rules).
    - **No v0.5.0 release.** That commit isn't tagged. The first public release is **v0.5.1**. It ships once CR-0, the soak fixes and the open security and content-safety fixes are merged and the CR-0 soak is accepted.
    - **Crawler throughput gate:** at least **100 verified torrents per minute**, sustained. The stretch target is **1,000 per minute**. Final parity is checked against a pinned bitmagnet run on the same host. This gates the v0.6 tag. The exact measurement rules are refinements the owner confirms before the first acceptance analysis (§0.6 of the build plan).
    - **Import scale, staged to the full corpus:** pilots run at 1M and then 10M torrents within the §1.6 budgets. Then a measured **50M scale gate** runs, and its new numbers come to the owner for approval before the owner's full Magnetico and Bitmagnet corpus (about 40–50M unique torrents) is imported.
    - **The runner restarts** with this approval. The v0.5-only freeze is lifted for the work listed in the build plan's §0. All of it runs in cloud sessions, never on the soak PC. The approved plan runs **at most four helper slices at a time** in this phase, filled by the priority order in §0.2; `runner/slices.yaml` `limits` (§2.15, 16 + 16) stay the ceiling once the owner lifts this.

26. **The runner follows its own recommendation on performance and UX decisions** (owner, 2026-10-31, build-runner session: "For the other decisions and similar decisions in the future, follow your recommendations for the best performance and UX."). This extends §2.21 from a one-time set of questions to a standing rule. It covers changes to numbers the owner set when the change serves performance or UX, such as size budgets and gate thresholds. Each decision is recorded in `docs/decisions/` and flagged to the owner, who can veto it (§2.19). Anything that weakens a security or content-safety decision still goes to the owner first. The decisions taken under this rule the same day are in `docs/decisions/2026-10-31-owner-delegation.md`.
    - **Branch clean-up approved** (same message: "Yes, OK to clean up"). The branches from the old history (§2.25), `next` (now part of `main`) and `measure/windows-nrt` will be deleted. The owner runs the deletion, because the proxy refuses branch deletes from cloud sessions.

27. **The v0.6 crawler gate stays, with a checkpoint; the handoff serves any agent** (owner, 2026-11-02, master-plan review session, answering the runner's questions while asking for the plan to be refreshed "detailed enough for another model to pickup where Claude leaves off when usage expires"). The plan is `docs/build-plan.md` §0; the details are in `docs/decisions/2026-11-02-master-plan-r2.md`.
    - **The v0.6 tag stays gated on ≥ 100 verified torrents per minute (§2.25), with a decision checkpoint.** Told that the crawler measures about 12/min and that no live A/B has run, the owner chose "Keep gate, add checkpoint" over decoupling the tag from the gate or setting a lower interim bar. Checkpoint 1 comes after the CR-ADM and CR-4 live results and a bitmagnet comparator run. If the projected single-node rate falls short, the runner reports the ceiling, the dominant limiter and the options before further tuning, and the owner re-decides. The runner never loosens the gate.
    - **The handoff works for both a restarted Claude runner and a non-Claude agent** (the owner chose "Both"). `docs/handoff.md` carries both paths.
    - **Scope:** the build plan, the slice graph, the handoff and a decisions file, as one docs PR ("Plan + graph + handoff"), to reach `main` "once all issues are resolved and plan is finalized".

28. **Every question waiting on the owner takes the runner's recommendation** (owner, 2026-11-02, build-runner session: "For any questions waiting on me: Use your recommended solution, for best UX/ performance."). This covers the questions open in `docs/build-plan.md` §0.3 and `docs/plan/2026-11-01-v06-sources.md` §5 on that date, including the content-safety, privacy and legal ones the runner had routed to the owner. Where a written recommendation existed, it applies as written. Where none did, the runner formed one under this instruction; those are marked below and listed in `docs/decisions/2026-11-02-owner-delegation-2.md`. The owner can veto any of them (§2.19).
    - **v0.5.1 and #357 (F-filter-load-fail-closed)** (written, build plan §0.3 row 1). If the soak has to be re-run anyway, cut rc.3 with #357 cherry-picked. Otherwise ship rc.2 and put #357 in v0.5.2 within days.
    - **The upgrade test** (formed under this instruction). r1's clean V3 → V12 upgrade of the Oct 22 volume counts, provided the migration code in rc.2 is the same as in r1's build. The runner checks that; if it differs, rc.2 gets a short upgrade smoke on a copy of that volume.
    - **Migrations on `main` before the v0.5.1 tag: allowed now** (formed under this instruction, lifting the owner's 11-01 hold; the runner's earlier view is in `docs/decisions/2026-11-02-master-plan-r2.md`). v0.5.1 ships from `release/v0.5.1` (V12), `main` already carries V13, and the v0.5.2 gate checks the V12 → V13+ upgrade and the rollback to a snapshot. SRC-1 (V14) may land; `ext:owner-migrations-on-main` holds. The guide keeps describing `nightly` as untested and only for trying Lodestar.
    - **Q2, the ToS and robots position per source** (written, plan §5). A plugin ships only if its recorded check finds automated feed or API access allowed. A grey case comes to the owner.
    - **Q15, metadata for `private_source` items** (written). Claims only by default. The `.torrent` is fetched through the private indexer only on the user's explicit interest, never automatically, rate-limited, with a warning that it may count as a snatch.
    - **Q1, consent wording** (written). The runner drafts plain-language texts per §10.14 in SRC-4's decisions file, and the owner approves them before SRC-15b merges. `ext:owner-q1` holds only once that approval is recorded.
    - **Q3, torrent caches** (written). Off by default, External consent, a user-editable list that starts empty with suggestions shown, never for `private_source`, only for user-interest and unverified items.
    - **Q7, the Sukebei feed in v0.6** (written). Allowed as a variant of the Nyaa plugin: off by default, External consent, every item forced into the Adult category, hidden unless "Show adult content" is on.
    - **#362's egress trade-off** (written, §0.3 row 4b). Accepted for v0.6: blocklist list fetches go through the SSRF guard and ignore `HTTPS_PROXY`; a whole-container VPN still applies. A guarded proxy mode can come later.
    - **Adult stand-in holds (#367)** (the runner's recommendation from its owner checklist, 2026-11-02). Not released automatically. They stay held and are counted for review in Insights; there's no stored-format change.
    - **Required checks** (written, §0.3 row 7). `Web / web` and `E2E / run` should be required in the ruleset. The owner changes the ruleset; the runner can't.
    - **Still needing the owner's hands, not a decision:** handing back the r3 soak evidence, the gluetun leg, tagging `v0.5.1` and approving the release environment, naming the crawler experiment host, the nightly signing key, the ruleset change above, the Unraid submission, and approving the Q1 consent texts once drafted. Checkpoint 1 and the 50M numbers come later, when they're ready.
    - **Not covered, because no recommendation existed yet:** B-import-server-2's API surface and the `.torrent` upload middleware exception. The runner brings a recommendation when those slices are planned.

29. **#357 ships in v0.5.1** (owner, 2026-11-02, master-plan review session). An external review of the master plan recommended that the first public release include F-filter-load-fail-closed (#357), the content-safety fix merged after the v0.5.1 cut. Asked to choose between that and §2.28's "rc.3 only if the soak must be re-run anyway", the owner chose "Include in v0.5.1". This replaces that item of §2.28.
    - **rc.3 = rc.2 + #357,** cherry-picked onto `release/v0.5.1`. The runner checks it applies without pulling in other post-cut changes and maps the delta to targeted tests.
    - **#357 changes filter loading and its reporting,** not the crawler lifecycle, the sampler or the endpoint policy, so under the delta rule (build plan §0.4) r3's soak evidence carries over. A short Unraid smoke of the rc.3 image confirms it starts, upgrades and serves. If the cherry-pick pulls in a class of change the delta rule names, the rule decides instead.

30. **Two worker slots, and a launch pause until the soak verdict** (owner, 2026-11-02, build-runner session, approaching the weekly usage limit). "Once tasks complete, bring the number of worker slots down to two" (about 15:05Z), then "Once all active sessions are pushed, pause start of further sessions. I want to wait until we get the soak results" (about 20:40Z).
    - **At most two slice sessions at once,** replacing the four of §2.25. `runner/slices.yaml` records it as `limits.phase_total: 2`.
    - **No new slice sessions start until the soak r3 results are in.** The runner then reports the verdict and resumes launches only on the owner's word. Sessions already running may still push, and their PRs may merge.

31. **rc.3 contents after soak r3, memory fix first** (owner, 2026-11-03, build-runner session). Soak r3 missed the memory bar (`docs/decisions/2026-11-03-soak-r3-verdict.md`). Asked to confirm the runner's recommended plan, the owner answered: "Go with your recommendation but the memory fix is the priority."
    - **rc.3 = rc.2 + #357 + #402 + the memory fix,** then one fresh frozen soak (r4) on rc.3. This replaces §2.29's "rc.3 = rc.2 + #357". The memory fix is the first thing built.
    - **Launches stay paused (§2.30),** except work by the sessions that already exist. The soak containers are stopped at the owner's convenience.

32. **Launches resume with four slots** (owner, 2026-11-04, build-runner session, after the usage limit reset): "My limit reset. You can continue building at full speed with all four slots."
    - **At most four slice sessions at once,** replacing §2.30's two and ending its launch pause. `runner/slices.yaml` records it as `limits.phase_total: 4`. The ladder in build plan §0.2 still decides what fills a slot.
    - **What the message leaves unchanged (runner's reading, per §2.31 and build plan §0.2 rung 4):** soak r4, the gluetun leg, the Windows smoke and the `v0.5.1` tag follow §2.31, and the epics held for the soak verdict (E-M6, E-v06-workers, E-v06-companions) stay held until r4's verdict.

33. **Up to eight slots, and auto-approval of best-practice decisions** (owner, 2026-11-04, build-runner session): "Auto approve plans and normal decisions as long as they support UX and performance best practices. Are you able to increase number of slots up to 8 without losing efficiency?" The runner answered that eight works while that many ready slices own disjoint files, and raised the cap.
    - **At most eight slice sessions at once,** replacing §2.32's four. `runner/slices.yaml` records it as `limits.phase_total: 8`. A slot stays empty rather than take a slice whose `owns` overlaps a running one.
    - **Auto-approval.** The runner approves its own plans and normal decisions without asking first, as long as they support UX and performance best practice. Each is still recorded in a `docs/decisions/` file and named in the daily summary, where the owner can veto it (§2.19, §2.26). This extends §2.26 and §2.28.
    - **Still for the owner (runner's reading, from CLAUDE.md's order of authority and §2.28):**
      - reversing or loosening one of the owner's decisions;
      - weakening a security or content-safety decision;
      - changing a number the owner set, other than performance and UX changes under §2.26;
      - a terms-of-service grey case (§2.28 Q2);
      - steps that need the owner's hands: tags, hosts, releases, rulesets.

34. **CR-SPEC confirmed, with the runner's answers to O1-O6** (owner, 2026-11-04, relayed by a helper session; the owner approved the runner's recommendations in chat).
    - **CR-SPEC (`docs/decisions/2026-11-02-CR-SPEC.md`) is confirmed.** `owner-cr-spec` holds.
    - **O1: exclude.** The 24 h gate window excludes the 6 h first-start suppression and the 60 min warm-up (rule 1.1). Startup is still published (rule 1.7). O2-O6 are accepted as written.
    - **#367 stays out of v0.5.1** and ships in v0.5.2 from `main`, with no rc.4.
    - **Still queued:** the Q1 consent texts wait until SRC-4's drafts can be reviewed, and Checkpoint 1 stays queued.

35. **A narrow `unsafe` exception for SQLite's allocator and memory statistics, and UTC-only records** (owner, 2026-11-04, build-runner session). Asked two questions, the owner answered "Yes" to the first and "Follow your recommendation" to the second.
    - **The exception.** The workspace keeps `unsafe_code = "forbid"` (§1b.5), except in `lodestar-store` alone, where it is `deny`. There, `#[allow(unsafe_code)]` is permitted only on:
      - the argument-free reads `sqlite3_memory_used()` and `sqlite3_memory_highwater()`, for the memory attribution gauges (X-mem-gauges);
      - one `sqlite3_config(SQLITE_CONFIG_MALLOC, …)` call before the first connection opens, which routes SQLite's allocations through mimalloc (X-sqlite-malloc). The memory-routing slice was named in the question.

      Each `allow` sits on the smallest possible block, with a `// SAFETY:` comment. Each PR that adds one runs `/security-review` and records it in its decisions file. Any other `unsafe`, in this crate or elsewhere, still needs the owner.
    - **Not covered: the allocator callbacks.** `SQLITE_CONFIG_MALLOC` takes a `sqlite3_mem_methods` table of `extern "C"` callbacks (xMalloc, xFree, xRealloc, xSize, xRoundup, xInit, xShutdown). Each of those needs `unsafe` of its own and would call mimalloc through a new `libmimalloc-sys` dependency. The runner had described the routing fix as one start-up call, which understated it (corrected by #428's review). So before X-sqlite-malloc launches, the runner brings its full `unsafe` surface to the owner, or a route with no `unsafe` (for example mimalloc's `override` feature or a build-time flag).
    - **Times in records are UTC.** New docs, reviews, soak notes and evidence give times in UTC (`Z`), never a local time zone, offset or place. The existing files on `main` that name a local time zone stay as they are, because git history keeps them anyway (runner's recommendation, accepted).

36. **Up to twelve slots, filled only when it makes sense** (owner, 2026-11-04; given in the dashboard session and relayed to the runner in their words): "Let's increase slot limit to 12, although not mandatory to fill all slots if there isn't enough work or doesn't make sense for the roadmap / plan."
    - **At most twelve slice sessions at once,** replacing §2.33's eight. `runner/slices.yaml` records it as `limits.phase_total: 12`.
    - **Filling them all isn't required.** A slot stays empty when no ready slice fits, when a slice's `owns` would overlap a running one, or when starting it doesn't make sense for the roadmap or plan. That's the runner's judgment, as under §2.33. Review bandwidth stays the real limit: every slice still gets a fresh Opus review before merge.
    - §2.33's auto-approval and its "still for the owner" list are unchanged.

37. **The owner's answers to the owner queue** (owner, 2026-11-04 about 20:30Z; given in the dashboard session and relayed to the runner). Item numbers are the queue's as of 2026-11-04 before this record; the queue was renumbered after it. Queue items 1, 2, 8 and 9 were left unanswered and stay open.
    - **Required web checks: yes** (queue item 5, the runner's recommendation). `web-ok` and `e2e-ok`, the always-reporting aggregates X-required-web-checks added, join `ci-ok` as required checks on `main`. Adding them to the ruleset is the owner's step. Until it's done, the runner judges them by the green rule (`runner/README.md`).
    - **Imports through the running server, and the `.torrent` upload middleware exception** (queue item 6): "go with the recommended solution once identified." The runner identifies the recommendation when B-import-server-2 or E-M7's upload slice is planned, records it in that slice's decisions file, flags it (§2.19) and runs `/security-review` on the slice. It goes back to the owner only if the solution built deviates from that recommendation. `owner-import-server-surface` and `owner-upload-exception` hold on those terms.
    - **Unraid Community Apps:** the owner submits after the 1.0 release is complete, not before (queue item 7).
    - **Rule-pack Q1-Q5: the runner's recommended answers, as written in `docs/plan/2026-11-04-v06-rulepacks.md` §5** (queue item 10). That table is the decision; the bullets below summarise it. `owner-rpq2`, `owner-rpq3` and `owner-rpq4` hold.
      - Q1: no separate pack signing channel in v0.6. Packs change only with a signed release, and RP-0's versions and changelogs make each change visible. A between-release channel can come later with its own key, like §2.21's freshness key, if the owner wants faster pack fixes.
      - Q2: ThePornDB and StashDB are sent only verified, Adult-classified items; never held, hidden, `private_source` or unverified items. Only the display name or the parsed site, date and title, never the hash or the files. User interest first, then a capped backfill. Nothing while "Show adult content" is off.
      - Q3: the child-safety core judges enrichment text, stricter only. It can add a hold through the core's pattern rules (with their age-signal thresholds for Adult), never remove one, and never counts as a counter-signal. Descriptions aren't fetched or stored.
      - Q4: performer names are stored and shown on the item they belong to (the drawer only). No `performer:` search field or facet, no per-performer pages or cross-item lists in v0.6, and no other personal fields.
      - Q5: no age data from enrichment in v0.6. Birth dates are neither requested nor stored.
    - **SRC-12's grey case: skip** (queue item 11): "skip public trackers whose published rules forbid scraping." Lodestar ships and suggests no such tracker, and the settings text and the user guide tell the user not to add a public tracker whose published rules forbid scraping. The plugin can't detect those rules (`docs/decisions/2026-11-04-SRC-12.md`), so it relies on the user's list.
    - **No veto** (queue item 12) of SB-6's `size_bucket`, SRC-11's automatic `.torrent` export, or dropping SRC-10b.
    - **Signing keys** (queue item 4): the owner reports the release and freshness environments and keys as created. The freshness key (generation 0, `FCA9AFC5C8D864FE`) was already embedded by #211, merged 2026-10-30. The nightly key is skipped for v0.5.1, so that item stays open and optional.
    - **Q1 consent texts: not yet approved** (queue item 3). The draft is in `docs/decisions/2026-11-02-SRC-4.md`, "Draft consent texts". `owner-q1` stays unmet.
38. **No Anna's Archive or LibGen plugins, and a legal notice rather than terms of use** (owner, 2026-11-04, legal-notice session). Asked whether plugins for Anna's Archive and LibGen should ship, given the US default judgments and injunctions against both sites (`docs/legal/2026-11-04-liability-review.md` §2.1), the owner answered: "We can skip Anna's Archive and LibGen plugins." In the same session the owner, after a second review found that a terms-of-use conflicts with the AGPL, asked for the model BitTorrent clients use and approved the result: "I would keep this two-document approach."
    - **Neither plugin is built or listed.** The mentions in `docs/architecture.md` (§16's roadmap line, the §18.1 catalogue row, the §18 v0.8 row and the §18.7 Restricted example) are marked superseded in place by the same PR; the "Anna's Archive / LibGen importer (restricted consent)" item in `docs/build-plan.md` §0 is the runner's to remove (`docs/decisions/2026-11-04-X-legal-docs.md`). The Restricted consent class stays for tracker-announce peer finding and any future high-risk source.
    - **Lodestar ships a Legal Notice** (`docs/legal/legal-notice.md`, confirmed as read at first run) and operator guidance for public instances (`docs/legal/operator-guidance.md`), with no additional licence terms beyond the AGPL. Colorado counsel reviews both, as the next point says.
    - **Counsel's review is advisory only** (owner to the runner, 2026-11-04). The Colorado counsel review of the Legal Notice and the operator guidance goes in the owner queue and does not block the v0.5.1 tag.
    - **Slices (runner, 2026-11-04):** `LEGAL-1`, one Opus slice in v0.5.1 (rc.4), ships the notice in the binary, the wizard step with its checkbox, `POST /setup` refusing a missing or stale `notice_version`, the login gate that re-shows a raised version, and About → Legal. `LEGAL-2`, the crawler paused on a fresh data directory until setup completes, is adopted by the runner under §2.26 for after v0.5.1, so rc.4 stays small. *Refined by §2.40 (2026-11-05): v0.5.1 gets `LEGAL-1r`, the notice on the release branch's setup screen; `LEGAL-1`'s wizard step follows on `main`.*
39. **Rule packs are on by default, the user opts out, and an unauthorised-copy pack ships in the first release** *(moved to v0.5.2 by §2.40)* (owner, 2026-11-04, legal-notice session). Asked whether the release-parser corpus could back an optional exclusion filter for copyrighted material, the owner answered: "I want all filters pre-checked by default and have the user review everything, where they will be allowed to uncheck everything as needed, so the user opts out rather than opts in to filters. I want it shipped in the first version."
    - **Every rule pack is on from the first start, including on an install that skips the wizard.** The wizard's packs step shows all of them checked, for review; the user can uncheck any pack there (through the weakening flow: effect warning plus the password just set) and any pack or rule in Settings → Filters & Safety at any time. This **supersedes §2.8's "off until chosen" and "never on by default for installs that skip the wizard"**, architecture §16's "M5 decisions" on packs, and the "fixed constraint" in `docs/plan/2026-11-04-v06-rulepacks.md` §1. The core's locked floor and the password on weakening changes (§1.8.8, §20.11) are unchanged.
    - **A sixth pack, `unauthorised-copy`** (rules UC-1 camcorder and screener rips, UC-2 cracked software, UC-3 scene-style film and TV rips with an exemption for official sources and a built-in allow list of public-domain and Creative Commons titles), built on the release parser's fields; the parser's golden corpus is its test set, with a lawful-names set that must never be hidden. Details in `docs/decisions/2026-11-04-X-legal-docs.md`.
    - **Cracked software also feeds the malware pack** (owner, 2026-11-04). Offered the choice, the owner answered "Go with your recommendations" to the recommendation to "wire the parser's existing `cracked` flag into the malware rule pack so it becomes a safety signal". That rule is MW-6 (architecture §20.6; specified in the decisions file: the flag plus a crack-specific token, never `patch` alone), with its own switch in the malware pack, so switching the `unauthorised-copy` pack off doesn't bring cracked software back. SC-1's guard, which left offline cracks uncovered as a preference, now points to MW-6.
    - **Existing installs are offered, not flipped** (runner, 2026-11-04, under §1.8.7; flagged to the owner under §2.19). The defaults apply to a fresh data directory, a store created on this start with no prior schema, marked as fresh by the migration that creates it: at the first pack start every pack gets an explicit on row and every rule is seeded at its current version (`pack_switches` keeps reading no row as off), so the wizard shows them all on and the user may opt out. A store that existed before the RP-DEF build gets an explicit off row for every pack that has none, the new `unauthorised-copy` pack included, and a marker, so it keeps exactly its previous behaviour. The wizard's review or the filters screen offers to turn packs on through the existing preview → apply flow, which is the §1.8.7 impact preview and records the current versions. New rules in a pack that is already on (MW-6 in a malware pack the user turned on) stay in shadow, as RP-0 designed, until the accept route and UI ship (RP-3 and RP-6, after v0.5.1).
    - **Both ship in v0.5.1.** This adds to the release scope in §2.25 and §2.28; the runner schedules the slices. **Through rc.4,** together with the memory fixes, with soak r5 covering them (owner to the runner, 2026-11-04, ~23:30Z). *Superseded by §2.40 (2026-11-05): RP-DEF and RP-UC ship in v0.5.2; v0.5.1 carries the Legal Notice only.*
    - **Not changed by this decision:** blocklist subscriptions (§2.9) stay off until chosen, because downloading them contacts GitHub and the system is offline by default (§1.2.3). The owner can extend the decision to them.

40. **rc.4 carries the memory fixes and a release-branch Legal Notice; rule packs on by default and the unauthorised-copy pack move to v0.5.2** (owner to the runner, in chat, 2026-11-05 ~01:10Z). Soak r4 on rc.3 failed its memory gate (`docs/decisions/2026-11-05-soak-r4-verdict.md`). The runner then found that `release/v0.5.1` split from `main` on 2026-10-30 and that `main` is about 1,180 commits ahead: the release branch has the older setup screen, no setup wizard and five fixed packs, and none of RP-0's pack versions that RP-DEF and RP-UC are specified on. Offered four options (memory fixes plus a release-branch notice; memory fixes only; all three legal slices ported to the release branch; v0.5.1 cut from `main`), the owner chose "Memory + notice only (Recommended)", whose text read: "rc.4 = the memory fixes plus a small Legal Notice made for the release branch's setup screen (setup and login refuse until it's accepted). Packs on by default and the unauthorised-copy pack ship in v0.5.2, built on main now as specced. Soak r5 starts soonest, and v0.5.1 still opens with the Legal Notice."
    - **The memory fixes:** X-thp-off (#434), X-mem-gauges (#433) with X-mem-gauges-2 (#442), and X-mimalloc-override (#448), brought onto the release branch by `R-rc4-cut`. Soak r5 runs on the frozen rc.4 image.
    - **`LEGAL-1r`** builds the notice on the release branch.
    - **`RP-DEF` and `RP-UC` ship in v0.5.2**, as specified in `docs/decisions/2026-11-04-X-legal-docs.md`. This supersedes §2.39's "Both ship in v0.5.1 … through rc.4" for them; the rest of §2.39 stands.
    - **Runner additions** (§2.26, §2.33; flagged to the owner under §2.19):
      - rc.4 also carries X-log-url-redact (#432), a security fix that rc.1–rc.3 lack (URL credentials could leak into logs after a stray `://`, and an IPv6 look-ahead could panic).
      - `LEGAL-1r` also shows the notice in About.
      - It stores the accepted version in `auth.json`, beside the admin password and in the same write as setup, rather than in `settings` as the X-legal-docs spec has it. So rc.4 changes no database schema, and r1's upgrade evidence still counts under §2.28. `LEGAL-1r`'s decisions file records this, and `LEGAL-1` on `main` follows it.
      - The release copies of the notice and the operator guidance are corrected to what the v0.5.1 build does: no packs on by default, no unauthorised-copy pack, and no Settings controls for filters. `main`'s copies keep their "(planned)" markers.
      - `LEGAL-1`, the wizard step on `main`, follows `LEGAL-1r`. `LEGAL-2` stays after v0.5.1.
    - **Host steps for soak r5** (owner, via the dashboard session, 2026-11-05 ~01:25Z): Appdata Backup stays disabled until all soaks are done, and the temporary SSH key stays for read-only checks during r5.

41. **Fill every free slot until the cut to four** (owner, via the dashboard session, 2026-11-05 ~02:45Z): "Let's do 1, 2, and 3 until tomorrow at 5pm, when it goes back to 4 slots." The cut is the step from §2.36's twelve slots back to four, already scheduled by the runner for 2026-11-06T00:00Z. The three items the dashboard session offered:
    1. **A standing rule until 2026-11-06T00:00Z:** the runner launches the next ready slice whenever a slot is free, up to 12, ranked by critical path first and then by how many slices it unlocks. Overlapping `owns` still keeps a slice out (§2.36). A slot stays empty only for a reason the runner records: an `owns` conflict, the release branch, a stale ready slice, or an owner pause. The slot then shows `status: idle` with the reason in `doing`. rc.4's critical path still comes first when it needs the CI queue.
    2. **A dashboard alarm,** "free slots, work ready", built by the dashboard session from `slices.yaml` `owns` against the running slices.
    3. **An hourly check to the runner** from the dashboard session until the cut. On each one, the runner launches a ready slice that fits, or replies with one line giving the reason.

42. **At most four slots from 2026-11-06T00:00Z** (owner, 2026-11-04, runner chat): "Automatically change max number of slots to 4 slots at 5 MST on 11/5". Asked to confirm, the owner confirmed the cut is 2026-11-06T00:00Z and said that sessions running beyond four at the cut are let finish.
    - **At most four slice sessions at once,** replacing §2.36's twelve (and §2.43's interim eight). `runner/slices.yaml` records it as `limits.phase_total: 4`. §2.41's standing fill rule and hourly check end at the cut.
    - **Nothing is stopped.** Slices running at the cut finish normally, through review and merge. The runner launches nothing new until fewer than four are running.
    - §2.36's other terms stand: filling every slot isn't required, a slot stays empty rather than overlap a running slice's `owns`, and §2.33's auto-approval and "still for the owner" list are unchanged.

43. **An interim limit of eight** (owner, 2026-11-05T16:15Z): "Let's bring the slots down to 8. Complete the existing slices."
    - **At most eight slices at once until the cut,** down from §2.36's twelve. Running slices completed; nothing new launched while eight or more were running. §2.42 supersedes it at 2026-11-06T00:00Z.

44. **Keep running** (owner, 2026-11-05T23:25Z, relayed to the runner by the dashboard session): "Keep running. We have usage available."
    - **The runner's hold on launches is lifted.** The runner had held new launches only to save the weekly usage budget, which was at its warning level until 2026-11-06T00:00Z. X-claim-store-polish launched under this decision, at 23:26Z.

45. **A no-limit window until about 2026-11-06T00:15Z** (owner, 2026-11-05T23:35Z, relayed by the dashboard session): "Use as many resources as needed to get as much done in the next 30 mins as possible." Then, about 23:45Z, in the runner chat: "There is no limit to slices or subagents for the next 30 mins if it would be useful to overall progress."
    - **For those 30 minutes, no slot limit applied** to slices or sub-agents that were useful to overall progress. The rules that keep quality stayed: disjoint `owns`, a fresh Opus review before merge, and the order of authority.
    - **Launched in the window:** P-v07, P-v06-workers, P-v06-companions, P-m6-audit, P-v052-manifest, P-m8, P-v08, X-tcache-recheck, X-sources-low, RP-G and X-sp4-followups, plus a read-only security pre-audit of the v0.6 sources code.
    - **They run to completion under §2.42.** No new slice launches until fewer than four are running.

46. **The runner decides the planning-wave questions on performance and UX** (owner, 2026-11-06T04:26Z, relayed to the runner by the dashboard session): "Have the runner decide based on what's best for performance and UX. Provide a summary of the decisions." It answers owner-queue item 10, the planning-wave list (v0.7 Q1–Q6 and Q10(d), companions cq1–cq6, Q20 and Q21, M8 OQ1–OQ6, M6 Q6, Q8, Q10, Q13 and Q14, workers wkq1–3, v0.8 Q1, Q3 and Q4). Each question's decision and reason are in `docs/decisions/2026-11-06-runner-planning-wave-answers.md`.
    - **Decided by the runner under this delegation,** each as the plan's own recommendation. None of them loosens an owner decision, weakens security or content safety, or changes an owner-set number:
      - v0.7 Q5 (`owner-v07-q5`), except debrid's hash lookups, which go with Q4, and Q6 (`owner-v07-q6`);
      - companions cq1 (`owner-cq1`), cq2 (`owner-cq2`, with the pin-time signature check flagged under §2.19) and Q20 (`owner-cq7`: no relaxation; each item CMP-G's smoke finds comes back to the owner);
      - workers wkq3 (`owner-wkq3`; back to the owner if a worker measures over 256 MB at the owner's corpus);
      - M8 OQ2 (`owner-m8-q2`), OQ3 (`owner-m8-q3`), OQ4 (`owner-m8-q4`) and OQ5 (it gates nothing);
      - M6 Q10;
      - v0.8 Q3's drafting.

      Those conditions hold.
    - **Not covered by this reply, so they stay with the owner** (CLAUDE.md: the runner doesn't loosen owner decisions, weaken security or content safety, decide a source's terms or consent, or change an owner-set number on its own). The dashboard session asks the owner to confirm each one by name:
      - v0.7 Q1 (the Stremio addon: a URL-path token and CORS, and whether it ships at all);
      - v0.7 Q2, Q3 and Q4 (source terms);
      - companions cq3 (a companion under Lodestar's own account can read its secrets), cq4 (every option loosens §1.7.2's automatic installs on Windows), cq5 (revealing a generated credential), cq6 (whose third-party binaries run) and Q21 (FlareSolverr);
      - M8 OQ1 (a self-hosted runner on a public repository) and OQ6 (disclosure and the `SECURITY.md` contact);
      - M6 Q13 (the privileged MSI helper);
      - workers wkq1 (it widens §19.6's list and leaves sealed names of child-safety matches on a worker's disk) and wkq2 (accepting the join token from the environment);
      - v0.8 Q1 (Nostr and Lemmy terms) and Q4 (third-party adult claims).
    - **v0.7 Q10(d) isn't needed yet.** Steps (a)–(c) shrink the producers first, under §2.26. If they don't bring the torrent index under 512 B, step (d) trades one owner number for another (trigrams off by default against "Regex < 500 ms"), so it comes back to the owner by name, with the measured numbers.
    - **v0.8 Q3:** the runner drafts the per-worker consent wording. Approving the text stays with the owner, as for the Q1 consent texts, so `owner-v08-q3` stays unmet.
    - **Still the owner's own steps:** M6 Q6 (winget, after 1.0 if wanted), Q8 (the nightly signing key, owner-queue item 5) and Q14 (the G-m6 manual installs and the release approval, `owner-m6-manual`).

47. **The runner's recommendation on every question §2.46 left with the owner** (owner, 2026-11-06T05:23Z, relayed by the dashboard session): "Go with the runner's recommendation on all". The dashboard session showed each of the 16 questions with the plan's recommendation. The owner then confirmed it directly to the runner (runner chat, about 2026-11-06T05:26Z), with the security and content-safety loosenings named: "Yes, record all 16". The owner also confirmed the five largest loosenings by name, relayed by the dashboard session at about 05:27Z: "I confirm these: * The Stremio CORS rule and URL-path token. * The privileged MSI updater helper. * `LODESTAR_TOKEN` accepted from the environment on Docker and Unraid. * Sealed names kept on worker disks. * The companion running under Lodestar's own account." Each question is now decided as **the plan's recommendation in full, as its plan row gives it**: `docs/plan/2026-11-05-v07-m7.md` §6 Q1–Q4, `docs/plan/2026-11-06-v06-companions.md` §5 Q3–Q5, Q15 and Q21, `docs/plan/2026-11-06-v06-workers.md` §5 Q1–Q2, `docs/plan/2026-11-06-m8-hardening.md` §5 OQ1 and OQ6, `docs/plan/2026-11-06-m6-gap-audit.md` §5 Q13, and `docs/plan/2026-11-06-v08.md` §6 Q-o1 and Q-o4. **The plan row is the decision.** The bullets below are short summaries: they name the main safeguards, but not every one. Where a bullet and a plan row differ, or a bullet leaves something out, the plan row wins.
    - **v0.7:**
      - **Q1:** the Stremio addon ships, off by default.
        - A dedicated `stremio` key scope: read-only, shown once and revocable.
        - The token appears only in the addon's own paths (`/stremio/{token}/…`) and is redacted from logs.
        - CORS `*` on those paths only, never with credentials.
        - The user guide says HTTPS needs the user's own reverse proxy, and recommends LAN only.
        - Inducement guard: the addon's catalogues and help text never suggest infringing use, and every filter and pack applies.
      - **Q2:** private-tracker IRC announces ship off, with Account consent and `private_source`.
        - There's no shipped definition for a tracker whose published rules forbid announce bots.
        - The settings text and the user guide tell the user to follow their tracker's rules.
        - `.torrent` files are fetched only on explicit interest.
      - **Q3:** IRC-3 ships a network and channel only if its operators publish that read-only clients are fine, recorded in IRC-3's decisions file. Otherwise the user enters their own, and the text says to check the channel's rules.
      - **Q4:** yes for the four grey enrichment services, each slice recording its terms check.
        - AniDB: a registered client, at most 1 request every 2 s, interest-only.
        - srrDB and PreDB: at most 1 a second, user interest first.
        - Debrid: only for a provider that offers a cache check today and whose terms allow it, otherwise dropped from v0.7. Its hash lookups of verified public items are allowed (v0.7 Q5's exception).
    - **Companions:**
      - **cq3:** the same account, sandboxed. A plain warning shows where no sandbox applies. Never root or administrator.
      - **cq4:** a guided install for qBittorrent on Windows only, as the one exception to §1.7.2's fully automatic installs.
      - **cq5:** a credential can be revealed after the password is re-entered, as a logged event. It never appears in exports, backups or support bundles.
      - **cq6:** every binary pinned by SHA-256 or image digest, from these sources:
        - the userdocs static `qbittorrent-nox` builds on Linux;
        - Prowlarr from upstream;
        - linuxserver.io's qBittorrent and Prowlarr images over hotio's;
        - gluetun and FlareSolverr from their own projects.
      - **Q21:** FlareSolverr is opt-in only.
    - **Workers:**
      - **wkq1:** drop happens on the worker; the other actions are marked there and applied at commit. Sealed names may sit in the spool up to 1 GB or 24 h. This widens architecture §19.6's list.
      - **wkq2:** `LODESTAR_TOKEN` is accepted only for the first join, while no credentials exist.
        - Afterwards it's ignored, with a warning to remove it.
        - Its value is never logged.
        - The token is single-use, bound to a name, and expires in 15 minutes.
    - **M8:**
      - **OQ1:** the self-hosted bench runner, with mitigations (a)–(d).
      - **OQ6:** GitHub private vulnerability reporting. Unfixed blocking or important details are kept in draft security advisories the owner opens, and fix PRs describe the fix without a proof of concept until the release ships. **The contact** (owner, relayed about 05:28Z): "GitHub private vulnerability reporting only. This should have been activated." So `SECURITY.md` names no email address and points to the repository's "Report a vulnerability" button. The runner checked on 2026-11-06: the repository's private vulnerability reporting is on (`GET /repos/dvortsis/lodestar-v2/private-vulnerability-reporting` returns `enabled: true`).
    - **M6 Q13:** the privileged MSI helper with its six rules, so the PK plan's main branch stands.
    - **v0.8:**
      - **Q-o1:** both lists are the user's and start empty.
        - Nostr may suggest general-purpose relays, each only after a recorded check of its published policy.
        - Lemmy suggests nothing: the user enters their own instance and communities, and accepts that instance's terms.
        - Relays that need auth or payment show "blocked by source".
      - **Q-o4:** yes, narrowly (ADH-0). A third-party adult claim may only hide an item, pending local detection, and only while Show adult content is off.
        - It never feeds CS-5 or tombstones.
        - It never moves a title that detection reads as mainstream into Adult.
        - It never touches a torrent another source found.

    These conditions hold: `owner-v07-q1`–`q4`, `owner-cq3`, `owner-cq4`, `owner-cq5`, `owner-cq6`, `owner-cq8`, `owner-wkq1`, `owner-wkq2`, `owner-m8-q1`, `owner-m8-q6`, `owner-m6q13`, `owner-v08-q1` and `owner-v08-q4`. The owner's own steps M6 Q6, Q8 and Q14 are unchanged.

48. **Legal documents are kept current with each release, and content reports never go to the maintainers** (owner, 2026-11-06, legal-notice session). After reviewing the version-3 notice against `main` (private findings can become public on a public sighting; the backup leaves out `secrets.key`; the torrent cache and the Sonarr/Radarr webhooks are built, not planned; log and restart-witness locations; the peer table's 30-minute refresh; the notice and the operator guidance disagreed on filter availability): "Tell the runner to ensure the legal documents are up to date before publishing versions."
    - **The reporting text is the owner's** (2026-11-06): users are told not to send reports of suspected illegal content to Lodestar's maintainers, who develop the software and neither operate independently hosted instances, investigate their indexed material nor handle removal requests for them; removal requests go to the instance's operator, suspected child sexual exploitation to the NCMEC CyberTipline in the United States or the appropriate authority elsewhere, other suspected crimes to law enforcement; no suspected illegal files, screenshots, magnet links, hashes or identifying descriptions to the maintainers, and bug or security reports use harmless synthetic examples; these directions override no independently applicable legal obligation. Notice section 9 carries it.
    - **The notice names the software it applies to** separately from its own version (the line under the version heading), and "this version" means that software.
    - **The publisher's reading of AGPL §16 is removed** from notice section 8 (owner, 2026-11-06): the summary stays, and the licence text controls.
    - **Runner addition (§2.19, §2.26):** before any version is tagged, the Legal Notice and the operator guidance are checked against that build and share one release baseline; a substantive change raises `NOTICE_VERSION` (the substance test is X-legal-docs', `docs/decisions/2026-11-04-X-legal-docs.md`; §2.33 covers who makes the call). The runner adds the step to the release checklist (build-plan §0). v0.5.1: the owner chose (2026-11-06, directly to the runner): "No, do the text-only rc.5 (a)". Version 2 on `release/v0.5.1` is corrected in rc.5 before the `v0.5.1` tag (the data-folder sentence, "within half an hour", the §8 reading of AGPL §16, the older section 9; slice `R-rc5-notice`). `docs/decisions/2026-11-06-X-legal-docs-r2.md` specifies a `LEGAL-3` slice that checks the notice's factual claims against the code in CI.

49. **The SRC-4 consent texts are approved (Q1)** (owner, 2026-11-06T16:20Z, relayed by the dashboard session): "https://github.com/dvortsis/lodestar-v2/blob/main/docs/decisions/2026-11-02-SRC-4.md?plain=1#L79 looks good". The link is the "Draft consent texts" section. Approving them lifts a consent gate, so the runner confirmed it with the owner directly (runner chat, about 2026-11-06T16:35Z). The owner chose: "Approve, bound that sentence (Recommended)".
    - **What's approved:** all four drafts (External, Account, Restricted and Overlay) as written in `docs/decisions/2026-11-02-SRC-4.md`, unchanged since `f3222361`, with one bounded change to the Account text. **That section is the approved text,** with this change applied:
      - Before: "Results found through your account stay on this server: Lodestar never shares them through Torznab, Stremio, exports or public peer finding, because private trackers usually forbid it."
      - After: "Results found only through your account stay on this server: Lodestar never shares them through Torznab, Stremio, exports or public peer finding, because private trackers usually forbid it. If a public source reports the same torrent, it's treated as public."
      - The change matches the narrowing in Legal Notice version 3 (§2.48, #534): a private finding can become public on a public sighting.
    - **Who uses them:** SRC-15b shows these texts verbatim, with the Account change, and its PR checks its strings against this section. Where an approved text differs in substance from an earlier draft, `consent::text_version` rises for its class, as SRC-4 sets out.
    - **X-consent-gate-052 (#537) stays.** A source class is let through only once SRC-15b's dialog shows its approved text.

    This condition holds: `owner-q1`.

## 3. Points still open (not decided by the owner)
- **The libtorrent interop harness.** A commit adding it was rejected *(M0–M2, 10-20 23:41:23)*, possibly as an interruption rather than a refusal. It isn't built.
- **First-load budget.** The approved plan said JS < 200 KB gzipped; the later UI plan (§10.1) uses ≤ 100 KB total first load (JS ≤ 45 KB). The stricter §10.1 figures apply until the owner says otherwise.
- **Repository visibility.** The repo is public. The owner created it that way and hasn't commented since.
