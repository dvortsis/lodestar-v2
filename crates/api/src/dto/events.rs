//! `GET /events` (§9): the server-sent event stream's topics and the
//! payload of each event, which travels as the frame's `data:` line.
//!
//! **Payloads are small and carry no names or hashes**: a client that
//! wants a torrent asks the API for it, so nothing private (§18.7) leaves
//! through the stream, and the live banner (§10.4) needs only counts.

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

/// What a client subscribes to with `?topics=`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Topic {
    /// New torrents became searchable ([`TorrentsEvent`]).
    Torrents,
    /// A crawler stats snapshot every 5 s ([`CrawlerEvent`]).
    Crawler,
    /// Index rebuild progress when it changes ([`RebuildEvent`]).
    Rebuild,
    /// Saved-search alerts (reserved for B9; [`AlertsEvent`]).
    Alerts,
    /// Insights deltas every 30 s (reserved for B11a; [`InsightsEvent`]).
    Insights,
}

impl Topic {
    pub const ALL: [Topic; 5] = [
        Topic::Torrents,
        Topic::Crawler,
        Topic::Rebuild,
        Topic::Alerts,
        Topic::Insights,
    ];

    /// The name in `?topics=` and in the frame's `event:` line.
    pub fn name(self) -> &'static str {
        match self {
            Topic::Torrents => "torrents",
            Topic::Crawler => "crawler",
            Topic::Rebuild => "rebuild",
            Topic::Alerts => "alerts",
            Topic::Insights => "insights",
        }
    }

    pub fn parse(name: &str) -> Option<Topic> {
        Topic::ALL.into_iter().find(|t| t.name() == name)
    }

    /// A bit for topic sets.
    pub(crate) fn bit(self) -> u8 {
        1 << self as u8
    }
}

/// `GET /events` parameters.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct EventsQuery {
    /// Comma-separated topics, e.g. `torrents,crawler`; all of them when
    /// absent. An unknown or empty one is a 400 with `field: "topics"`.
    #[param(value_type = Option<Vec<Topic>>, style = Form, explode = false)]
    pub topics: Option<String>,
}

/// `event: torrents`: torrents became searchable. Commits close together
/// may arrive as one event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({ "added": 12, "max_id": 48211 }))]
pub struct TorrentsEvent {
    /// Torrents new to the index since the previous `torrents` event;
    /// can be 0 when only `max_id` moved on.
    pub added: u64,
    /// The highest torrent id searchable now.
    pub max_id: i64,
}

/// `event: crawler`: the same counters as the status page and
/// `/metrics`, sent every 5 s while anyone subscribes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({
    "at": 1_700_000_000u64, "dht_nodes": 812, "fetch_active": 40, "fetch_pending": 1200,
    "fetched": 3400, "harvested": 91000, "stored": 3100, "torrents": 48211
}))]
pub struct CrawlerEvent {
    /// Unix seconds.
    pub at: u64,
    /// Nodes in the DHT routing table; null when the DHT is off or
    /// hasn't reported yet.
    #[schema(required = true)]
    pub dht_nodes: Option<u64>,
    /// Metadata fetches running now.
    pub fetch_active: u64,
    /// Hashes waiting for a fetch.
    pub fetch_pending: u64,
    /// Metadata fetched since start.
    pub fetched: u64,
    /// Hashes received from sources since start.
    pub harvested: u64,
    /// Torrents stored since start.
    pub stored: u64,
    /// Torrents in the store.
    pub torrents: u64,
}

/// A rebuild's phase; `done` once the new index serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RebuildPhase {
    Building,
    Finishing,
    Swapping,
    Failed,
    Done,
}

/// `event: rebuild`: index rebuild progress (§8, the "Index rebuilding"
/// banner of §10.4), sent when it changes, and once more with `done`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RebuildEvent {
    /// Torrents in the new index so far.
    pub done: u64,
    /// Why it stopped, once `failed`.
    #[schema(required = true)]
    pub error: Option<String>,
    /// The index serving meanwhile, e.g. `v3-t1`.
    pub from: String,
    pub phase: RebuildPhase,
    /// The index being built.
    pub to: String,
    /// Torrents to build before finishing.
    pub total: u64,
    /// Fields that match nothing until the swap.
    pub unavailable: Vec<String>,
}

