//! Export and import (§10.10.6): the values saved in the app, as a TOML
//! file laid out by section, and that file read back through the same
//! checks as a batch patch.
//!
//! **Export** holds only values saved in the app: never a secret, a value
//! an environment variable sets, or a read-only field; `preferences` only
//! when asked. Filter, floor, pack and subscription state isn't in any
//! section (§20.7), so it can't be in the file. Each key carries its
//! `config.toml` key and `LODESTAR_*` variable in a comment. The layout
//! follows the sections rather than `config.toml`'s tables because they
//! don't map one to one (`network.dht_port` is the port of `dht.bind`'s
//! addresses), and the file must read back exactly.
//!
//! **Import** parses the file (at most [`MAX_IMPORT`] bytes; the TOML
//! parser bounds its own nesting), lists every key it leaves alone
//! (unknown, read-only, secret, set by the environment) and every value
//! it changes, then saves the changes as one batch, all or nothing. A
//! file's values equal to the current ones aren't changes.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use lodestar_api::settings::{
    IF_REVISION, ImportChange, ImportReport, ImportSkip, Imported, MAX_IMPORT, SettingSource,
    SettingsBatch, SettingsError, SettingsFailure, SettingsImport, SettingsPatch,
};
use serde_json::{Map, Value};

use super::content::CONFIRM_ADULT;
use super::content::Content;
use super::preferences::Preferences;
use super::{Registry, Section};

/// The top-level key naming the file's format.
const FORMAT_KEY: &str = "lodestar_settings";
/// The format this build writes and reads.
const FORMAT: i64 = 1;

