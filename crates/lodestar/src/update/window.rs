//! The install window (§12.2): `HH:MM-HH:MM` in the host's local time,
//! which may wrap midnight. Every automatic install waits for it,
//! security fixes included (§2.13); Install now skips it.
//!
//! Local time is the host's: `TZ`, else `/etc/localtime` (jiff's system
//! zone, with the zoneinfo database for a `TZ` name). With no zone found
//! the window is in UTC, and `/updates` says so (`window_zone`). A start
//! that falls in a DST gap opens at the transition (the first moment the
//! clock reads past it); one in a repeated hour opens at the first.

use jiff::Timestamp;
use jiff::civil::{Date, Time};
use jiff::tz::TimeZone;

/// A parsed window: minutes after local midnight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    start: u16,
    end: u16,
}

/// Where `now` is in relation to the window (Unix seconds).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    /// Inside it, until it closes.
    Open { closes: i64 },
    /// It next opens then.
    Opens(i64),
}

impl Window {
    /// `HH:MM-HH:MM`, each 00:00–23:59; a start equal to its end is
    /// refused (that would be no time, or all of it).
    pub fn parse(s: &str) -> Result<Window, String> {
        let bad = || format!("{s:?} isn't a window like 03:00-05:00");
        let (a, b) = s.split_once('-').ok_or_else(bad)?;
        let minutes = |t: &str| -> Option<u16> {
            let (h, m) = t.split_once(':')?;
            if h.len() != 2 || m.len() != 2 {
                return None;
            }
            let (h, m): (u16, u16) = (h.parse().ok()?, m.parse().ok()?);
            (h < 24 && m < 60).then_some(h * 60 + m)
        };
        let (start, end) = (minutes(a).ok_or_else(bad)?, minutes(b).ok_or_else(bad)?);
        if start == end {
            return Err(format!("{s:?} starts and ends at the same time"));
        }
        Ok(Window { start, end })
    }

    fn contains(&self, minute: u16) -> bool {
        if self.start < self.end {
            (self.start..self.end).contains(&minute)
        } else {
            minute >= self.start || minute < self.end
        }
    }

    /// Whether the window is open at `now`, and until when; else when it
    /// next opens. Never more than a day away.
    pub fn when(&self, now: i64, tz: &TimeZone) -> When {
        let Ok(at) = Timestamp::from_second(now) else {
            return When::Opens(now);
        };
        let local = at.to_zoned(tz.clone());
        let minute = (local.hour() as u16) * 60 + local.minute() as u16;
        if self.contains(minute) {
            When::Open {
                closes: next_at(self.end, now, local.date(), tz),
            }
        } else {
            When::Opens(next_at(self.start, now, local.date(), tz))
        }
    }
}

/// The first moment after `now` when the local clock reads `minute`
/// (today, else tomorrow; the day after only around a DST change).
fn next_at(minute: u16, now: i64, today: Date, tz: &TimeZone) -> i64 {
    let time = Time::constant((minute / 60) as i8, (minute % 60) as i8, 0, 0);
    for days in 0..3 {
        let Ok(date) = today.checked_add(jiff::Span::new().days(days)) else {
            break;
        };
        if let Some(ts) = resolve(date.to_datetime(time), tz) {
            if ts > now {
                return ts;
            }
        }
    }
    now + 86_400
}

/// The moment a local time happens: the first of a repeated hour; for
/// a time in a DST gap, the transition itself (the first moment the
/// clock reads past it).
fn resolve(dt: jiff::civil::DateTime, tz: &TimeZone) -> Option<i64> {
    use jiff::tz::AmbiguousOffset;
    let ambiguous = tz.to_ambiguous_timestamp(dt);
    match ambiguous.offset() {
        AmbiguousOffset::Gap { before, .. } => {
            // Read with the offset before the gap, the time lands after
            // the transition; the transition before that is the one.
            let past = before.to_timestamp(dt).ok()?;
            let t = tz.preceding(past).next()?;
            Some(t.timestamp().as_second())
        }
        _ => ambiguous.earlier().ok().map(|t| t.as_second()),
    }
}

