//! Preferences (§10.10.1, §10.10.9): how the UI looks and behaves for
//! the account (theme, mode, density, motion, illustrations, shortcuts,
//! language, time zone, first day of the week). They change nothing on
//! the server, apply at once, and have no `config.toml` key or
//! environment variable.
//!
//! **Per account.** Lodestar has one admin account, so there's one set,
//! saved under `settings.preferences.<field>`. When accounts arrive, a
//! migration moves those keys to the first account's; nothing else
//! changes (`docs/decisions/2026-10-22-B2b.md`).
//!
//! "Show adult content" isn't here: it's the server-wide
//! `content.show_adult` (B17).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{Auto, Field, Section};
use crate::config::Config;

/// Light, dark, or following the system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    System,
    Light,
    Dark,
}

/// Result rows (§10.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Density {
    Compact,
    Comfortable,
    Cards,
}

/// Animation: following the system's `prefers-reduced-motion`, always
/// reduced, or always full.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Motion {
    System,
    Reduced,
    Full,
}

/// Where calendars start; `auto` follows the language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum FirstDay {
    Auto,
    Monday,
    Saturday,
    Sunday,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    /// The theme family (`riverstone`, or a custom theme's id).
    pub theme: String,
    /// A different family in dark mode; empty uses `theme`.
    pub theme_dark: String,
    #[schema(inline)]
    pub mode: Mode,
    #[schema(inline)]
    pub density: Density,
    #[schema(inline)]
    pub motion: Motion,
    /// Show illustrations in empty and onboarding states.
    pub illustrations: bool,
    /// Single-key shortcuts (`j`, `k`, `/`…). They never fire while
    /// typing.
    pub shortcuts: bool,
    /// Remapped shortcuts: an action's id and its key. Actions left out
    /// keep their default key.
    pub shortcut_keys: BTreeMap<String, String>,
    /// The UI language as a BCP 47 tag (`en`, `zh-CN`); empty follows the
    /// browser.
    pub language: String,
    /// An IANA time zone (`Europe/Berlin`); empty follows the browser.
    pub time_zone: String,
    #[schema(inline)]
    pub first_day_of_week: FirstDay,
}

impl Default for Preferences {
    fn default() -> Self {
        Preferences {
            theme: "riverstone".to_string(),
            theme_dark: String::new(),
            mode: Mode::System,
            density: Density::Comfortable,
            motion: Motion::System,
            illustrations: true,
            shortcuts: true,
            shortcut_keys: BTreeMap::new(),
            language: String::new(),
            time_zone: String::new(),
            first_day_of_week: FirstDay::Auto,
        }
    }
}

/// Theme ids, action ids and the like.
const MAX_ID: usize = 64;
/// Remapped shortcuts.
const MAX_SHORTCUTS: usize = 64;

