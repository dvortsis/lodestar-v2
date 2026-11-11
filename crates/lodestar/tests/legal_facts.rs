//! The Legal Notice's facts, checked against the code they describe
//! (LEGAL-3a, `docs/decisions/2026-11-06-X-legal-docs-r2.md`).
//!
//! Each claim is one row of a table: the notice section it lives in, a short
//! canonical phrase the notice must contain (the wording around it stays the
//! writer's), and a check against the code. A failure names the section to
//! update. A person then decides whether the notice's version rises
//! (`docs/owner-decisions.md` §2.33); nothing edits the notice. See
//! `docs/legal/README.md`.
//!
//! `lodestar` is a binary crate, so facts that live in its own modules are
//! read from the source text; facts in library crates use the real values.

use std::path::{Path, PathBuf};
use std::time::Duration;

use lodestar_integrations::SecretStore;
use lodestar_integrations::notify::Channel;
use lodestar_metainfo::Info;
use lodestar_sources::Consent;
use lodestar_sources::SourcePlugin;
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig, flags};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(rel: &str) -> String {
    let p = root().join(rel);
    // Normalised: a Windows checkout may have CRLF line endings.
    std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("reading {}: {e}", p.display()))
        .replace("\r\n", "\n")
}

const NOTICE: &str = "docs/legal/legal-notice.md";
const EMBEDDED: &str = "crates/api/src/legal/notice.md";

/// Fails naming the notice section and the phrase that no longer matches.
fn require(notice: &str, section: &str, phrase: &str, fact: &str) {
    assert!(
        notice.contains(phrase),
        "Legal Notice section {section} must contain the phrase {phrase:?} ({fact}). The code \
         and the notice disagree: update that section (a person decides whether the notice \
         version rises, owner-decisions §2.33), then update tests/legal_facts.rs and \
         docs/legal/README.md."
    );
}

/// Fails when the notice mentions `phrase` although the code says it shouldn't.
fn forbid(notice: &str, section: &str, phrase: &str, why: &str) {
    assert!(
        !notice.contains(phrase),
        "Legal Notice section {section} must not mention {phrase:?}: {why}."
    );
}

/// The text between `start` and the next line that is just `}`.
fn item<'a>(src: &'a str, start: &str) -> &'a str {
    let at = src
        .find(start)
        .unwrap_or_else(|| panic!("{start:?} not found; update tests/legal_facts.rs"));
    let rest = &src[at..];
    &rest[..rest.find("\n}\n").unwrap_or(rest.len())]
}

#[test]
fn the_two_notice_copies_are_identical() {
    assert_eq!(
        std::fs::read(root().join(NOTICE)).unwrap(),
        std::fs::read(root().join(EMBEDDED)).unwrap(),
        "docs/legal/legal-notice.md and crates/api/src/legal/notice.md must be byte-identical"
    );
}

#[test]
fn peer_ttl_matches_the_thirty_minute_sentence() {
    let n = read(NOTICE);
    let minutes = lodestar_dht::PEER_TTL.as_secs() / 60;
    assert_eq!(
        lodestar_dht::PEER_TTL,
        Duration::from_secs(minutes * 60),
        "Legal Notice section 2: PEER_TTL is no longer a whole number of minutes"
    );
    require(
        &n,
        "2 (What Lodestar does on your network)",
        &format!("an announcement expires {minutes} minutes after its most recent refresh"),
        "lodestar_dht::PEER_TTL",
    );
}

