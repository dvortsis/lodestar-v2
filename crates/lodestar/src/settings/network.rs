//! Network: the HTTP listener, the session cookie and the DHT socket.
//! All restart-only: the listener and sockets are bound at start.

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{Field, Section};
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
    /// behind a TLS reverse proxy).
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

    fn apply(&self, config: &mut Config) {
        config.http.bind = self.http_bind;
        config.http.secure_cookies = self.secure_cookies;
        for addr in &mut config.dht.bind {
            addr.set_port(self.dht_port);
        }
        config.dht.enabled = self.dht_enabled;
    }
}
