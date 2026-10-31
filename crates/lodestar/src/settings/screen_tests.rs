//! What the Settings screen needs of the registry (B2b, §10.10.9): the
//! form hints, revisions and stale saves, multi-section batches (all or
//! nothing), the preview and its property test against the save,
//! preferences, secret fields, and export and import.

use std::path::Path;

use lodestar_api::settings::{
    MAX_IMPORT, SettingSource, SettingsApi, SettingsBatch, SettingsError, SettingsFailure,
    SettingsImport,
};
use proptest::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use utoipa::ToSchema;

use super::crawler::{Crawler, Intensity};
use super::preferences::Preferences;
use super::tests::{config, logs, open, patch, registry};
use super::*;

fn batch(v: Value) -> SettingsBatch {
    serde_json::from_value(v).unwrap()
}

fn saved(r: &Registry) -> Vec<(String, String)> {
    r.store
        .lock()
        .unwrap()
        .settings_with_prefix(KEY_PREFIX)
        .unwrap()
}

fn revision(r: &Registry, section: &str) -> u64 {
    r.section(section).unwrap().revision
}

fn failure(section: &str, error: SettingsError) -> SettingsFailure {
    SettingsFailure::new(section, error)
}

#[test]
fn every_field_has_its_form_hints() {
    let dir = tempfile::tempdir().unwrap();
    let r = registry(dir.path(), &config(dir.path(), None, &[]));
    let units = ["seconds", "mebibytes", "connections", "days", "files"];
    let widgets = [
        "radio",
        "slider",
        "theme",
        "shortcuts",
        "language",
        "time-zone",
        "log-events",
        "log-components",
        "log-kinds",
        "duration",
    ];
    let dangers = ["disruptive", "privacy", "destructive", "security"];
    // Ours, and utoipa's for integers.
    let formats = [
        "socket-addr",
        "log-filter",
        "time-range",
        "int32",
        "int64",
        "uint16",
        "uint32",
        "uint64",
    ];
    for s in r.index().sections {
        let entry = r.find(&s.section).unwrap();
        let fields = entry.current();
        for (order, (f, ..)) in fields.iter().enumerate() {
            let p = &s.schema["properties"][f.name];
            let at = format!("{}.{}", s.section, f.name);
            let group = p["x-group"].as_str().unwrap();
            assert!(
                !group.is_empty() && group.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
                "{at}: {group:?}"
            );
            assert_eq!(p["x-order"], json!(order), "{at}");
            assert!(
                p["x-advanced"].is_boolean() && p["x-preview"].is_boolean(),
                "{at}"
            );
            assert_eq!(p.get("x-config").and_then(Value::as_str), f.file, "{at}");
            assert_eq!(p.get("x-env").and_then(Value::as_str), f.env, "{at}");
            let keywords = p.get("x-keywords").and_then(Value::as_str);
            assert_eq!(
                keywords.map(str::to_string),
                f.keywords
                    .then(|| format!("settings.{}.{}.keywords", s.section, f.name)),
                "{at}"
            );
            for (key, allowed) in [
                ("x-unit", &units[..]),
                ("x-widget", &widgets[..]),
                ("x-danger", &dangers[..]),
                ("format", &formats[..]),
            ] {
                if let Some(v) = p.get(key) {
                    assert!(allowed.contains(&v.as_str().unwrap()), "{at}: {key} {v}");
                }
            }
            // An automatic value is a valid value of the field.
            if let Some(auto) = p.get("x-auto") {
                let mut m = Map::new();
                m.insert(f.name.to_string(), auto.clone());
                assert!(entry.plan(&m).is_ok(), "{at}: x-auto {auto} isn't accepted");
            }
            // `writeOnly` exactly on secrets, and on list items' secret
            // keys.
            assert_eq!(p.get("writeOnly") == Some(&json!(true)), f.secret, "{at}");
            for k in f.secret_keys {
                assert_eq!(
                    p["items"]["properties"][*k]["writeOnly"],
                    json!(true),
                    "{at}.{k}"
                );
            }
        }
    }
    let index = r.index();
    let prop = |s: &str, f: &str| {
        index
            .sections
            .iter()
            .find(|x| x.section == s)
            .unwrap()
            .schema["properties"][f]
            .clone()
    };
    let sockets = prop("crawler", "max_sockets");
    assert_eq!(
        (
            &sockets["x-auto"],
            &sockets["x-unit"],
            &sockets["x-advanced"]
        ),
        (&json!(0), &json!("connections"), &json!(true))
    );
    let bind = prop("network", "http_bind");
    assert_eq!(
        (&bind["format"], &bind["x-danger"], &bind["x-preview"]),
        (&json!("socket-addr"), &json!("disruptive"), &json!(true))
    );
    assert_eq!(bind["x-env"], json!("LODESTAR_HTTP_BIND"));
    assert_eq!(bind["x-config"], json!("http.bind"));
    assert_eq!(prop("preferences", "theme_dark")["x-auto"], json!(""));
    assert_eq!(prop("logging", "libraries")["format"], json!("log-filter"));
    assert_eq!(prop("logging", "data")["x-danger"], json!("privacy"));
    assert_eq!(prop("logging", "verbosity")["x-env"], json!("LODESTAR_LOG"));
}

#[test]
fn revisions_count_saves_and_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let c = config(dir.path(), None, &[]);
    let r = registry(dir.path(), &c);
    assert_eq!(revision(&r, "crawler"), 0);
    r.patch("crawler", patch(json!({ "intensity": "low" })))
        .unwrap();
    assert_eq!(revision(&r, "crawler"), 1);
    // A save that changes nothing isn't one.
    let same = r
        .patch("crawler", patch(json!({ "intensity": "low" })))
        .unwrap();
    assert_eq!(same.revision, 1);
    r.patch("crawler", patch(json!({ "intensity": null })))
        .unwrap();
    assert_eq!(revision(&r, "crawler"), 2);
    // Other sections keep their own.
    assert_eq!(revision(&r, "transport"), 0);
    drop(r);
    let r = registry(dir.path(), &c);
    assert_eq!(revision(&r, "crawler"), 2);
    // A revision row someone broke starts again rather than failing.
    r.store
        .lock()
        .unwrap()
        .set_settings_text(&[("settings.transport#revision", "x")])
        .unwrap();
    drop(r);
    let r = registry(dir.path(), &c);
    assert_eq!(revision(&r, "transport"), 0);
}