#[test]
fn registered_sources_and_consent_classes_match_section_2() {
    let n = read(NOTICE);
    let s2 = "2 (What Lodestar does on your network)";
    // Services::start registers exactly these three plugins.
    let services = read("crates/lodestar/src/services.rs");
    assert_eq!(
        services.matches("plugins.push(").count(),
        3,
        "Services::start registers a different set of source plugins; update {s2}'s \
         \"This version has:\" inventory and the built/planned class list"
    );
    for (what, needle) in [
        ("DHT crawler", "plugins.push(dht)"),
        ("torrent cache", "plugins.push(cache.plugin.clone())"),
        ("Sonarr and Radarr webhooks", "plugins.push(arr)"),
    ] {
        assert!(
            services.contains(needle),
            "Legal Notice section 2: {what} is no longer registered"
        );
    }
    require(
        &n,
        s2,
        "**This version has:** the DHT crawler",
        "registered plugins",
    );
    require(&n, s2, "the **torrent cache**", "registered plugins");
    require(
        &n,
        s2,
        "**Sonarr and Radarr webhooks**",
        "registered plugins",
    );

    // The torrent cache is External; the webhooks need no consent and
    // contact nothing; the DHT needs none.
    assert_eq!(
        lodestar_sources_plugins::torrent_cache::MANIFEST.consent,
        Consent::External,
        "Legal Notice section 2: the torrent cache's consent class changed"
    );
    let arr = lodestar_sources_plugins::arr::ArrWebhooks::new();
    assert_eq!(
        arr.manifest().consent,
        Consent::None,
        "Legal Notice section 2: the webhooks' consent class changed"
    );
    assert!(
        arr.manifest().endpoints.is_empty(),
        "Legal Notice section 2: the webhooks now contact something"
    );
    assert!(
        read("crates/pipeline/src/dht.rs").contains("consent: Consent::None"),
        "Legal Notice section 2: the DHT source's consent changed"
    );
    require(
        &n,
        s2,
        "one External source, the **torrent cache**",
        "torrent cache is Consent::External",
    );
    require(
        &n,
        s2,
        "- **External** sources contact named public websites",
        "External class",
    );
    require(
        &n,
        s2,
        "Built: the torrent cache.",
        "only the torrent cache is External",
    );
    require(
        &n,
        s2,
        "The Sonarr and Radarr webhooks contact nothing and turn on with their switch",
        "webhooks are Consent::None with no endpoints",
    );
    // The torrent cache can't be turned on in this build: the consent gate
    // refuses every class but None (X-consent-gate-052, P-v052-cut). When
    // X-consent-gate-lift opens External, this fails until section 2 says
    // how it is turned on.
    assert!(
        !lodestar_pipeline::control::consent_text_approved(
            lodestar_sources_plugins::torrent_cache::MANIFEST.consent
        ),
        "Legal Notice section 2 says the torrent cache can't be turned on in this version, but \
         the consent gate now lets its class through"
    );
    require(
        &n,
        s2,
        "In this version the torrent cache can't be turned on.",
        "the consent gate refuses Consent::External",
    );
    require(
        &n,
        s2,
        "after you accept the websites it contacts",
        "torrent cache is Consent::External",
    );
    forbid(
        &n,
        s2,
        "turned on through the API",
        "the consent gate refuses to enable it through the API too",
    );
    // No Account, Restricted or Overlay source is registered.
    for class in ["Account", "Restricted", "Overlay"] {
        require(&n, s2, &format!("- **{class}** sources"), "class list");
    }
    assert_eq!(
        n.matches("None is built yet.").count(),
        3,
        "Legal Notice section 2: Account, Restricted and Overlay are each \"None is built yet.\" in section 2"
    );
}

#[test]
fn backup_files_match_section_5() {
    let n = read(NOTICE);
    let s5 = "5 (Your data)";
    let src = read("crates/lodestar/src/cli/backup.rs");
    let decl = src
        .split("pub const BACKUP_FILES")
        .nth(1)
        .and_then(|r| r.split_once('=')?.1.split(';').next())
        .expect("BACKUP_FILES constant");
    let files: Vec<&str> = decl
        .split('"')
        .enumerate()
        .filter_map(|(i, s)| (i % 2 == 1).then_some(s))
        .collect();
    assert_eq!(
        files,
        ["lodestar.db", "config.toml", "seal.key", "auth.json"],
        "the backup file list changed; update {s5}'s backup sentence"
    );
    assert_eq!(
        SecretStore::KEY_FILE,
        "secrets.key",
        "Legal Notice section 5: the credential key file was renamed"
    );
    // The seal key's real path is the one the server uses, not just the
    // backup list's name for it.
    assert!(
        read("crates/lodestar/src/services.rs").contains("data_dir.join(\"seal.key\")"),
        "Legal Notice section 5: the seal key file was renamed or moved"
    );
    assert!(
        !files.contains(&SecretStore::KEY_FILE),
        "Legal Notice section 5: a backup now holds secrets.key; the notice says it doesn't"
    );
    require(
        &n,
        s5,
        "contains the database, the configuration file, the admin password hash with the \
         sign-in sessions and API keys, and the key that unseals quarantined names (`seal.key`)",
        "BACKUP_FILES",
    );
    require(
        &n,
        s5,
        &format!("It does not contain `{}`", SecretStore::KEY_FILE),
        "SecretStore::KEY_FILE not in BACKUP_FILES",
    );
    require(
        &n,
        s5,
        &format!(
            "a key file (`{}`) stored separately from the database",
            SecretStore::KEY_FILE
        ),
        "SecretStore::KEY_FILE",
    );
}

