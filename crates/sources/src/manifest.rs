//! What a plugin declares about itself (architecture §18.1). The settings
//! UI shows the endpoints, risk and consent class before a plugin can be
//! enabled.

/// What a plugin does. One plugin can be several kinds: the Mainline DHT
/// both discovers keys and finds peers for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Emits keys (plus optional claims) as it finds them.
    Discovery,
    /// Like `Discovery`, run as a resumable job with progress.
    Importer,
    /// Finds peers for keys that have none yet.
    PeerFinder,
    Enrichment,
    /// A whole network tier with its own keys, fetch and link format.
    Network,
    Output,
}

/// What enabling the plugin requires (§18.7), from least to most.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Consent {
    None,
    External,
    Account,
    Restricted,
    Overlay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Risk {
    Low,
    Medium,
    High,
}

/// §18.6 network tiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Network {
    Bt,
    BtI2p,
    Ed2k,
    Ipfs,
    Usenet,
}

/// An external endpoint the plugin contacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Endpoint {
    pub protocol: &'static str,
    pub host: &'static str,
}

/// A source's share of the fetch pool (§18.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Priority {
    Low,
    #[default]
    Normal,
    High,
    Max,
}

impl Priority {
    /// The deficit-round-robin weight.
    pub fn weight(self) -> u32 {
        match self {
            Priority::Low => 1,
            Priority::Normal => 3,
            Priority::High => 6,
            Priority::Max => 10,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// Stable id, recorded in the store's `sources` table. Never rename.
    pub id: &'static str,
    pub name: &'static str,
    pub version: &'static str,
    pub kinds: &'static [Kind],
    pub networks: &'static [Network],
    pub endpoints: &'static [Endpoint],
    pub risk: Risk,
    pub consent: Consent,
    pub default_priority: Priority,
    /// Only the Mainline DHT is on out of the box (§18.1).
    pub enabled_by_default: bool,
}

impl Manifest {
    pub fn is(&self, kind: Kind) -> bool {
        self.kinds.contains(&kind)
    }
}

/// Where a plugin may run once remote workers exist (§18.1, §19.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Distribution {
    /// Any number of workers run it independently.
    Everywhere,
    /// Workers split the key space between them (the DHT).
    ShardedKeyspace,
    /// Workers split the plugin's units (indexers, feeds) between them.
    ShardedUnit,
    /// Exactly one instance runs, wherever the coordinator places it.
    Singleton,
    /// Only the coordinator runs it. The safe default for a plugin that
    /// hasn't said, because it never multiplies requests to a third party.
    Coordinator,
}

/// A daily time window in minutes since local midnight, `start` inclusive
/// and `end` exclusive (§18.4: "importers and full scrapes 01:00–07:00").
/// A window whose end is at or before its start wraps past midnight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub start: u16,
    pub end: u16,
}

/// Minutes in a day.
pub const DAY_MINUTES: u16 = 24 * 60;

impl Window {
    /// `start` and `end` are taken modulo a day.
    pub const fn new(start: u16, end: u16) -> Window {
        Window {
            start: start % DAY_MINUTES,
            end: end % DAY_MINUTES,
        }
    }

    pub fn contains(self, minute: u16) -> bool {
        let m = minute % DAY_MINUTES;
        let (s, e) = (self.start % DAY_MINUTES, self.end % DAY_MINUTES);
        if s < e {
            (s..e).contains(&m)
        } else {
            // Wraps midnight; `s == e` is the whole day.
            m >= s || m < e
        }
    }
}

/// When a plugin may run. An empty list means any time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Schedule(pub &'static [Window]);

impl Schedule {
    pub const ALWAYS: Schedule = Schedule(&[]);

    pub fn is_open(self, minute: u16) -> bool {
        self.0.is_empty() || self.0.iter().any(|w| w.contains(minute))
    }