#[test]
fn a_stale_revision_is_refused_with_the_current_values() {
    let dir = tempfile::tempdir().unwrap();
    let r = registry(dir.path(), &config(dir.path(), None, &[]));
    // Another tab saved since this one loaded revision 0.
    r.patch("crawler", patch(json!({ "intensity": "low" })))
        .unwrap();
    let before = saved(&r);
    let mut rx = r.subscribe::<Crawler>();
    rx.borrow_and_update();
    let refused = r
        .patch(
            "crawler",
            patch(json!({ "if_revision": 0, "intensity": "aggressive" })),
        )
        .unwrap_err();
    let SettingsError::Stale(current) = refused else {
        panic!("{refused:?}");
    };
    assert_eq!(current, [r.section("crawler").unwrap()]);
    assert_eq!(current[0].fields["intensity"].value, json!("low"));
    assert_eq!(saved(&r), before);
    assert!(!rx.has_changed().unwrap());
    // From the current revision, it saves.
    let ok = r
        .patch(
            "crawler",
            patch(json!({ "if_revision": 1, "intensity": "aggressive" })),
        )
        .unwrap();
    assert_eq!(
        (ok.revision, &ok.fields["intensity"].value),
        (2, &json!("aggressive"))
    );
    assert!(!ok.fields.contains_key("if_revision"));
    // Only a whole number is a revision.
    for bad in [json!(-1), json!("2"), json!(1.5), json!(null)] {
        assert_eq!(
            r.patch("crawler", patch(json!({ "if_revision": bad.clone() })))
                .map(|_| ()),
            Err(SettingsError::Invalid {
                field: "if_revision".into(),
                message:
                    "Must be the section's revision, a whole number from GET /settings/{section}."
                        .into()
            }),
            "{bad}"
        );
    }
    // Every stale section of a batch is reported, with its values, even
    // beside a valid one; the first in display order is named.
    r.patch("transport", patch(json!({ "utp": true }))).unwrap();
    let err = r
        .save(batch(json!({
            "transport": { "if_revision": 0, "utp": false },
            "crawler": { "if_revision": 0 },
            "storage": { "if_revision": 0, "archive_raw": true },
        })))
        .unwrap_err();
    assert_eq!(err.section.as_deref(), Some("crawler"));
    let SettingsError::Stale(current) = err.error else {
        panic!("{err:?}");
    };
    let names: Vec<&str> = current.iter().map(|v| v.section.as_str()).collect();
    assert_eq!(names, ["crawler", "transport"]);
    assert!(!r.get::<super::storage::Storage>().archive_raw);
}

#[test]
fn a_batch_saves_every_section_or_none() {
    let dir = tempfile::tempdir().unwrap();
    let c = config(dir.path(), None, &[("LODESTAR_DHT_PORT", "7000")]);
    let r = registry(dir.path(), &c);
    let mut crawler = r.subscribe::<Crawler>();
    let mut transport = r.subscribe::<super::transport::Transport>();
    crawler.borrow_and_update();
    transport.borrow_and_update();
    let out = r
        .save(batch(json!({
            "transport": { "utp": true },
            "crawler": { "intensity": "low", "timeout_secs": 12 },
            "storage": {},
        })))
        .unwrap();
    // Display order; an empty patch is answered and changes nothing.
    let names: Vec<&str> = out.sections.iter().map(|v| v.section.as_str()).collect();
    assert_eq!(names, ["crawler", "transport", "storage"]);
    assert_eq!(
        out.changed,
        [("crawler".to_string(), 1), ("transport".to_string(), 1)]
    );
    assert!(crawler.has_changed().unwrap() && transport.has_changed().unwrap());
    assert_eq!(r.get::<Crawler>().intensity, Intensity::Low);
    let before = saved(&r);
    // One bad field anywhere saves nothing anywhere, and names its
    // section and field.
    let refusals = [
        (
            json!({ "crawler": { "intensity": "aggressive" }, "transport": { "utp": "yes" } }),
            "transport",
            "utp",
            "invalid_setting",
        ),
        (
            json!({ "crawler": { "timeout_secs": 0 }, "transport": { "utp": false } }),
            "crawler",
            "timeout_secs",
            "invalid_setting",
        ),
        (
            json!({ "crawler": { "intensity": "aggressive" }, "network": { "dht_port": 7001 } }),
            "network",
            "dht_port",
            "set_by_environment",
        ),
        (
            json!({ "crawler": { "intensity": "aggressive" }, "general": { "data_dir": "/x" } }),
            "general",
            "data_dir",
            "read_only",
        ),
        (
            json!({ "crawler": { "intensity": "aggressive" }, "transport": { "utpp": true } }),
            "transport",
            "utpp",
            "unknown_field",
        ),
    ];
    for (body, section, field, code) in refusals {
        let err = r.save(batch(body.clone())).unwrap_err();
        assert_eq!(err.section.as_deref(), Some(section), "{body}");
        let (got_field, got_code) = match &err.error {
            SettingsError::Invalid { field, .. } => (field.as_str(), "invalid_setting"),
            SettingsError::SetByEnv { field, .. } => (field.as_str(), "set_by_environment"),
            SettingsError::ReadOnly(field) => (field.as_str(), "read_only"),
            SettingsError::UnknownField(field) => (field.as_str(), "unknown_field"),
            other => panic!("{body}: {other:?}"),
        };
        assert_eq!((got_field, got_code), (field, code), "{body}");
        assert_eq!(saved(&r), before, "{body}");
    }
    let err = r
        .save(batch(
            json!({ "crawler": { "intensity": "aggressive" }, "filters": {} }),
        ))
        .unwrap_err();
    assert_eq!(err, failure("filters", SettingsError::UnknownSection));
    assert_eq!(saved(&r), before);
    crawler.borrow_and_update();
    assert_eq!(r.get::<Crawler>().intensity, Intensity::Low);
}

