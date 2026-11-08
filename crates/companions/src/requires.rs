//! Which source plugins need a companion (architecture §18.10; plan
//! 2026-11-06-v06-companions §4.3, CMP-10).
//!
//! A closed table, compiled in: a plugin id maps to what it needs, and
//! nothing a plugin declares at run time can add to it or change it.
//! The plugin host (`lodestar-pipeline`'s `requires`) reads it to decide
//! whether a plugin may start; a plugin with no entry is never held for a
//! companion.
//!
//! - The Prowlarr plugin needs Prowlarr itself.
//! - The download-client source needs a download client: any catalogue
//!   entry needed for [`Need::DownloadClient`] will do, and the one to
//!   suggest is qBittorrent (§18.10's wizard recommendation).
//! - i2pd's plugins get their entry when they and i2pd's catalogue entry
//!   exist (v1.1).
//!
//! Plugin ids are written out here, not imported: this crate never
//! depends on the plugins' crates or the pipeline (the pipeline depends
//! on this one). `lodestar-pipeline`'s `tests/requires.rs` checks every
//! id against the plugin registry, and this module's tests check every
//! companion id against the built-in catalogue, so either drifting fails
//! a test.

use crate::catalogue::{Catalogue, Need};

/// What a plugin needs before it may start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requirement {
    /// Exactly this companion, by catalogue id.
    Companion(&'static str),
    /// Any catalogue companion needed for `need`; `suggest` is the one
    /// named while none is there, and counts even when the catalogue
    /// can't be read.
    AnyOf { need: Need, suggest: &'static str },
}

impl Requirement {
    /// The companion ids that satisfy it, the suggested one first and
    /// then the rest in catalogue order. With no catalogue (it failed to
    /// load), only the named one: the set only ever shrinks.
    pub fn candidates(&self, catalogue: Option<&Catalogue>) -> Vec<&'static str> {
        match *self {
            Requirement::Companion(id) => vec![id],
            Requirement::AnyOf { need, suggest } => {
                let mut ids = vec![suggest];
                if let Some(c) = catalogue {
                    for e in c.entries() {
                        if e.needs(need) {
                            let id = e.id.as_str();
                            if let Some(known) = MAPPED_IDS.iter().find(|k| **k == id)
                                && !ids.contains(known)
                            {
                                ids.push(known);
                            }
                        }
                    }
                }
                ids
            }
        }
    }

    /// The companion to name while it isn't met.
    pub fn suggested(&self) -> &'static str {
        match *self {
            Requirement::Companion(id) => id,
            Requirement::AnyOf { suggest, .. } => suggest,
        }
    }
}

/// The map: plugin manifest id → what it needs.
const MAP: &[(&str, Requirement)] = &[
    ("prowlarr", Requirement::Companion("prowlarr")),
    (
        "download_clients",
        Requirement::AnyOf {
            need: Need::DownloadClient,
            suggest: "qbittorrent",
        },
    ),
];

/// Every companion id a requirement may name, as `'static` strings so a
/// requirement's result never borrows the catalogue. A catalogue entry
/// with a needed role but no id here can't satisfy anything until it's
/// added (the tests say so).
const MAPPED_IDS: &[&str] = &["qbittorrent", "prowlarr"];

/// What plugin `id` needs; `None` for a plugin no companion gates.
pub fn requirement(plugin: &str) -> Option<Requirement> {
    MAP.iter().find(|(p, _)| *p == plugin).map(|(_, r)| *r)
}

/// Every entry, in table order.
pub fn entries() -> impl Iterator<Item = (&'static str, Requirement)> {
    MAP.iter().copied()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::builtin;

    #[test]
    fn plugin_ids_are_unique_and_unmapped_ones_need_nothing() {
        let ids: HashSet<&str> = entries().map(|(p, _)| p).collect();
        assert_eq!(ids.len(), MAP.len());
        assert_eq!(requirement("dht"), None);
        assert_eq!(requirement("nyaa"), None);
        assert_eq!(requirement(""), None);
        assert_eq!(requirement("PROWLARR"), None, "ids match exactly");
        assert_eq!(
            requirement("prowlarr"),
            Some(Requirement::Companion("prowlarr"))
        );
    }

    #[test]
    fn every_companion_named_is_in_the_builtin_catalogue_for_the_role() {
        let c = builtin().expect("the built-in catalogue loads");
        for (plugin, r) in entries() {
            for id in r.candidates(Some(c)) {
                let e = c
                    .get(id)
                    .unwrap_or_else(|| panic!("{plugin}: {id} not in the catalogue"));
                if let Requirement::AnyOf { need, .. } = r {
                    assert!(e.needs(need), "{plugin}: {id} isn't needed for {need:?}");
                }
            }
        }
        for id in MAPPED_IDS {
            assert!(c.get(id).is_some(), "{id} not in the catalogue");
        }
    }

    #[test]
    fn every_catalogue_download_client_can_satisfy_the_download_client_source() {
        let c = builtin().unwrap();
        let r = requirement("download_clients").unwrap();
        let got = r.candidates(Some(c));
        assert_eq!(got[0], "qbittorrent", "the suggestion comes first");
        for e in c.entries().iter().filter(|e| e.needs(Need::DownloadClient)) {
            assert!(
                got.contains(&e.id.as_str()),
                "{} is a download client but missing from MAPPED_IDS",
                e.id
            );
        }
        // Not a download client: never a candidate.
        assert!(!got.contains(&"prowlarr") && !got.contains(&"gluetun"));
    }

    #[test]
    fn without_a_catalogue_only_the_named_companion_counts() {
        let r = requirement("download_clients").unwrap();
        assert_eq!(r.candidates(None), vec!["qbittorrent"]);
        assert_eq!(r.suggested(), "qbittorrent");
        let p = requirement("prowlarr").unwrap();
        assert_eq!(p.candidates(None), vec!["prowlarr"]);
        assert_eq!(p.suggested(), "prowlarr");
    }
}
