//! The registry: layering and sources, environment read-only, saved
//! values across a restart, patch validation and atomicity, live
//! propagation, restart flags, and that filtering state is out of reach.

use std::path::Path;
use std::sync::{Arc, Mutex};

use lodestar_api::settings::{SettingSource, SettingsApi, SettingsError, SettingsPatch};
use lodestar_store::{Store, StoreConfig};
use serde_json::{Value, json};

use super::crawler::{Crawler, Intensity};
use super::general::General;
use super::*;
use crate::config::Config;

struct Env(Vec<(&'static str, &'static str)>);

impl Env {
    fn get(&self, k: &str) -> Option<String> {
        self.0
            .iter()
            .find(|(n, _)| *n == k)
            .map(|(_, v)| v.to_string())
    }
}

pub(super) fn config(
    dir: &Path,
    file: Option<&str>,
    env: &[(&'static str, &'static str)],
) -> Config {
    let env = Env(env.to_vec());
    let path = file.map(|text| {
        let p = dir.join("config.toml");
        std::fs::write(&p, text).unwrap();
        p
    });
    Config::load(path.as_deref(), |k| env.get(k)).unwrap()
}

pub(super) fn open(dir: &Path) -> Arc<Mutex<Store>> {
    Arc::new(Mutex::new(
        Store::open(dir.join("t.db"), StoreConfig::default()).unwrap(),
    ))
}

pub(super) fn registry(dir: &Path, config: &Config) -> Registry {
    Registry::load(config, open(dir)).unwrap()
}

pub(super) fn patch(v: Value) -> SettingsPatch {
    match v {
        Value::Object(m) => SettingsPatch(m),
        _ => panic!("a patch is an object"),
    }
}

fn field(r: &Registry, section: &str, name: &str) -> (Value, SettingSource, bool) {
    let s = r.section(section).unwrap();
    let f = &s.fields[name];
    (f.value.clone(), f.source, f.read_only)
}

#[test]
fn layers_resolve_in_order_with_their_source() {
    // Every combination of file / saved / environment for one field.
    for file in [false, true] {
        for saved in [false, true] {
            for env in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let text = file.then_some("[dht]\nintensity = \"low\"\n");
                let vars: &[(&str, &str)] = if env {
                    &[("LODESTAR_INTENSITY", "aggressive")]
                } else {
                    &[]
                };
                if saved {
                    let s = open(dir.path());
                    s.lock()
                        .unwrap()
                        .set_settings_text(&[("settings.crawler.intensity", "\"balanced\"")])
                        .unwrap();
                    // Default is balanced too: use a value that's distinct
                    // from every other layer instead.
                    s.lock()
                        .unwrap()
                        .set_settings_text(&[("settings.crawler.timeout_secs", "33")])
                        .unwrap();
                }
                let c = config(dir.path(), text, vars);
                let r = registry(dir.path(), &c);
                let (value, source, read_only) = field(&r, "crawler", "intensity");
                let (want_value, want_source) = if env {
                    ("aggressive", SettingSource::Env)
                } else if saved {
                    ("balanced", SettingSource::Db)
                } else if file {
                    ("low", SettingSource::File)
                } else {
                    ("balanced", SettingSource::Default)
                };
                let case = format!("file={file} saved={saved} env={env}");
                assert_eq!(value, json!(want_value), "{case}");
                assert_eq!(source, want_source, "{case}");
                assert_eq!(read_only, env, "{case}");
                let env_name = r.section("crawler").unwrap().fields["intensity"]
                    .env
                    .clone();
                assert_eq!(
                    env_name.as_deref(),
                    env.then_some("LODESTAR_INTENSITY"),
                    "{case}"
                );
                // The effective config the services start with agrees.
                let eff = r.effective_config(&c);
                assert_eq!(
                    eff.dht.intensity,
                    crate::config::IntensityName::from(r.get::<Crawler>().intensity),
                    "{case}"
                );
                if saved {
                    assert_eq!(eff.fetch.timeout_secs, 33, "{case}");
                }
            }
        }
    }
}

