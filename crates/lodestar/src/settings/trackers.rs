//! Trackers (§10.10.8, §15): the tracker scraper's list, whether it runs
//! and how often it asks each tracker. The same keys as `config.toml`'s
//! `[scrape]`, applied at the next start. Each tracker's health is state
//! the scraper reports (Insights → Index), not a setting.
//!
//! - The intervals use 0 for "automatic" (no field may be nullable): UDP
//!   follows the crawler's intensity, HTTP is three times UDP's.
//! - An error names a tracker by its position, never its URL: an HTTP
//!   tracker URL may carry a passkey.

use lodestar_tracker_scrape::{Scheme, TrackerUrl};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{Auto, Field, Section};
use crate::config::Config;

/// The most trackers the list holds.
pub const MAX_TRACKERS: usize = 1000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Trackers {
    /// Keep seeders and trackers-reporting fresh.
    pub scrape: bool,
    /// Announce URLs (udp, http or https) to scrape.
    pub trackers: Vec<String>,
    /// BEP 33 DHT scrapes for new, viewed and rediscovered torrents.
    pub dht_scrape: bool,
    /// Seconds between requests to one UDP tracker; 0 follows the
    /// crawler's intensity.
    pub interval_secs: u64,
    /// Seconds between requests to one HTTP tracker; 0 is three times the
    /// UDP interval.
    pub http_interval_secs: u64,
}

impl Default for Trackers {
    fn default() -> Self {
        Trackers::from_config(&Config::default())
    }
}

/// Checks each tracker URL as the scraper will read it.
fn check_trackers(trackers: &[String]) -> Result<(), String> {
    if trackers.len() > MAX_TRACKERS {
        return Err(format!("Must be at most {MAX_TRACKERS} trackers."));
    }
    for (i, raw) in trackers.iter().enumerate() {
        let n = i + 1;
        let url = TrackerUrl::parse(raw)
            .map_err(|_| format!("Tracker {n} isn't a udp://, http:// or https:// tracker URL."))?;
        if url.scheme() != Scheme::Udp && url.scrape_url().is_none() {
            return Err(format!(
                "Tracker {n}: an HTTP tracker URL must end in /announce to be scraped."
            ));
        }
    }
    Ok(())
}

impl Section for Trackers {
    const SECTION: &'static str = "trackers";
    const FIELDS: &'static [Field] = &[
        Field::new("scrape")
            .file("scrape.enabled")
            .env("LODESTAR_SCRAPE_ENABLED")
            .restart()
            .group("scrape")
            .keywords(),
        Field::new("trackers")
            .file("scrape.trackers")
            .restart()
            .group("scrape"),
        Field::new("dht_scrape")
            .file("scrape.dht")
            .restart()
            .group("scrape")
            .advanced(),
        Field::new("interval_secs")
            .file("scrape.interval_secs")
            .restart()
            .range(0, 86400)
            .auto(Auto::Int(0))
            .unit("seconds")
            .group("cadence")
            .advanced(),
        Field::new("http_interval_secs")
            .file("scrape.http_interval_secs")
            .restart()
            .range(0, 86400)
            .auto(Auto::Int(0))
            .unit("seconds")
            .group("cadence")
            .advanced(),
    ];

    fn from_config(config: &Config) -> Self {
        let s = &config.scrape;
        Trackers {
            scrape: s.enabled,
            trackers: s.trackers.clone(),
            dht_scrape: s.dht,
            interval_secs: s.interval_secs.unwrap_or(0),
            http_interval_secs: s.http_interval_secs.unwrap_or(0),
        }
    }

    fn apply(&self, field: &str, config: &mut Config) {
        let s = &mut config.scrape;
        let auto = |n: u64| (n != 0).then_some(n);
        match field {
            "scrape" => s.enabled = self.scrape,
            "trackers" => s.trackers = self.trackers.clone(),
            "dht_scrape" => s.dht = self.dht_scrape,
            "interval_secs" => s.interval_secs = auto(self.interval_secs),
            "http_interval_secs" => s.http_interval_secs = auto(self.http_interval_secs),
            _ => {}
        }
    }

    fn check(&self, field: &str) -> Result<(), String> {
        match field {
            "trackers" => check_trackers(&self.trackers),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trackers_are_checked_by_position() {
        let mut t = Trackers::default();
        assert!(!t.trackers.is_empty() && t.check("trackers").is_ok());
        t.trackers = vec![
            "udp://tracker.example:6969/announce".into(),
            "https://private.example/passkey123/stats".into(),
        ];
        let e = t.check("trackers").unwrap_err();
        assert!(
            e.starts_with("Tracker 2") && !e.contains("passkey123"),
            "{e}"
        );
        t.trackers[1] = "wss://x.example/".into();
        assert!(t.check("trackers").is_err());
    }

    #[test]
    fn applied_values_reach_the_scraper_config() {
        let t = Trackers {
            scrape: false,
            trackers: vec!["udp://a.example:1/announce".into()],
            dht_scrape: false,
            interval_secs: 0,
            http_interval_secs: 30,
        };
        let mut c = Config::default();
        for f in Trackers::FIELDS {
            t.apply(f.name, &mut c);
        }
        assert!(!c.scrape.enabled && !c.scrape.dht);
        assert_eq!(c.scrape.trackers.len(), 1);
        assert_eq!(
            (c.scrape.interval_secs, c.scrape.http_interval_secs),
            (None, Some(30))
        );
        assert_eq!(Trackers::from_config(&c), t);
    }
}
