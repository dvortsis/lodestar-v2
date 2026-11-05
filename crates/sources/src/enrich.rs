//! Adult-enrichment plugins (architecture §18.1, §20.9; plan
//! `docs/plan/2026-11-04-v06-rulepacks.md` ENR-1, Q2 and Q15;
//! `docs/decisions/2026-11-05-ENR-1.md`): the contract between the
//! pipeline's enrichment runtime and a service such as ThePornDB or
//! StashDB.
//!
//! **What may leave the install is decided by the runtime, not here**
//! (`lodestar_pipeline::adult_enrich`): it hands a plugin a [`Query`] only
//! for an item its eligibility gate passed. A [`Query`] is the only thing
//! a plugin is ever given about an item, and it can hold only:
//! - the item's display name ([`Query::Name`]), or
//! - the release parser's site, date and title fields ([`Query::Parsed`]),
//!
//! whichever the plugin asks for ([`AdultEnricher::query_form`]). There's
//! no field for an info hash, a file list, trackers or anything about the
//! install, so a plugin can't be handed one. Its `Debug` prints lengths
//! only.
//!
//! A plugin answers with a [`Lookup`]: a match as ENR-0's
//! [`AdultEnrichment`] section, or no match. The runtime turns a match
//! into a claim under the plugin's source id, which goes through SRC-2's
//! filter-before-storage path like every other claim.
//!
//! [`StoreState`] is a [`PluginState`] over the store's `source_state`
//! table, where the runtime keeps its caches (Q15).

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_store::{SourceId, Store};

use crate::claims::{
    AdultEnrichment, MAX_NAME, MAX_SCENE_TITLE, MAX_STUDIO, ReleaseDate, truncate,
};
use crate::manifest::{Manifest, Policy};
use crate::plugin::{BoxFuture, Kit, PluginError};
use crate::state::{PluginState, StateError, check};

/// Background lookups one service may make a day (plan Q15). Lookups for
/// user interest (an opened detail drawer) jump the queue and aren't
/// counted here, but share the same pacing ([`MIN_INTERVAL`]).
pub const BACKGROUND_PER_DAY: u32 = 5_000;
/// The least time between two lookups to one service (Q15: at most one
/// request a second).
pub const MIN_INTERVAL: Duration = Duration::from_secs(1);
/// How long a match is cached before the item may be looked up again
/// (Q15: 30 days).
pub const HIT_TTL_SECS: i64 = 30 * 86_400;
/// How long a miss is cached (Q15: 7 days).
pub const MISS_TTL_SECS: i64 = 7 * 86_400;
/// The prefix of every `source_state` key the runtime keeps for a plugin.
/// A plugin must not use keys under it.
pub const RUNTIME_PREFIX: &str = "enr1/";

/// Which of the two allowed forms a plugin wants an item in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryForm {
    /// The display name.
    Name,
    /// The release parser's site, date and title.
    Parsed,
}

/// What a plugin is told about one item: nothing else ever leaves the
/// install for a lookup (plan Q2). Build it with [`Query::name`] or
/// [`Query::parsed`], which bound every field.
#[derive(Clone, PartialEq, Eq)]
pub enum Query {
    /// The display name, at most [`MAX_NAME`] bytes.
    Name(String),
    /// The parser's fields: a title of at most [`MAX_SCENE_TITLE`] bytes,
    /// and a site of at most [`MAX_STUDIO`].
    Parsed {
        site: Option<String>,
        date: Option<ReleaseDate>,
        title: String,
    },
}

/// Lengths only: the name may be one a rule would hold, and it's adult.
impl fmt::Debug for Query {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Query::Name(n) => f.debug_struct("Name").field("len", &n.len()).finish(),
            Query::Parsed { site, date, title } => f
                .debug_struct("Parsed")
                .field("site_len", &site.as_ref().map(String::len))
                .field("date", &date.is_some())
                .field("title_len", &title.len())
                .finish(),
        }
    }
}

impl Query {
    /// The display name, trimmed and cut to [`MAX_NAME`] bytes. `None`
    /// when nothing is left.
    pub fn name(name: &str) -> Option<Query> {
        bounded(name, MAX_NAME).map(Query::Name)
    }

    /// The parser's fields. `None` without a title: a site and a date
    /// alone don't name a scene.
    pub fn parsed(site: Option<&str>, date: Option<ReleaseDate>, title: &str) -> Option<Query> {
        Some(Query::Parsed {
            title: bounded(title, MAX_SCENE_TITLE)?,
            site: site.and_then(|s| bounded(s, MAX_STUDIO)),
            date,
        })
    }

    /// Every byte of text the query carries, for tests and audits.
    pub fn text(&self) -> Vec<&str> {
        match self {
            Query::Name(n) => vec![n],
            Query::Parsed { site, title, .. } => {
                let mut out = vec![title.as_str()];
                out.extend(site.as_deref());
                out
            }
        }
    }
}

fn bounded(s: &str, max: usize) -> Option<String> {
    let mut s = s.trim().to_owned();
    truncate(&mut s, max);
    let s = s.trim_end().to_owned();
    (!s.is_empty()).then_some(s)
}

/// A plugin's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// The service matched the item to this scene or movie.
    Match(AdultEnrichment),
    /// The service has nothing for it.
    NoMatch,
}

/// An adult-enrichment service (ENR-4, ENR-5). Compiled in, like every
/// plugin; the runtime runs it as a source of its own, so it's registered,
/// enabled, consented to and purged like any other.
pub trait AdultEnricher: Send + Sync {
    /// Its kinds include [`Kind::Enrichment`](crate::Kind::Enrichment);
    /// its consent class is Account (§20.9).
    fn manifest(&self) -> &Manifest;

