//! Safety rules over every manifest in the crate, read from the
//! catalogue (X-adult-catalogue). The catalogue's own unit tests check
//! that it lists every plugin; these check what the manifests say.

use std::collections::BTreeSet;
use std::sync::Arc;

use lodestar_sources::enrich::AdultEnricher;
use lodestar_sources::{Kind, Manifest, SourcePlugin};
use lodestar_sources_plugins::full_scrape::{StaticPrivateTrackers, SystemClock};
use lodestar_sources_plugins::{
    StaticCredentials, academic, archive_org, arr, catalogue, clients, full_scrape, nyaa, prowlarr,
    stashdb, torrent_cache, tpdb,
};

/// The adult enrichers are picked by id (`PipelineConfig::adult_enrichers`)
/// because the torrent cache is `Kind::Enrichment` too. So no manifest may
/// be both an enricher and always-adult: that would mix the two roles.
#[test]
fn no_manifest_is_both_enrichment_and_always_adult() {
    for m in catalogue::manifests() {
        assert!(
            !(m.is(Kind::Enrichment) && m.always_adult),
            "{}: both Enrichment and always_adult",
            m.id
        );
    }
}

/// The store's `sources` table is keyed by these ids.
#[test]
fn ids_are_unique() {
    let mut seen = BTreeSet::new();
    for m in catalogue::manifests() {
        assert!(seen.insert(m.id), "{}: listed twice", m.id);
    }
}

/// Every find of an always-adult source is classified Adult, whatever it
/// says (owner Q7). A new one changes this list on purpose.
#[test]
fn the_always_adult_plugins_are_exactly_sukebei() {
    let ids: Vec<_> = catalogue::always_adult().map(|m| m.id).collect();
    assert_eq!(ids, [nyaa::SUKEBEI_ID]);
}

/// Each entry is the manifest its plugin reports, apart from the torrent
/// cache's endpoints, which come from its config.
#[test]
fn each_entry_is_its_plugins_own_manifest() {
    let creds = || Arc::new(StaticCredentials::default());
    let (cache, _queue, _verified) =
        torrent_cache::TorrentCache::new(torrent_cache::Config::new::<&str>(&[]).unwrap());
    let tpdb = tpdb::Tpdb::new(creds());
    let stashdb = stashdb::StashDb::new(creds());
    let plugins: Vec<Manifest> = vec![
        academic::Academic::new().manifest().clone(),
        archive_org::ArchiveOrg::new().manifest().clone(),
        arr::ArrWebhooks::new().manifest().clone(),
        clients::Clients::unconfigured().manifest().clone(),
        full_scrape::FullScrape::unconfigured(
            Arc::new(StaticPrivateTrackers::none()),
            Arc::new(SystemClock::default()),
        )
        .manifest()
        .clone(),
        nyaa::Nyaa::new().manifest().clone(),
        nyaa::Sukebei::new().manifest().clone(),
        prowlarr::Prowlarr::unconfigured(creds()).manifest().clone(),
        AdultEnricher::manifest(&stashdb).clone(),
        cache.manifest().clone(),
        AdultEnricher::manifest(&tpdb).clone(),
    ];
    let listed: Vec<&Manifest> = catalogue::manifests().collect();
    assert_eq!(plugins.len(), listed.len());
    for (mut own, m) in plugins.into_iter().zip(listed) {
        if own.id == torrent_cache::ID {
            own.endpoints = m.endpoints;
        }
        assert_eq!(own, *m, "{}", m.id);
    }
}
