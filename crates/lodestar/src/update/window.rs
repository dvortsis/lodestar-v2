//! The install window (§12.2): `HH:MM-HH:MM` in the host's local time,
//! which may wrap midnight. Every automatic install waits for it,
//! security fixes included (§2.13); Install now skips it.
//!
//! Local time is the host's: `TZ`, else `/etc/localtime` (jiff's system
//! zone, with the zoneinfo database for a `TZ` name). With no zone found
//! the window is in UTC, and `/updates` says so (`window_zone`). A
//! boundary in a DST gap is the transition itself (the first moment the
//! clock reads past it). A repeated hour counts on both passes: the
//! window is open whenever the local clock reads inside it, and `when`
//! gives the first moment after now that it opens or closes.

use jiff::Timestamp;
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
            let digits = |x: &str| x.len() == 2 && x.bytes().all(|b| b.is_ascii_digit());
            if !digits(h) || !digits(m) {
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
        let open = |t: i64| local(t, tz).is_some_and(|m| self.contains(m));
        if open(now) {
            When::Open {
                closes: first(now, tz, |t| !open(t)).unwrap_or(now + DAY),
            }
        } else {
            When::Opens(first(now, tz, open).unwrap_or(now + DAY))
        }
    }
}

const DAY: i64 = 86_400;

/// The local minute of the day at `t`.
fn local(t: i64, tz: &TimeZone) -> Option<u16> {
    let z = Timestamp::from_second(t).ok()?.to_zoned(tz.clone());
    Some((z.hour() as u16) * 60 + z.minute() as u16)
}

/// The first moment after `now`, within three days, where `hit` holds.
/// The local minute changes only on a whole minute or at a zone
/// transition, so those are the only moments to try: the answer is exact
/// on both passes of a repeated hour and across a gap.
fn first(now: i64, tz: &TimeZone, hit: impl Fn(i64) -> bool) -> Option<i64> {
    let limit = now + 3 * DAY;
    let mut transitions = Timestamp::from_second(now)
        .ok()
        .map(|t| {
            tz.following(t)
                .map(|t| t.timestamp().as_second())
                .take_while(|&t| t <= limit)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
        .into_iter()
        .filter(|&t| t > now)
        .peekable();
    let mut minute = (now.div_euclid(60) + 1) * 60;
    while minute <= limit {
        // A transition between whole minutes comes first.
        while let Some(&t) = transitions.peek() {
            if t >= minute {
                break;
            }
            transitions.next();
            if hit(t) {
                return Some(t);
            }
        }
        if hit(minute) {
            return Some(minute);
        }
        minute += 60;
    }
    None
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
            "+3:00-05:00",
            "03:+5-05:00",
            "03:00-+5:00",
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

    /// Now on the first pass of the repeated hour: the answer is the first
    /// moment the window opens or closes, including a re-entry or a brief
    /// re-close on the second pass.
    #[test]
    fn the_first_pass_of_a_repeated_hour_sees_the_second() {
        // 2026-10-25, CET: 02:30 CEST is 00:30Z; 02:00 CET is 01:00Z.
        let w = Window::parse("00:00-02:30").unwrap();
        assert_eq!(
            w.when(ts("2026-10-25T00:30:00Z"), &cet()),
            When::Opens(ts("2026-10-25T01:00:00Z")),
            "02:00-02:30 CET reopens it"
        );
        let w = Window::parse("02:15-03:45").unwrap();
        assert_eq!(
            w.when(ts("2026-10-25T00:20:00Z"), &cet()),
            When::Open {
                closes: ts("2026-10-25T01:00:00Z")
            },
            "02:00-02:15 CET is outside it"
        );
        assert_eq!(
            w.when(ts("2026-10-25T01:00:00Z"), &cet()),
            When::Opens(ts("2026-10-25T01:15:00Z"))
        );
    }

    /// Across the fall-back days in Europe and the US, every few minutes:
    /// nothing between now and the answer changes the state, checked on a
    /// grid offset from the whole minutes the search tries.
    #[test]
    fn the_answer_is_the_first_change_across_the_fall_back_hour() {
        let us = TimeZone::posix("EST5EDT,M3.2.0,M11.1.0").unwrap();
        let days = [
            (cet(), "2026-10-24T20:00:00Z"),
            (us, "2026-11-01T02:00:00Z"),
        ];
        let windows = [
            "00:00-02:30",
            "02:15-03:45",
            "01:00-02:00",
            "01:30-01:45",
            "02:00-02:15",
            "23:00-01:30",
            "01:59-02:01",
        ];
        for (tz, from) in &days {
            let open = |w: &Window, t: i64| local(t, tz).is_some_and(|m| w.contains(m));
            for w in windows {
                let win = Window::parse(w).unwrap();
                let mut now = ts(from);
                while now < ts(from) + 10 * 3600 {
                    let (at, state) = match win.when(now, tz) {
                        When::Open { closes } => (closes, true),
                        When::Opens(at) => (at, false),
                    };
                    assert_eq!(open(&win, now), state, "{w} at {now}");
                    assert_eq!(open(&win, at), !state, "{w}: {now} -> {at}");
                    assert_eq!(open(&win, at - 1), state, "{w}: {now} -> {at}");
                    let mut t = now + 30;
                    while t < at {
                        assert_eq!(open(&win, t), state, "{w}: {now} -> {at} misses {t}");
                        t += 60;
                    }
                    now += 300;
                }
            }
        }
    }
}