#[test]
fn every_section_reports_every_field() {
    let dir = tempfile::tempdir().unwrap();
    let c = config(dir.path(), None, &[]);
    let r = registry(dir.path(), &c);
    let index = r.index();
    let names: Vec<&str> = index.sections.iter().map(|s| s.section.as_str()).collect();
    assert_eq!(
        names,
        [
            "general",
            "network",
            "crawler",
            "transport",
            "storage",
            "search",
            "content",
            "preferences"
        ]
    );
    for s in &index.sections {
        let values = r.section(&s.section).unwrap();
        let fields: Vec<&str> = s.fields.iter().map(|f| f.name.as_str()).collect();
        let mut sorted = fields.clone();
        sorted.sort_unstable();
        assert_eq!(
            values.fields.keys().map(String::as_str).collect::<Vec<_>>(),
            sorted,
            "{}",
            s.section
        );
        // The schema has exactly these properties, inline (a form can't
        // follow `$ref`s), and refuses others.
        let props = s.schema["properties"].as_object().unwrap();
        let mut keys: Vec<&str> = props.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, sorted, "{}", s.section);
        assert!(!s.schema.to_string().contains("$ref"), "{}", s.section);
        assert_eq!(
            s.schema["additionalProperties"],
            json!(false),
            "{}",
            s.section
        );
        assert!(values.restart_required.is_empty());
        for f in &s.fields {
            assert_eq!(
                f.label_key,
                format!("settings.{}.{}.label", s.section, f.name)
            );
            assert_eq!(
                f.help_key,
                format!("settings.{}.{}.help", s.section, f.name)
            );
            assert_eq!(values.fields[&f.name].source, SettingSource::Default);
        }
    }
    let crawler = index
        .sections
        .iter()
        .find(|s| s.section == "crawler")
        .unwrap();
    assert_eq!(
        crawler.schema["properties"]["timeout_secs"]["minimum"],
        json!(1)
    );
    assert_eq!(
        crawler.schema["properties"]["timeout_secs"]["maximum"],
        json!(120)
    );
    assert_eq!(
        crawler.schema["properties"]["intensity"]["enum"],
        json!(["low", "balanced", "aggressive"])
    );
}

#[test]
fn the_file_and_defaults_are_told_apart() {
    let dir = tempfile::tempdir().unwrap();
    let c = config(
        dir.path(),
        Some(
            "log = \"debug\"\n[http]\nbind = \"127.0.0.1:9000\"\n[dht]\nbind = [\"0.0.0.0:7000\"]\n",
        ),
        &[],
    );
    let r = registry(dir.path(), &c);
    assert_eq!(
        field(&r, "general", "log_level"),
        (json!("debug"), SettingSource::File, false)
    );
    assert_eq!(
        field(&r, "network", "http_bind"),
        (json!("127.0.0.1:9000"), SettingSource::File, false)
    );
    assert_eq!(
        field(&r, "network", "dht_port"),
        (json!(7000), SettingSource::File, false)
    );
    assert_eq!(
        field(&r, "network", "secure_cookies"),
        (json!(false), SettingSource::Default, true)
    );
    // The data directory is never editable here, whatever its source.
    assert_eq!(
        field(&r, "general", "data_dir"),
        (json!("data"), SettingSource::Default, true)
    );
}

#[test]
fn environment_fields_are_read_only_and_name_the_variable() {
    let dir = tempfile::tempdir().unwrap();
    let c = config(
        dir.path(),
        None,
        &[("LODESTAR_DHT_PORT", "7777"), ("LODESTAR_LOG", "warn")],
    );
    let r = registry(dir.path(), &c);
    assert_eq!(
        field(&r, "network", "dht_port"),
        (json!(7777), SettingSource::Env, true)
    );
    let err = r
        .patch("network", patch(json!({ "dht_port": 7000 })))
        .unwrap_err();
    assert_eq!(
        err,
        SettingsError::SetByEnv {
            field: "dht_port".into(),
            var: "LODESTAR_DHT_PORT".into()
        }
    );
    // A refused field refuses the whole patch: nothing saved.
    let err = r
        .patch("general", patch(json!({ "log_level": "debug" })))
        .unwrap_err();
    assert!(matches!(err, SettingsError::SetByEnv { ref var, .. } if var == "LODESTAR_LOG"));
    assert!(
        r.store
            .lock()
            .unwrap()
            .settings_with_prefix(KEY_PREFIX)
            .unwrap()
            .is_empty()
    );
    // Other fields of the section stay editable.
    r.patch("network", patch(json!({ "dht_enabled": false })))
        .unwrap();
}

#[test]
fn a_saved_value_never_overrides_the_environment() {
    let dir = tempfile::tempdir().unwrap();
    open(dir.path())
        .lock()
        .unwrap()
        .set_settings_text(&[("settings.crawler.intensity", "\"low\"")])
        .unwrap();
    let c = config(dir.path(), None, &[("LODESTAR_INTENSITY", "aggressive")]);
    let r = registry(dir.path(), &c);
    assert_eq!(r.get::<Crawler>().intensity, Intensity::Aggressive);
    assert_eq!(
        r.effective_config(&c).dht.intensity,
        crate::config::IntensityName::Aggressive
    );
    assert_eq!(
        field(&r, "crawler", "intensity"),
        (json!("aggressive"), SettingSource::Env, true)
    );
}

