//! A lookup service's terms of use, as the identify runtime enforces them
//! (ENR-terms, `docs/decisions/2026-11-16-ENR-terms.md`; the runner's
//! decision of 2026-11-16 on #948, #938 and #954).
//!
//! Every identify plugin has [`Terms`]. It's [`Identifier::terms`], a
//! provided method returning [`Terms::STRICT`], so a plugin that says
//! nothing gets the strictest reading:
//!
//! - **no background walk:** it looks up only the items you open, within
//!   its rate cap and the day's count;
//! - **no redistribution:** its answers stay in Lodestar's own UI, never
//!   renaming the record that search, Torznab and export serve;
//! - **retention:** its claims are refreshed on the next user interest
//!   once they're [`Retention::refresh_after`] old, and dropped at
//!   [`Retention::drop_after`] even if no one opens the item;
//! - **no attribution** to show;
//! - **an adult marker is required:** an answer the service didn't mark as
//!   not adult is refused.
//!
//! A plugin loosens only what its service's terms clearly allow, with a
//! comment saying why.
//!
//! [`Identifier::terms`]: crate::identify::Identifier::terms

use std::time::Duration;

const DAY: u64 = 86_400;

/// How long a plugin's claims may be kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retention {
    /// The hard limit: no claim from the plugin is ever kept this long.
    pub max_age: Duration,
}

impl Retention {
    /// 180 days, the runner's default for every lookup service. TMDB's six
    /// months is met with margin: the shortest six months is 181 days.
    pub const DEFAULT: Retention = Retention::days(180);

    /// The shortest `max_age` the runtime accepts. A shorter one reads as
    /// this (two days leave room for the drop margin).
    pub const MIN_MAX_AGE: Duration = Duration::from_secs(2 * DAY);

    pub const fn days(days: u64) -> Retention {
        Retention {
            max_age: Duration::from_secs(days * DAY),
        }
    }

    fn max_age_secs(&self) -> i64 {
        let secs = self.max_age.as_secs().max(Retention::MIN_MAX_AGE.as_secs());
        i64::try_from(secs).unwrap_or(i64::MAX)
    }

    /// Age (seconds) past which a claim is refreshed on the next user
    /// interest: `max_age` less 30 days, and never under half of it.
    pub fn refresh_after(&self) -> i64 {
        let max = self.max_age_secs();
        max.saturating_sub(30 * DAY as i64).max(max / 2)
    }

    /// Age (seconds) at which a claim is dropped, whether anyone opens the
    /// item or not: `max_age` less one day, so a sweep that runs late
    /// still drops it before the limit.
    pub fn drop_after(&self) -> i64 {
        self.max_age_secs().saturating_sub(DAY as i64)
    }
}

/// What a service asks to be credited with, shown beside its answers in
/// the detail drawer and in its Sources row, in its own prescribed words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attribution {
    pub text: &'static str,
    pub link: &'static str,
    /// The id of a bundled logo asset, when the service requires its logo.
    pub logo: Option<&'static str>,
}

/// A service's terms, as the runtime enforces them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Terms {
    /// Its terms clearly allow bulk use, so the runtime may walk the
    /// whole index in the background. Otherwise only opened items.
    pub background_walk: bool,
    /// Its answers may leave the install: rename the record that search,
    /// Torznab and export serve, or reach another app through the API.
    pub redistribute: bool,
    pub retention: Retention,
    pub attribution: Option<Attribution>,
    /// An answer the service didn't mark as not adult is refused. A
    /// plugin for a service that doesn't mark adult content opts out
    /// explicitly, with a comment. An answer marked adult is always
    /// refused.
    pub adult_marker_required: bool,
}

impl Terms {
    /// What a plugin gets when it says nothing.
    pub const STRICT: Terms = Terms {
        background_walk: false,
        redistribute: false,
        retention: Retention::DEFAULT,
        attribution: None,
        adult_marker_required: true,
    };

    /// Whether an answer carrying the service's adult `marker` (`None`
    /// when it didn't mark it) may be used.
    pub fn accepts(&self, marker: Option<bool>) -> bool {
        match marker {
            Some(adult) => !adult,
            None => !self.adult_marker_required,
        }
    }

