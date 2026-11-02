//! Fetch transport: how metadata fetches reach peers. Stored and shown
//! only; they change nothing but what's saved, and the UI labels them
//! "not active yet". uTP runs behind the CR-4 switch `dht.utp` while its
//! A/B decides (a saved value would override the experiment's env var);
//! see docs/decisions/2026-11-02-CR-4.md.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{Field, Section};
use crate::config::Config;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Transport {
    /// Fetch metadata over uTP (BEP 29) as well as TCP.
    pub utp: bool,
    /// When a uTP connection fails, try the peer over TCP.
    pub fallback_to_tcp: bool,
}

impl Default for Transport {
    fn default() -> Self {
        Transport {
            utp: false,
            fallback_to_tcp: true,
        }
    }
}

impl Section for Transport {
    const SECTION: &'static str = "transport";
    const FIELDS: &'static [Field] = &[
        Field::new("utp").restart().group("transport").keywords(),
        Field::new("fallback_to_tcp")
            .restart()
            .group("transport")
            .advanced(),
    ];

    /// No `config.toml` key backs these yet.
    fn from_config(_: &Config) -> Self {
        Transport::default()
    }

    fn apply(&self, _: &str, _: &mut Config) {}
}