#[test]
fn saved_values_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let c = config(dir.path(), None, &[]);
    {
        let r = registry(dir.path(), &c);
        let out = r
            .patch(
                "crawler",
                patch(json!({ "intensity": "low", "timeout_secs": 20, "max_sockets": 0 })),
            )
            .unwrap();
        assert_eq!(out.fields["intensity"].source, SettingSource::Db);
        r.patch("transport", patch(json!({ "utp": true }))).unwrap();
    }
    // A new process: the store reopened, the same file and environment.
    let r = registry(dir.path(), &c);
    let crawler = r.get::<Crawler>();
    assert_eq!(crawler.intensity, Intensity::Low);
    assert_eq!(crawler.timeout_secs, 20);
    assert_eq!(crawler.max_sockets, 0);
    assert_eq!(field(&r, "crawler", "max_sockets").1, SettingSource::Db);
    assert_eq!(
        field(&r, "transport", "utp"),
        (json!(true), SettingSource::Db, false)
    );
    // Restart-only changes have now taken effect: none pending.
    assert!(r.section("crawler").unwrap().restart_required.is_empty());
    let eff = r.effective_config(&c);
    assert_eq!(eff.fetch.timeout_secs, 20);
    assert_eq!(eff.pipeline_config().unwrap().pool.max_sockets, 100);
}

#[test]
fn bad_saved_values_are_skipped() {
    let dir = tempfile::tempdir().unwrap();
    open(dir.path())
        .lock()
        .unwrap()
        .set_settings_text(&[
            ("settings.crawler.intensity", "\"turbo\""),
            ("settings.crawler.timeout_secs", "9999"),
            ("settings.crawler.gone", "1"),
            ("settings.crawler.max_metadata_mib", "not json"),
            ("settings.crawler.encryption", "\"require\""),
            ("settings.general.data_dir", "\"/elsewhere\""),
        ])
        .unwrap();
    let c = config(dir.path(), None, &[]);
    let r = registry(dir.path(), &c);
    let crawler = r.get::<Crawler>();
    assert_eq!(
        crawler,
        Crawler {
            encryption: super::crawler::Encryption::Require,
            ..Crawler::default()
        }
    );
    // A saved data_dir is never honoured.
    assert_eq!(r.get::<General>().data_dir, "data");
    assert_eq!(
        r.effective_config(&c).data_dir,
        std::path::PathBuf::from("data")
    );
}

#[test]
fn patches_are_validated_per_field() {
    let dir = tempfile::tempdir().unwrap();
    let c = config(dir.path(), None, &[]);
    let r = registry(dir.path(), &c);
    let invalid = |section: &str, body: Value, at: &str| match r.patch(section, patch(body.clone()))
    {
        Err(SettingsError::Invalid { field, message }) => {
            assert_eq!(field, at, "{body}");
            assert!(!message.is_empty());
        }
        other => panic!("{body}: {other:?}"),
    };
    // Bounds.
    invalid("crawler", json!({ "timeout_secs": 0 }), "timeout_secs");
    invalid("crawler", json!({ "timeout_secs": 121 }), "timeout_secs");
    invalid("crawler", json!({ "max_sockets": 4097 }), "max_sockets");
    invalid("network", json!({ "dht_port": 0 }), "dht_port");
    invalid("search", json!({ "heap_mib": 8 }), "heap_mib");
    invalid("storage", json!({ "mmap_mib": -1 }), "mmap_mib");
    // Types and enums.
    invalid("crawler", json!({ "timeout_secs": "10" }), "timeout_secs");
    invalid("crawler", json!({ "timeout_secs": 1.5 }), "timeout_secs");
    invalid("crawler", json!({ "intensity": "turbo" }), "intensity");
    invalid("crawler", json!({ "encryption": 1 }), "encryption");
    invalid("network", json!({ "dht_enabled": "yes" }), "dht_enabled");
    invalid("network", json!({ "dht_port": 70000 }), "dht_port");
    // Socket addresses.
    invalid(
        "network",
        json!({ "http_bind": "localhost:80" }),
        "http_bind",
    );
    invalid("network", json!({ "http_bind": "0.0.0.0" }), "http_bind");
    invalid("network", json!({ "http_bind": 8090 }), "http_bind");
    invalid("network", json!({ "http_bind": "0.0.0.0:0" }), "http_bind");
    // Log filters.
    invalid("general", json!({ "log_level": "info,=[" }), "log_level");
    invalid(
        "general",
        json!({ "log_level": "x".repeat(2000) }),
        "log_level",
    );
    // The field in error is the one named, even with valid ones beside it.
    invalid(
        "crawler",
        json!({ "intensity": "low", "timeout_secs": 500 }),
        "timeout_secs",
    );
    // Unknown fields and sections; read-only fields.
    assert_eq!(
        r.patch("crawler", patch(json!({ "intensty": "low" }))),
        Err(SettingsError::UnknownField("intensty".into()))
    );
    assert_eq!(
        r.patch("nope", patch(json!({}))),
        Err(SettingsError::UnknownSection)
    );
    assert_eq!(
        r.patch("general", patch(json!({ "data_dir": "/tmp/x" }))),
        Err(SettingsError::ReadOnly("data_dir".into()))
    );
    // Nothing refused was saved or published.
    assert!(
        r.store
            .lock()
            .unwrap()
            .settings_with_prefix(KEY_PREFIX)
            .unwrap()
            .is_empty()
    );
    assert_eq!(r.get::<Crawler>(), Crawler::default());
    // Valid edge values pass, and addresses are saved normalized.
    let out = r
        .patch(
            "network",
            patch(json!({ "http_bind": "[::1]:65535", "dht_port": 65535 })),
        )
        .unwrap();
    assert_eq!(out.fields["http_bind"].value, json!("[::1]:65535"));
    r.patch(
        "crawler",
        patch(json!({ "timeout_secs": 1, "max_metadata_mib": 64 })),
    )
    .unwrap();
    r.patch(
        "general",
        patch(json!({ "log_level": "info,lodestar_dht=debug" })),
    )
    .unwrap();
}

