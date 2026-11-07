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
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("reading {}: {e}", p.display()))
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
    assert_eq!(lodestar_dht::PEER_TTL, Duration::from_secs(minutes * 60));
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
        assert!(services.contains(needle), "{what} is no longer registered");
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
        Consent::External
    );
    let arr = lodestar_sources_plugins::arr::ArrWebhooks::new();
    assert_eq!(arr.manifest().consent, Consent::None);
    assert!(
        arr.manifest().endpoints.is_empty(),
        "the webhooks contact nothing"
    );
    assert!(
        read("crates/pipeline/src/dht.rs").contains("Consent::None"),
        "the DHT source's consent changed"
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
    require(
        &n,
        s2,
        "The torrent cache can't be turned on until you accept the websites it contacts",
        "torrent cache is Consent::External",
    );
    // No Account, Restricted or Overlay source is registered.
    for class in ["Account", "Restricted", "Overlay"] {
        require(&n, s2, &format!("- **{class}** sources"), "class list");
    }
    assert_eq!(
        n.matches("None is built yet.").count(),
        3,
        "Account, Restricted and Overlay are each \"None is built yet.\" in section 2"
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
    assert_eq!(SecretStore::KEY_FILE, "secrets.key");
    assert!(
        !files.contains(&SecretStore::KEY_FILE),
        "a backup now holds secrets.key; the notice says it doesn't"
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
        "It does not contain `secrets.key`",
        "SecretStore::KEY_FILE not in BACKUP_FILES",
    );
    require(
        &n,
        s5,
        "a key file (`secrets.key`) stored separately from the database",
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
    let names: Vec<&str> = [
        Channel::Webhook,
        Channel::Ntfy,
        Channel::Discord,
        Channel::Email,
        Channel::Rss,
    ]
    .into_iter()
    .filter_map(sent)
    .collect();
    assert_eq!(names, ["webhooks", "ntfy", "Discord", "email"]);
    require(
        &n,
        "5 (Your data)",
        "for notification channels you set up (ntfy, Discord, email, webhooks)",
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
    assert_ne!(flag(&store), 0, "stored by a private source");
    assert_eq!(put(&mut store, flags::PRIVATE_SOURCE), id);
    assert_ne!(flag(&store), 0, "a second private sighting keeps it");
    assert_eq!(put(&mut store, 0), id);
    assert_eq!(flag(&store), 0, "a public sighting clears it");

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
    // No code writes to the Windows Event Log, so the notice doesn't name it.
    forbid(
        &n,
        s5,
        "Event Log",
        "no code writes to the Windows Event Log",
    );
}