#[test]
fn notification_channels_match_the_outbound_list() {
    let n = read(NOTICE);
    // Exhaustive: a new channel doesn't compile until it is named here.
    let sent = |c: Channel| -> Option<&'static str> {
        match c {
            Channel::Webhook => Some("webhooks"),
            Channel::Ntfy => Some("ntfy"),
            Channel::Discord => Some("Discord"),
            Channel::Email => Some("email"),
            // Served as a feed by Lodestar; nothing is sent out.
            Channel::Rss => None,
        }
    };
    let mods = read("crates/integrations/src/notify/mod.rs");
    let modules = mods.lines().filter(|l| l.starts_with("pub mod ")).count();
    assert_eq!(
        modules, 5,
        "a notify module was added or removed; update section 5's list"
    );
    // In the notice's order: the mapping above decides the phrase.
    let names: Vec<&str> = [
        Channel::Ntfy,
        Channel::Discord,
        Channel::Email,
        Channel::Webhook,
        Channel::Rss,
    ]
    .into_iter()
    .filter_map(sent)
    .collect();
    let phrase = format!(
        "for notification channels you set up ({})",
        names.join(", ")
    );
    require(
        &n,
        "5 (Your data)",
        &phrase,
        "integrations::notify::Channel",
    );
}

#[test]
fn private_flag_clears_on_a_public_sighting() {
    // The raw info dictionary: {length, name, piece length, pieces}.
    let mut raw = b"d6:lengthi1e4:name7:private12:piece lengthi16384e6:pieces20:".to_vec();
    raw.extend_from_slice(&[0; 20]);
    raw.push(b'e');
    let info = Info::parse(&raw).unwrap();
    let source = SourceId::new(0).unwrap();
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let put = |store: &mut Store, extra_flags: i64| {
        store
            .insert(&[NewTorrent {
                info: &info,
                raw_info: Some(&raw),
                discovered_at: 1_700_000_000,
                metadata_at: 1_700_000_000,
                extra_flags,
                source,
            }])
            .unwrap()[0]
            .id()
    };
    let id = put(&mut store, flags::PRIVATE_SOURCE);
    let flag = |store: &Store| store.torrent(id).unwrap().unwrap().flags & flags::PRIVATE_SOURCE;
    assert_ne!(
        flag(&store),
        0,
        "Legal Notice section 2 (private sentence): a private source's find must be flagged"
    );
    assert_eq!(put(&mut store, flags::PRIVATE_SOURCE), id);
    assert_ne!(
        flag(&store),
        0,
        "Legal Notice section 2 (private sentence): a second private sighting must keep the flag"
    );
    assert_eq!(put(&mut store, 0), id);
    assert_eq!(
        flag(&store),
        0,
        "Legal Notice section 2 (private sentence): a public sighting must clear the flag"
    );

    let n = read(NOTICE);
    let s2 = "2 (What Lodestar does on your network)";
    require(
        &n,
        s2,
        "marked private for as long as only private sources have reported it",
        "a private then a public sighting clears PRIVATE_SOURCE",
    );
    require(
        &n,
        s2,
        "If a public source such as the DHT later reports the same torrent, Lodestar treats it \
         as public from then on",
        "a public sighting clears PRIVATE_SOURCE",
    );
}

#[test]
fn data_folder_exceptions_match_section_5() {
    let n = read(NOTICE);
    let s5 = "5 (Your data)";
    // Log folder: `<data>/logs` unless changed.
    let logging = read("crates/lodestar/src/settings/logging.rs");
    assert!(
        logging.contains("config.data_dir.join(\"logs\")"),
        "the default log folder changed; update {s5}'s data-folder sentence"
    );
    require(
        &n,
        s5,
        "(by default the `logs` folder inside the data folder)",
        "settings.logging.file_folder default",
    );
    // The safe-restart witness lives outside the data folder by design.
    let cfg = read("crates/lodestar/src/config.rs");
    let witness = item(&cfg, "pub fn default_witness_dir");
    assert!(
        !witness.contains("data_dir"),
        "the restart witness default now depends on the data folder; update {s5}"
    );
    require(
        &n,
        s5,
        "keeps its restart record in a folder outside the data folder",
        "default_witness_dir",
    );
    // The Windows service's temp log.
    let temp_log = read("crates/lodestar/src/windows/temp_log.rs");
    assert!(
        temp_log.contains("pub const NAME: &str = \"lodestar-service.log\";"),
        "the Windows service log name changed; update {s5}"
    );
    require(
        &n,
        s5,
        "`%SystemRoot%\\Temp\\lodestar-service.log`",
        "windows/temp_log.rs NAME",
    );
    // The count of the list above, now five with the Event Log.
    require(
        &n,
        s5,
        "in your data folder, with five exceptions:",
        "section 5's list",
    );
}