#[test]
fn a_partial_patch_leaves_other_fields_alone() {
    let dir = tempfile::tempdir().unwrap();
    let c = config(dir.path(), Some("[fetch]\ntimeout_secs = 30\n"), &[]);
    let r = registry(dir.path(), &c);
    let before = r.section("crawler").unwrap();
    let after = r
        .patch("crawler", patch(json!({ "intensity": "low" })))
        .unwrap();
    for (name, f) in &after.fields {
        if name != "intensity" {
            assert_eq!(f, &before.fields[name], "{name}");
        }
    }
    assert_eq!(after.fields["timeout_secs"].source, SettingSource::File);
    // Only the patched key was written.
    assert_eq!(
        r.store
            .lock()
            .unwrap()
            .settings_with_prefix("settings.crawler.")
            .unwrap(),
        vec![(
            "settings.crawler.intensity".to_string(),
            "\"low\"".to_string()
        )]
    );
    // Other sections are untouched too.
    assert_eq!(r.get::<General>(), General::default());
}

#[test]
fn a_failed_save_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let c = config(dir.path(), None, &[]);
    let r = registry(dir.path(), &c);
    let mut rx = r.subscribe::<Crawler>();
    rx.borrow_and_update();
    // A second connection holds the write lock, so the save times out
    // (after the store's busy timeout).
    let blocker = rusqlite_connection(dir.path());
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    let err = r
        .patch(
            "crawler",
            patch(json!({ "intensity": "low", "timeout_secs": 20 })),
        )
        .unwrap_err();
    assert!(matches!(err, SettingsError::Store(_)), "{err:?}");
    blocker.execute_batch("ROLLBACK").unwrap();
    assert!(!rx.has_changed().unwrap());
    assert_eq!(r.get::<Crawler>(), Crawler::default());
    assert_eq!(field(&r, "crawler", "intensity").1, SettingSource::Default);
    assert!(
        r.store
            .lock()
            .unwrap()
            .settings_with_prefix(KEY_PREFIX)
            .unwrap()
            .is_empty()
    );
}

fn rusqlite_connection(dir: &Path) -> lodestar_store::rusqlite::Connection {
    lodestar_store::rusqlite::Connection::open(dir.join("t.db")).unwrap()
}

#[test]
fn restart_only_changes_are_listed_until_restart() {
    let dir = tempfile::tempdir().unwrap();
    let c = config(dir.path(), None, &[]);
    let r = registry(dir.path(), &c);
    let out = r
        .patch(
            "network",
            patch(json!({ "http_bind": "127.0.0.1:9999", "dht_enabled": true })),
        )
        .unwrap();
    // `dht_enabled` didn't change, so it doesn't wait for anything.
    assert_eq!(out.restart_required, ["http_bind"]);
    assert!(out.fields["http_bind"].restart_required);
    // Setting it back clears it.
    let out = r
        .patch("network", patch(json!({ "http_bind": "0.0.0.0:8090" })))
        .unwrap();
    assert!(out.restart_required.is_empty());
    // Live fields are never listed.
    let out = r
        .patch("crawler", patch(json!({ "intensity": "low" })))
        .unwrap();
    assert!(out.restart_required.is_empty());
    assert!(!out.fields["intensity"].restart_required);
    let out = r
        .patch("general", patch(json!({ "log_level": "debug" })))
        .unwrap();
    assert!(out.restart_required.is_empty());
}