/// The host's time zone and its name; UTC (`found` false) when none can
/// be found.
pub fn local_zone() -> (TimeZone, String, bool) {
    match TimeZone::try_system() {
        Ok(tz) => {
            let name = tz.iana_name().unwrap_or("local").to_string();
            (tz, name, true)
        }
        Err(_) => (TimeZone::UTC, "UTC".into(), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> i64 {
        s.parse::<Timestamp>().unwrap().as_second()
    }

    #[test]
    fn parses_and_refuses() {
        assert_eq!(
            Window::parse("03:00-05:00"),
            Ok(Window {
                start: 180,
                end: 300
            })
        );
        assert!(Window::parse("23:30-00:30").is_ok());
        for bad in [
            "",
            "3:00-5:00",
            "03:00",
            "03:00-05",
            "24:00-01:00",
            "03:60-04:00",
            "03:00-03:00",
            "aa:bb-cc:dd",
            "03:00-05:00-06:00",
        ] {
            assert!(Window::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn open_and_next_in_utc_across_midnight() {
        let w = Window::parse("23:30-00:30").unwrap();
        let utc = TimeZone::UTC;
        assert_eq!(
            w.when(ts("2026-10-23T23:45:00Z"), &utc),
            When::Open {
                closes: ts("2026-10-24T00:30:00Z")
            }
        );
        assert_eq!(
            w.when(ts("2026-10-24T00:10:00Z"), &utc),
            When::Open {
                closes: ts("2026-10-24T00:30:00Z")
            }
        );
        assert_eq!(
            w.when(ts("2026-10-24T00:30:00Z"), &utc),
            When::Opens(ts("2026-10-24T23:30:00Z"))
        );
        let day = Window::parse("03:00-05:00").unwrap();
        assert_eq!(
            day.when(ts("2026-10-23T12:00:00Z"), &utc),
            When::Opens(ts("2026-10-24T03:00:00Z"))
        );
        assert_eq!(
            day.when(ts("2026-10-23T02:59:59Z"), &utc),
            When::Opens(ts("2026-10-23T03:00:00Z"))
        );
    }

    /// Central European time, from its POSIX rule (no zoneinfo needed).
    fn cet() -> TimeZone {
        TimeZone::posix("CET-1CEST,M3.5.0,M10.5.0/3").unwrap()
    }

    #[test]
    fn a_start_in_the_spring_gap_opens_just_after_it() {
        // 2026-03-29: 02:00 local jumps to 03:00 (01:00 UTC).
        let w = Window::parse("02:30-04:00").unwrap();
        assert_eq!(
            w.when(ts("2026-03-28T23:00:00Z"), &cet()),
            When::Opens(ts("2026-03-29T01:00:00Z")),
            "the first moment after the gap, 03:00 CEST"
        );
    }

    #[test]
    fn a_start_in_the_repeated_autumn_hour_opens_at_its_first() {
        // 2026-10-25: 03:00 CEST falls back to 02:00 CET; 02:30 happens
        // twice, first at 00:30 UTC.
        let w = Window::parse("02:30-05:00").unwrap();
        assert_eq!(
            w.when(ts("2026-10-24T22:00:00Z"), &cet()),
            When::Opens(ts("2026-10-25T00:30:00Z"))
        );
        // Inside it on the second 02:45, it closes at 05:00 CET.
        assert_eq!(
            w.when(ts("2026-10-25T01:45:00Z"), &cet()),
            When::Open {
                closes: ts("2026-10-25T04:00:00Z")
            }
        );
    }

    #[test]
    fn the_next_opening_is_never_more_than_a_day_away() {
        let w = Window::parse("03:00-03:01").unwrap();
        let start = ts("2026-01-01T00:00:00Z");
        for step in 0..(2 * 24 * 4) {
            let now = start + step * 900;
            if let When::Opens(at) = w.when(now, &cet()) {
                assert!(at > now && at - now <= 25 * 3600, "{now} -> {at}");
            }
        }
    }
}
