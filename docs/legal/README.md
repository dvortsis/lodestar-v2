# Legal documents

- `legal-notice.md`: the Legal Notice shown before first use. `crates/api/src/legal/notice.md` is the embedded, published copy and must stay byte-identical.
- `operator-guidance.md`: guidance for people who make an instance reachable by others.
- `2026-11-04-liability-review.md`: the liability review behind them.

## The facts test

`crates/lodestar/tests/legal_facts.rs` checks facts the notice states against the code they describe. Each claim is matched by a short canonical phrase the notice must contain; the wording around it stays the writer's. The test never edits the notice.

### Checked phrases

| Section | Phrase the notice must contain | Checked against |
|---|---|---|
| (both copies) | the two files are byte-identical | `docs/legal/legal-notice.md` and `crates/api/src/legal/notice.md` |
| 2 | `an announcement expires 30 minutes after its most recent refresh` | `lodestar_dht::PEER_TTL` (the minute count is taken from it) |
| 2 | `**This version has:** the DHT crawler`, `the **torrent cache**`, `**Sonarr and Radarr webhooks**` | `Services::start` registers exactly three plugins: the DHT, the torrent cache, the webhooks |
| 2 | `one External source, the **torrent cache**`, `Built: the torrent cache.`, `after you accept the websites it contacts` | torrent cache manifest is `Consent::External` |
| 2 | `In this version the torrent cache can't be turned on.`; must **not** contain `turned on through the API` | `lodestar_pipeline::control::consent_text_approved` refuses `Consent::External` (the consent gate) |
| 2 | `The Sonarr and Radarr webhooks contact nothing and turn on with their switch` | webhooks manifest is `Consent::None` with no endpoints |
| 2 | `- **External** sources contact named public websites`, `- **Account** sources`, `- **Restricted** sources`, `- **Overlay** sources`, and `None is built yet.` three times | no registered plugin is Account, Restricted or Overlay |
| 2 | `marked private for as long as only private sources have reported it`; `If a public source such as the DHT later reports the same torrent, Lodestar treats it as public from then on` | store test: a private then a public sighting clears `flags::PRIVATE_SOURCE` |
| 5 | ``contains the database, the configuration file, the admin password hash with the sign-in sessions and API keys, and the key that unseals quarantined names (`seal.key`)``; ``It does not contain `secrets.key` `` | `BACKUP_FILES` in `crates/lodestar/src/cli/backup.rs`, and `services.rs` still opens `seal.key` in the data folder |
| 5 | ``a key file (`secrets.key`) stored separately from the database`` | `SecretStore::KEY_FILE` |
| 5 | `for notification channels you set up (ntfy, Discord, email, webhooks)` | `lodestar_integrations::notify::Channel` (RSS is served, not sent) |
| 5 | ``(by default the `logs` folder inside the data folder)`` | default `settings.logging.file_folder` in `settings/logging.rs` |
| 5 | `keeps its restart record in a folder outside the data folder` | `default_witness_dir` in `config.rs` doesn't use the data folder |
| 5 | `` `%SystemRoot%\Temp\lodestar-service.log` `` | `NAME` in `windows/temp_log.rs` |
| 5 | `in your data folder, with five exceptions:` | the count of section 5's list |
| 5 | `to the Windows Application event log, which every user of the computer can read`, `(version numbers, the step that stopped and folder paths only)` | the only Event Log writers (`eventcreate`, `ReportEvent`, `RegisterEventSource`, `EvtReport`, `Write-EventLog`, `New-EventLog`, .NET's `System.Diagnostics.EventLog` and `WriteEntry`, in `crates/*/src` and `packaging/**/*.ps1`, compared without case) are `update/windows/helper.rs` and `windows/service.rs`; the only `event_log(` callers are those two and `update/windows/rollback.rs`; no Event Log crate; `windows/service.rs` builds its message with `event_log_text`; `packaging/windows/main.wxs` registers both sources ("Lodestar", "Lodestar update helper") and removes them on uninstall |

`lodestar` is a binary crate, so facts in its own modules (`config.rs`, `services.rs`, `settings/logging.rs`, `windows/temp_log.rs`, `windows/service.rs`, `update/windows/helper.rs`, `cli/backup.rs`) are read from the source text. Moving or renaming one of those items fails the test with a message naming it; update the test to follow.

### When the test fails

1. Read the message: it names the notice section and the phrase.
2. Decide which side is wrong. If the code changed on purpose, the notice is out of date; if the notice was edited and lost the phrase, restore the fact.
3. A person updates the notice in both copies. The notice's wording is theirs; this test never rewrites it.
4. A person decides whether the change is substantive enough to raise `NOTICE_VERSION` (owner-decisions §2.33). Ask the runner.
5. Update the phrase here and in the test if the wording changed.

### Not checked yet
- Section 5's installer log (`%SystemRoot%\Temp\lodestar-secure-<random>`). The count "five exceptions" is required as a phrase, but not counted against the list.
- Section 2's "(an import doesn't clear the mark)".
- Phrases are searched in the whole notice, not only in the section named.