#[tokio::test]
async fn changes_reach_subscribers() {
    let dir = tempfile::tempdir().unwrap();
    let c = config(dir.path(), None, &[]);
    let r = registry(dir.path(), &c);
    let mut crawler = r.subscribe::<Crawler>();
    let mut general = r.subscribe::<General>();
    r.patch("crawler", patch(json!({ "intensity": "aggressive" })))
        .unwrap();
    crawler.changed().await.unwrap();
    assert_eq!(crawler.borrow_and_update().intensity, Intensity::Aggressive);
    assert!(!general.has_changed().unwrap());
    // Saving the same value again wakes nobody.
    r.patch("crawler", patch(json!({ "intensity": "aggressive" })))
        .unwrap();
    assert!(!crawler.has_changed().unwrap());
    r.patch("general", patch(json!({ "log_level": "warn" })))
        .unwrap();
    general.changed().await.unwrap();
    assert_eq!(general.borrow_and_update().log_level, "warn");
}

#[tokio::test]
async fn the_log_filter_follows_live() {
    use tracing_subscriber::{EnvFilter, Registry as Subscriber, reload};
    let dir = tempfile::tempdir().unwrap();
    let c = config(dir.path(), Some("log = \"info\"\n"), &[]);
    let r = registry(dir.path(), &c);
    let (_layer, handle) = reload::Layer::<EnvFilter, Subscriber>::new(EnvFilter::new("error"));
    let (seen_tx, mut seen) = tokio::sync::mpsc::unbounded_channel();
    let h = handle.clone();
    let task = log::follow_with(r.subscribe(), move |filter| {
        let result = log::reload(&h, filter);
        let _ = seen_tx.send(filter.to_string());
        result
    });
    // It applies the effective value at once (main started with the file's).
    assert_eq!(seen.recv().await.unwrap(), "info");
    r.patch("general", patch(json!({ "log_level": "lodestar=trace" })))
        .unwrap();
    assert_eq!(seen.recv().await.unwrap(), "lodestar=trace");
    let current = handle.with_current(|f| f.to_string()).unwrap();
    assert_eq!(current, "lodestar=trace");
    task.abort();
}

