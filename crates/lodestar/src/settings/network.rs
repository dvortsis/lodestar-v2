//! Network: the HTTP listener, the session cookie and the DHT socket.
//! All restart-only: the listener and sockets are bound at start.

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use lodestar_store::Store;

use super::{Field, KEY_PREFIX, Section};
use crate::config::Config;

/// The DHT port when no bind address is configured.
const DEFAULT_DHT_PORT: u16 = 6881;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Network {
    /// Where the UI and API listen (`address:port`).
    #[schema(value_type = String, example = "0.0.0.0:8090")]
    pub http_bind: SocketAddr,
    /// Mark the session cookie `Secure`, when served over HTTPS (e.g.
    /// behind a TLS reverse proxy). Set in `config.toml` or
    /// `LODESTAR_SECURE_COOKIES` only: turned on in the app while served
    /// over plain HTTP, browsers would drop the cookie and nobody could log
    /// in to turn it off.
    pub secure_cookies: bool,
    /// The UDP port the DHT listens on, for every bound address.
    pub dht_port: u16,
    /// Crawl the DHT.
    pub dht_enabled: bool,
}

impl Default for Network {
    fn default() -> Self {
        Network::from_config(&Config::default())
    }
}

impl Section for Network {
    const SECTION: &'static str = "network";
    const FIELDS: &'static [Field] = &[
        Field::new("http_bind")
            .file("http.bind")
            .env("LODESTAR_HTTP_BIND")
            .restart(),
        Field::new("secure_cookies")
            .file("http.secure_cookies")
            .env("LODESTAR_SECURE_COOKIES")
            .read_only()
            .restart(),
        Field::new("dht_port")
            .file("dht.bind")
            .env("LODESTAR_DHT_PORT")
            .restart()
            .range(1, 65535),
        Field::new("dht_enabled")
            .file("dht.enabled")
            .env("LODESTAR_DHT_ENABLED")
            .restart(),
    ];

    fn from_config(config: &Config) -> Self {
        Network {
            http_bind: config.http.bind,
            secure_cookies: config.http.secure_cookies,
            dht_port: config
                .dht
                .bind
                .first()
                .map_or(DEFAULT_DHT_PORT, SocketAddr::port),
            dht_enabled: config.dht.enabled,
        }
    }

    fn apply(&self, field: &str, config: &mut Config) {
        match field {
            "http_bind" => config.http.bind = self.http_bind,
            // A saved port moves every DHT socket to it.
            "dht_port" => {
                for addr in &mut config.dht.bind {
                    addr.set_port(self.dht_port);
                }
            }
            "dht_enabled" => config.dht.enabled = self.dht_enabled,
            // `secure_cookies` is read-only here.
            _ => {}
        }
    }

    fn check(&self, field: &str) -> Result<(), String> {
        if field == "http_bind" && self.http_bind.port() == 0 {
            return Err("Must name a port from 1 to 65535.".into());
        }
        Ok(())
    }
}

/// Where the server listens, as `lodestar healthcheck` must probe it: a
/// bind saved in the app wins over the file's, the environment over both
/// (as at start). The store is opened read-only; without one, or with
/// nothing valid saved, it's the config's.
pub fn effective_bind(config: &Config) -> SocketAddr {
    if config.layers.in_env("LODESTAR_HTTP_BIND") {
        return config.http.bind;
    }
    let key = format!("{KEY_PREFIX}{}.http_bind", Network::SECTION);
    let saved = Store::open_reader(config.db_path(), config.store_config())
        .and_then(|s| s.setting_text(&key))
        .ok()
        .flatten()
        .and_then(|text| serde_json::from_str::<SocketAddr>(&text).ok())
        .filter(|addr| addr.port() != 0);
    saved.unwrap_or(config.http.bind)
}
