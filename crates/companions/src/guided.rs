//! Companions Lodestar didn't install: the record of one you added by
//! detection or by a URL, and confirmed or guided (plan
//! 2026-11-06-v06-companions CMP-5g, Q12, Q19; architecture §18.10).
//!
//! **Where it's kept** (runner, 2026-11-09): three kinds of `settings`
//! rows, no migration, and nothing under `<data>/companions/<id>/`, which
//! CMP-5's recovery treats as Lodestar's own:
//!
//! - `companions.<id>.target`: where it is, as JSON of [`StoredTarget`]:
//!   `{"kind":"loopback","port":N}`, `{"kind":"container","port":N}` or
//!   `{"kind":"url","url":"…"}`. A URL is CMP-0's [`EnteredUrl`], which
//!   refuses user names and passwords, queries, fragments and any scheme
//!   but `http` and `https`, so no credential lands in this non-secret row.
//! - `companions.<id>.origin`: [`Origin`], `confirmed` or `guided`.
//! - `companions.<id>.provenance.<field>`: [`Provenance`], `pasted` or
//!   `generated`, for the companion's one credential field
//!   ([`crate::manager::field_of`]); no other field is accepted. It sits
//!   outside CMP-3's `secret.` prefix: it isn't a secret.
//!
//! **Read strictly.** Each row is parsed into its closed type, and
//! anything unparseable, unknown or out of place (a port the catalogue
//! doesn't list for the companion, a container target outside a container,
//! a field that isn't the companion's) reads as no record. Nothing is
//! guessed or repaired. Rows are written by CMP-5g's endpoints only.

use serde::{Deserialize, Serialize};

use crate::adapter::{EnteredUrl, Target};
use crate::catalogue::Entry;
use crate::credentials::{Field, ROW_PREFIX};
use crate::detect::{Runtime, probe_targets};
use crate::inputs::{CompanionId, PortNumber};
use crate::manager::field_of;

/// The longest row value read, in bytes. A target is a short JSON object.
const MAX_ROW: usize = 4096;

/// How you came to have it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Detected or entered, and you confirmed its configuration.
    Confirmed,
    /// A guided install (Docker, Unraid): you pasted its key or temporary
    /// password (Q19).
    Guided,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Confirmed => "confirmed",
            Origin::Guided => "guided",
        }
    }

    pub fn parse(s: &str) -> Option<Origin> {
        match s {
            "confirmed" => Some(Origin::Confirmed),
            "guided" => Some(Origin::Guided),
            _ => None,
        }
    }
}

/// Where a stored credential came from. Only a generated one may ever be
/// revealed (Q5; CMP-6's decisions): a pasted one is never echoed back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    Pasted,
    Generated,
}

impl Provenance {
    pub fn as_str(self) -> &'static str {
        match self {
            Provenance::Pasted => "pasted",
            Provenance::Generated => "generated",
        }
    }

    pub fn parse(s: &str) -> Option<Provenance> {
        match s {
            "pasted" => Some(Provenance::Pasted),
            "generated" => Some(Provenance::Generated),
            _ => None,
        }
    }
}

/// A target as its row holds it. Converted to an adapter [`Target`] only
/// against the catalogue entry and the runtime ([`StoredTarget::target`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StoredTarget {
    Loopback { port: u16 },
    Container { port: u16 },
    Url { url: String },
}

impl StoredTarget {
    /// The row for an adapter target.
    pub fn of(target: &Target) -> StoredTarget {
        match target {
            Target::Loopback(p) => StoredTarget::Loopback { port: p.get() },
            Target::Container(_, p) => StoredTarget::Container { port: p.get() },
            Target::Entered(u) => StoredTarget::Url {
                url: u.as_str().to_owned(),
            },
        }
    }

    /// The adapter target, if it's one detection may use for `entry` here
    /// (Q12): loopback or, in a container, the entry's container name, on
    /// one of its internal ports; or a URL that still parses. `None`
    /// otherwise.
    pub fn target(&self, entry: &Entry, runtime: Runtime) -> Option<Target> {
        let t = match self {
            StoredTarget::Loopback { port } => Target::Loopback(PortNumber::new(*port).ok()?),
            StoredTarget::Container { port } => {
                let name = entry.container.as_ref()?.name().clone();
                Target::Container(name, PortNumber::new(*port).ok()?)
            }
            StoredTarget::Url { url } => return EnteredUrl::parse(url).ok().map(Target::Entered),
        };
        probe_targets(entry, runtime).contains(&t).then_some(t)
    }