#[tokio::test]
async fn the_intensity_reaches_the_running_dht() {
    use lodestar_dht::node::{Config as DhtConfig, Intensity as DhtIntensity, Node};
    let dir = tempfile::tempdir().unwrap();
    let c = config(dir.path(), None, &[]);
    let r = registry(dir.path(), &c);
    let slot = Arc::new(Mutex::new(None));
    let task = crawler::follow_intensity(r.subscribe(), slot.clone());
    // A change before the node starts waits for it.
    r.patch("crawler", patch(json!({ "intensity": "low" })))
        .unwrap();
    let node = Node::start(
        DhtConfig {
            bind: vec![std::net::SocketAddr::from(([127, 0, 0, 1], 0))],
            bootstrap: Vec::new(),
            state_path: None,
            ..DhtConfig::default()
        },
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        node.snapshot().await.unwrap().intensity,
        DhtIntensity::Balanced
    );
    *slot.lock().unwrap() = Some(node.handle());
    let until = |want: DhtIntensity| {
        let node = &node;
        async move {
            for _ in 0..100 {
                if node.snapshot().await.unwrap().intensity == want {
                    return true;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            false
        }
    };
    assert!(until(DhtIntensity::Low).await);
    r.patch("crawler", patch(json!({ "intensity": "aggressive" })))
        .unwrap();
    assert!(until(DhtIntensity::Aggressive).await);
    // A change to another crawler field leaves the node alone.
    r.patch("crawler", patch(json!({ "timeout_secs": 30 })))
        .unwrap();
    assert!(until(DhtIntensity::Aggressive).await);
    task.abort();
    node.shutdown().await.unwrap();
}

#[test]
fn filtering_state_is_out_of_reach() {
    let dir = tempfile::tempdir().unwrap();
    // Filtering and floor state living in the same table.
    let s = open(dir.path());
    s.lock()
        .unwrap()
        .set_setting_i64("first_run_at", 1)
        .unwrap();
    s.lock()
        .unwrap()
        .set_settings_text(&[
            ("floor.file:/etc/list.txt", "x"),
            ("floor.pending:file:/etc/list.txt", "x"),
            ("packs", "{}"),
            ("subscriptions", "[]"),
        ])
        .unwrap();
    drop(s);
    let c = config(dir.path(), None, &[]);
    let r = registry(dir.path(), &c);
    for section in [
        "filters",
        "filter",
        "floor",
        "packs",
        "rules",
        "subscriptions",
        "blocklist",
        "safety",
        "secrets",
        "integrations",
    ] {
        assert!(r.section(section).is_none(), "{section}");
        assert_eq!(
            r.patch(section, patch(json!({ "enabled": false }))),
            Err(SettingsError::UnknownSection),
            "{section}"
        );
    }
    let words = [
        "floor",
        "pack",
        "subscription",
        "blocklist",
        "filter",
        "rule",
        "secret",
        "password",
        "token",
    ];
    for s in r.index().sections {
        for f in &s.fields {
            for w in words {
                assert!(!f.name.contains(w), "{}.{}", s.section, f.name);
            }
        }
        for w in [
            "floor",
            "packs",
            "subscriptions",
            "blocklist",
            "catalog",
            "lists",
        ] {
            assert_eq!(
                r.patch(&s.section, patch(json!({ w: [] }))),
                Err(SettingsError::UnknownField(w.to_string())),
                "{}.{w}",
                s.section
            );
        }
    }
    // Every write stays under the registry's prefix; the rest is as it was.
    r.patch("crawler", patch(json!({ "intensity": "low" })))
        .unwrap();
    let store = r.store.lock().unwrap();
    assert_eq!(
        store
            .setting_text("floor.file:/etc/list.txt")
            .unwrap()
            .as_deref(),
        Some("x")
    );
    assert_eq!(store.setting_text("packs").unwrap().as_deref(), Some("{}"));
    assert_eq!(store.setting_i64("first_run_at").unwrap(), Some(1));
    // The value and the section's revision.
    assert_eq!(
        store.settings_with_prefix(KEY_PREFIX).unwrap(),
        [
            ("settings.crawler#revision".to_string(), "1".to_string()),
            (
                "settings.crawler.intensity".to_string(),
                "\"low\"".to_string()
            )
        ]
    );
}

#[test]
fn the_default_sections_match_the_config_defaults() {
    let c = Config::default();
    assert_eq!(General::from_config(&c), General::default());
    assert_eq!(Crawler::from_config(&c), Crawler::default());
    let mut applied = Config::default();
    for f in Crawler::FIELDS {
        Crawler::default().apply(f.name, &mut applied);
    }
    assert_eq!(applied.fetch.max_sockets, c.fetch.max_sockets);
    assert_eq!(applied.dht.intensity, c.dht.intensity);
    // 0 sockets is "follow the intensity".
    Crawler {
        max_sockets: 250,
        ..Crawler::default()
    }
    .apply("max_sockets", &mut applied);
    assert_eq!(applied.fetch.max_sockets, Some(250));
    assert!(!super::transport::Transport::default().utp);
    assert!(super::transport::Transport::default().fallback_to_tcp);
}

#[test]
fn unsaved_values_reach_the_services_exactly_as_the_file_has_them() {
    let dir = tempfile::tempdir().unwrap();
    // Two DHT sockets on different ports: the section shows one port, but
    // nothing saved means nothing rewritten.
    let c = config(
        dir.path(),
        Some("[dht]\nbind = [\"0.0.0.0:6881\", \"[::]:6882\"]\n[fetch]\nmax_sockets = 300\n"),
        &[],
    );
    let r = registry(dir.path(), &c);
    let eff = r.effective_config(&c);
    assert_eq!(eff.dht.bind, c.dht.bind);
    assert_eq!(eff.fetch.max_sockets, Some(300));
    // A saved field elsewhere in the section changes only itself.
    r.patch("network", patch(json!({ "dht_enabled": false })))
        .unwrap();
    r.patch("crawler", patch(json!({ "timeout_secs": 9 })))
        .unwrap();
    let r = registry(dir.path(), &c);
    let eff = r.effective_config(&c);
    assert_eq!(eff.dht.bind, c.dht.bind);
    assert!(!eff.dht.enabled);
    assert_eq!(eff.fetch.timeout_secs, 9);
    assert_eq!(eff.fetch.max_sockets, Some(300));
    // A saved port moves every socket to it.
    r.patch("network", patch(json!({ "dht_port": 7000 })))
        .unwrap();
    let eff = registry(dir.path(), &c).effective_config(&c);
    assert!(eff.dht.bind.iter().all(|a| a.port() == 7000));
    // An environment-set field keeps the environment's value.
    let c = config(dir.path(), None, &[("LODESTAR_DHT_ENABLED", "true")]);
    assert!(registry(dir.path(), &c).effective_config(&c).dht.enabled);
}

#[test]
fn secure_cookies_come_only_from_the_file_or_environment() {
    let dir = tempfile::tempdir().unwrap();
    open(dir.path())
        .lock()
        .unwrap()
        .set_settings_text(&[("settings.network.secure_cookies", "true")])
        .unwrap();
    let c = config(dir.path(), None, &[]);
    let r = registry(dir.path(), &c);
    // A saved value (from an earlier build) is never applied.
    assert_eq!(
        field(&r, "network", "secure_cookies"),
        (json!(false), SettingSource::Default, true)
    );
    assert!(!r.effective_config(&c).http.secure_cookies);
    for v in [json!(true), json!(null)] {
        assert_eq!(
            r.patch("network", patch(json!({ "secure_cookies": v }))),
            Err(SettingsError::ReadOnly("secure_cookies".into()))
        );
    }
    let c = config(dir.path(), Some("[http]\nsecure_cookies = true\n"), &[]);
    let r = registry(dir.path(), &c);
    assert_eq!(
        field(&r, "network", "secure_cookies"),
        (json!(true), SettingSource::File, true)
    );
    assert!(r.effective_config(&c).http.secure_cookies);
    let c = config(dir.path(), None, &[("LODESTAR_SECURE_COOKIES", "true")]);
    let r = registry(dir.path(), &c);
    assert_eq!(
        field(&r, "network", "secure_cookies"),
        (json!(true), SettingSource::Env, true)
    );
    let index = r.index();
    let network = index
        .sections
        .iter()
        .find(|s| s.section == "network")
        .unwrap();
    assert_eq!(
        network.schema["properties"]["secure_cookies"]["readOnly"],
        json!(true)
    );
    assert!(
        network.schema["properties"]["http_bind"]
            .get("readOnly")
            .is_none()
    );
}

#[tokio::test]
async fn null_resets_a_field_to_the_file_or_default() {
    let dir = tempfile::tempdir().unwrap();
    let c = config(dir.path(), Some("[fetch]\ntimeout_secs = 30\n"), &[]);
    let r = registry(dir.path(), &c);
    let mut rx = r.subscribe::<Crawler>();
    r.patch(
        "crawler",
        patch(json!({ "timeout_secs": 20, "intensity": "low" })),
    )
    .unwrap();
    rx.borrow_and_update();
    // Reset one and save another, together.
    let out = r
        .patch(
            "crawler",
            patch(json!({ "timeout_secs": null, "encryption": "require" })),
        )
        .unwrap();
    assert_eq!(out.fields["timeout_secs"].value, json!(30));
    assert_eq!(out.fields["timeout_secs"].source, SettingSource::File);
    assert_eq!(out.fields["intensity"].source, SettingSource::Db);
    assert!(rx.has_changed().unwrap());
    assert_eq!(rx.borrow_and_update().timeout_secs, 30);
    assert_eq!(
        r.store
            .lock()
            .unwrap()
            .settings_with_prefix("settings.crawler.")
            .unwrap(),
        vec![
            (
                "settings.crawler.encryption".to_string(),
                "\"require\"".to_string()
            ),
            (
                "settings.crawler.intensity".to_string(),
                "\"low\"".to_string()
            ),
        ]
    );
    // A live field resets live; a field with no file value goes back to
    // the default.
    r.patch("crawler", patch(json!({ "intensity": null })))
        .unwrap();
    assert_eq!(rx.borrow_and_update().intensity, Intensity::Balanced);
    assert_eq!(field(&r, "crawler", "intensity").1, SettingSource::Default);
    // Resetting what isn't saved changes nothing and wakes nobody.
    r.patch("crawler", patch(json!({ "max_sockets": null })))
        .unwrap();
    assert!(!rx.has_changed().unwrap());
    // Restart-only: back to the running value, nothing pending.
    let out = r
        .patch("crawler", patch(json!({ "encryption": null })))
        .unwrap();
    assert!(out.restart_required.is_empty());
    // No value is saved; only the section's revision, which counted every
    // save that changed it (and not the reset that changed nothing).
    assert_eq!(
        r.store
            .lock()
            .unwrap()
            .settings_with_prefix(KEY_PREFIX)
            .unwrap(),
        [("settings.crawler#revision".to_string(), "4".to_string())]
    );
    assert_eq!(out.revision, 4);
    // It survives a restart as reset.
    let r = registry(dir.path(), &c);
    assert_eq!(field(&r, "crawler", "timeout_secs").1, SettingSource::File);
    // Resets obey the same refusals as values.
    assert_eq!(
        r.patch("crawler", patch(json!({ "nope": null }))),
        Err(SettingsError::UnknownField("nope".into()))
    );
    assert_eq!(
        r.patch("general", patch(json!({ "data_dir": null }))),
        Err(SettingsError::ReadOnly("data_dir".into()))
    );
    let c = config(dir.path(), None, &[("LODESTAR_INTENSITY", "low")]);
    let r = registry(dir.path(), &c);
    assert!(matches!(
        r.patch("crawler", patch(json!({ "intensity": null }))),
        Err(SettingsError::SetByEnv { .. })
    ));
}

#[test]
fn the_healthcheck_probes_the_bind_the_server_uses() {
    let dir = tempfile::tempdir().unwrap();
    let text = format!(
        "data_dir = {:?}\n[http]\nbind = \"127.0.0.1:9000\"\n",
        dir.path().display().to_string()
    );
    let c = config(dir.path(), Some(&text), &[]);
    // No store yet: the file's, and probing creates none.
    assert_eq!(
        network::effective_bind(&c),
        "127.0.0.1:9000".parse().unwrap()
    );
    assert!(!c.db_path().exists());
    let store = Arc::new(Mutex::new(
        Store::open(c.db_path(), StoreConfig::default()).unwrap(),
    ));
    let r = Registry::load(&c, store).unwrap();
    r.patch("network", patch(json!({ "http_bind": "127.0.0.1:9100" })))
        .unwrap();
    assert_eq!(
        network::effective_bind(&c),
        "127.0.0.1:9100".parse().unwrap()
    );
    // The environment wins over a saved bind, as at start.
    let c = config(
        dir.path(),
        Some(&text),
        &[("LODESTAR_HTTP_BIND", "127.0.0.1:9200")],
    );
    assert_eq!(
        network::effective_bind(&c),
        "127.0.0.1:9200".parse().unwrap()
    );
}

/// Runs `f` with a subscriber that records what it logs, and returns that.
pub(super) fn logs(f: impl FnOnce()) -> String {
    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buf = Buf::default();
    let writer = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_max_level(tracing::Level::INFO)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, f);
    String::from_utf8(buf.0.lock().unwrap().clone()).unwrap()
}

#[test]
fn saved_values_of_now_read_only_fields_are_removed_once() {
    let dir = tempfile::tempdir().unwrap();
    open(dir.path())
        .lock()
        .unwrap()
        .set_settings_text(&[
            ("settings.network.secure_cookies", "true"),
            ("settings.general.data_dir", "\"/elsewhere\""),
            ("settings.network.dht_enabled", "false"),
        ])
        .unwrap();
    let c = config(dir.path(), None, &[]);
    let out = logs(|| drop(registry(dir.path(), &c)));
    assert!(
        out.contains("removed saved network.secure_cookies"),
        "{out}"
    );
    assert!(out.contains("removed saved general.data_dir"), "{out}");
    assert!(!out.contains("/elsewhere"), "{out}");
    // Only the retired rows went; the next start has nothing to say.
    let r = registry(dir.path(), &c);
    assert_eq!(
        r.store
            .lock()
            .unwrap()
            .settings_with_prefix(KEY_PREFIX)
            .unwrap(),
        vec![(
            "settings.network.dht_enabled".to_string(),
            "false".to_string()
        )]
    );
    drop(r);
    let out = logs(|| drop(registry(dir.path(), &c)));
    assert!(!out.contains("removed saved"), "{out}");
}

#[test]
fn a_saved_value_over_the_file_is_noted_without_its_value() {
    let dir = tempfile::tempdir().unwrap();
    let c = config(
        dir.path(),
        Some("[fetch]\ntimeout_secs = 30\nmax_metadata_mib = 16\n"),
        &[],
    );
    open(dir.path())
        .lock()
        .unwrap()
        .set_settings_text(&[
            ("settings.crawler.timeout_secs", "47"),
            // The same as the file: nothing to note.
            ("settings.crawler.max_metadata_mib", "16"),
            // Over a default, not the file: nothing to note either.
            ("settings.crawler.intensity", "\"low\""),
        ])
        .unwrap();
    let out = logs(|| drop(registry(dir.path(), &c)));
    assert!(out.contains("crawler.timeout_secs"), "{out}");
    assert!(out.contains("set it to null in the app to reset"), "{out}");
    assert!(!out.contains("47"), "{out}");
    assert!(!out.contains("crawler.max_metadata_mib"), "{out}");
    assert!(!out.contains("crawler.intensity"), "{out}");
}
