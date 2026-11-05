//! Every plugin compiled into this crate, by its manifest
//! (X-adult-catalogue, `docs/decisions/2026-11-05-X-adult-catalogue.md`).
//!
//! Registered or not: ThePornDB, which is on hold, is listed too. Code
//! that can't ask the running plugins, such as `lodestar import`, reads
//! a manifest's fixed fields (`id`, `kinds`, `always_adult`) here, so a
//! new plugin is seen everywhere once it's listed below, and a unit test
//! fails if it isn't.
//!
//! Each entry is the plugin's own `MANIFEST` const, the one its
//! [`SourcePlugin::manifest`](lodestar_sources::SourcePlugin::manifest)
//! returns. The torrent cache's plugin adds the configured caches as
//! `endpoints`; its entry here is [`torrent_cache::MANIFEST`], the same
//! manifest with no cache listed. Endpoints never come from here.
//!
//! Only plugins in this crate are listed. An always-adult plugin must
//! live here, or `lodestar import`'s mask won't see it.

use lodestar_sources::Manifest;

use crate::{
    academic, archive_org, arr, clients, full_scrape, nyaa, prowlarr, torrent_cache, tpdb,
};

/// `(module, manifest)` for every plugin, in `lib.rs` order. A module
/// may hold more than one plugin (`nyaa` holds Nyaa and Sukebei).
const PLUGINS: &[(&str, &Manifest)] = &[
    ("academic", &academic::MANIFEST),
    ("archive_org", &archive_org::MANIFEST),
    ("arr", &arr::MANIFEST),
    ("clients", &clients::MANIFEST),
    ("full_scrape", &full_scrape::MANIFEST),
    ("nyaa", &nyaa::MANIFEST),
    ("nyaa", &nyaa::SUKEBEI_MANIFEST),
    ("prowlarr", &prowlarr::MANIFEST),
    ("torrent_cache", &torrent_cache::MANIFEST),
    ("tpdb", &tpdb::MANIFEST),
];

/// The manifest of every plugin compiled into this crate.
pub fn manifests() -> impl Iterator<Item = &'static Manifest> {
    PLUGINS.iter().map(|(_, m)| *m)
}

/// The plugins whose every find is adult (Sukebei today).
pub fn always_adult() -> impl Iterator<Item = &'static Manifest> {
    manifests().filter(|m| m.always_adult)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    use super::PLUGINS;

    /// `pub mod` lines in `lib.rs` that aren't plugins.
    const NOT_PLUGINS: &[&str] = &["catalogue"];

    /// Every `mod x;` line in `lib.rs`, whatever its visibility. A plugin
    /// is a file module (`src/x.rs`), never an inline `mod x { … }`.
    fn modules() -> Vec<&'static str> {
        include_str!("lib.rs")
            .lines()
            .filter_map(|l| {
                let l = l.trim();
                let l = l
                    .strip_prefix("pub(crate) ")
                    .or_else(|| l.strip_prefix("pub "))
                    .unwrap_or(l);
                l.strip_prefix("mod ")?.strip_suffix(';')
            })
            .collect()
    }

    fn listed(module: &str) -> usize {
        PLUGINS.iter().filter(|(m, _)| *m == module).count()
    }

    /// A new module in `lib.rs` is either a plugin listed in the
    /// catalogue or named in [`NOT_PLUGINS`], on purpose.
    #[test]
    fn every_module_is_listed_or_not_a_plugin() {
        let modules = modules();
        assert!(modules.len() >= 9, "only {} modules found", modules.len());
        for m in &modules {
            let not_plugin = NOT_PLUGINS.contains(m);
            assert!(
                (listed(m) > 0) != not_plugin,
                "`mod {m}`: add its manifest to the catalogue, or to NOT_PLUGINS if it isn't \
                 a plugin"
            );
        }
        let known: BTreeSet<_> = modules.iter().copied().collect();
        for (m, _) in PLUGINS {
            assert!(
                known.contains(m),
                "the catalogue lists `{m}`, not in lib.rs"
            );
        }
    }

    /// A module with two plugins (Nyaa and Sukebei) lists both: as many
    /// entries as its file has plugins, a plugin being any type with a
    /// `fn manifest(&self)` (a `SourcePlugin` or an `AdultEnricher`).
    /// Plugins are written in their module's own file, not a nested one.
    #[test]
    fn every_plugin_in_a_module_is_listed() {
        let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        for m in modules().into_iter().filter(|m| !NOT_PLUGINS.contains(m)) {
            let path = src.join(format!("{m}.rs"));
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let impls = text.matches("fn manifest(&self)").count();
            assert_eq!(
                impls,
                listed(m),
                "{m}: {impls} plugin(s) in the file, {} in the catalogue",
                listed(m)
            );
        }
    }
}