#[test]
fn a_batch_the_store_refuses_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let r = registry(dir.path(), &config(dir.path(), None, &[]));
    // The second section's row fails inside the transaction, after the
    // first section's were written.
    let conn = lodestar_store::rusqlite::Connection::open(dir.path().join("t.db")).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER no_utp BEFORE INSERT ON settings
         WHEN NEW.key = 'settings.transport.utp'
         BEGIN SELECT RAISE(ABORT, 'refused'); END;",
    )
    .unwrap();
    let mut rx = r.subscribe::<Crawler>();
    rx.borrow_and_update();
    let err = r
        .save(batch(json!({
            "crawler": { "intensity": "low" },
            "transport": { "utp": true },
        })))
        .unwrap_err();
    assert!(matches!(err.error, SettingsError::Store(_)), "{err:?}");
    assert!(saved(&r).is_empty());
    assert!(!rx.has_changed().unwrap());
    assert_eq!(revision(&r, "crawler"), 0);
    assert_eq!(r.get::<Crawler>().intensity, Intensity::Balanced);
}

#[test]
fn the_adult_confirmation_applies_in_a_batch() {
    let dir = tempfile::tempdir().unwrap();
    let r = registry(dir.path(), &config(dir.path(), None, &[]));
    let unconfirmed = json!({
        "crawler": { "intensity": "low" },
        "content": { "show_adult": true },
    });
    let err = r.save(batch(unconfirmed.clone())).unwrap_err();
    assert_eq!(err.section.as_deref(), Some("content"));
    assert!(matches!(
        err.error,
        SettingsError::NeedsConfirmation { ref field, code: "adult_confirmation_required", .. }
            if field == "show_adult"
    ));
    // Nothing of the batch was saved, the other section included.
    assert!(saved(&r).is_empty());
    // The preview refuses it the same way.
    assert_eq!(r.preview(batch(unconfirmed)).unwrap_err(), err);
    let out = r
        .save(batch(json!({
            "crawler": { "intensity": "low" },
            "content": { "show_adult": true, "confirm_adult": true },
        })))
        .unwrap();
    assert_eq!(out.sections[1].fields["show_adult"].value, json!(true));
    assert!(!out.sections[1].fields.contains_key("confirm_adult"));
}

#[test]
fn the_preview_checks_like_a_save_and_saves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let r = registry(dir.path(), &config(dir.path(), None, &[]));
    let mut rx = r.subscribe::<Crawler>();
    rx.borrow_and_update();
    let p = r
        .preview(batch(json!({
            "crawler": { "intensity": "low", "timeout_secs": 20 },
            "network": { "http_bind": "127.0.0.1:9000", "dht_enabled": false },
            "search": { "enabled": false, "heap_mib": 256 },
            "logging": { "components": { "dht": "debug" } },
        })))
        .unwrap();
    let crawler = &p.sections["crawler"];
    assert_eq!(crawler.revision, 0);
    let intensity = &crawler.fields["intensity"];
    assert_eq!(intensity.value, json!("low"));
    assert!(!intensity.restart_required);
    assert_eq!(
        serde_json::to_value(&intensity.estimates).unwrap(),
        json!([{ "name": "sockets", "value": 100 }])
    );
    let timeout = &crawler.fields["timeout_secs"];
    assert!(timeout.restart_required && timeout.estimates.is_empty());
    let bind = &p.sections["network"].fields["http_bind"];
    assert_eq!(bind.estimates[0].value, json!("http://127.0.0.1:9000/"));
    assert_eq!(bind.warnings[0].code, "loopback_only");
    let codes = |s: &str, f: &str| -> Vec<String> {
        p.sections[s].fields[f]
            .warnings
            .iter()
            .map(|w| w.code.clone())
            .collect()
    };
    assert_eq!(codes("network", "dht_enabled"), ["crawling_stops"]);
    assert_eq!(codes("search", "enabled"), ["search_stops"]);
    assert_eq!(codes("logging", "components"), ["verbose_logs"]);
    assert_eq!(
        p.sections["search"].fields["heap_mib"].estimates[0].value,
        json!(256)
    );
    // Only the patched fields, and nothing was saved or published.
    assert_eq!(crawler.fields.len(), 2);
    assert!(saved(&r).is_empty());
    assert!(!rx.has_changed().unwrap());
    assert_eq!(revision(&r, "crawler"), 0);
    // An explicit address and a reset preview too.
    let p = r
        .preview(batch(json!({
            "network": { "http_bind": "[::]:8191" },
            "crawler": { "max_sockets": 250, "intensity": null },
        })))
        .unwrap();
    let bind = &p.sections["network"].fields["http_bind"];
    assert_eq!(bind.estimates[0].value, json!("http://localhost:8191/"));
    assert!(bind.warnings.is_empty());
    assert_eq!(
        p.sections["crawler"].fields["intensity"].value,
        json!("balanced")
    );
    assert_eq!(
        p.sections["crawler"].fields["max_sockets"].estimates[0].value,
        json!(250)
    );
}

/// Patch values the property test draws from: valid and invalid for
/// every field type, plus keys that aren't fields.
fn any_value() -> impl Strategy<Value = Value> {
    prop::sample::select(vec![
        json!(null),
        json!(true),
        json!(false),
        json!(0),
        json!(1),
        json!(12),
        json!(120),
        json!(121),
        json!(-1),
        json!(4096),
        json!(70000),
        json!(1.5),
        json!("low"),
        json!("aggressive"),
        json!("turbo"),
        json!("require"),
        json!("127.0.0.1:9000"),
        json!("0.0.0.0:0"),
        json!("localhost:80"),
        json!("info"),
        json!("info,lodestar_dht=debug"),
        json!("[["),
        json!("riverstone"),
        json!("Not A Theme"),
        json!(""),
        json!("zh-CN"),
        json!("Europe/Berlin"),
        json!("dark"),
        json!("cards"),
        json!({}),
        json!({ "results.next": "n" }),
        json!({ "a": "j", "b": "j" }),
        json!({ "dht": "debug" }),
        json!({ "logging.applied": false }),
        json!({ "app.start": false }),
        json!({ "name": 1 }),
        json!("detailed"),
        json!("full"),
        json!(-1),
        json!(4_000_000_000i64),
        json!([1]),
    ])
}

