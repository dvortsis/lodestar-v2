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

/// A character that draws nothing on its own: a format character
/// (zero-width, bidirectional controls, the BOM, the soft hyphen) or a
/// combining mark. Without a Unicode table, the blocks that hold them.
fn invisible(c: char) -> bool {
    matches!(
        c as u32,
        0x00AD
            | 0x034F
            | 0x061C
            | 0x115F..=0x1160
            | 0x17B4..=0x17B5
            | 0x180B..=0x180F
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x206F
            | 0x3164
            | 0xFE00..=0xFE0F
            | 0xFEFF
            | 0xFFA0
            | 0xFFF0..=0xFFFB
            | 0x0300..=0x036F
            | 0x0483..=0x0489
            | 0x0591..=0x05BD
            | 0x0610..=0x061A
            | 0x064B..=0x065F
            | 0x1AB0..=0x1AFF
            | 0x1DC0..=0x1DFF
            | 0x20D0..=0x20FF
            | 0xFE20..=0xFE2F
            | 0xE0000..=0xE007F
            | 0xE0100..=0xE01EF
    )
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
