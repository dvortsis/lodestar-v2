//! The filters and classification API (architecture §20.7–§20.12): the
//! rule set, the review queue, changes and their previews, the editor's
//! live test, and classification rules and corrections.

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::REDACTED;
use super::error::ErrorBody;

/// What an override, exception or allow entry matches, as sent: exactly
/// one of the four.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct MatchInput {
    /// Info hashes, 40 hex digits each.
    pub hashes: Option<Vec<String>>,
    pub keyword: Option<String>,
    pub query: Option<String>,
    /// A payload signature, 16 hex digits.
    pub payload: Option<String>,
}

/// One change to filtering (§20.7), tagged by `op`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Change {
    CoreSwitch {
        rule: String,
        on: bool,
    },
    /// A pack (`malware`) or one pack rule (`MW-2`).
    PackSwitch {
        id: String,
        on: bool,
    },
    AddRule {
        query: String,
        /// `flag`, `demote`, `hide`, `quarantine` or `drop`.
        action: String,
        unless: Option<String>,
        shadow_days: Option<u32>,
    },
    UpdateRule {
        id: i64,
        query: Option<String>,
        action: Option<String>,
        enabled: Option<bool>,
        /// An empty string removes the clause.
        unless: Option<String>,
        shadow_days: Option<u32>,
    },
    RemoveRule {
        id: i64,
    },
    AddOverride {
        #[serde(flatten)]
        what: MatchInput,
        #[serde(default)]
        neutralize: bool,
        scope: Option<String>,
        expires_days: Option<u32>,
        shadow_days: Option<u32>,
    },
    RemoveOverride {
        id: i64,
    },
    AddException {
        rule: String,
        #[serde(flatten)]
        what: MatchInput,
    },
    RemoveException {
        id: i64,
    },
    AddAllow {
        query: String,
    },
    RemoveAllow {
        id: i64,
    },
    /// Report torrents as child-abuse material (§20.5.2).
    ReportAbuse {
        hashes: Vec<String>,
    },
    RemoveCs1 {
        hashes: Vec<String>,
    },
    /// A quarantined torrent a rule caught wrongly (§20.12): an exception
    /// for that torrent and that rule only, a hashed must-not-match
    /// fixture for a core rule, and a release.
    FalsePositive {
        hash: String,
    },
    /// Releases a quarantined or dropped torrent: fetched and processed
    /// normally next time it's seen.
    Release {
        hash: String,
    },
    /// Confirms a quarantined torrent as abuse material: into CS-1, sealed
    /// name deleted.
    Confirm {
        hash: String,
    },
    RevealName {
        hash: String,
    },
    Subscribe {
        url: String,
        name: Option<String>,
        /// `content` or `ip`.
        kind: String,
        /// `hashes`, `p2p`, `dat` or `cidr`.
        format: Option<String>,
        /// `drop` (the default) or `quarantine`.
        action: Option<String>,
        #[serde(default)]
        floor: bool,
        refresh_hours: Option<u32>,
    },
    Unsubscribe {
        id: i64,
    },
    PauseSubscription {
        id: i64,
        enabled: bool,
    },
    /// Accepts or discards a version the sanity gates held back.
    ResolveHeld {
        id: i64,
        accept: bool,
    },
    /// Settles floor entries a config file or a floor-marked list dropped
    /// (owner's decision §2.7). They stay blocked until this: `remove`
    /// takes them out of the floor, which weakens it; otherwise they stay
    /// in it as ordinary CS-1b entries. `source` is a pending source's key
    /// (`file:<path>`, `list:<id>`).
    ///
    /// A removal confirmed with the password must name its entries: send
    /// back the assessment's [`Assessment::floor_entries`], so the
    /// password confirms what the warning listed. Keeping may leave
    /// `hashes` out, which keeps what the assessment counted.
    ResolveFloorRemoval {
        source: String,
        /// 40 hex digits each, at most 10,000.
        hashes: Option<Vec<String>>,
        #[serde(default)]
        remove: bool,
    },
    Undo,
}

/// `POST /filters/preview`, `/filters/preview/jobs` and `/filters/apply`.
/// `Debug` shows whether a password was sent, never the password.
#[derive(Clone, Serialize, Deserialize, ToSchema)]
pub struct ChangeBody {
    pub change: Change,
    /// The admin password, for a change that weakens filtering (§20.11);
    /// ignored by previews.
    pub password: Option<String>,
}