    /// Minutes from `minute` until a window is open: 0 when one already
    /// is. Always below a day, and 0 for [`Schedule::ALWAYS`].
    pub fn minutes_until_open(self, minute: u16) -> u16 {
        let m = minute % DAY_MINUTES;
        (0..DAY_MINUTES)
            .find(|d| self.is_open((m + d) % DAY_MINUTES))
            .unwrap_or(0)
    }
}

/// Requests to one host: a token bucket of `burst` tokens refilled one
/// per `interval`. In any span of time `t` at most
/// `burst + t / interval` requests go to the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostRate {
    pub interval: std::time::Duration,
    pub burst: u32,
}

impl HostRate {
    /// `n` requests per second, no burst.
    pub const fn per_second(n: u32) -> HostRate {
        let n = if n == 0 { 1 } else { n };
        let ns = 1_000_000_000 / n as u64;
        HostRate {
            // Never zero, which would mean no limit at all.
            interval: std::time::Duration::from_nanos(if ns == 0 { 1 } else { ns }),
            burst: 1,
        }
    }

    /// One request per `interval`, no burst.
    pub const fn every(interval: std::time::Duration) -> HostRate {
        HostRate { interval, burst: 1 }
    }

    /// The stricter of two rates in each part: the longer interval and the
    /// smaller burst. `burst` is at least 1.
    pub fn stricter(self, other: HostRate) -> HostRate {
        HostRate {
            interval: self.interval.max(other.interval),
            burst: self.burst.min(other.burst).max(1),
        }
    }
}

/// Exponential back-off after failures: the `n`th consecutive failure
/// waits between half and all of `base × 2^(n−1)`, never more than `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    pub base: std::time::Duration,
    pub max: std::time::Duration,
}

/// The hard politeness caps a plugin declares (§18.1). Nothing the user
/// sets can loosen them: [`Policy::limits`] always takes the stricter side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    /// The shortest time between two polls of one feed or unit.
    pub min_poll_interval: std::time::Duration,
    /// The fastest the plugin may talk to any one host.
    pub per_host: HostRate,
    /// Full scrapes per tracker in any 24 hours (BEP 48: at most 1).
    /// 0 for a plugin that never full-scrapes.
    pub max_full_scrapes_per_day: u32,
    /// The back-off floor and ceiling. The user can lengthen both.
    pub backoff: Backoff,
}

impl Caps {
    /// What a plugin gets if it declares nothing: one request per host
    /// every 10 s, polls at most every 15 min, no full scrapes, back-off
    /// from 30 s up to 6 h.
    pub const STRICT: Caps = Caps {
        min_poll_interval: std::time::Duration::from_secs(15 * 60),
        per_host: HostRate::every(std::time::Duration::from_secs(10)),
        max_full_scrapes_per_day: 0,
        backoff: Backoff {
            base: std::time::Duration::from_secs(30),
            max: std::time::Duration::from_secs(6 * 3600),
        },
    };
}

/// A plugin's declared politeness, config and placement: the v2 manifest
/// fields (§18.1, §18.4). It sits beside [`Manifest`] and comes from
/// [`SourcePlugin::policy`](crate::SourcePlugin::policy), whose default is
/// [`Policy::STRICT`], so plugins written before it keep compiling.
///
/// Write a plugin's policy as `Policy { caps: …, ..Policy::STRICT }`, so a
/// field added later takes the strict default rather than breaking the
/// build or loosening anything.
///
/// Declared caps are trusted no further than the global [`floors`]:
/// [`Policy::limits`] and every [`HostLimiter`](crate::HostLimiter) apply
/// them, so a zero interval or back-off can't turn into a request storm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub caps: Caps,
    /// A JSON Schema (draft 2020-12) object for the plugin's settings. The
    /// settings UI is generated from it (§10.10). Secrets are marked
    /// `"writeOnly": true` and are never echoed back.
    pub config_schema: &'static str,
    pub distribution: Distribution,
    /// When the plugin runs unless the user sets other windows.
    pub schedule: Schedule,
}