    /// Politeness caps; the runtime paces lookups on top of these.
    fn policy(&self) -> &Policy {
        &Policy::STRICT
    }

    /// The form it wants items in.
    fn query_form(&self) -> QueryForm;

    /// Looks one item up through `kit`'s client, the only way it may send
    /// anything. It must not keep the query beyond the call.
    fn lookup<'a>(
        &'a self,
        kit: &'a Kit,
        query: &'a Query,
    ) -> BoxFuture<'a, Result<Lookup, PluginError>>;
}

/// One source's [`PluginState`] in the store's `source_state` table, over
/// a connection the caller shares. Each call runs off the async threads.
#[derive(Clone)]
pub struct StoreState {
    store: Arc<Mutex<Store>>,
    source: SourceId,
}

impl fmt::Debug for StoreState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoreState")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

impl StoreState {
    pub fn new(store: Arc<Mutex<Store>>, source: SourceId) -> StoreState {
        StoreState { store, source }
    }

    /// Runs `f` on the store off the async threads: the connection's lock
    /// may be held by a long read elsewhere.
    async fn with<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Store, SourceId) -> lodestar_store::Result<T> + Send + 'static,
    ) -> Result<T, StateError> {
        let (store, source) = (self.store.clone(), self.source);
        let done = tokio::task::spawn_blocking(move || {
            let mut store = store.lock().unwrap_or_else(|e| e.into_inner());
            f(&mut store, source)
        })
        .await
        .map_err(|_| StateError::Store("the state task failed".into()))?;
        done.map_err(|e| match e {
            // The limits were checked first, so only the key count is left.
            lodestar_store::Error::Invalid(_) => StateError::Full,
            e => StateError::Store(e.to_string()),
        })
    }
}

impl PluginState for StoreState {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<Vec<u8>>, StateError>> {
        Box::pin(async move {
            check(key, None)?;
            let key = key.to_owned();
            self.with(move |s, source| s.plugin_state(source, &key))
                .await
        })
    }

    fn set<'a>(&'a self, key: &'a str, value: Vec<u8>) -> BoxFuture<'a, Result<(), StateError>> {
        Box::pin(async move {
            check(key, Some(&value))?;
            let key = key.to_owned();
            self.with(move |s, source| s.set_plugin_state(source, &key, Some(&value)))
                .await
        })
    }

    fn remove<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), StateError>> {
        Box::pin(async move {
            check(key, None)?;
            let key = key.to_owned();
            self.with(move |s, source| s.set_plugin_state(source, &key, None))
                .await
        })
    }

    fn compare_and_set<'a>(
        &'a self,
        key: &'a str,
        expected: Option<&'a [u8]>,
        new: Option<Vec<u8>>,
    ) -> BoxFuture<'a, Result<bool, StateError>> {
        Box::pin(async move {
            check(key, new.as_deref())?;
            let (key, expected) = (key.to_owned(), expected.map(<[u8]>::to_vec));
            self.with(move |s, source| {
                s.compare_and_set_plugin_state(source, &key, expected.as_deref(), new.as_deref())
            })
            .await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_are_bounded_and_never_empty() {
        assert_eq!(Query::name("  \t "), None);
        let long = "é".repeat(MAX_NAME);
        let Some(Query::Name(n)) = Query::name(&long) else {
            panic!("a name")
        };
        assert!(n.len() <= MAX_NAME && n.chars().all(|c| c == 'é'));
        assert_eq!(Query::parsed(Some("Site"), None, " "), None);
        let q = Query::parsed(Some(&"s".repeat(MAX_STUDIO + 9)), None, "Title").unwrap();
        let Query::Parsed { site, title, .. } = &q else {
            panic!("parsed")
        };
        assert_eq!(site.as_ref().unwrap().len(), MAX_STUDIO);
        assert_eq!(title, "Title");
        assert_eq!(q.text(), vec!["Title", &"s".repeat(MAX_STUDIO)]);
    }

    #[test]
    fn a_query_prints_no_text() {
        let q = Query::name("Secret Scene Name").unwrap();
        let p = Query::parsed(
            Some("Secret Site"),
            ReleaseDate::parse("2024-01-02"),
            "Secret",
        )
        .unwrap();
        for s in [format!("{q:?}"), format!("{p:?}")] {
            assert!(!s.contains("Secret"), "{s}");
        }
    }

    #[tokio::test]
    async fn store_state_keeps_values_in_source_state() {
        let mut store = Store::open_in_memory(lodestar_store::StoreConfig::default()).unwrap();
        let source = store.register_source("tpdb", "1", 0).unwrap();
        let shared = Arc::new(Mutex::new(store));
        let s = StoreState::new(shared.clone(), source);
        assert_eq!(s.get("enr1/k").await, Ok(None));
        s.set("enr1/k", b"v".to_vec()).await.unwrap();
        assert_eq!(
            shared
                .lock()
                .unwrap()
                .plugin_state(source, "enr1/k")
                .unwrap(),
            Some(b"v".to_vec())
        );
        assert_eq!(
            s.compare_and_set("enr1/k", Some(b"x"), None).await,
            Ok(false)
        );
        assert_eq!(
            s.compare_and_set("enr1/k", Some(b"v"), Some(b"w".to_vec()))
                .await,
            Ok(true)
        );
        assert_eq!(s.get("enr1/k").await, Ok(Some(b"w".to_vec())));
        s.remove("enr1/k").await.unwrap();
        assert_eq!(s.get("enr1/k").await, Ok(None));
        assert_eq!(s.get("").await, Err(StateError::BadKey));
    }
}