/// The saved settings as TOML (see the module docs).
pub(super) fn export(registry: &Registry, include_preferences: bool) -> String {
    let mut out = String::from(
        "# Lodestar settings: the values saved in the app.\n\
         # Import this file in Settings > General > Backup & portability.\n\
         # Never included: secrets, values set by environment variables, and\n\
         # filter, floor, pack or subscription state.\n",
    );
    let _ = writeln!(out, "{FORMAT_KEY} = {FORMAT}");
    for s in &registry.sections {
        if s.name() == Preferences::SECTION && !include_preferences {
            continue;
        }
        let saved: Vec<_> = s
            .current()
            .into_iter()
            .filter(|(f, _, source, env)| {
                *source == SettingSource::Db && !env && !f.read_only && !f.secret
            })
            .collect();
        if saved.is_empty() {
            continue;
        }
        let _ = write!(out, "\n[{}]\n", s.name());
        for (f, value, _, _) in saved {
            let where_ = [
                f.file.map(|p| format!("config.toml: {p}")),
                f.env.map(str::to_string),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
            if !where_.is_empty() {
                let _ = writeln!(out, "# {}", where_.join(" · "));
            }
            let _ = writeln!(out, "{} = {}", toml_key(f.name), toml_value(&value));
        }
    }
    out
}

/// A key as TOML writes it: bare when it can be.
fn toml_key(k: &str) -> String {
    let bare = !k.is_empty()
        && k.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if bare { k.to_string() } else { toml_string(k) }
}

/// A TOML basic string.
fn toml_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A JSON value as a TOML value. Settings have no nulls (null resets a
/// field), so one never reaches here; it's written as an empty string to
/// keep the file readable.
fn toml_value(v: &Value) -> String {
    match v {
        Value::Null => "\"\"".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => toml_string(s),
        Value::Array(a) => {
            let items: Vec<String> = a.iter().map(toml_value).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Object(m) => {
            let items: Vec<String> = m
                .iter()
                .map(|(k, v)| format!("{} = {}", toml_key(k), toml_value(v)))
                .collect();
            if items.is_empty() {
                "{}".to_string()
            } else {
                format!("{{ {} }}", items.join(", "))
            }
        }
    }
}

fn invalid(message: impl Into<String>) -> SettingsFailure {
    SettingsFailure::from(SettingsError::InvalidImport(message.into()))
}

/// A TOML value as JSON; `None` for a float JSON can't hold.
fn json_value(v: toml::Value) -> Option<Value> {
    Some(match v {
        toml::Value::String(s) => Value::String(s),
        toml::Value::Integer(n) => Value::from(n),
        toml::Value::Float(f) => Value::Number(serde_json::Number::from_f64(f)?),
        toml::Value::Boolean(b) => Value::Bool(b),
        toml::Value::Datetime(d) => Value::String(d.to_string()),
        toml::Value::Array(a) => {
            Value::Array(a.into_iter().map(json_value).collect::<Option<_>>()?)
        }
        toml::Value::Table(t) => Value::Object(
            t.into_iter()
                .map(|(k, v)| Some((k, json_value(v)?)))
                .collect::<Option<_>>()?,
        ),
    })
}

/// The line `offset` falls on, counting from 1.
fn line_of(text: &str, offset: usize) -> usize {
    text.as_bytes()[..offset.min(text.len())]
        .iter()
        .filter(|&&b| b == b'\n')
        .count()
        + 1
}

/// Reads `import.toml`, reports the diff, and unless `dry_run` saves it.
pub(super) fn import(
    registry: &Registry,
    import: SettingsImport,
    dry_run: bool,
) -> Result<Imported, SettingsFailure> {
    if import.toml.len() > MAX_IMPORT {
        return Err(SettingsError::ImportTooLarge.into());
    }
    // The parser's own message, never the line it quotes.
    let table: toml::Table = toml::from_str(&import.toml).map_err(|e| {
        let at = e
            .span()
            .map(|s| format!(" (line {})", line_of(&import.toml, s.start)))
            .unwrap_or_default();
        invalid(format!(
            "This isn't a settings file: {}{at}.",
            e.message().trim_end_matches('.')
        ))
    })?;
    let mut skipped = Vec::new();
    let skip = |key: String, reason: &str, env: Option<&str>| ImportSkip {
        env: env.map(str::to_string),
        key,
        reason: reason.to_string(),
    };
    let mut patches: BTreeMap<String, Map<String, Value>> = BTreeMap::new();
    let mut changes = Vec::new();
    let mut unchanged = 0u32;
    // The whole check runs under the write lock, so the diff and the save
    // see the same values.
    let _write = registry.write.lock().expect("settings write lock");
    for (name, value) in table {
        if name == FORMAT_KEY {
            if value.as_integer() != Some(FORMAT) {
                return Err(invalid(
                    "This file is from a newer Lodestar, or isn't a settings export.",
                ));
            }
            continue;
        }
        let (Some(section), toml::Value::Table(fields)) = (registry.find(&name), value) else {
            skipped.push(skip(name, "unknown", None));
            continue;
        };
        let current = section.current();
        for (field_name, v) in fields {
            let full = format!("{name}.{field_name}");
            let Some((f, now, source, env)) = current.iter().find(|(f, ..)| f.name == field_name)
            else {
                skipped.push(skip(full, "unknown", None));
                continue;
            };
            if f.read_only {
                skipped.push(skip(full, "read_only", None));
            } else if f.secret {
                skipped.push(skip(full, "secret", None));
            } else if *env {
                skipped.push(skip(full, "set_by_environment", f.env));
            } else {
                let Some(to) = json_value(v) else {
                    return Err(SettingsFailure::new(
                        name,
                        SettingsError::Invalid {
                            field: field_name,
                            message: "Not a valid value: not a finite number.".to_string(),
                        },
                    ));
                };
                if &to == now {
                    unchanged += 1;
                    continue;
                }
                changes.push(ImportChange {
                    field: field_name.clone(),
                    from: now.clone(),
                    from_source: *source,
                    restart_required: f.restart_required,
                    section: name.clone(),
                    to: to.clone(),
                });
                patches
                    .entry(name.clone())
                    .or_default()
                    .insert(field_name, to);
            }
        }
    }
    for (name, revision) in import.revisions.iter().flatten() {
        patches
            .entry(name.clone())
            .or_default()
            .insert(IF_REVISION.to_string(), Value::from(*revision));
    }
    if import.confirm_adult
        && let Some(content) = patches.get_mut(Content::SECTION)
    {
        content.insert(CONFIRM_ADULT.to_string(), Value::Bool(true));
    }
    let batch = SettingsBatch(
        patches
            .into_iter()
            .map(|(k, v)| (k, SettingsPatch(v)))
            .collect(),
    );
    // Exactly the batch patch's checks, all or nothing.
    let plans = registry.plan(&batch)?;
    let apply = !dry_run && !changes.is_empty();
    let changed = if apply {
        registry.commit(plans)?.changed
    } else {
        Vec::new()
    };
    let revisions = registry
        .sections
        .iter()
        .map(|s| (s.name().to_string(), s.values().revision))
        .collect();
    Ok(Imported {
        report: ImportReport {
            applied: apply,
            changes,
            revisions,
            skipped,
            unchanged,
        },
        changed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_written_as_toml_reads_them() {
        let cases = [
            Value::from(true),
            Value::from(12),
            Value::from(-3),
            Value::from("plain"),
            Value::from("quote \" back\\slash \n tab\t \u{7} ünï 漢"),
            serde_json::json!(["a", 1, false]),
            serde_json::json!({ "j": "next", "needs quoting": "x", "": "empty" }),
            serde_json::json!({}),
        ];
        for v in cases {
            let text = format!("k = {}\n", toml_value(&v));
            let back: toml::Table = toml::from_str(&text).unwrap_or_else(|e| panic!("{text}: {e}"));
            let got = json_value(back["k"].clone()).unwrap();
            assert_eq!(got, v, "{text}");
        }
    }

    #[test]
    fn lines_count_from_one() {
        assert_eq!(line_of("a\nb\nc", 0), 1);
        assert_eq!(line_of("a\nb\nc", 2), 2);
        assert_eq!(line_of("a\nb\nc", 99), 3);
    }
}