    /// The part of the terms that readers without the plugin need (the
    /// backfill's `identify::stored_title` in the pipeline): the runtime
    /// keeps it in the plugin's `source_state`, where the plugin can't
    /// write.
    pub fn record(&self) -> Vec<u8> {
        let flags = u8::from(self.background_walk)
            | u8::from(self.redistribute) << 1
            | u8::from(self.adult_marker_required) << 2;
        let mut v = vec![RECORD_VERSION, flags];
        v.extend(self.retention.max_age.as_secs().to_be_bytes());
        v
    }

    /// Reads [`record`](Terms::record). `None` for anything else: a reader
    /// then takes [`Terms::STRICT`].
    pub fn from_record(v: &[u8]) -> Option<Terms> {
        let [RECORD_VERSION, flags, rest @ ..] = v else {
            return None;
        };
        let secs = u64::from_be_bytes(rest.try_into().ok()?);
        if flags & !0b111 != 0 {
            return None;
        }
        Some(Terms {
            background_walk: flags & 1 != 0,
            redistribute: flags & 2 != 0,
            retention: Retention {
                max_age: Duration::from_secs(secs),
            },
            attribution: None,
            adult_marker_required: flags & 4 != 0,
        })
    }
}

const RECORD_VERSION: u8 = 1;

impl Default for Terms {
    fn default() -> Terms {
        Terms::STRICT
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn strict_is_the_default() {
        let t = Terms::default();
        assert!(!t.background_walk && !t.redistribute && t.adult_marker_required);
        assert_eq!(t.attribution, None);
        assert_eq!(t.retention.max_age, Duration::from_secs(180 * DAY));
    }

    #[test]
    fn the_default_ages_are_derived_from_max_age() {
        let r = Retention::DEFAULT;
        assert_eq!(r.refresh_after(), 150 * DAY as i64);
        assert_eq!(r.drop_after(), 179 * DAY as i64);
        // The shortest six months (181 days) is never reached.
        assert!(r.drop_after() < 181 * DAY as i64);
        // A tiny limit is floored, and still drops before it.
        let tiny = Retention {
            max_age: Duration::ZERO,
        };
        assert_eq!(tiny.drop_after(), DAY as i64);
        assert_eq!(tiny.refresh_after(), DAY as i64);
    }

    #[test]
    fn the_marker_rule() {
        let strict = Terms::STRICT;
        assert!(strict.accepts(Some(false)));
        assert!(!strict.accepts(Some(true)));
        assert!(!strict.accepts(None));
        let open = Terms {
            adult_marker_required: false,
            ..Terms::STRICT
        };
        assert!(open.accepts(None) && open.accepts(Some(false)));
        assert!(!open.accepts(Some(true)), "marked adult is always refused");
    }

    #[test]
    fn a_bad_record_reads_as_nothing() {
        assert_eq!(Terms::from_record(&[]), None);
        assert_eq!(Terms::from_record(&[2, 0, 0, 0, 0, 0, 0, 0, 0, 0]), None);
        assert_eq!(Terms::from_record(&[1, 8, 0, 0, 0, 0, 0, 0, 0, 0]), None);
        assert_eq!(Terms::from_record(&[1, 0, 0]), None);
    }

    proptest! {
        #[test]
        fn records_round_trip(bg: bool, re: bool, adult: bool, days in 0u64..100_000) {
            let t = Terms {
                background_walk: bg,
                redistribute: re,
                retention: Retention::days(days),
                attribution: None,
                adult_marker_required: adult,
            };
            prop_assert_eq!(Terms::from_record(&t.record()), Some(t));
        }

        #[test]
        fn refresh_comes_before_drop_before_the_limit(secs in 0u64..u64::MAX) {
            let r = Retention { max_age: Duration::from_secs(secs) };
            prop_assert!(0 < r.refresh_after());
            prop_assert!(r.refresh_after() <= r.drop_after());
            prop_assert!(r.drop_after() < r.max_age_secs());
        }
    }
}