impl std::fmt::Debug for ChangeBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChangeBody")
            .field("change", &self.change)
            .field("password", &self.password.as_ref().map(|_| REDACTED))
            .finish()
    }
}

/// What a change would do (§20.10). Counts come from the review queue,
/// the tombstones, the filter hits of the replay window and the index.
///
/// Fields are in the order `POST /filters/preview` has always sent them;
/// where an assessment is nested in another body, its keys are sorted.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Preview {
    /// Quarantined items it would release now.
    pub quarantine: usize,
    /// Dropped items it would let be fetched again.
    pub tombstones: usize,
    /// Stored items whose hide, demote or flag mark it would clear.
    pub stored: u64,
    /// Decisions in the replay window by the rules it affects.
    pub caught_30d: i64,
    /// Of those, by the child-safety core.
    pub core_caught_30d: i64,
    pub per_day: f64,
    /// Counts from names only (sealed names, no file lists), so files'
    /// evidence isn't re-checked: treat as an estimate.
    pub estimate: bool,
    /// Entries that would leave the locked floor.
    pub removed_from_floor: usize,
}

/// A change's assessment (§20.10, §20.11). Field order: see [`Preview`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Assessment {
    pub weakening: bool,
    /// The password is needed (every weakening change, and marking a list
    /// as part of the locked floor).
    pub confirm: bool,
    /// The plain-language effect warning; null for a strengthening change.
    #[schema(required = true)]
    pub warning: Option<String>,
    pub preview: Preview,
    /// For a keyword override: the same entry as *Neutralize term*.
    #[schema(required = true)]
    pub neutralize: Option<Preview>,
    /// For a floor removal: the entries the warning counts, 40 hex digits
    /// each. A removal settles at most 10,000 at a time, so a longer
    /// pending list goes in batches. Send these back as `hashes` to
    /// confirm the removal with the password.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub floor_entries: Option<Vec<String>>,
    /// For a subscription change: how strict your lists would be after
    /// it, as the enabled lists together (§2.9). Subscribing shows what
    /// a list adds; unsubscribing or pausing shows what's left.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub strictness: Option<Strictness>,
    /// All of them, for applying.
    #[serde(skip)]
    pub(crate) settles: Option<Vec<[u8; 20]>>,
}

/// `POST /filters/apply`: the change took effect.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ApplyResponse {
    /// Always `true`.
    pub applied: bool,
    #[serde(serialize_with = "super::sorted")]
    pub assessment: Assessment,
    /// `report_abuse`, `confirm`: stored torrents deleted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub deleted: Option<usize>,
    /// `false_positive`: the exception's id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub exception: Option<i64>,
    /// `add_rule`, `add_override`, `add_exception`, `add_allow`,
    /// `subscribe`: the new entry's id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub id: Option<i64>,
    /// `reveal_name`: the sealed name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub name: Option<String>,
    /// `core_switch` off, `remove_rule`, `unsubscribe`,
    /// `pause_subscription` off, `resolve_held` accepted: hashes released
    /// from quarantine and tombstones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub released: Option<usize>,
    /// `remove_cs1`: hashes removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub removed: Option<usize>,
    /// `resolve_floor_removal`: pending floor entries settled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub settled: Option<usize>,
    /// The rule set's version after the change.
    pub version: u64,
}

/// `POST /filters/preview/jobs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct JobStarted {
    pub job: u64,
}