/// The source files under `dir` (recursively) with the extension `ext`, as
/// paths relative to the repository root with `/` separators.
fn files(dir: &str, ext: &str) -> Vec<String> {
    fn walk(at: &Path, ext: &str, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(at) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, ext, out);
            } else if p.extension().is_some_and(|x| x == ext) {
                out.push(p);
            }
        }
    }
    let base = root();
    let mut out = Vec::new();
    walk(&base.join(dir), ext, &mut out);
    let mut rel: Vec<String> = out
        .iter()
        .map(|p| {
            let r = p.strip_prefix(&base).unwrap_or(p);
            r.to_string_lossy().replace('\\', "/")
        })
        .collect();
    rel.sort();
    rel
}

/// Whether `src` calls or defines a function named exactly `name`: `name(`
/// not preceded by an identifier character (so `event_log_text(` and
/// `my_event_log(` don't count).
fn calls(src: &str, name: &str) -> bool {
    let pat = format!("{name}(");
    src.match_indices(&pat).any(|(at, _)| {
        !src[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
    })
}

/// Section 5's fifth exception (owner §2.55): the notice names the Windows
/// Application event log, and only the modules it covers write there: the
/// update helper (an update that couldn't be installed or was undone, also
/// from its rollback) and the service (a refused start). A new writer fails
/// here, naming section 5, until a person updates the notice.
#[test]
fn event_log_writers_match_section_5() {
    let n = read(NOTICE);
    let s5 = "5 (Your data)";
    require(
        &n,
        s5,
        "to the Windows Application event log, which every user of the computer can read",
        "the update helper and the service write there",
    );
    require(
        &n,
        s5,
        "(version numbers, the step that stopped and folder paths only)",
        "what the helper and the service write there",
    );

    // What writes: `eventcreate`, the Win32 calls, or PowerShell's cmdlets,
    // in every crate's source and the Windows packaging scripts.
    const WRITERS: [&str; 6] = [
        "eventcreate",
        "ReportEvent",
        "RegisterEventSource",
        "EvtReport",
        "Write-EventLog",
        "New-EventLog",
    ];
    let mut sources = files("crates", "rs");
    sources.retain(|f| f.contains("/src/"));
    sources.extend(files("packaging", "ps1"));
    let writing: Vec<&String> = sources
        .iter()
        .filter(|f| {
            let src = read(f);
            WRITERS.iter().any(|w| src.contains(w))
        })
        .collect();
    assert_eq!(
        writing,
        [
            "crates/lodestar/src/update/windows/helper.rs",
            "crates/lodestar/src/windows/service.rs",
        ],
        "these files write to the Windows Event Log ({WRITERS:?}). Legal Notice section {s5} \
         names only the update helper and the service's start refusal there: update that \
         section (a person decides whether the notice version rises, owner-decisions §2.33), \
         then tests/legal_facts.rs and docs/legal/README.md"
    );
    // No Event Log crate either.
    for manifest in files("crates", "toml") {
        let toml = read(&manifest);
        for dep in ["eventlog", "winlog", "win_etw", "tracing-etw"] {
            assert!(
                !toml.contains(dep),
                "{manifest} names {dep}: an Event Log writer Legal Notice section {s5} doesn't \
                 cover"
            );
        }
    }

    // Who calls the two writers (`System::event_log` in the helper, the
    // service's own `event_log`).
    let callers: Vec<&String> = sources
        .iter()
        .filter(|f| f.starts_with("crates/lodestar/src/") && calls(&read(f), "event_log"))
        .collect();
    assert_eq!(
        callers,
        [
            "crates/lodestar/src/update/windows/helper.rs",
            "crates/lodestar/src/update/windows/rollback.rs",
            "crates/lodestar/src/windows/service.rs",
        ],
        "these files write to the Windows Event Log through event_log(). Legal Notice section \
         {s5} names only an update that couldn't be installed or was undone, and the service's \
         refused start: update that section, then tests/legal_facts.rs and docs/legal/README.md"
    );

    // The service's writes are the refusals alone, with the folder and
    // the fix (`event_log_text`, unit-tested in windows/service.rs).
    let service = read("crates/lodestar/src/windows/service.rs");
    assert!(
        service.contains("pub fn event_log_text(refusal: Refusal, dir: &Path) -> String"),
        "the service's Event Log message builder changed; check it against section {s5}"
    );
    // The sources the MSI registers are the ones written.
    assert!(service.contains("pub const EVENT_SOURCE: &str = \"Lodestar\";"));
    let helper = read("crates/lodestar/src/update/windows/helper.rs");
    assert!(helper.contains("\"Lodestar update helper\","));
    let wxs = read("packaging/windows/main.wxs");
    for source in ["Lodestar", "Lodestar update helper"] {
        assert!(
            wxs.contains(&format!(
                "Key=\"SYSTEM\\CurrentControlSet\\Services\\EventLog\\Application\\{source}\" \
                 ForceDeleteOnUninstall=\"yes\""
            )),
            "main.wxs must register the {source:?} Event Log source and remove it on uninstall"
        );
    }
}