fn any_key(section: &'static str) -> impl Strategy<Value = String> {
    let mut keys: Vec<String> = match section {
        "general" => super::general::General::FIELDS,
        "network" => super::network::Network::FIELDS,
        "crawler" => Crawler::FIELDS,
        "transport" => super::transport::Transport::FIELDS,
        "storage" => super::storage::Storage::FIELDS,
        "search" => super::search::Search::FIELDS,
        "content" => super::content::Content::FIELDS,
        "logging" => super::logging::Logging::FIELDS,
        "preferences" => Preferences::FIELDS,
        _ => &[],
    }
    .iter()
    .map(|f| f.name.to_string())
    .collect();
    keys.extend(["bogus", "if_revision", "confirm_adult"].map(str::to_string));
    prop::sample::select(keys)
}

fn any_patch(section: &'static str) -> impl Strategy<Value = Map<String, Value>> {
    prop::collection::btree_map(any_key(section), any_value(), 0..4)
        .prop_map(|m| m.into_iter().collect())
}

fn any_batch() -> impl Strategy<Value = SettingsBatch> {
    let sections = [
        "general",
        "network",
        "crawler",
        "transport",
        "storage",
        "search",
        "content",
        "preferences",
        "logging",
        "nope",
    ];
    prop::collection::vec(prop::sample::select(sections.to_vec()), 0..4).prop_flat_map(|names| {
        names
            .into_iter()
            .map(|n| any_patch(n).prop_map(move |p| (n.to_string(), Value::Object(p))))
            .collect::<Vec<_>>()
            .prop_map(|pairs| batch(Value::Object(pairs.into_iter().collect())))
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    /// §10.10.9: the preview accepts exactly what the save accepts, with
    /// the same refusal, and previews the values the save then saves.
    #[test]
    fn the_preview_accepts_exactly_what_a_save_accepts(
        earlier in any_batch(),
        b in any_batch(),
    ) {
        let dir = tempfile::tempdir().unwrap();
        let mut c = config(dir.path(), Some("[fetch]\ntimeout_secs = 30\n"), &[("LODESTAR_DHT_PORT", "7000")]);
        c.data_dir = dir.path().to_path_buf();
        let r = registry(dir.path(), &c);
        // Some saved state first, so revisions and saved values vary.
        let _ = r.save(earlier);
        let preview = r.preview(b.clone());
        let before = saved(&r);
        prop_assert_eq!(saved(&r), before.clone(), "the preview saved something");
        let save = r.save(b.clone());
        match (&preview, &save) {
            (Ok(p), Ok(s)) => {
                for v in &s.sections {
                    for (name, f) in &p.sections[&v.section].fields {
                        prop_assert_eq!(&f.value, &v.fields[name].value, "{}.{}", v.section, name);
                    }
                }
            }
            (Err(p), Err(s)) => prop_assert_eq!(p, s),
            _ => prop_assert!(false, "preview {:?} but save {:?} for {:?}", preview, save, b),
        }
        if save.is_err() {
            prop_assert_eq!(saved(&r), before, "a refused save changed something");
        }
    }
}

#[test]
fn preferences_apply_at_once_and_are_checked() {
    let dir = tempfile::tempdir().unwrap();
    let c = config(dir.path(), None, &[]);
    let r = registry(dir.path(), &c);
    let mut rx = r.subscribe::<Preferences>();
    rx.borrow_and_update();
    let out = r
        .patch(
            "preferences",
            patch(json!({
                "theme": "tokyo-night",
                "mode": "dark",
                "density": "compact",
                "shortcut_keys": { "results.next": "n" },
                "language": "zh-CN",
                "time_zone": "Asia/Shanghai",
            })),
        )
        .unwrap();
    assert!(out.restart_required.is_empty());
    assert_eq!(out.fields["theme"].source, SettingSource::Db);
    assert!(rx.has_changed().unwrap());
    for (body, field) in [
        (json!({ "theme": "Tokyo Night" }), "theme"),
        (json!({ "theme_dark": "../x" }), "theme_dark"),
        (json!({ "language": "english" }), "language"),
        (json!({ "time_zone": "../../etc/passwd" }), "time_zone"),
        (
            json!({ "shortcut_keys": { "a": "j", "b": "j" } }),
            "shortcut_keys",
        ),
        (json!({ "mode": "sepia" }), "mode"),
        (json!({ "show_adult": true }), "show_adult"),
    ] {
        let err = r.patch("preferences", patch(body.clone())).unwrap_err();
        let got = match &err {
            SettingsError::Invalid { field, .. } | SettingsError::UnknownField(field) => field,
            other => panic!("{body}: {other:?}"),
        };
        assert_eq!(got, field, "{body}");
    }
    // Kept across a restart; the server's config is untouched.
    drop(r);
    let r = registry(dir.path(), &c);
    let p = r.get::<Preferences>();
    assert_eq!(
        (p.theme.as_str(), p.language.as_str()),
        ("tokyo-night", "zh-CN")
    );
    assert_eq!(format!("{:?}", r.effective_config(&c)), format!("{c:?}"));
    let keys: Vec<String> = saved(&r).into_iter().map(|(k, _)| k).collect();
    assert!(
        keys.iter().all(|k| k.starts_with("settings.preferences")),
        "{keys:?}"
    );
}

/// A section with a secret, as Integrations will have.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct Vault {
    label: String,
    token: String,
}

impl Section for Vault {
    const SECTION: &'static str = "vault";
    const FIELDS: &'static [Field] = &[
        Field::new("label").group("vault"),
        Field::new("token").group("vault").secret(),
    ];

    fn from_config(_: &Config) -> Self {
        Vault {
            label: String::new(),
            token: String::new(),
        }
    }

    fn apply(&self, _: &str, _: &mut Config) {}

    fn check(&self, field: &str) -> Result<(), String> {
        if field == "token" && self.token.len() > 16 {
            return Err("Must be at most 16 characters.".into());
        }
        Ok(())
    }
}