/// `GET /filters/preview/jobs/{id}`: progress while it runs, then the
/// assessment or the error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct JobStatus {
    /// Once `done`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "super::sorted_some"
    )]
    #[schema(nullable = false)]
    pub assessment: Option<Assessment>,
    /// Once `failed`: the error the preview would have returned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub error: Option<ErrorBody>,
    pub progress: JobProgress,
    pub state: JobState,
    /// Once `failed`: the HTTP status of `error`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub status: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Running,
    Done,
    Failed,
    /// Stopped by `POST /filters/preview/jobs/{id}/cancel`.
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct JobProgress {
    /// Items of this phase done.
    pub done: u64,
    pub phase: JobPhase,
    /// Items in this phase.
    pub total: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobPhase {
    Reading,
    Quarantine,
    Tombstones,
    Done,
}

/// `GET /filters`: the rule set, switches and lists, as the Filters &
/// Safety screens show them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Overview {
    pub allow: Vec<AllowEntry>,
    pub core: Vec<CoreRuleState>,
    /// Hashes in the child-abuse list.
    pub cs1_entries: usize,
    pub exceptions: Vec<ExceptionEntry>,
    /// Ranges in the IP lists.
    pub ip_ranges: usize,
    pub overrides: Vec<OverrideEntry>,
    pub packs: Vec<PackState>,
    /// Entries a config file or a floor-marked list dropped, still blocked
    /// until you settle them (§2.7).
    pub pending_floor_removals: Vec<PendingFloorEntry>,
    /// Items in quarantine.
    pub quarantine: usize,
    pub rules: Vec<RuleEntry>,
    pub subscriptions: Vec<SubscriptionEntry>,
    pub version: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AllowEntry {
    pub id: i64,
    pub query: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CoreRuleState {
    pub action: String,
    /// Part of the locked floor: can't be switched off.
    pub floor: bool,
    #[schema(example = "CS-3a")]
    pub id: String,
    pub on: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PackState {
    #[schema(example = "malware")]
    pub id: String,
    pub on: bool,
    pub rules: Vec<PackRuleState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PackRuleState {
    pub action: String,
    #[schema(example = "MW-2")]
    pub id: String,
    pub on: bool,
}

/// One of your rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RuleEntry {
    /// Null for an action code this server doesn't know.
    #[schema(required = true)]
    pub action: Option<String>,
    pub enabled: bool,
    pub id: i64,
    pub query: String,
    /// Shadow mode ends then: until then it only records what it would do.
    #[schema(required = true)]
    pub shadow_until: Option<i64>,
    #[schema(required = true)]
    pub unless: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OverrideEntry {
    #[schema(required = true)]
    pub expires_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub hashes: Option<Vec<String>>,
    pub id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub keyword: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub payload: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub query: Option<String>,
    /// Applies only to what this query matches; null = everything.
    #[schema(required = true)]
    pub scope: Option<String>,
    #[schema(required = true)]
    pub shadow_until: Option<i64>,
    /// `override` or `neutralize`.
    pub strength: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ExceptionEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub hashes: Option<Vec<String>>,
    pub id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub keyword: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub payload: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub query: Option<String>,
    /// The rule it releases from.
    pub rule: String,
}

/// Floor entries one source dropped, waiting for a decision (§2.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PendingFloorEntry {
    pub entries: usize,
    /// Up to 1,000 of them, 40 hex digits each.
    pub hashes: Vec<String>,
    /// `file` or `list`.
    pub kind: String,
    /// The file's path, or the list's name.
    pub name: String,
    /// `file:<path>` or `list:<id>`, for `resolve_floor_removal`.
    pub source: String,
}

/// `POST /filters/strictness`: a candidate set of lists to assess,
/// subscriptions by id and catalog lists by key (`lodestar lists`), in
/// any mix.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct StrictnessBody {
    #[serde(default)]
    pub subscriptions: Vec<i64>,
    #[serde(default)]
    pub catalog: Vec<String>,
}

/// `POST /filters/strictness`: what the lists would block, and a warning
/// matched to it (§2.9, §20.13).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Strictness {
    /// `anti_p2p`, `peers`, `content`, `child_abuse`, …
    pub categories: Vec<String>,
    /// Some figures are the catalog's measurements of lists not fetched
    /// yet, and lists may overlap: a lower bound.
    pub estimate: bool,
    /// Torrents a floor-marked list blocks by hash.
    pub floor_hashes: u64,
    /// Torrents blocked by hash.
    pub hashes: u64,
    /// The share of public IPv4 blocked (0–1); the level reads this.
    pub ipv4_public_share: f64,
    /// The share of all IPv4 blocked (0–1).
    pub ipv4_share: f64,
    pub ipv6_ranges: usize,
    /// The share of IPv6 global unicast blocked (0–1).
    pub ipv6_share: f64,
    /// `none`, `notice`, `strong` or `severe`.
    pub level: String,
    pub lists: Vec<StrictnessList>,
    /// The plain-language warning.
    pub text: String,
    /// Lists never fetched, so what they block isn't known.
    pub unknown: Vec<String>,
}

/// One list's figures in a [`Strictness`]; null where unknown.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct StrictnessList {
    pub categories: Vec<String>,
    pub floor: bool,
    #[schema(required = true)]
    pub hashes: Option<u64>,
    #[schema(required = true)]
    pub ipv4_public_share: Option<f64>,
    #[schema(required = true)]
    pub ipv4_share: Option<f64>,
    #[schema(required = true)]
    pub ipv6_ranges: Option<usize>,
    /// `content` or `ip`.
    pub kind: String,
    /// The figures are a catalog measurement, not this list's data.
    pub measured: bool,
    pub name: String,
}