    pub fn to_row(&self) -> String {
        serde_json::to_string(self).expect("a closed type serializes")
    }

    pub fn from_row(s: &str) -> Option<StoredTarget> {
        if s.len() > MAX_ROW {
            return None;
        }
        serde_json::from_str(s).ok()
    }
}

/// `companions.<id>.target`.
pub fn target_key(id: &CompanionId) -> String {
    format!("{ROW_PREFIX}{id}.target")
}

/// `companions.<id>.origin`.
pub fn origin_key(id: &CompanionId) -> String {
    format!("{ROW_PREFIX}{id}.origin")
}

/// `companions.<id>.provenance.<field>`, for the companion's own field
/// only.
pub fn provenance_key(id: &CompanionId, field: Field) -> Option<String> {
    (field_of(id) == Some(field)).then(|| format!("{ROW_PREFIX}{id}.provenance.{}", field.as_str()))
}

/// Every row key this module reads or writes for `id`, for removal.
pub fn row_keys(id: &CompanionId) -> Vec<String> {
    let mut keys = vec![target_key(id), origin_key(id)];
    keys.extend(field_of(id).and_then(|f| provenance_key(id, f)));
    keys
}

/// A companion you added and confirmed: where it is and how it came.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub id: CompanionId,
    pub target: Target,
    pub origin: Origin,
}

impl Record {
    /// Reads `entry`'s record through `row` (a row key to its text).
    /// `None` unless both rows are there and each is valid here.
    pub fn read(
        entry: &Entry,
        runtime: Runtime,
        row: impl Fn(&str) -> Option<String>,
    ) -> Option<Record> {
        let target =
            StoredTarget::from_row(&row(&target_key(&entry.id))?)?.target(entry, runtime)?;
        let origin = Origin::parse(&row(&origin_key(&entry.id))?)?;
        Some(Record {
            id: entry.id.clone(),
            target,
            origin,
        })
    }
}