/// A config whose data directory, where the secrets key goes, is `dir`.
fn sealed_config(dir: &Path) -> Config {
    let mut c = config(dir, None, &[]);
    c.data_dir = dir.to_path_buf();
    c
}

fn vault(dir: &Path) -> Registry {
    let c = sealed_config(dir);
    let mut r = Registry::empty(open(dir));
    r.register::<super::crawler::Crawler>(&c).unwrap();
    r.register::<Vault>(&c).unwrap();
    r
}

#[test]
fn secrets_are_never_returned_exported_or_echoed() {
    let dir = tempfile::tempdir().unwrap();
    let r = vault(dir.path());
    let schema = &r.index().sections[1].schema;
    assert_eq!(schema["properties"]["token"]["writeOnly"], json!(true));
    assert!(schema["properties"]["label"].get("writeOnly").is_none());
    let out = r
        .patch(
            "vault",
            patch(json!({ "token": "hunter2", "label": "home" })),
        )
        .unwrap();
    // Not returned; `source` says one is set.
    assert_eq!(out.fields["token"].value, Value::Null);
    assert_eq!(out.fields["token"].source, SettingSource::Db);
    assert_eq!(out.fields["label"].value, json!("home"));
    let all = format!(
        "{:?}{:?}",
        r.section("vault"),
        r.preview(batch(json!({ "vault": { "token": "hunter3" } })))
            .unwrap()
    );
    assert!(!all.contains("hunter"), "{all}");
    // Not exported.
    let exported = r.export(true);
    assert!(exported.contains("label = \"home\""), "{exported}");
    assert!(
        !exported.contains("token") && !exported.contains("hunter"),
        "{exported}"
    );
    // Not imported: listed as skipped.
    let report = r
        .import(
            SettingsImport {
                toml: "[vault]\ntoken = \"stolen\"\n".into(),
                ..Default::default()
            },
            false,
        )
        .unwrap()
        .report;
    assert_eq!(report.skipped[0].key, "vault.token");
    assert_eq!(report.skipped[0].reason, "secret");
    assert!(!report.applied);
    // Not in an error, whether the type or the check refuses it.
    for bad in [json!(12345678), json!("x-secret-value-too-long")] {
        let err = r
            .patch("vault", patch(json!({ "token": bad.clone() })))
            .unwrap_err();
        let shown = format!("{err:?}");
        assert!(
            !shown.contains("12345678") && !shown.contains("x-secret"),
            "{shown}"
        );
    }
    // Nor in the log when a saved one is skipped at start.
    r.store
        .lock()
        .unwrap()
        .set_settings_text(&[("settings.vault.token", "98765432")])
        .unwrap();
    drop(r);
    let text = logs(|| drop(vault(dir.path())));
    assert!(text.contains("section=vault field=token"), "{text}");
    assert!(!text.contains("98765432"), "{text}");
}

/// A registry with values saved in several sections, an environment
/// variable and a read-only field.
fn exported(dir: &Path) -> (Registry, Config) {
    let s = open(dir);
    // Saved once, now shadowed by the environment.
    s.lock()
        .unwrap()
        .set_settings_text(&[("settings.network.dht_port", "7001")])
        .unwrap();
    // Filter state beside the settings, as in a real store.
    s.lock()
        .unwrap()
        .set_settings_text(&[("floor.file:/etc/list.txt", "x"), ("packs", "{}")])
        .unwrap();
    drop(s);
    let c = config(dir, None, &[("LODESTAR_DHT_PORT", "7000")]);
    let r = registry(dir, &c);
    r.save(batch(json!({
        "crawler": { "intensity": "low", "timeout_secs": 12 },
        "network": { "http_bind": "127.0.0.1:9000" },
        "content": { "show_adult": true, "confirm_adult": true },
        "preferences": { "theme": "nord", "shortcut_keys": { "results.next": "n", "search.focus": "/" } },
    })))
    .unwrap();
    (r, c)
}