/// `GET /filters/subscriptions`: the one-click catalog, your lists, and
/// how strict the enabled ones are together (§2.9, §20.13). Nothing is
/// subscribed by default; connecting is `subscribe` through
/// `/filters/preview` and `/filters/apply`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Subscriptions {
    pub catalog: Vec<CatalogList>,
    /// Your lists, paused ones included, each with its own figures.
    pub connected: Vec<ConnectedList>,
    /// The enabled lists together.
    pub strictness: Strictness,
}

/// A list in the one-click catalog. Send `url`, `kind` and `format` as a
/// `subscribe` change to connect it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct CatalogList {
    /// What it blocks, in plain language.
    pub about: String,
    /// Its catalog measurement, or its own data once connected.
    pub figures: StrictnessList,
    /// `hashes`, `p2p`, `dat` or `cidr`.
    pub format: String,
    pub key: String,
    /// `content` or `ip`.
    pub kind: String,
    pub name: String,
    /// The subscription's id once you connected it.
    #[schema(required = true)]
    pub subscription: Option<i64>,
    pub url: String,
}

/// One of your lists and its own figures.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ConnectedList {
    pub figures: StrictnessList,
    /// A version the sanity gates held back waits for *Accept anyway*
    /// or *Keep previous* (`resolve_held`).
    pub held: bool,
    pub subscription: SubscriptionEntry,
}

/// A list subscription (§20.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SubscriptionEntry {
    #[schema(required = true)]
    pub action: Option<String>,
    #[schema(required = true)]
    pub checked_at: Option<i64>,
    pub enabled: bool,
    pub entries: i64,
    #[schema(required = true)]
    pub error: Option<String>,
    pub floor: bool,
    #[schema(required = true)]
    pub format: Option<String>,
    pub id: i64,
    /// `content` or `ip`.
    pub kind: String,
    pub name: String,
    /// `new`, `ok`, `unchanged`, `held` or `error`.
    pub status: String,
    #[schema(required = true)]
    pub updated_at: Option<i64>,
    pub url: String,
}

/// `GET /filters/queue`: one page of the quarantine, newest first.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[serde(default)]
#[into_params(parameter_in = Query)]
pub struct QueueQuery {
    /// The previous page's `next_cursor`.
    pub cursor: Option<String>,
    /// Quarantine rows per page: 1–1000, 1000 by default.
    #[param(minimum = 1, maximum = 1000)]
    pub limit: Option<usize>,
}

/// `GET /filters/tombstones`: one page of tombstones, newest first.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[serde(default)]
#[into_params(parameter_in = Query)]
pub struct TombstonesQuery {
    /// Only adult holds: torrents held because you relabelled them adult,
    /// which the 30-day purge left as tombstones.
    pub adult: bool,
    /// The previous page's `next_cursor`.
    pub cursor: Option<String>,
    /// Tombstones per page: 1–1000, 1000 by default.
    #[param(minimum = 1, maximum = 1000)]
    pub limit: Option<usize>,
}

/// `GET /filters/tombstones`: dropped torrents, by hash and rule (no
/// names), so one can be found and released.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Tombstones {
    /// The next page, if there is one: pass as `cursor`.
    #[schema(required = true)]
    pub next_cursor: Option<String>,
    pub tombstones: Vec<TombstoneEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TombstoneEntry {
    /// Left by an adult hold's quarantine at the 30-day purge.
    pub adult_hold: bool,
    pub at: i64,
    pub hash: String,
    pub rule: String,
}

