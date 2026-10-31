//! Insights & digest (§10.9.5, §10.9.7, §10.10.8): the digest's schedule
//! and channels, and query logging. Stored and shown; the digest job and
//! the search log read them.
//!
//! - **Query logging is off by default.** Turning it on records what
//!   people search for, so it's marked `privacy` (§10.10.7). The log is
//!   stored locally and kept `query_retention_days` (at most 30).
//! - The digest goes out at `digest_hour`, local time (default 03:00, a
//!   quiet hour), on `digest_day` when it's weekly.
//! - Clearing usage data is an action (`POST /usage/clear`), not a field.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{Danger, Field, Section};
use crate::config::Config;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Digest {
    #[default]
    Off,
    Daily,
    Weekly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Weekday {
    #[default]
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

/// Where a digest goes (§10.9.7): the notification channels set up in
/// Notifications, or the RSS feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum DigestChannel {
    Email,
    Ntfy,
    Discord,
    Rss,
}

/// The section. Enum fields are undocumented so their schema stays inline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Insights {
    #[schema(inline)]
    pub digest: Digest,
    #[schema(inline)]
    pub digest_day: Weekday,
    /// The hour it goes out, local time (0–23).
    pub digest_hour: u8,
    #[schema(inline)]
    pub digest_channels: Vec<DigestChannel>,
    /// Record searches for Insights' top queries (off by default).
    pub query_logging: bool,
    /// Days a logged search is kept (1–30).
    pub query_retention_days: u32,
}

impl Default for Insights {
    fn default() -> Self {
        Insights {
            digest: Digest::Off,
            digest_day: Weekday::Mon,
            digest_hour: 3,
            digest_channels: Vec::new(),
            query_logging: false,
            query_retention_days: 30,
        }
    }
}

impl Section for Insights {
    const SECTION: &'static str = "insights";
    const FIELDS: &'static [Field] = &[
        Field::new("digest")
            .group("digest")
            .widget("radio")
            .keywords(),
        Field::new("digest_day").group("digest"),
        Field::new("digest_hour").group("digest").range(0, 23),
        Field::new("digest_channels").group("digest"),
        Field::new("query_logging")
            .group("usage")
            .danger(Danger::Privacy)
            .keywords(),
        Field::new("query_retention_days")
            .group("usage")
            .range(1, 30)
            .unit("days"),
    ];

    /// No `config.toml` key backs these yet.
    fn from_config(_: &Config) -> Self {
        Insights::default()
    }

    fn apply(&self, _: &str, _: &mut Config) {}

    fn check(&self, field: &str) -> Result<(), String> {
        match field {
            "digest_channels" => {
                let mut c = self.digest_channels.clone();
                c.sort();
                c.dedup();
                if c.len() != self.digest_channels.len() {
                    return Err("Lists a channel twice.".into());
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_logging_is_off_and_kept_thirty_days_by_default() {
        let i = Insights::default();
        assert!(!i.query_logging);
        assert_eq!((i.query_retention_days, i.digest_hour), (30, 3));
        assert_eq!(i.digest, Digest::Off);
        let f = Insights::FIELDS
            .iter()
            .find(|f| f.name == "query_logging")
            .unwrap();
        assert_eq!(f.danger, Some(Danger::Privacy));
    }

    #[test]
    fn a_channel_listed_twice_is_refused() {
        let i = Insights {
            digest_channels: vec![DigestChannel::Rss, DigestChannel::Rss],
            ..Default::default()
        };
        assert!(i.check("digest_channels").is_err());
    }
}