#[test]
fn export_holds_only_saved_values() {
    let dir = tempfile::tempdir().unwrap();
    let (r, _) = exported(dir.path());
    let text = r.export(false);
    assert_eq!(
        text,
        "# Lodestar settings: the values saved in the app.\n\
         # Import this file in Settings > General > Backup & portability.\n\
         # Never included: secrets, values set by environment variables, and\n\
         # filter, floor, pack or subscription state.\n\
         lodestar_settings = 1\n\
         \n\
         [network]\n\
         # config.toml: http.bind · LODESTAR_HTTP_BIND\n\
         http_bind = \"127.0.0.1:9000\"\n\
         \n\
         [crawler]\n\
         # config.toml: dht.intensity · LODESTAR_INTENSITY\n\
         intensity = \"low\"\n\
         # config.toml: fetch.timeout_secs\n\
         timeout_secs = 12\n\
         \n\
         [content]\n\
         # config.toml: content.show_adult · LODESTAR_SHOW_ADULT\n\
         show_adult = true\n"
    );
    // §20 state and environment values never; preferences when asked.
    let values: String = text
        .lines()
        .filter(|l| !l.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    for word in [
        "floor",
        "packs",
        "dht_port",
        "7001",
        "data_dir",
        "preferences",
    ] {
        assert!(!values.contains(word), "{word}");
    }
    let with = r.export(true);
    assert!(with.ends_with(
        "\n[preferences]\ntheme = \"nord\"\nshortcut_keys = { \"results.next\" = \"n\", \"search.focus\" = \"/\" }\n"
    ), "{with}");
}

fn import(
    r: &Registry,
    text: &str,
    dry_run: bool,
) -> Result<lodestar_api::settings::Imported, SettingsFailure> {
    r.import(
        SettingsImport {
            toml: text.to_string(),
            ..Default::default()
        },
        dry_run,
    )
}

#[test]
fn an_export_imports_back_exactly() {
    let one = tempfile::tempdir().unwrap();
    let (r1, _) = exported(one.path());
    let text = r1.export(true);
    let two = tempfile::tempdir().unwrap();
    let r2 = registry(two.path(), &config(two.path(), None, &[]));
    // The adult setting needs the 18+ confirmation here too.
    let err = import(&r2, &text, false).unwrap_err();
    assert_eq!(err.section.as_deref(), Some("content"));
    assert!(matches!(err.error, SettingsError::NeedsConfirmation { .. }));
    assert!(saved(&r2).is_empty());
    // The dry run: the diff, nothing saved.
    let dry = r2
        .import(
            SettingsImport {
                toml: text.clone(),
                confirm_adult: true,
                revisions: None,
            },
            true,
        )
        .unwrap();
    assert!(!dry.report.applied && dry.changed.is_empty());
    assert!(saved(&r2).is_empty());
    let changed: Vec<String> = dry
        .report
        .changes
        .iter()
        .map(|c| format!("{}.{}={}", c.section, c.field, c.to))
        .collect();
    assert_eq!(
        changed,
        [
            "content.show_adult=true",
            "crawler.intensity=\"low\"",
            "crawler.timeout_secs=12",
            "network.http_bind=\"127.0.0.1:9000\"",
            "preferences.shortcut_keys={\"results.next\":\"n\",\"search.focus\":\"/\"}",
            "preferences.theme=\"nord\"",
        ]
    );
    let first = &dry.report.changes[1];
    assert_eq!(
        (first.from.clone(), first.from_source),
        (json!("balanced"), SettingSource::Default)
    );
    // Applied with the dry run's revisions: the same values, saved.
    let done = r2
        .import(
            SettingsImport {
                toml: text.clone(),
                confirm_adult: true,
                revisions: Some(dry.report.revisions.clone()),
            },
            false,
        )
        .unwrap();
    assert!(done.report.applied);
    assert_eq!(done.changed.len(), 4);
    assert_eq!(r2.export(true), text);
    // Again: nothing left to change.
    let again = r2
        .import(
            SettingsImport {
                toml: text.clone(),
                confirm_adult: true,
                revisions: None,
            },
            false,
        )
        .unwrap();
    assert!(!again.report.applied && again.report.changes.is_empty());
    assert_eq!(again.report.unchanged, 6);
    // A dry run's revisions refuse the import once something changed.
    r2.patch("crawler", patch(json!({ "intensity": "aggressive" })))
        .unwrap();
    let err = r2
        .import(
            SettingsImport {
                toml: text,
                confirm_adult: true,
                revisions: Some(dry.report.revisions),
            },
            false,
        )
        .unwrap_err();
    assert!(matches!(err.error, SettingsError::Stale(_)), "{err:?}");
    assert_eq!(r2.get::<Crawler>().intensity, Intensity::Aggressive);
}

#[test]
fn import_skips_what_it_must_not_change_and_is_all_or_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let c = config(dir.path(), None, &[("LODESTAR_DHT_PORT", "7000")]);
    let r = registry(dir.path(), &c);
    let text = "lodestar_settings = 1\n\
                stray = 1\n\
                [filters]\nrules = []\n\
                [general]\ndata_dir = \"/elsewhere\"\n\
                [network]\ndht_port = 7001\n\
                [crawler]\nintensity = \"low\"\nturbo = true\n";
    let out = import(&r, text, false).unwrap();
    let skipped: Vec<(String, String, Option<String>)> = out
        .report
        .skipped
        .iter()
        .map(|s| (s.key.clone(), s.reason.clone(), s.env.clone()))
        .collect();
    let s =
        |k: &str, r: &str, e: Option<&str>| (k.to_string(), r.to_string(), e.map(str::to_string));
    assert_eq!(
        skipped,
        [
            s("crawler.turbo", "unknown", None),
            s("filters", "unknown", None),
            s("general.data_dir", "read_only", None),
            s(
                "network.dht_port",
                "set_by_environment",
                Some("LODESTAR_DHT_PORT")
            ),
            s("stray", "unknown", None),
        ]
    );
    assert!(out.report.applied);
    assert_eq!(r.get::<Crawler>().intensity, Intensity::Low);
    assert_eq!(out.changed, [("crawler".to_string(), 1)]);
    // One bad value: nothing saved, the error names it.
    let before = saved(&r);
    let err = import(
        &r,
        "[crawler]\nintensity = \"aggressive\"\n[transport]\nutp = \"yes\"\n",
        false,
    )
    .unwrap_err();
    assert_eq!(err.section.as_deref(), Some("transport"));
    assert!(matches!(err.error, SettingsError::Invalid { ref field, .. } if field == "utp"));
    assert_eq!(saved(&r), before);
    let err = import(&r, "[crawler]\ntimeout_secs = nan\n", true).unwrap_err();
    assert!(
        matches!(err.error, SettingsError::Invalid { ref field, .. } if field == "timeout_secs")
    );
}