/// `GET /filters/queue`: quarantine (never with names) and the replay
/// window's decisions per rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Queue {
    pub decisions_30d: Vec<DecisionCount>,
    /// The next page of quarantine, if there is one: pass as `cursor`.
    #[schema(required = true)]
    pub next_cursor: Option<String>,
    pub quarantine: Vec<QuarantineEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DecisionCount {
    #[schema(required = true)]
    pub action: Option<String>,
    pub count: i64,
    pub rule: String,
    /// Decided in shadow mode: recorded, not acted on.
    pub shadow: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct QuarantineEntry {
    /// Held only because you relabelled it adult (a correction or a
    /// classification rule) and the Adult-only core caught it: at
    /// `expires_at` it becomes a tombstone rather than being released.
    pub adult_hold: bool,
    pub at: i64,
    /// When its sealed name is deleted and its quarantine ends.
    pub expires_at: i64,
    pub hash: String,
    pub rule: String,
    /// A sealed name is kept (see `reveal_name`).
    pub sealed: bool,
}

/// `GET /filters/fixtures`: the core rules' hashed examples, run against
/// the rules as they are (§20.8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Fixtures {
    pub core: Vec<FixtureCounts>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FixtureCounts {
    /// Examples the rule now gets wrong.
    pub failing: usize,
    pub must_match: usize,
    pub must_not_match: usize,
    pub rule: String,
}

/// One example torrent for `POST /filters/test`, never stored or logged.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Sample {
    pub name: String,
    #[serde(default)]
    pub files: Vec<SampleFile>,
    /// A content type (`movie`, `adult`, …), replacing the classifier's.
    #[serde(rename = "type")]
    pub content_type: Option<String>,
}

/// A sample's file: a path, or a path and its size.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum SampleFile {
    Path(String),
    Sized { path: String, size: u64 },
}

/// `POST /filters/test`: a draft rule and examples to try it on.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TestBody {
    pub query: Option<String>,
    pub unless: Option<String>,
    /// At most 50.
    pub samples: Vec<Sample>,
}

/// `POST /filters/test`: per sample, in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TestResponse {
    pub results: Vec<TestResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TestResult {
    /// What the current rules decide; null = nothing.
    #[schema(required = true)]
    pub decision: Option<Decision>,
    /// Whether the draft matches; null without a draft.
    #[schema(required = true)]
    pub draft: Option<bool>,
    /// Rules in shadow mode that would have decided.
    pub shadow: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Decision {
    pub action: String,
    pub rule: String,
}

/// `POST /classify/correct`: your correction to one stored torrent,
/// replacing any earlier one. `adult: null` and `not_junk: false` clear it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CorrectBody {
    pub hash: String,
    pub adult: Option<bool>,
    #[serde(default)]
    pub not_junk: bool,
}

/// `POST /classify/correct`: the torrent classified again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CorrectResponse {
    /// Non-null only when `type` is `adult`.
    #[schema(required = true)]
    pub adult_kind: Option<String>,
    /// When relabelling it adult made filtering hold it back (§20.9): the
    /// rule that did. It's then dropped or quarantined, no longer stored.
    #[schema(required = true)]
    pub held_by: Option<String>,
    /// Junk score, 0–100.
    pub junk: u8,
    #[serde(rename = "type")]
    pub content_type: String,
}

/// `GET /classify`: your classification rules, and how detection
/// compares with your per-torrent corrections (§20.9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Classes {
    pub corrections: usize,
    pub detection: Detection,
    pub rules: Vec<ClassRule>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Detection {
    /// Corrections that say adult or not.
    pub checked: usize,
    /// Of those, ones detection gets wrong.
    pub disagree: usize,
    /// Up to 50 of them.
    pub examples: Vec<DetectionExample>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DetectionExample {
    /// What your correction says.
    pub adult: bool,
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ClassRule {
    pub adult: bool,
    pub id: i64,
    pub query: String,
}

/// `POST /classify/rules`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ClassRuleBody {
    /// Can't test `type:` or `adult:`: the rule decides them.
    pub query: String,
    pub adult: bool,
}

/// `POST /classify/rules`: the new rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ClassRuleAdded {
    pub id: i64,
    /// Stored torrents queued to be classified again; null without a
    /// search index (only torrents arriving later are affected).
    #[schema(required = true)]
    pub queued: Option<usize>,
    /// More stored torrents matched than are queued at once (1,000,000):
    /// the rest are classified again when next re-enriched.
    pub truncated: bool,
}

/// `POST /classify/rules/remove`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct IdBody {
    pub id: i64,
}

/// `POST /classify/rules/remove`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ClassRuleRemoved {
    /// As [`ClassRuleAdded::queued`].
    #[schema(required = true)]
    pub queued: Option<usize>,
    /// As [`ClassRuleAdded::truncated`].
    pub truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_shows_the_password() {
        let body = ChangeBody {
            change: Change::Undo,
            password: Some("hunter2-correct-horse".into()),
        };
        let shown = format!("{body:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(
            shown.contains(REDACTED) && shown.contains("Undo"),
            "{shown}"
        );
        let none = format!(
            "{:?}",
            ChangeBody {
                change: Change::Undo,
                password: None,
            }
        );
        assert!(none.contains("password: None"), "{none}");
    }
}
