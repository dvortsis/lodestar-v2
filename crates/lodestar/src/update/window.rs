//! The install window (§12.2): `HH:MM-HH:MM` in the host's local time,
//! which may wrap midnight. Every automatic install waits for it,
//! security fixes included (§2.13); Install now skips it.
//!
//! Local time is the host's: `TZ`, else `/etc/localtime` (jiff's system
//! zone, with the zoneinfo database for a `TZ` name). With no zone found
//! the window is in UTC, and `/updates` says so (`window_zone`). A
//! boundary in a DST gap is the transition itself (the first moment the
//! clock reads past it). A repeated hour counts on both passes: the
//! window is open whenever the local clock reads inside it, and it opens
//! and closes at the first matching moment after now.

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
    /// next opens: within a day, or two when a DST gap swallows the
    /// window on the day between.
    pub fn when(&self, now: i64, tz: &TimeZone) -> When {
        let Some((date, minute)) = local(now, tz) else {
            return When::Opens(now);
        };
        if self.contains(minute) {
            return When::Open {
                closes: next_at(self.end, now, date, tz),
            };
        }
        // A window wholly inside a DST gap doesn't happen that day: its
        // start resolves to the transition, where it's already over.
        // Look again from there.
        let mut from = (now, date);
        for _ in 0..3 {
            let at = next_at(self.start, from.0, from.1, tz);
            match local(at, tz) {
                Some((_, m)) if self.contains(m) => return When::Opens(at),
                Some((d, _)) => from = (at, d),
                None => break,
            }
        }
        When::Opens(next_at(self.start, now, date, tz))
    }
}

/// The local date and minute of the day at `t`.
fn local(t: i64, tz: &TimeZone) -> Option<(Date, u16)> {
    let z = Timestamp::from_second(t).ok()?.to_zoned(tz.clone());
    Some((z.date(), (z.hour() as u16) * 60 + z.minute() as u16))
}

/// The first moment after `now` when the local clock reads `minute`
/// (today, else tomorrow; the day after only around a DST change).
fn next_at(minute: u16, now: i64, today: Date, tz: &TimeZone) -> i64 {
    let time = Time::constant((minute / 60) as i8, (minute % 60) as i8, 0, 0);
    for days in -1..3 {
        let Ok(date) = today.checked_add(jiff::Span::new().days(days)) else {
            continue;
        };
        if let Some(ts) = instants(date.to_datetime(time), tz)
            .into_iter()
            .find(|&ts| ts > now)
        {
            return ts;
        }
    }
    now + 86_400
}

/// Every moment the local clock reads `dt`, earliest first: one; both
/// passes of a repeated hour; for a time in a DST gap, the transition
/// itself (the first moment the clock reads past it).
fn instants(dt: jiff::civil::DateTime, tz: &TimeZone) -> Vec<i64> {
    use jiff::tz::AmbiguousOffset;
    let ambiguous = tz.to_ambiguous_timestamp(dt);
    match ambiguous.offset() {
        AmbiguousOffset::Gap { before, .. } => {
            // Read with the offset before the gap, the time lands at or
            // after the transition; the last transition at or before
            // that moment is the one.
            let Ok(past) = before.to_timestamp(dt) else {
                return vec![];
            };
            let Ok(just_after) = past.checked_add(jiff::SignedDuration::from_secs(1)) else {
                return vec![];
            };
            tz.preceding(just_after)
                .next()
                .map(|t| vec![t.timestamp().as_second()])
                .unwrap_or_default()
        }
        AmbiguousOffset::Fold { .. } => [ambiguous.earlier(), ambiguous.later()]
            .into_iter()
            .filter_map(|t| t.ok().map(|t| t.as_second()))
            .collect(),
        AmbiguousOffset::Unambiguous { .. } => ambiguous
            .earlier()
            .ok()
            .map(|t| vec![t.as_second()])
            .unwrap_or_default(),
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

    /// A boundary exactly at the gap's start (02:00, when the clock
    /// jumps to 03:00): the transition, the same day.
    #[test]
    fn a_boundary_at_the_start_of_the_gap_is_the_transition() {
        let w = Window::parse("02:00-04:00").unwrap();
        assert_eq!(
            w.when(ts("2026-03-28T23:00:00Z"), &cet()),
            When::Opens(ts("2026-03-29T01:00:00Z"))
        );
        let ends = Window::parse("00:00-02:00").unwrap();
        assert_eq!(
            ends.when(ts("2026-03-28T23:30:00Z"), &cet()),
            When::Open {
                closes: ts("2026-03-29T01:00:00Z")
            }
        );
    }

    /// On the second pass of the repeated hour the window is open, and it
    /// closes on that pass, not tomorrow.
    #[test]
    fn the_second_pass_of_a_repeated_hour_counts() {
        // 02:30 CET happens at 00:30Z and 01:30Z on 2026-10-25.
        let w = Window::parse("01:00-02:30").unwrap();
        assert_eq!(
            w.when(ts("2026-10-25T01:10:00Z"), &cet()),
            When::Open {
                closes: ts("2026-10-25T01:30:00Z")
            }
        );
        // 02:00-02:15 is open on both passes; between them it opens at the
        // second.
        let short = Window::parse("02:00-02:15").unwrap();
        assert_eq!(
            short.when(ts("2026-10-25T00:20:00Z"), &cet()),
            When::Opens(ts("2026-10-25T01:00:00Z"))
        );
        assert!(matches!(
            short.when(ts("2026-10-25T01:05:00Z"), &cet()),
            When::Open { .. }
        ));
    }

    /// Across both DST changes in Europe and the US, with boundaries on
    /// and around the changing hours: a close is after now and within a
    /// day; an opening is open when it comes, within a day or two when a
    /// gap swallows that day's window.
    #[test]
    fn answers_agree_across_dst_changes() {
        let us = TimeZone::posix("EST5EDT,M3.2.0,M11.1.0").unwrap();
        let zones = [cet(), us];
        let windows = [
            "02:00-03:00",
            "02:30-03:30",
            "01:00-02:00",
            "03:00-04:00",
            "23:30-02:30",
            "01:59-02:01",
        ];
        let weeks = [
            ("2026-03-07T00:00:00Z", "2026-03-31T00:00:00Z"),
            ("2026-10-24T00:00:00Z", "2026-11-03T00:00:00Z"),
        ];
        for tz in &zones {
            for w in windows {
                let win = Window::parse(w).unwrap();
                for (from, to) in weeks {
                    let mut now = ts(from);
                    while now < ts(to) {
                        match win.when(now, tz) {
                            When::Open { closes } => {
                                assert!(
                                    closes > now && closes - now <= 25 * 3600,
                                    "{w} {now} closes {closes}"
                                );
                            }
                            When::Opens(at) => {
                                assert!(at > now && at - now <= 49 * 3600, "{w} {now} opens {at}");
                                assert!(
                                    matches!(win.when(at, tz), When::Open { .. }),
                                    "{w}: not open when it opens at {at}"
                                );
                            }
                        }
                        now += 600;
                    }
                }
            }
        }
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