/// A theme id: lowercase words joined by `-` (as `themes.json` names
/// them).
fn theme_id(id: &str) -> bool {
    id.len() <= MAX_ID
        && !id.is_empty()
        && id.split('-').all(|w| {
            !w.is_empty()
                && w.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

/// A BCP 47 tag's shape: a 2–3 letter language, then up to four
/// subtags of 1–8 letters or digits.
fn language_tag(tag: &str) -> bool {
    let mut parts = tag.split('-');
    let lang = parts.next().unwrap_or("");
    let rest: Vec<&str> = parts.collect();
    (2..=3).contains(&lang.len())
        && lang.bytes().all(|b| b.is_ascii_alphabetic())
        && rest.len() <= 4
        && rest
            .iter()
            .all(|p| (1..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// An IANA zone name's shape (`UTC`, `America/Argentina/Buenos_Aires`,
/// `Etc/GMT+5`). Whether the zone exists is the browser's to say.
fn time_zone(tz: &str) -> bool {
    let parts: Vec<&str> = tz.split('/').collect();
    tz.len() <= MAX_ID
        && parts.len() <= 3
        && parts.iter().all(|p| {
            p.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'+'))
        })
}

/// An action id (`results.next`).
fn action_id(id: &str) -> bool {
    id.len() <= MAX_ID
        && id.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.'))
}

/// The single-key shortcuts (§10.16) and the keys they have by default.
/// A remap is checked against these, so two actions never share a key.
pub const DEFAULT_SHORTCUTS: &[(&str, &str)] = &[
    ("search.focus", "/"),
    ("results.next", "j"),
    ("results.previous", "k"),
    ("detail.open_magnet", "o"),
    ("detail.copy_magnet", "c"),
    ("detail.send", "s"),
    ("results.select", "x"),
    ("help.shortcuts", "?"),
];

/// A character a shortcut key can't be, because the UI couldn't show it as
/// the key (§10.16):
/// - **default-ignorable** ([`crate::text::default_ignorable`]): it renders
///   as nothing, in any font;
/// - **a format character** (general category Cf,
///   [`lodestar_metainfo::format_char`]): invisible, or it reorders the
///   text around it. That includes the few Cf characters some fonts draw,
///   such as the interlinear annotation marks U+FFF9–FFFB and the Arabic
///   number signs U+0600–0605: they mark up the text around them and aren't
///   keys anyone types, so refusing them costs nothing;
/// - **a combining mark** (Mn, Mc or Me, in any script): it draws only on
///   the character before it, so on its own it shows as a dotted circle or
///   nothing.
///
/// Controls and whitespace are refused by [`shortcut_key`] itself.
fn invisible(c: char) -> bool {
    crate::text::default_ignorable(c)
        || lodestar_metainfo::format_char(c)
        || unicode_normalization::char::is_combining_mark(c)
}

/// A single-key shortcut: one visible character.
fn shortcut_key(key: &str) -> bool {
    let mut chars = key.chars();
    matches!(
        (chars.next(), chars.next()),
        (Some(c), None) if !c.is_control() && !c.is_whitespace() && !invisible(c)
    )
}

impl Section for Preferences {
    const SECTION: &'static str = "preferences";
    const FIELDS: &'static [Field] = &[
        Field::new("theme")
            .group("appearance")
            .widget("theme")
            .keywords(),
        Field::new("theme_dark")
            .group("appearance")
            .widget("theme")
            .advanced()
            .auto(Auto::Str("")),
        Field::new("mode").group("appearance").keywords(),
        Field::new("density").group("appearance"),
        Field::new("motion").group("appearance").keywords(),
        Field::new("illustrations").group("appearance"),
        Field::new("shortcuts").group("keyboard").keywords(),
        Field::new("shortcut_keys")
            .group("keyboard")
            .widget("shortcuts"),
        Field::new("language")
            .group("region")
            .widget("language")
            .auto(Auto::Str(""))
            .keywords(),
        Field::new("time_zone")
            .group("region")
            .widget("time-zone")
            .auto(Auto::Str("")),
        Field::new("first_day_of_week").group("region"),
    ];

    /// Nothing in `config.toml` sets a preference.
    fn from_config(_: &Config) -> Self {
        Preferences::default()
    }

    /// Nor does a preference change how the server runs.
    fn apply(&self, _: &str, _: &mut Config) {}

    /// Saved remaps with a key this build refuses (an older build allowed
    /// it): see [`repair_shortcuts`].
    fn repair_saved(field: &str, value: &serde_json::Value) -> Option<serde_json::Value> {
        if field != "shortcut_keys" {
            return None;
        }
        let keys: BTreeMap<String, String> = serde_json::from_value(value.clone()).ok()?;
        if check_shortcuts(&keys).is_ok() {
            return None;
        }
        let repaired = repair_shortcuts(&keys);
        (repaired != keys).then(|| serde_json::to_value(repaired).expect("a map of strings"))
    }

    fn check(&self, field: &str) -> Result<(), String> {
        let ok = match field {
            "theme" => theme_id(&self.theme),
            "theme_dark" => self.theme_dark.is_empty() || theme_id(&self.theme_dark),
            "language" => self.language.is_empty() || language_tag(&self.language),
            "time_zone" => self.time_zone.is_empty() || time_zone(&self.time_zone),
            "shortcut_keys" => return check_shortcuts(&self.shortcut_keys),
            _ => true,
        };
        if ok {
            return Ok(());
        }
        Err(match field {
            "theme" | "theme_dark" => "Must be a theme's id, such as riverstone.",
            "language" => "Must be a language tag, such as en or zh-CN, or empty.",
            _ => "Must be a time zone, such as Europe/Berlin, or empty.",
        }
        .to_string())
    }
}

/// Saved remaps as this build reads them, when they no longer pass
/// [`check_shortcuts`]: each remap whose key is now refused is dropped, so
/// its action has its default key again. That default can be a key another
/// remap took, so a remap that now shares a key with a restored default is
/// dropped too, until nothing changes. The other remaps are kept. Whatever
/// is still wrong is left to the check, which then ignores the field as for
/// any invalid saved value. At most [`MAX_SHORTCUTS`] rounds.
fn repair_shortcuts(keys: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut keys = keys.clone();
    keys.retain(|_, key| shortcut_key(key));
    loop {
        let restored: Vec<&str> = DEFAULT_SHORTCUTS
            .iter()
            .filter(|(action, _)| !keys.contains_key(*action))
            .map(|(_, key)| *key)
            .collect();
        let clash: Vec<String> = keys
            .iter()
            .filter(|(_, key)| restored.contains(&key.as_str()))
            .map(|(action, _)| action.clone())
            .collect();
        if clash.is_empty() {
            return keys;
        }
        for action in clash {
            keys.remove(&action);
        }
    }
}

fn check_shortcuts(keys: &BTreeMap<String, String>) -> Result<(), String> {
    if keys.len() > MAX_SHORTCUTS {
        return Err(format!(
            "At most {MAX_SHORTCUTS} shortcuts can be remapped."
        ));
    }
    for (action, key) in keys {
        if !action_id(action) {
            return Err("Each shortcut must name an action, such as results.next.".to_string());
        }
        if !shortcut_key(key) {
            return Err(format!("{action}: a shortcut is one key, such as j or /."));
        }
    }
    // Every action's key as it would be: its remap, else its default.
    let mut taken: BTreeMap<&str, &str> = BTreeMap::new();
    let defaults = DEFAULT_SHORTCUTS
        .iter()
        .filter(|(action, _)| !keys.contains_key(*action))
        .map(|(a, k)| (*a, *k));
    let remapped = keys.iter().map(|(a, k)| (a.as_str(), k.as_str()));
    for (action, key) in defaults.chain(remapped) {
        if let Some(other) = taken.insert(key, action) {
            return Err(format!(
                "{other} and {action} both use {key}; give each its own key."
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_tags_and_zones() {
        for ok in ["riverstone", "tokyo-night", "rose-pine", "custom-2"] {
            assert!(theme_id(ok), "{ok}");
        }
        for bad in [
            "",
            "Riverstone",
            "a--b",
            "-a",
            "a b",
            "x/../y",
            &"a".repeat(65),
        ] {
            assert!(!theme_id(bad), "{bad}");
        }
        for ok in ["en", "zh-CN", "zh-Hant-TW", "ast", "de-1996"] {
            assert!(language_tag(ok), "{ok}");
        }
        for bad in ["", "e", "english", "en_US", "en-", "en-toolongsubtag", "1n"] {
            assert!(!language_tag(bad), "{bad}");
        }
        for ok in [
            "UTC",
            "Europe/Berlin",
            "America/Argentina/Buenos_Aires",
            "Etc/GMT+5",
        ] {
            assert!(time_zone(ok), "{ok}");
        }
        for bad in [
            "",
            "/Berlin",
            "Europe/",
            "../etc/passwd",
            "a/b/c/d",
            "Europe/Ber lin",
        ] {
            assert!(!time_zone(bad), "{bad}");
        }
    }

    #[test]
    fn shortcuts_are_single_keys_without_conflicts() {
        let map = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs
                .iter()
                .map(|(a, k)| (a.to_string(), k.to_string()))
                .collect()
        };
        assert!(check_shortcuts(&map(&[("results.next", "n"), ("results.prev", "p")])).is_ok());
        assert!(check_shortcuts(&map(&[("search.focus", "/"), ("x", "ü")])).is_ok());
        let conflict = check_shortcuts(&map(&[("a", "j"), ("b", "j")])).unwrap_err();
        assert!(conflict.contains("both use j"), "{conflict}");
        // Against the defaults too: k is still results.previous's.
        let conflict = check_shortcuts(&map(&[("results.next", "k")])).unwrap_err();
        assert!(
            conflict.contains("results.previous and results.next both use k"),
            "{conflict}"
        );
        let conflict = check_shortcuts(&map(&[("custom.thing", "/")])).unwrap_err();
        assert!(conflict.contains("search.focus"), "{conflict}");
        // Swapping two defaults is fine.
        assert!(check_shortcuts(&map(&[("results.next", "k"), ("results.previous", "j")])).is_ok());
        for (action, key) in [
            ("a", ""),
            ("a", "jk"),
            ("a", " "),
            ("a", "\u{7}"),
            ("A", "j"),
            ("", "j"),
            // Invisible: a zero-width space, a lone combining accent, a
            // bidirectional override, the BOM.
            ("a", "\u{200B}"),
            ("a", "\u{0301}"),
            ("a", "\u{202E}"),
            ("a", "\u{FEFF}"),
        ] {
            assert!(
                check_shortcuts(&map(&[(action, key)])).is_err(),
                "{action:?} {key:?}"
            );
        }
        let many: BTreeMap<String, String> = (0..65)
            .map(|i| {
                (
                    format!("a{i}"),
                    char::from_u32(0x4e00 + i).unwrap().to_string(),
                )
            })
            .collect();
        assert!(check_shortcuts(&many).is_err());
    }

    /// Over all of `char`, a one-character key is refused exactly when it's
    /// a control, whitespace, default-ignorable, Cf or a mark (Mn, Mc, Me),
    /// the categories read from `unicode-properties`, not from the tables
    /// the rule uses.
    #[test]
    fn a_shortcut_key_is_one_character_that_shows() {
        use unicode_properties::{GeneralCategory as G, UnicodeGeneralCategory};
        let mut refused = 0;
        let mut newer = Vec::new();
        for c in (0..=0x10FFFF).filter_map(char::from_u32) {
            // The rule's mark table can follow a later Unicode than the
            // reference (unicode-normalization 0.1.26 is Unicode 18,
            // unicode-properties 0.1.4 is 17): a code point the reference
            // leaves unassigned may be a mark in the rule's. Those are
            // counted, never compared.
            if c.general_category() == G::Unassigned && !crate::text::default_ignorable(c) {
                if !shortcut_key(&c.to_string()) {
                    newer.push(c);
                }
                continue;
            }
            let hidden = c.is_control()
                || c.is_whitespace()
                || crate::text::default_ignorable(c)
                || matches!(
                    c.general_category(),
                    G::Format | G::NonspacingMark | G::SpacingMark | G::EnclosingMark
                );
            refused += usize::from(hidden);
            assert_eq!(
                shortcut_key(&c.to_string()),
                !hidden,
                "{:?}",
                c.escape_unicode()
            );
        }
        assert!(refused > 2_000, "{refused}");
        // Unicode 18's new marks, and nothing like a whole plane.
        assert!(newer.len() < 500, "{}: {newer:?}", newer.len());
        for c in [
            '\u{0941}',  // Devanagari vowel sign U (Mn), passed before
            '\u{093e}',  // Devanagari vowel sign AA (Mc)
            '\u{20dd}',  // combining enclosing circle (Me)
            '\u{1d173}', // musical symbol begin beam, default-ignorable
            '\u{e0080}', // unassigned in plane 14, default-ignorable
            '\u{200b}',  // zero-width space
            '\u{fffa}',  // interlinear annotation separator (Cf)
        ] {
            assert!(!shortcut_key(&c.to_string()), "{:?}", c.escape_unicode());
        }
        for key in ["a", "ü", "/", "?", "字", "\u{0905}"] {
            assert!(shortcut_key(key), "{key:?}");
        }
    }

    #[test]
    fn repair_drops_refused_keys_and_what_their_defaults_displace() {
        let map = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs
                .iter()
                .map(|(a, k)| (a.to_string(), k.to_string()))
                .collect()
        };
        // Nothing refused, no clash: unchanged.
        let fine = map(&[("results.next", "k"), ("results.previous", "j")]);
        assert_eq!(repair_shortcuts(&fine), fine);
        // A chain: next's j comes back, so send's j goes and its s comes
        // back, so select's s goes. search.focus's f stays.
        let chain = map(&[
            ("results.next", "\u{0941}"),
            ("detail.send", "j"),
            ("results.select", "s"),
            ("search.focus", "f"),
        ]);
        let repaired = repair_shortcuts(&chain);
        assert_eq!(repaired, map(&[("search.focus", "f")]));
        assert!(check_shortcuts(&repaired).is_ok());
        // Every remap refused: none left, the defaults hold.
        let all = map(&[("results.next", "\u{200b}"), ("x.y", "\u{e0080}")]);
        assert!(repair_shortcuts(&all).is_empty());
    }

    #[test]
    fn the_defaults_follow_the_browser_and_system() {
        let p = Preferences::default();
        assert_eq!(
            (p.theme.as_str(), p.mode, p.motion, p.language.as_str()),
            ("riverstone", Mode::System, Motion::System, "")
        );
        for f in Preferences::FIELDS {
            assert!(p.check(f.name).is_ok(), "{}", f.name);
            assert!(f.file.is_none() && f.env.is_none() && !f.restart_required);
        }
    }
}
