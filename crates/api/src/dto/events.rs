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
    /// Saved-search alerts ([`AlertsEvent`]).
    Alerts,
    /// Insights deltas every 30 s (reserved for B11a; [`InsightsEvent`]).
    Insights,
    /// A settings section was saved ([`SettingsEvent`]).
    Settings,
    /// The update engine's state changed ([`UpdatesEvent`]).
    Updates,
    /// New matches for your own live queries ([`SearchLiveEvent`]). Last,
    /// so the other topics keep their bits.
    SearchLive,
}

impl Topic {
    pub const ALL: [Topic; 8] = [
        Topic::Torrents,
        Topic::Crawler,
        Topic::Rebuild,
        Topic::Alerts,
        Topic::Insights,
        Topic::Settings,
        Topic::Updates,
        Topic::SearchLive,
    ];

    /// The name in `?topics=` and in the frame's `event:` line.
    pub fn name(self) -> &'static str {
        match self {
            Topic::Torrents => "torrents",
            Topic::Crawler => "crawler",
            Topic::Rebuild => "rebuild",
            Topic::Alerts => "alerts",
            Topic::Insights => "insights",
            Topic::Settings => "settings",
            Topic::Updates => "updates",
            Topic::SearchLive => "search_live",
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

// Topic sets are a `u8`: past 8 topics, widen it (§10.10.9).
const _: () = assert!(Topic::ALL.len() <= u8::BITS as usize);

/// `GET /events` parameters.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct EventsQuery {
    /// Comma-separated topics, e.g. `torrents,crawler`; all of them when
    /// absent. An unknown or empty one is a 400 with `field: "topics"`.
    #[param(value_type = Option<Vec<Topic>>, style = Form, explode = false)]
    pub topics: Option<String>,
}

/// `event: torrents`: torrents you can see became searchable. Commits
/// close together may arrive as one event. While "Show adult content" is
/// off, Adult torrents are left out entirely: not counted, and a commit
/// of only those sends no event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({ "added": 12, "max_id": 48211 }))]
pub struct TorrentsEvent {
    /// Torrents new to the index since the previous `torrents` event,
    /// leaving out Adult while it's hidden; at least 1.
    pub added: u64,
    /// The highest torrent id searchable now, while Adult is shown.
    /// **Null while Adult is hidden:** ids are sequential, so any real id
    /// sent beside an exact `added` would show, in its gap from the last
    /// one, how many hidden torrents arrived. To find what's new, use the
    /// newest id among your own results (or a cursor), not this.
    #[schema(required = true)]
    pub max_id: Option<i64>,
}

/// `event: crawler`: the same counters as the status page and
/// `/metrics`, sent every 5 s while anyone subscribes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({
    "at": 1_700_000_000u64, "dht_nodes": 812, "fetch_active": 40, "fetch_pending": 1200,
    "fetched": 3400, "harvested": 91000, "paused": false, "stored": 3100, "torrents": 48211
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
    /// The crawler is paused (`POST /crawler/pause`): the counters stand
    /// still because nothing is harvested or fetched, not because
    /// something broke.
    pub paused: bool,
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

/// `event: alerts`: a saved search matched torrents that just became
/// searchable. One event per saved search per percolation run; the
/// matches themselves come from `GET /saved-searches/{id}/alerts`. Only
/// torrents search would show count: none a filter rule hid, and no
/// Adult while "Show adult content" is off (§20.9). A run whose new
/// matches are all hidden sends no event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({ "alert": 913, "matched": 2, "max_id": 48211, "notify": true, "saved_search": 4 }))]
pub struct AlertsEvent {
    /// The highest new alert's id (shown ones only).
    pub alert: i64,
    /// New alerts on torrents you can see; at least 1.
    pub matched: u64,
    /// The highest of their torrent ids.
    pub max_id: i64,
    /// The saved search alerts in the app (its `alert` flag): notify only
    /// when true. Otherwise the event only refreshes counts.
    pub notify: bool,
    /// The saved search's id.
    pub saved_search: i64,
}

/// `event: insights`, every 30 s while anyone subscribes: the default
/// `7d` overview's stage strip (`docs/metrics.md` §6.1, §7.5, §7.7). A
/// client showing another range refetches its own on each event.
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
    /// Null when the range starts before fetches were counted by event
    /// time.
    #[schema(required = true)]
    pub fetch: Option<u64>,
    /// Null with search off.
    #[schema(required = true)]
    pub index: Option<u64>,
    /// Null until the search counters exist.
    #[schema(required = true)]
    pub serve: Option<u64>,
    pub understand: u64,
}

/// `event: settings`: a section was saved (by any tab, device or API
/// client) and is now at `revision`. It carries no values: fetch them
/// with `GET /settings/{section}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({ "revision": 4, "section": "crawler" }))]
pub struct SettingsEvent {
    pub revision: u64,
    pub section: String,
}

/// Where the update engine is (§12.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UpdatesState {
    Checking,
    /// A check ended with nothing to install (every check ends with
    /// `checked`, `available` or `failed`).
    Checked,
    Available,
    Downloading,
    Verifying,
    WaitingWindow,
    Installing,
    Restarting,
    CheckingHealth,
    Done,
    RolledBack,
    Failed,
}

