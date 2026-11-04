//! Source plugins that poll outside services (architecture §18.1, §18.2).
//!
//! Each plugin is glue over the plugin kit in `lodestar_sources`: a
//! manifest and policy, a config parser, the mapping from a parser's
//! items (`lodestar_sources_feeds`) to keys and claims, and a `run` loop
//! that sends HTTP only through `ctx.kit.http`. `README.md` is the
//! pattern every plugin here follows, with its recorded-fixture contract
//! tests.
//!
//! Plugins never build an HTTP client, never read credentials from their
//! config, and never put a credential in an error, a log line or a
//! `Debug` string.

use std::fmt;
use std::sync::Arc;

use lodestar_integrations::secret::Secret;
use lodestar_sources::BoxFuture;
use lodestar_sources::consent::SourceCredentials;
use lodestar_sources::egress::CredentialError;

pub mod archive_org;
pub mod nyaa;
pub mod prowlarr;

/// Where a plugin reads its stored credentials (SRC-4's per-source
/// store). Asked when the plugin starts, never per request.
pub trait Credentials: Send + Sync {
    /// `plugin`'s credential `name`, or `None` when none is stored.
    fn get<'a>(
        &'a self,
        plugin: &'a str,
        name: &'a str,
    ) -> BoxFuture<'a, Result<Option<Secret>, CredentialError>>;
}

impl Credentials for SourceCredentials {
    /// Reads on the blocking pool: the store is SQLite behind a mutex.
    fn get<'a>(
        &'a self,
        plugin: &'a str,
        name: &'a str,
    ) -> BoxFuture<'a, Result<Option<Secret>, CredentialError>> {
        let this = self.clone();
        let (plugin, name) = (plugin.to_owned(), name.to_owned());
        Box::pin(async move {
            tokio::task::spawn_blocking(move || this.get(&plugin, &name))
                .await
                .map_err(|_| CredentialError)?
        })
    }
}

/// Fixed credentials, by name, for every plugin. For tests and fixtures
/// only: never wire it into the running app.
#[doc(hidden)]
#[derive(Clone, Default)]
pub struct StaticCredentials(Arc<Vec<(String, Secret)>>);

impl StaticCredentials {
    pub fn new(values: impl IntoIterator<Item = (&'static str, &'static str)>) -> Self {
        StaticCredentials(Arc::new(
            values
                .into_iter()
                .map(|(n, v)| (n.to_owned(), Secret::new(v)))
                .collect(),
        ))
    }
}

impl fmt::Debug for StaticCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StaticCredentials(***)")
    }
}

impl Credentials for StaticCredentials {
    fn get<'a>(
        &'a self,
        _: &'a str,
        name: &'a str,
    ) -> BoxFuture<'a, Result<Option<Secret>, CredentialError>> {
        let found = self
            .0
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| Secret::new(v.expose()));
        Box::pin(async move { Ok(found) })
    }
}