#[test]
fn import_refuses_what_isnt_an_export() {
    let dir = tempfile::tempdir().unwrap();
    let r = registry(dir.path(), &config(dir.path(), None, &[]));
    let message = |text: &str| match import(&r, text, true) {
        Err(SettingsFailure {
            error: SettingsError::InvalidImport(m),
            ..
        }) => m,
        other => panic!("{text:?}: {other:?}"),
    };
    let m = message("[crawler]\nintensity = \"low\"\nsecret-ish = \"hunter2\" junk\n");
    assert!(
        m.starts_with("This isn't a settings file:") && m.contains("(line 3)"),
        "{m}"
    );
    // The parser's message, never the file's text.
    assert!(!m.contains("hunter2"), "{m}");
    assert!(message("lodestar_settings = 2\n").contains("newer Lodestar"));
    assert!(message("lodestar_settings = \"1\"\n").contains("newer Lodestar"));
    // Nesting is bounded by the parser: refused, not a stack overflow.
    let deep = format!("a = {}{}\n", "[".repeat(20_000), "]".repeat(20_000));
    message(&deep);
    let big = format!("# {}\n", "x".repeat(MAX_IMPORT));
    assert_eq!(
        import(&r, &big, true).unwrap_err().error,
        SettingsError::ImportTooLarge
    );
    assert!(saved(&r).is_empty());
}

proptest! {
    /// Whatever the text, an import answers (a report or a refusal) and
    /// never panics.
    #[test]
    fn import_never_panics(text in "(\\[[a-z_.\\]]{0,12}\\]|[a-z_\"]{0,10} ?= ?[0-9a-z\"{}\\[\\],.=-]{0,16}|\n){0,12}") {
        let dir = tempfile::tempdir().unwrap();
        let r = registry(dir.path(), &config(dir.path(), None, &[]));
        let _ = import(&r, &text, true);
        prop_assert!(saved(&r).is_empty());
    }
}

#[test]
fn the_registry_reserves_route_names_and_patch_keys() {
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
    struct Export {
        x: bool,
    }
    impl Section for Export {
        const SECTION: &'static str = "export";
        const FIELDS: &'static [Field] = &[Field::new("x").group("x")];
        fn from_config(_: &Config) -> Self {
            Export { x: false }
        }
        fn apply(&self, _: &str, _: &mut Config) {}
    }
    let dir = tempfile::tempdir().unwrap();
    let c = config(dir.path(), None, &[]);
    let store = open(dir.path());
    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        Registry::empty(store.clone()).register::<Export>(&c)
    }));
    assert!(refused.is_err());
}

#[test]
fn secrets_are_sealed_at_rest_and_bound_to_their_field() {
    let dir = tempfile::tempdir().unwrap();
    let r = vault(dir.path());
    r.patch(
        "vault",
        patch(json!({ "token": "hunter2", "label": "hunter2" })),
    )
    .unwrap();
    let rows = saved(&r);
    let token = &rows
        .iter()
        .find(|(k, _)| k == "settings.vault.token")
        .unwrap()
        .1;
    assert!(
        token.starts_with("\"sealed:") && !token.contains("hunter2"),
        "{token}"
    );
    // A new registry over the same store and key opens it.
    drop(r);
    let r = vault(dir.path());
    assert_eq!(r.get::<Vault>().token, "hunter2");
    assert_eq!(
        r.section("vault").unwrap().fields["token"].source,
        SettingSource::Db
    );
    // A sealed value moved onto another field doesn't open there.
    let moved = token.clone();
    r.store
        .lock()
        .unwrap()
        .set_settings_text(&[("settings.vault.label", moved.as_str())])
        .unwrap();
    drop(r);
    let r = vault(dir.path());
    assert_ne!(r.get::<Vault>().label, "hunter2");
    // Without the key file, nothing opens: skipped, never fatal.
    drop(r);
    std::fs::remove_file(dir.path().join(SecretStore::KEY_FILE)).unwrap();
    let text = logs(|| {
        let r = vault(dir.path());
        assert_eq!(r.get::<Vault>().token, "");
    });
    assert!(
        text.contains("field=token") && text.contains("not_sealed"),
        "{text}"
    );
}

#[test]
fn the_new_sections_round_trip_and_check_their_values() {
    use super::insights::Insights;
    use super::notifications::Notifications;
    use super::trackers::Trackers;
    let dir = tempfile::tempdir().unwrap();
    let r = registry(dir.path(), &sealed_config(dir.path()));
    r.save(batch(json!({
        "trackers": { "scrape": false, "interval_secs": 10,
                      "trackers": ["udp://tracker.example:6969/announce"] },
        "notifications": { "email_host": "smtp.example.com", "email_to": ["me@example.com"],
                           "route_alerts": ["email", "ntfy"], "ntfy_url": "https://ntfy.sh/x" },
        "insights": { "query_logging": true, "query_retention_days": 7, "digest": "weekly" }
    })))
    .unwrap();
    drop(r);
    let r = registry(dir.path(), &sealed_config(dir.path()));
    let t = r.get::<Trackers>();
    assert_eq!(
        (t.scrape, t.interval_secs, t.trackers.len()),
        (false, 10, 1)
    );
    let n = r.get::<Notifications>();
    assert_eq!(n.email_to, ["me@example.com"]);
    assert_eq!(n.ntfy_url.0, "https://ntfy.sh/x");
    let i = r.get::<Insights>();
    assert!(i.query_logging && i.query_retention_days == 7);
    // Saved tracker values reach the scraper's config at the next start.
    let eff = r.effective_config(&sealed_config(dir.path()));
    assert!(!eff.scrape.enabled && eff.scrape.interval_secs == Some(10));
    for (section, p) in [
        ("trackers", json!({ "trackers": ["wss://x.example/"] })),
        ("trackers", json!({ "interval_secs": 3601 })),
        ("notifications", json!({ "email_to": ["nobody"] })),
        ("notifications", json!({ "ntfy_priority": 6 })),
        ("notifications", json!({ "route_health": ["ntfy", "ntfy"] })),
        ("notifications", json!({ "route_health": ["pager"] })),
        ("insights", json!({ "query_retention_days": 31 })),
        ("insights", json!({ "query_retention_days": 0 })),
        ("integrations", json!({ "default_client": "nas" })),
        ("integrations", json!({ "torznab_url": "ftp://x/" })),
    ] {
        assert!(r.patch(section, patch(p.clone())).is_err(), "{section} {p}");
    }
}

