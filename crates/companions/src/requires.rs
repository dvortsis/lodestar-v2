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
//! - The download-client source needs a download client (runner,
//!   2026-11-08): a client configured in Settings → Integrations that
//!   Lodestar doesn't manage, or any catalogue companion needed for
//!   [`Need::DownloadClient`]. With neither, the one to suggest is
//!   qBittorrent (§18.10's wizard recommendation). An external client's
//!   own health stays the integration's business; only a managed
//!   companion's health can hold the plugin.
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

/// One thing that meets a [`Requirement`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Satisfier {
    /// This companion, by catalogue id, installed or detected and healthy.
    Companion(&'static str),
    /// Any catalogue companion needed for this role, healthy.
    AnyCompanion(Need),
    /// A download client configured in Settings → Integrations that
    /// Lodestar doesn't manage. Its health isn't judged here.
    ExternalClient,
}

/// What a plugin needs before it may start: any one of its satisfiers,
/// and the companion to suggest while none is there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Requirement {
    pub satisfiers: &'static [Satisfier],
    /// Named while it isn't met; a catalogue id. It always counts as a
    /// candidate, even when the catalogue can't be read.
    pub suggest: &'static str,
}

impl Requirement {
    /// The companion ids that can meet it, the suggested one first and
    /// then the rest in catalogue order. With no catalogue (it failed to
    /// load), the role adds nothing: the set only ever shrinks.
    pub fn companions(&self, catalogue: Option<&Catalogue>) -> Vec<&'static str> {
        let mut ids = vec![self.suggest];
        let mut add = |id: &'static str| {
            if !ids.contains(&id) {
                ids.push(id);
            }
        };
        for s in self.satisfiers {
            match *s {
                Satisfier::Companion(id) => add(id),
                Satisfier::AnyCompanion(need) => {
                    for e in catalogue.map(Catalogue::entries).unwrap_or_default() {
                        if e.needs(need)
                            && let Some(id) = MAPPED_IDS.iter().find(|k| **k == e.id.as_str())
                        {
                            add(id);
                        }
                    }
                }
                Satisfier::ExternalClient => {}
            }
        }
        ids
    }

    /// Whether a client configured in Integrations meets it.
    pub fn takes_external_client(&self) -> bool {
        self.satisfiers.contains(&Satisfier::ExternalClient)
    }
}

/// The map: plugin manifest id → what it needs.
const MAP: &[(&str, Requirement)] = &[
    (
        "prowlarr",
        Requirement {
            satisfiers: &[Satisfier::Companion("prowlarr")],
            suggest: "prowlarr",
        },
    ),
    (
        "download_clients",
        Requirement {
            satisfiers: &[
                Satisfier::ExternalClient,
                Satisfier::AnyCompanion(Need::DownloadClient),
            ],
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
        let p = requirement("prowlarr").unwrap();
        assert_eq!(p.satisfiers, &[Satisfier::Companion("prowlarr")]);
        assert!(!p.takes_external_client(), "Prowlarr is a hard need");
        assert!(
            requirement("download_clients")
                .unwrap()
                .takes_external_client()
        );
    }

    #[test]
    fn every_companion_named_is_in_the_builtin_catalogue_for_its_role() {
        let c = builtin().expect("the built-in catalogue loads");
        for (plugin, r) in entries() {
            for id in r.companions(Some(c)) {
                let e = c
                    .get(id)
                    .unwrap_or_else(|| panic!("{plugin}: {id} not in the catalogue"));
                for s in r.satisfiers {
                    if let Satisfier::AnyCompanion(need) = s {
                        assert!(e.needs(*need), "{plugin}: {id} isn't needed for {need:?}");
                    }
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
        let got = requirement("download_clients").unwrap().companions(Some(c));
        assert_eq!(got[0], "qbittorrent", "the suggestion comes first");
        for e in c.entries().iter().filter(|e| e.needs(Need::DownloadClient)) {
            assert!(
                got.contains(&e.id.as_str()),
                "{} is a download client but missing from MAPPED_IDS",
                e.id
            );
        }
        assert!(!got.contains(&"prowlarr") && !got.contains(&"gluetun"));
    }

    #[test]
    fn without_a_catalogue_only_the_named_companions_count() {
        let r = requirement("download_clients").unwrap();
        assert_eq!(r.companions(None), vec!["qbittorrent"]);
        let p = requirement("prowlarr").unwrap();
        assert_eq!(p.companions(None), vec!["prowlarr"]);
    }
}