/// `event: alerts` (reserved: B9 publishes it): a saved search matched
/// new torrents. The matches themselves come from the API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AlertsEvent {
    /// The alert's id.
    pub alert: i64,
    /// New matches.
    pub matched: u64,
    /// The highest matching torrent id.
    pub max_id: i64,
    /// The saved search's id.
    pub saved_search: i64,
}

/// `event: insights` (reserved: B11a publishes it every 30 s): the stage
/// strip's headline numbers (`docs/metrics.md` §6.1, §7.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct InsightsEvent {
    pub stages: InsightsStages,
    /// Unix seconds.
    pub updated_at: i64,
}

/// `docs/metrics.md` §7.5 `stages`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct InsightsStages {
    /// Always approximate.
    pub discover: u64,
    pub fetch: u64,
    pub index: u64,
    pub serve: u64,
    pub understand: u64,
}

/// Why a `resync` was sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResyncReason {
    /// `Last-Event-ID` is from before a restart.
    Restarted,
    /// The events after `Last-Event-ID` are no longer kept.
    Expired,
    /// `Last-Event-ID` isn't one of ours.
    Malformed,
    /// The client fell behind and events were skipped.
    Lagged,
}

/// `event: resync`: events were missed; refetch what the page shows, then
/// carry on with the events that follow. It has no `id:`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({ "reason": "lagged" }))]
pub struct ResyncEvent {
    pub reason: ResyncReason,
}

/// One frame of the stream: `event:` names the variant and `data:` holds
/// its payload. Every event but `resync` has an `id:` of the form
/// `<epoch>-<seq>` to send back as `Last-Event-ID`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum Event {
    Torrents(TorrentsEvent),
    Crawler(CrawlerEvent),
    Rebuild(RebuildEvent),
    Alerts(AlertsEvent),
    Insights(InsightsEvent),
    Resync(ResyncEvent),
}

impl Event {
    /// The topic it's published on; `None` for `resync`, which every
    /// client gets.
    pub fn topic(&self) -> Option<Topic> {
        Some(match self {
            Event::Torrents(_) => Topic::Torrents,
            Event::Crawler(_) => Topic::Crawler,
            Event::Rebuild(_) => Topic::Rebuild,
            Event::Alerts(_) => Topic::Alerts,
            Event::Insights(_) => Topic::Insights,
            Event::Resync(_) => return None,
        })
    }

    /// The `event:` line.
    pub fn name(&self) -> &'static str {
        self.topic().map_or("resync", Topic::name)
    }

    /// The `data:` line: the payload alone.
    pub fn data(&self) -> String {
        let v = match self {
            Event::Torrents(e) => serde_json::to_string(e),
            Event::Crawler(e) => serde_json::to_string(e),
            Event::Rebuild(e) => serde_json::to_string(e),
            Event::Alerts(e) => serde_json::to_string(e),
            Event::Insights(e) => serde_json::to_string(e),
            Event::Resync(e) => serde_json::to_string(e),
        };
        v.expect("event payloads serialize")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_names_round_trip_and_match_serde() {
        for t in Topic::ALL {
            assert_eq!(Topic::parse(t.name()), Some(t));
            assert_eq!(serde_json::to_value(t).unwrap(), t.name());
        }
        assert_eq!(Topic::parse("Torrents"), None);
        assert_eq!(Topic::parse(""), None);
    }

    #[test]
    fn the_frame_name_and_data_match_the_tagged_schema() {
        let e = Event::Torrents(TorrentsEvent {
            added: 3,
            max_id: 9,
        });
        let tagged = serde_json::to_value(&e).unwrap();
        assert_eq!(tagged["event"], e.name());
        assert_eq!(
            tagged["data"],
            serde_json::from_str::<serde_json::Value>(&e.data()).unwrap()
        );
        assert_eq!(e.data(), r#"{"added":3,"max_id":9}"#);
        let r = Event::Resync(ResyncEvent {
            reason: ResyncReason::Lagged,
        });
        assert_eq!((r.name(), r.topic()), ("resync", None));
        assert_eq!(r.data(), r#"{"reason":"lagged"}"#);
    }
}