#[test]
fn the_scraper_switch_follows_its_environment_variable() {
    let dir = tempfile::tempdir().unwrap();
    let c = config(dir.path(), None, &[("LODESTAR_SCRAPE_ENABLED", "false")]);
    let r = registry(dir.path(), &c);
    let f = &r.section("trackers").unwrap().fields["scrape"];
    assert_eq!(
        (f.read_only, f.env.as_deref(), &f.value),
        (true, Some("LODESTAR_SCRAPE_ENABLED"), &json!(false))
    );
    assert_eq!(
        r.patch("trackers", patch(json!({ "scrape": true }))),
        Err(SettingsError::SetByEnv {
            field: "scrape".into(),
            var: "LODESTAR_SCRAPE_ENABLED".into()
        })
    );
}

#[test]
fn a_registry_with_no_secret_saved_never_creates_the_key_file() {
    let dir = tempfile::tempdir().unwrap();
    let r = vault(dir.path());
    r.patch("vault", patch(json!({ "label": "home" }))).unwrap();
    assert!(!dir.path().join(SecretStore::KEY_FILE).exists());
}

#[test]
fn list_item_secrets_are_kept_redacted_and_removable() {
    use super::integrations::Integrations;
    let dir = tempfile::tempdir().unwrap();
    let r = registry(dir.path(), &sealed_config(dir.path()));
    let item = |pw: Value| {
        let mut m = json!({
            "id": "home", "name": "Home", "kind": "qbittorrent",
            "url": "http://192.168.1.5:8080/"
        });
        if !pw.is_null() {
            m["password"] = pw;
        }
        m
    };
    let out = r
        .patch(
            "integrations",
            patch(json!({ "clients": [item(json!("hunter2"))] })),
        )
        .unwrap();
    let f = &out.fields["clients"];
    assert_eq!(f.secrets_set, vec!["home.password".to_string()]);
    assert!(f.value[0].get("password").is_none(), "{:?}", f.value);
    assert!(!format!("{out:?}{:?}", r.section("integrations")).contains("hunter2"));
    // Sent back without it (as GET returned it): kept.
    r.patch(
        "integrations",
        patch(
            json!({ "clients": [{ "id": "home", "name": "Renamed", "kind": "qbittorrent",
                                    "url": "http://192.168.1.5:8080/" }] }),
        ),
    )
    .unwrap();
    let c = r.get::<Integrations>();
    assert_eq!(c.clients[0].name, "Renamed");
    assert_eq!(c.clients[0].password.as_ref().unwrap().0, "hunter2");
    // At rest, sealed; not in the preview, an export or a log line.
    assert!(saved(&r).iter().all(|(_, v)| !v.contains("hunter2")));
    let shown = format!(
        "{:?}{}",
        r.preview(batch(
            json!({ "integrations": { "clients": [item(json!("hunter3"))] } })
        ))
        .unwrap(),
        r.export(true)
    );
    assert!(!shown.contains("hunter"), "{shown}");
    // An error never quotes it, even when the item is refused.
    let err = r
        .patch(
            "integrations",
            patch(
                json!({ "clients": [{ "id": "home", "name": "x", "kind": "qbittorrent",
                                        "url": "hunter4://x", "password": "hunter4" }] }),
            ),
        )
        .unwrap_err();
    assert!(!format!("{err:?}").contains("hunter4"), "{err:?}");
    // `""` removes it.
    let out = r
        .patch(
            "integrations",
            patch(json!({ "clients": [item(json!(""))] })),
        )
        .unwrap();
    assert!(out.fields["clients"].secrets_set.is_empty());
    assert!(r.get::<Integrations>().clients[0].password.is_none());
    // Import never sets one.
    let report = r
        .import(
            SettingsImport {
                toml: "[integrations]\nclients = []\ntorznab_api_key = \"x\"\n".into(),
                ..Default::default()
            },
            true,
        )
        .unwrap()
        .report;
    assert!(
        report.skipped.iter().all(|s| s.reason == "secret"),
        "{report:?}"
    );
    assert_eq!(report.skipped.len(), 2);
}

/// A section whose check compares two fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct Window {
    low: u32,
    high: u32,
}

impl Section for Window {
    const SECTION: &'static str = "window";
    const FIELDS: &'static [Field] = &[
        Field::new("low").group("window"),
        Field::new("high").group("window"),
    ];

    fn from_config(_: &Config) -> Self {
        Window { low: 5, high: 10 }
    }

    fn apply(&self, _: &str, _: &mut Config) {}

    fn check(&self, field: &str) -> Result<(), String> {
        if self.low > self.high {
            return Err(match field {
                "low" => "Must be at most the high mark.",
                _ => "Must be at least the low mark.",
            }
            .to_string());
        }
        Ok(())
    }
}

#[test]
fn checks_see_the_whole_patch() {
    let dir = tempfile::tempdir().unwrap();
    let c = config(dir.path(), None, &[]);
    let mut r = Registry::empty(open(dir.path()));
    r.register::<Window>(&c).unwrap();
    // Each value is fine beside the old other one; together they aren't.
    let err = r
        .patch("window", patch(json!({ "low": 8, "high": 6 })))
        .unwrap_err();
    assert_eq!(
        err,
        SettingsError::Invalid {
            field: "low".into(),
            message: "Must be at most the high mark.".into()
        }
    );
    assert_eq!(
        r.preview(batch(json!({ "window": { "low": 8, "high": 6 } })))
            .unwrap_err()
            .error,
        err
    );
    // And the other way round: each is wrong beside the old other one,
    // together they're fine.
    r.patch("window", patch(json!({ "low": 20, "high": 30 })))
        .unwrap();
    assert_eq!(r.get::<Window>(), Window { low: 20, high: 30 });
    // A reset is checked against the rest too: low back to 5 is fine,
    // high back to 10 isn't (low is 20).
    let err = r
        .patch("window", patch(json!({ "high": null })))
        .unwrap_err();
    assert!(matches!(err, SettingsError::Invalid { ref field, .. } if field == "high"));
    assert!(
        saved(&r)
            .iter()
            .any(|(k, v)| k == "settings.window.high" && v == "30")
    );
}
