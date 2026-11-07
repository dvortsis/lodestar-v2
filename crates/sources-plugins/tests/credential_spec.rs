//! Each plugin's declared credentials (`Policy::credentials`,
//! X-sources-api-spec) against what it actually reads and what the
//! credential store and `consent::need` accept. A plugin that asks for a
//! credential it doesn't declare, or declares one it never asks for or
//! can't be stored, fails here.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use lodestar_sources::consent::Secret;
use lodestar_sources::consent::{self, CredentialSpec};
use lodestar_sources::egress::CredentialError;
use lodestar_sources::enrich::{AdultEnricher, Query};
use lodestar_sources::testing::{FakeCtx, wait_for};
use lodestar_sources::{
    BoxFuture, Consent, Kit, Manifest, MemoryState, NoEgress, Policy, SourcePlugin, UserLimits,
};
use lodestar_sources_plugins::full_scrape::{StaticPrivateTrackers, SystemClock};
use lodestar_sources_plugins::prowlarr::{Config, Manager};
use lodestar_sources_plugins::{
    Credentials, StaticCredentials, academic, archive_org, arr, catalogue, clients, full_scrape,
    nyaa, prowlarr, stashdb, torrent_cache, tpdb,
};
use lodestar_store::SourceId;

/// Remembers every name a plugin asks for, and has none of them.
#[derive(Default)]
struct Recording(Mutex<Vec<(String, String)>>);

impl Recording {
    fn asked(&self) -> BTreeSet<String> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|(_, n)| n.clone())
            .collect()
    }
    fn plugins(&self) -> BTreeSet<String> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|(p, _)| p.clone())
            .collect()
    }
}

impl Credentials for Recording {
    fn get<'a>(
        &'a self,
        plugin: &'a str,
        name: &'a str,
    ) -> BoxFuture<'a, Result<Option<Secret>, CredentialError>> {
        self.0
            .lock()
            .unwrap()
            .push((plugin.to_string(), name.to_string()));
        Box::pin(async { Ok(None) })
    }
}

fn names(spec: &[CredentialSpec]) -> BTreeSet<String> {
    spec.iter().map(|c| c.name.to_string()).collect()
}

/// Runs a source plugin until it ends (it ends at once without its key).
async fn run_until_it_ends<P: SourcePlugin + 'static>(p: Arc<P>) -> String {
    let mut ctx = FakeCtx::new(&*p, SourceId::new(5).unwrap());
    let running = ctx.start(p);
    wait_for("the plugin to end", || running.is_finished()).await;
    running.stop().await.unwrap().result.unwrap_err().0
}

async fn lookup_once<E: AdultEnricher>(e: &E) -> String {
    let kit = Kit::new(
        Arc::new(MemoryState::new()),
        Arc::new(NoEgress),
        e.policy(),
        &UserLimits::default(),
    );
    let q = Query::name("Example.Studio.24.02.29.Synthetic.Scene.One.1080p").unwrap();
    e.lookup(&kit, &q).await.unwrap_err().0
}