impl Policy {
    /// No settings, coordinator only, any time, [`Caps::STRICT`].
    pub const STRICT: Policy = Policy {
        caps: Caps::STRICT,
        config_schema: EMPTY_SCHEMA,
        distribution: Distribution::Coordinator,
        schedule: Schedule::ALWAYS,
    };

    /// The limits that apply once the user's settings meet the hard caps.
    /// Each part takes the stricter of the two, so no setting can loosen a
    /// cap.
    pub fn limits(&self, user: &UserLimits) -> Limits {
        let caps = &self.caps;
        let poll = user
            .poll_interval
            .map_or(caps.min_poll_interval, |p| p.max(caps.min_poll_interval));
        let per_host = user
            .per_host
            .map_or(caps.per_host, |r| r.stricter(caps.per_host));
        let scrapes = user
            .max_full_scrapes_per_day
            .map_or(caps.max_full_scrapes_per_day, |n| {
                n.min(caps.max_full_scrapes_per_day)
            });
        let (base, max) = user
            .backoff
            .map_or((caps.backoff.base, caps.backoff.max), |b| (b.base, b.max));
        let base = base.max(caps.backoff.base);
        Limits {
            poll_interval: poll,
            per_host,
            max_full_scrapes_per_day: scrapes,
            backoff: Backoff {
                base,
                max: max.max(caps.backoff.max),
            },
        }
        .floored()
    }
}

/// The global floors under every plugin's declared caps.
pub mod floors {
    use std::time::Duration;

    /// The shortest time between two requests to one host.
    pub const HOST_INTERVAL: Duration = Duration::from_secs(1);
    /// The largest burst to one host.
    pub const MAX_BURST: u32 = 4;
    /// The shortest time between two polls of one unit.
    pub const POLL_INTERVAL: Duration = Duration::from_secs(60);
    /// The shortest first back-off.
    pub const BACKOFF_BASE: Duration = Duration::from_secs(1);
    /// Full scrapes per tracker per day (BEP 48, §18.1).
    pub const FULL_SCRAPES_PER_DAY: u32 = 1;
}

/// A schema that accepts only an empty object.
pub const EMPTY_SCHEMA: &str = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{},"additionalProperties":false}"#;

/// What the user asked for. `None` takes the cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UserLimits {
    pub poll_interval: Option<std::time::Duration>,
    pub per_host: Option<HostRate>,
    pub max_full_scrapes_per_day: Option<u32>,
    pub backoff: Option<Backoff>,
}

/// The limits in force: caps met with user settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub poll_interval: std::time::Duration,
    pub per_host: HostRate,
    pub max_full_scrapes_per_day: u32,
    pub backoff: Backoff,
}

impl Limits {
    /// These limits raised to the global [`floors`] where they fall
    /// below them. Never loosens anything.
    pub fn floored(self) -> Limits {
        let base = self.backoff.base.max(floors::BACKOFF_BASE);
        Limits {
            poll_interval: self.poll_interval.max(floors::POLL_INTERVAL),
            per_host: HostRate {
                interval: self.per_host.interval.max(floors::HOST_INTERVAL),
                burst: self.per_host.burst.clamp(1, floors::MAX_BURST),
            },
            max_full_scrapes_per_day: self
                .max_full_scrapes_per_day
                .min(floors::FULL_SCRAPES_PER_DAY),
            backoff: Backoff {
                base,
                max: self.backoff.max.max(base),
            },
        }
    }

    /// Whether these limits are at least as strict as `caps` everywhere.
    pub fn within(&self, caps: &Caps) -> bool {
        self.poll_interval >= caps.min_poll_interval
            && self.per_host.interval >= caps.per_host.interval
            && self.per_host.burst <= caps.per_host.burst.max(1)
            && self.per_host.burst >= 1
            && self.max_full_scrapes_per_day <= caps.max_full_scrapes_per_day
            && self.backoff.base >= caps.backoff.base
            && self.backoff.max >= caps.backoff.max
            && self.backoff.max >= self.backoff.base
    }
}
