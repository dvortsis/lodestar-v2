//! Saved values a newer rule refuses, at start (X-prefs-invisible-share):
//! preferences' shortcut keys keep every remap that still holds, any other
//! section loads exactly as before, and a patch is still refused.

use lodestar_api::settings::SettingsError;
use serde_json::{Value, json};

use super::crawler::{Crawler, Intensity};
use super::preferences::Preferences;
use super::tests::{config, logs, open, patch, registry};
use super::*;

fn keys(p: &Preferences) -> Vec<(&str, &str)> {
    p.shortcut_keys
        .iter()
        .map(|(a, k)| (a.as_str(), k.as_str()))
        .collect()
}

/// Saves `rows` as an older build would have, then starts a registry and
/// returns its preferences, its log and the rows the store now holds.
fn start_with(rows: &[(&str, &str)]) -> (Preferences, String, Vec<(String, String)>) {
    let dir = tempfile::tempdir().unwrap();
    open(dir.path())
        .lock()
        .unwrap()
        .set_settings_text(rows)
        .unwrap();
    let c = config(dir.path(), None, &[]);
    let mut r = None;
    let log = logs(|| r = Some(registry(dir.path(), &c)));
    let r = r.unwrap();
    let p = r.get::<Preferences>();
    let stored = r
        .store
        .lock()
        .unwrap()
        .settings_with_prefix("settings.preferences.")
        .unwrap();
    (p, log, stored)
}

#[test]
fn a_now_refused_shortcut_takes_its_default_and_the_rest_are_kept() {
    // U+0941 (a Devanagari vowel sign) and U+E0080 passed the old rule.
    let saved = json!({
        "results.next": "\u{0941}",
        "detail.open_magnet": "\u{e0080}",
        "search.focus": "f",
        "help.shortcuts": "h",
    })
    .to_string();
    let (p, log, stored) = start_with(&[
        ("settings.preferences.shortcut_keys", &saved),
        ("settings.preferences.theme", "\"nord\""),
    ]);
    assert_eq!(keys(&p), [("help.shortcuts", "h"), ("search.focus", "f")]);
    assert_eq!(p.theme, "nord");
    assert!(
        log.contains("section=preferences field=shortcut_keys reason=invalid_entries"),
        "{log}"
    );
    // Never the value.
    assert!(
        !log.contains('\u{0941}') && !log.contains("\\u{941}"),
        "{log}"
    );
    // Only in memory: the store keeps what was saved until the next save.
    let row = stored.iter().find(|(k, _)| k.ends_with("shortcut_keys"));
    assert_eq!(row.map(|(_, v)| v.as_str()), Some(saved.as_str()));
}

#[test]
fn a_restored_default_drops_the_remap_that_took_it_in_turn() {
    // results.next's key is refused, so it's j again; detail.send had taken
    // j, so it's s again; results.select had taken s, so it's x again.
    // search.focus's remap shares no key with any of them.
    let saved = json!({
        "results.next": "\u{200b}",
        "detail.send": "j",
        "results.select": "s",
        "search.focus": "f",
    })
    .to_string();
    let (p, log, _) = start_with(&[("settings.preferences.shortcut_keys", &saved)]);
    assert_eq!(keys(&p), [("search.focus", "f")]);
    assert!(log.contains("reason=invalid_entries"), "{log}");
}

#[test]
fn remaps_that_still_hold_load_untouched() {
    let saved = json!({ "results.next": "k", "results.previous": "j", "x.y": "ü" }).to_string();
    let (p, log, _) = start_with(&[("settings.preferences.shortcut_keys", &saved)]);
    assert_eq!(
        keys(&p),
        [
            ("results.next", "k"),
            ("results.previous", "j"),
            ("x.y", "ü")
        ]
    );
    assert!(!log.contains("shortcut_keys"), "{log}");
    assert!(
        Preferences::repair_saved("shortcut_keys", &serde_json::from_str(&saved).unwrap())
            .is_none()
    );
}

#[test]
fn what_repair_cannot_fix_is_ignored_as_before() {
    // Two remaps sharing a key were never valid: nothing is restored by a
    // drop, so the field is ignored whole, as any invalid saved value is.
    // Nor is one that a repair changes but can't make valid: it's ignored
    // whole, and logged once, as that.
    for saved in [
        json!({ "a.b": "q", "c.d": "q" }),
        json!({ "results.next": "\u{0941}", "a.b": "q", "c.d": "q" }),
    ] {
        let saved = saved.to_string();
        let (p, log, _) = start_with(&[("settings.preferences.shortcut_keys", &saved)]);
        assert!(p.shortcut_keys.is_empty(), "{saved}");
        assert!(log.contains("field=shortcut_keys reason=invalid"), "{log}");
        assert!(!log.contains("invalid_entries"), "{log}");
        assert_eq!(log.matches("field=shortcut_keys").count(), 1, "{log}");
    }
    // Nor is a value of the wrong shape repaired.
    for v in [json!("j"), json!(["j"]), json!({ "a.b": 1 })] {
        assert!(
            Preferences::repair_saved("shortcut_keys", &v).is_none(),
            "{v}"
        );
    }
    assert!(Preferences::repair_saved("theme", &json!("nord")).is_none());
}

#[test]
fn a_section_without_a_repair_loads_exactly_as_before() {
    let dir = tempfile::tempdir().unwrap();
    open(dir.path())
        .lock()
        .unwrap()
        .set_settings_text(&[
            ("settings.crawler.intensity", "\"low\""),
            ("settings.crawler.timeout_secs", "\"soon\""),
        ])
        .unwrap();
    let c = config(dir.path(), None, &[]);
    let mut r = None;
    let log = logs(|| r = Some(registry(dir.path(), &c)));
    let crawler = r.unwrap().get::<Crawler>();
    assert_eq!(crawler.intensity, Intensity::Low);
    assert_eq!(crawler.timeout_secs, Crawler::from_config(&c).timeout_secs);
    assert!(log.contains("field=timeout_secs reason=invalid"), "{log}");
    assert!(!log.contains("invalid_entries"), "{log}");
    for f in Crawler::FIELDS {
        assert!(Crawler::repair_saved(f.name, &Value::Null).is_none());
    }
}

#[test]
fn a_patch_with_a_refused_key_is_still_refused() {
    let dir = tempfile::tempdir().unwrap();
    let r = registry(dir.path(), &config(dir.path(), None, &[]));
    for key in ["\u{0941}", "\u{1d173}", "\u{e0080}", "\u{200b}"] {
        let body = json!({ "shortcut_keys": { "results.next": key, "search.focus": "f" } });
        match r.patch("preferences", patch(body)) {
            Err(SettingsError::Invalid { field, .. }) => assert_eq!(field, "shortcut_keys"),
            other => panic!("{key:?}: {other:?}"),
        }
    }
    assert!(r.get::<Preferences>().shortcut_keys.is_empty());
}