/// Checks `spec` the way the API and the credential store will use it,
/// and that `asked`, what the plugin read, is exactly what it declares.
fn check(m: &Manifest, policy: &Policy, rec: &Recording) {
    let spec = policy.credentials;
    assert_eq!(consent::check_spec(m, spec), Ok(()), "{}", m.id);
    assert_eq!(rec.asked(), names(spec), "{}: read vs declared", m.id);
    // Only under its own id: a plugin never reads another's credentials.
    if !spec.is_empty() {
        assert_eq!(
            rec.plugins(),
            BTreeSet::from([m.id.to_string()]),
            "{}",
            m.id
        );
    }
    // Every required credential stored, `need` asks for nothing more.
    let required: Vec<String> = spec
        .iter()
        .filter(|c| c.required)
        .map(|c| c.name.to_string())
        .collect();
    if m.consent == Consent::Account && !required.is_empty() {
        assert_eq!(consent::need(m, None, &required), None, "{}", m.id);
    }
    // A credential is never a setting: no settings field shares a
    // declared name, and no settings field is a secret (write-only).
    let schema: serde_json::Value = serde_json::from_str(policy.config_schema)
        .unwrap_or_else(|e| panic!("{}: the settings schema doesn't parse: {e}", m.id));
    if let Some(props) = schema["properties"].as_object() {
        for (k, p) in props {
            assert!(
                !names(spec).contains(k),
                "{}: `{k}` is both a setting and a credential",
                m.id
            );
            assert_ne!(
                p["writeOnly"], true,
                "{}: `{k}` is a write-only setting",
                m.id
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn prowlarr_reads_exactly_what_it_declares() {
    let rec = Arc::new(Recording::default());
    let config = Config::new("http://prowlarr.example:9696", Manager::Prowlarr, None).unwrap();
    let p = Arc::new(prowlarr::Prowlarr::new(config, rec.clone()));
    let e = run_until_it_ends(p.clone()).await;
    assert!(e.contains("no API key"), "{e}");
    check(p.manifest(), p.policy(), &rec);
    assert_eq!(
        names(p.policy().credentials),
        BTreeSet::from(["api_key".into()])
    );
}

#[tokio::test(start_paused = true)]
async fn the_enrichers_read_exactly_what_they_declare() {
    let rec = Arc::new(Recording::default());
    let t = tpdb::Tpdb::new(rec.clone());
    lookup_once(&t).await;
    check(AdultEnricher::manifest(&t), AdultEnricher::policy(&t), &rec);

    let rec = Arc::new(Recording::default());
    let s = stashdb::StashDb::new(rec.clone());
    lookup_once(&s).await;
    check(AdultEnricher::manifest(&s), AdultEnricher::policy(&s), &rec);
}

/// The rest are built without a credential store, so they can't read
/// one: each declares none. The download clients' credentials are
/// Settings → Integrations' and reach the plugin in its `ClientConfig`.
#[test]
fn plugins_without_a_credential_store_declare_none() {
    let (cache, _queue, _verified) =
        torrent_cache::TorrentCache::new(torrent_cache::Config::new::<&str>(&[]).unwrap());
    let scrape = full_scrape::FullScrape::unconfigured(
        Arc::new(StaticPrivateTrackers::none()),
        Arc::new(SystemClock::default()),
    );
    let own = |p: &dyn SourcePlugin| (p.manifest().clone(), *p.policy());
    let plugins: Vec<(Manifest, Policy)> = vec![
        own(&academic::Academic::new()),
        own(&archive_org::ArchiveOrg::new()),
        own(&arr::ArrWebhooks::new()),
        own(&clients::Clients::unconfigured()),
        own(&scrape),
        own(&nyaa::Nyaa::new()),
        own(&nyaa::Sukebei::new()),
        own(&cache),
    ];
    for (m, policy) in plugins {
        assert!(policy.credentials.is_empty(), "{}", m.id);
        check(&m, &policy, &Recording::default());
    }
}

/// The three above and these eight are every plugin in the catalogue, so
/// a new one has to be added here.
#[test]
fn every_catalogued_plugin_is_checked() {
    let checked = [
        prowlarr::ID,
        tpdb::ID,
        stashdb::ID,
        academic::ID,
        archive_org::ID,
        arr::ID,
        clients::ID,
        full_scrape::ID,
        nyaa::ID,
        nyaa::SUKEBEI_ID,
        torrent_cache::ID,
    ];
    let listed: BTreeSet<&str> = catalogue::manifests().map(|m| m.id).collect();
    assert_eq!(listed, BTreeSet::from(checked));
}

/// With its declared credential stored, an enricher gets past it: the
/// lookup goes on to its request, which `NoEgress` refuses.
#[tokio::test(start_paused = true)]
async fn the_declared_names_are_the_ones_it_uses() {
    let stored = || Arc::new(StaticCredentials::new([("api_key", "abcdef0123456789")]));
    let without = lookup_once(&tpdb::Tpdb::new(Arc::new(StaticCredentials::default()))).await;
    for e in [
        lookup_once(&tpdb::Tpdb::new(stored())).await,
        lookup_once(&stashdb::StashDb::new(stored())).await,
    ] {
        assert_ne!(e, without, "{e}");
    }
}