/// The provenance of `id`'s credential `field`, through `row`. `None` for
/// a field that isn't the companion's, no row, or one that doesn't parse.
pub fn provenance(
    id: &CompanionId,
    field: Field,
    row: impl Fn(&str) -> Option<String>,
) -> Option<Provenance> {
    Provenance::parse(&row(&provenance_key(id, field)?)?)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn entry(id: &str) -> &'static Entry {
        crate::builtin().unwrap().get(id).unwrap()
    }

    fn rows(pairs: &[(String, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.clone(), (*v).to_owned()))
            .collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn keys_are_namespaced_and_outside_the_secret_prefix() {
        let q = CompanionId::parse("qbittorrent").unwrap();
        assert_eq!(target_key(&q), "companions.qbittorrent.target");
        assert_eq!(origin_key(&q), "companions.qbittorrent.origin");
        assert_eq!(
            provenance_key(&q, Field::WebUiPassword).as_deref(),
            Some("companions.qbittorrent.provenance.webui_password")
        );
        assert_eq!(provenance_key(&q, Field::ApiKey), None);
        for k in row_keys(&q) {
            assert!(!crate::credentials::is_secret_row(&k), "{k}");
        }
        let g = CompanionId::parse("gluetun").unwrap();
        assert_eq!(row_keys(&g), vec![target_key(&g), origin_key(&g)]);
    }

    #[test]
    fn a_record_reads_back_what_was_written() {
        let e = entry("prowlarr");
        let url = EnteredUrl::parse("http://prowlarr.lan:9696/").unwrap();
        for (target, runtime) in [
            (
                Target::Loopback(PortNumber::new(9696).unwrap()),
                Runtime::Native,
            ),
            (Target::Entered(url), Runtime::Native),
            (
                Target::Container(
                    e.container.as_ref().unwrap().name().clone(),
                    PortNumber::new(9696).unwrap(),
                ),
                Runtime::Container,
            ),
        ] {
            let r = rows(&[
                (target_key(&e.id), &StoredTarget::of(&target).to_row()),
                (origin_key(&e.id), "guided"),
            ]);
            assert_eq!(
                Record::read(e, runtime, r),
                Some(Record {
                    id: e.id.clone(),
                    target,
                    origin: Origin::Guided
                })
            );
        }
    }

    /// Anything that isn't exactly a valid record here reads as none.
    #[test]
    fn anything_else_reads_as_no_record() {
        let e = entry("prowlarr");
        let t = target_key(&e.id);
        let o = origin_key(&e.id);
        let bad: &[(&str, &str, Runtime)] = &[
            // An unknown kind or field, or the wrong shape.
            (
                r#"{"kind":"lan","port":9696}"#,
                "confirmed",
                Runtime::Native,
            ),
            (
                r#"{"kind":"loopback","port":9696,"x":1}"#,
                "confirmed",
                Runtime::Native,
            ),
            (r#"{"kind":"loopback"}"#, "confirmed", Runtime::Native),
            (
                r#"{"kind":"loopback","port":"9696"}"#,
                "confirmed",
                Runtime::Native,
            ),
            ("not json", "confirmed", Runtime::Native),
            // A port the catalogue doesn't list for it, or port 0.
            (
                r#"{"kind":"loopback","port":8080}"#,
                "confirmed",
                Runtime::Native,
            ),
            (
                r#"{"kind":"loopback","port":0}"#,
                "confirmed",
                Runtime::Native,
            ),
            // A container name is trusted only under Docker's DNS.
            (
                r#"{"kind":"container","port":9696}"#,
                "confirmed",
                Runtime::Native,
            ),
            // A URL with a credential, a fragment or another scheme.
            (
                r#"{"kind":"url","url":"http://u:p@h:9696/"}"#,
                "confirmed",
                Runtime::Native,
            ),
            (
                r#"{"kind":"url","url":"http://h:9696/#x"}"#,
                "confirmed",
                Runtime::Native,
            ),
            (
                r#"{"kind":"url","url":"ftp://h/"}"#,
                "confirmed",
                Runtime::Native,
            ),
            // An origin that isn't one of the two.
            (
                r#"{"kind":"loopback","port":9696}"#,
                "installed",
                Runtime::Native,
            ),
            (
                r#"{"kind":"loopback","port":9696}"#,
                "Confirmed",
                Runtime::Native,
            ),
        ];
        for (target, origin, runtime) in bad {
            let r = rows(&[(t.clone(), target), (o.clone(), origin)]);
            assert_eq!(Record::read(e, *runtime, r), None, "{target} {origin}");
        }
        let huge = format!(
            r#"{{"kind":"url","url":"http://h/{}"}}"#,
            "a".repeat(MAX_ROW)
        );
        assert_eq!(
            Record::read(
                e,
                Runtime::Native,
                rows(&[(t.clone(), &huge), (o.clone(), "guided")])
            ),
            None
        );
        // Either row missing.
        let ok = StoredTarget::Loopback { port: 9696 }.to_row();
        assert_eq!(
            Record::read(e, Runtime::Native, rows(&[(t.clone(), &ok)])),
            None
        );
        assert_eq!(
            Record::read(e, Runtime::Native, rows(&[(o, "guided")])),
            None
        );
    }

    #[test]
    fn provenance_is_read_for_the_companions_own_field_only() {
        let q = CompanionId::parse("qbittorrent").unwrap();
        let own = provenance_key(&q, Field::WebUiPassword).unwrap();
        let r = rows(&[
            (own.clone(), "pasted"),
            (
                "companions.qbittorrent.provenance.api_key".into(),
                "generated",
            ),
        ]);
        assert_eq!(
            provenance(&q, Field::WebUiPassword, &r),
            Some(Provenance::Pasted)
        );
        assert_eq!(provenance(&q, Field::ApiKey, &r), None);
        assert_eq!(
            provenance(&q, Field::WebUiPassword, rows(&[(own, "typed")])),
            None
        );
    }
}