/// `event: updates`: the update engine's state changed. Progress
/// (`bytes_done`) is sent at most once a second; fetch `GET /updates`
/// for the rest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({ "state": "available", "version": "0.6.0", "bytes_done": null, "bytes_total": null, "error": null }))]
pub struct UpdatesEvent {
    pub state: UpdatesState,
    /// The release it concerns, when there is one.
    #[schema(required = true)]
    pub version: Option<String>,
    #[schema(required = true)]
    pub bytes_done: Option<u64>,
    #[schema(required = true)]
    pub bytes_total: Option<u64>,
    /// A stable error code with `failed`.
    #[schema(required = true)]
    pub error: Option<String>,
}

/// `POST /search/live`: the query whose new matches to count, as `/search`
/// takes it.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct SearchLiveRequest {
    /// The query text. Send `q` or `ast`, not both.
    pub q: Option<String>,
    /// A query AST, as `/parse` returns it; it must be one the parser
    /// could produce.
    #[schema(value_type = Option<crate::dto::search::AstNode>)]
    pub ast: Option<lodestar_search::Node>,
    /// Include torrents a filter rule hid (§20.3), as `/search` does.
    pub hidden: Option<bool>,
    /// The user's zone for `time:`, in minutes east of UTC (−840 to 840);
    /// 0 by default.
    pub tz: Option<i32>,
}

/// A live query's registration: listen for `search_live` events with
/// its `id` on `/events?topics=search_live` (or every topic).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({ "id": "4f1c9a0b7d2e8a61", "since": 48211 }))]
pub struct SearchLiveRegistration {
    /// Names it in `search_live` events and in `DELETE /search/live/{id}`.
    pub id: String,
    /// The highest torrent id searchable when it was made: matches are
    /// counted after it. **Null while Adult is hidden**, as `torrents`'
    /// `max_id` is: a real id would show, in its gap from a later one,
    /// how many hidden torrents arrived.
    #[schema(required = true)]
    pub since: Option<i64>,
}

/// `event: search_live`: one of your live queries (`POST /search/live`)
/// has new matches. `matched` counts every match since it was made, so
/// a missed event loses nothing. Each connection gets at most one per
/// live query every 5 s; counts in between are combined. Sent only to
/// the session that made it, never replayed (it has no `id:`), and only
/// torrents search would show count: none a filter rule hid (unless the
/// query asked for them) and no Adult while "Show adult content" is off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({ "id": "4f1c9a0b7d2e8a61", "matched": 3, "through": 48250 }))]
pub struct SearchLiveEvent {
    /// The registration's id.
    pub id: String,
    /// Matches since the registration was made; at least 1.
    pub matched: u64,
    /// The highest torrent id checked so far. **Null while Adult is
    /// hidden** (see `SearchLiveRegistration.since`).
    #[schema(required = true)]
    pub through: Option<i64>,
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
    /// "Show adult content" was turned off after events that counted
    /// Adult were sent; they're withheld rather than replayed.
    Visibility,
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
    Settings(SettingsEvent),
    Updates(UpdatesEvent),
    SearchLive(SearchLiveEvent),
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
            Event::Settings(_) => Topic::Settings,
            Event::Updates(_) => Topic::Updates,
            Event::SearchLive(_) => Topic::SearchLive,
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
            Event::Settings(e) => serde_json::to_string(e),
            Event::Updates(e) => serde_json::to_string(e),
            Event::SearchLive(e) => serde_json::to_string(e),
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
            max_id: Some(9),
        });
        let tagged = serde_json::to_value(&e).unwrap();
        assert_eq!(tagged["event"], e.name());
        assert_eq!(
            tagged["data"],
            serde_json::from_str::<serde_json::Value>(&e.data()).unwrap()
        );
        assert_eq!(e.data(), r#"{"added":3,"max_id":9}"#);
        // Hidden Adult: the key stays, null.
        let hidden = Event::Torrents(TorrentsEvent {
            added: 3,
            max_id: None,
        });
        assert_eq!(hidden.data(), r#"{"added":3,"max_id":null}"#);
        let r = Event::Resync(ResyncEvent {
            reason: ResyncReason::Lagged,
        });
        assert_eq!((r.name(), r.topic()), ("resync", None));
        assert_eq!(r.data(), r#"{"reason":"lagged"}"#);
        let s = Event::Settings(SettingsEvent {
            revision: 4,
            section: "crawler".into(),
        });
        assert_eq!((s.name(), s.topic()), ("settings", Some(Topic::Settings)));
        assert_eq!(s.data(), r#"{"revision":4,"section":"crawler"}"#);
        let l = Event::SearchLive(SearchLiveEvent {
            id: "ab".into(),
            matched: 2,
            through: None,
        });
        assert_eq!(
            (l.name(), l.topic()),
            ("search_live", Some(Topic::SearchLive))
        );
        assert_eq!(l.data(), r#"{"id":"ab","matched":2,"through":null}"#);
    }
}
