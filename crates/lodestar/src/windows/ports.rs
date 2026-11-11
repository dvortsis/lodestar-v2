//! The ephemeral-port check (architecture §11): every outbound TCP
//! connection takes a local port from Windows' dynamic range, and Hyper-V,
//! WSL or Docker Desktop can reserve blocks inside it. When what's left is
//! narrow for the configured fetch sockets, fetches fail with "address in
//! use" and other programs on the PC can lose network access. The check
//! runs once at start and logs the problem with its fix; Insights asks
//! [`pressure`] again at most hourly and lists it as an exception
//! (PK-exc).
//!
//! The parsing and the threshold are plain functions, tested on every OS;
//! only [`query`] runs `netsh`.

use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};

/// One protocol's dynamic port range, minus reserved ports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub start: u32,
    pub count: u32,
    /// Ports inside the range that are reserved (excluded port ranges).
    pub excluded: u32,
}

impl Range {
    /// Ports left for outbound connections.
    pub fn available(&self) -> u32 {
        self.count.saturating_sub(self.excluded)
    }

    /// The last port of the range.
    pub fn end(&self) -> u32 {
        (self.start + self.count).saturating_sub(1)
    }
}

/// Ports kept for the rest of the PC (browsers, updates, other apps).
pub const SYSTEM_RESERVE: u32 = 2048;
/// The size of Windows' default range (49152–65535).
pub const DEFAULT_COUNT: u32 = 16384;
/// The lowest start `netsh` accepts.
const LOWEST_START: u32 = 1025;

/// Ports Lodestar's fetches need with `max_sockets` open at once.
///
/// Outbound peer sockets close with `SO_LINGER(0)` on Windows, so a
/// finished fetch doesn't leave its port in TIME_WAIT for two minutes and
/// the steady state is about one port per open socket. Twice that covers
/// sockets still connecting or closing, and the tracker and integration
/// clients; [`SYSTEM_RESERVE`] covers everything else.
pub fn needed(max_sockets: usize) -> u32 {
    u32::try_from(max_sockets)
        .unwrap_or(u32::MAX)
        .saturating_mul(2)
        .saturating_add(SYSTEM_RESERVE)
}

/// Which protocol a range belongs to (`netsh int <ipv4|ipv6>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    V4,
    V6,
}

impl Family {
    pub fn netsh(self) -> &'static str {
        match self {
            Family::V4 => "ipv4",
            Family::V6 => "ipv6",
        }
    }
}

/// A range too narrow for the configured fetch sockets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    pub family: Family,
    pub range: Range,
    pub max_sockets: usize,
    pub needed: u32,
}

/// `Some` when `range` leaves fewer ports than `max_sockets` needs.
pub fn assess(family: Family, range: Range, max_sockets: usize) -> Option<Warning> {
    let needed = needed(max_sockets);
    (range.available() < needed).then_some(Warning {
        family,
        range,
        max_sockets,
        needed,
    })
}

impl Warning {
    /// Ports short of [`Warning::needed`].
    pub fn shortfall(&self) -> u32 {
        self.needed.saturating_sub(self.range.available())
    }

    /// The range to set: Windows' default when that's enough, else the
    /// needed size (plus the reserved ports) rounded up to 1,024, ending
    /// at 65535. `None` when no range can hold it.
    pub fn fix(&self) -> Option<(u32, u32)> {
        let want = self.needed.saturating_add(self.range.excluded);
        let largest = 65536 - LOWEST_START;
        if want > largest {
            return None;
        }
        let count = want
            .max(DEFAULT_COUNT)
            .div_ceil(1024)
            .saturating_mul(1024)
            .min(largest);
        Some((65536 - count, count))
    }

    /// The `ephemeral_ports` exception's detail for Insights.
    pub fn detail(&self) -> lodestar_api::insights::PortPressure {
        let fix = self.fix();
        lodestar_api::insights::PortPressure {
            family: match self.family {
                Family::V4 => lodestar_api::insights::PortFamily::Ipv4,
                Family::V6 => lodestar_api::insights::PortFamily::Ipv6,
            },
            start: self.range.start,
            end: self.range.end(),
            reserved: self.range.excluded,
            available: self.range.available(),
            needed: self.needed,
            max_sockets: u64::try_from(self.max_sockets).unwrap_or(u64::MAX),
            fix_start: fix.map(|(start, _)| start),
            fix_count: fix.map(|(_, count)| count),
        }
    }

    /// The command that sets [`Warning::fix`].
    pub fn command(&self) -> Option<String> {
        let (start, count) = self.fix()?;
        Some(format!(
            "netsh int {} set dynamicport tcp start={start} num={count}",
            self.family.netsh()
        ))
    }
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let r = &self.range;
        write!(
            f,
            "Windows has only {} ports left for outgoing {} connections (dynamic range {}–{}",
            r.available(),
            match self.family {
                Family::V4 => "IPv4",
                Family::V6 => "IPv6",
            },
            r.start,
            r.end(),
        )?;
        if r.excluded > 0 {
            write!(f, ", {} of them reserved by other software", r.excluded)?;
        }
        write!(
            f,
            "). Lodestar fetches from up to {} peers at once and needs about {}. \
             Fetches may fail, and other programs may lose network access. ",
            self.max_sockets, self.needed
        )?;
        match self.command() {
            Some(cmd) => write!(
                f,
                "To fix it, run this in a Command Prompt opened as administrator \
                 (it takes effect at once): {cmd}. Then check again with \
                 `netsh int {} show excludedportrange protocol=tcp`: Hyper-V and \
                 WinNAT can reserve more ports. Or lower the crawler's intensity \
                 or its max_sockets setting.",
                self.family.netsh()
            ),
            None => f.write_str(
                "No port range is large enough: lower the crawler's intensity \
                 or its max_sockets setting.",
            ),
        }
    }
}

/// The warning for the family that's shortest of ports, or `None` when
/// every range that could be read has enough. A tie goes to the family
/// listed first. `max_sockets` 0 (no fetch pool) never warns.
pub fn worst(
    ranges: impl IntoIterator<Item = (Family, Option<Range>)>,
    max_sockets: usize,
) -> Option<Warning> {
    if max_sockets == 0 {
        return None;
    }
    ranges
        .into_iter()
        .filter_map(|(family, range)| assess(family, range?, max_sockets))
        .fold(None, |best: Option<Warning>, w| match best {
            Some(b) if b.shortfall() >= w.shortfall() => Some(b),
            _ => Some(w),
        })
}

/// The `max_sockets` [`check`] ran with at start; 0 until it has (and
/// always off Windows).
static MAX_SOCKETS: AtomicUsize = AtomicUsize::new(0);

/// Whether [`pressure`] can answer: off Windows always, on Windows once
/// the start's [`check`] has recorded `max_sockets` (its thread waits up
/// to 5 s for logging first). Before then a caller should ask again soon.
pub fn ready() -> bool {
    cfg!(not(windows)) || MAX_SOCKETS.load(Ordering::Acquire) > 0
}

/// Reads both families' ranges now and returns [`worst`]'s answer for
/// the `max_sockets` the start's [`check`] used. It runs `netsh`, so
/// callers cache it (Insights re-checks at most hourly). `None` off
/// Windows, before the start's check, or when `netsh` can't be read.
pub fn pressure() -> Option<Warning> {
    let max_sockets = MAX_SOCKETS.load(Ordering::Acquire);
    #[cfg(windows)]
    {
        if max_sockets == 0 {
            return None;
        }
        worst([Family::V4, Family::V6].map(|f| (f, query(f))), max_sockets)
    }
    #[cfg(not(windows))]
    {
        let _ = max_sockets;
        None
    }
}

/// `netsh int ipv4 show dynamicport tcp`: the start port and the number
/// of ports. The labels are translated on non-English Windows, so this
/// takes the numbers after the colons, in order.
pub fn parse_dynamic(text: &str) -> Option<(u32, u32)> {
    let mut numbers = text
        .lines()
        .filter_map(|l| l.rsplit_once(':'))
        .filter_map(|(_, v)| v.trim().parse::<u32>().ok());
    let start = numbers.next()?;
    let count = numbers.next()?;
    (numbers.next().is_none() && count > 0 && start + count <= 65536).then_some((start, count))
}

/// `netsh int ipv4 show excludedportrange protocol=tcp`: the reserved
/// ranges, as `(first, last)`. Table rows start with two port numbers;
/// headings and the legend don't.
pub fn parse_excluded(text: &str) -> Vec<(u32, u32)> {
    text.lines()
        .filter_map(|l| {
            let mut words = l.split_whitespace();
            let first = words.next()?.parse::<u32>().ok()?;
            let last = words.next()?.parse::<u32>().ok()?;
            (first <= last && last <= 65535).then_some((first, last))
        })
        .collect()
}

/// How many ports of `start..start + count` the `excluded` ranges cover.
/// Ranges may overlap (administered and dynamic exclusions), so this
/// counts each port once.
pub fn overlap(start: u32, count: u32, excluded: &[(u32, u32)]) -> u32 {
    let end = start + count; // exclusive
    let mut spans: Vec<(u32, u32)> = excluded
        .iter()
        .map(|&(a, b)| (a.max(start), (b + 1).min(end)))
        .filter(|(a, b)| a < b)
        .collect();
    spans.sort_unstable();
    let mut total = 0;
    let mut covered_to = start;
    for (a, b) in spans {
        let a = a.max(covered_to);
        if b > a {
            total += b - a;
            covered_to = b;
        }
    }
    total
}

/// The range for `family` from `netsh`, or `None` when it can't be read.
#[cfg(windows)]
pub fn query(family: Family) -> Option<Range> {
    let dynamic = netsh(&["int", family.netsh(), "show", "dynamicport", "tcp"])?;
    let (start, count) = parse_dynamic(&dynamic)?;
    let excluded = netsh(&[
        "int",
        family.netsh(),
        "show",
        "excludedportrange",
        "protocol=tcp",
    ])
    .map(|t| overlap(start, count, &parse_excluded(&t)))
    .unwrap_or(0);
    Some(Range {
        start,
        count,
        excluded,
    })
}

/// How long `netsh` may take before it's killed and its answer counted
/// as unreadable: a check runs on a request's thread.
#[cfg(windows)]
const NETSH_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// Runs `netsh` from System32 (never from the search path) without a
/// console window, for at most [`NETSH_DEADLINE`].
#[cfg(windows)]
fn netsh(args: &[&str]) -> Option<String> {
    use std::io::Read;
    use std::process::Stdio;
    let mut child = super::system32_command("netsh.exe")?
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Read on a thread of its own, so a full pipe can't stall the wait.
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        out
    });
    let deadline = std::time::Instant::now() + NETSH_DEADLINE;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            _ => {
                // Killing it closes the pipe, which ends the reader.
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return None;
            }
        }
    };
    let out = reader.join().ok()?;
    status
        .success()
        .then(|| String::from_utf8_lossy(&out).into_owned())
}

/// Checks both families and logs a warning for each narrow one.
#[cfg(windows)]
pub fn check(max_sockets: usize) {
    MAX_SOCKETS.store(max_sockets, Ordering::Release);
    for family in [Family::V4, Family::V6] {
        use crate::logging::events::{
            WINDOWS_PORTS_LOW, WINDOWS_PORTS_OK, WINDOWS_PORTS_UNREADABLE,
        };
        match query(family) {
            Some(range) => match assess(family, range, max_sockets) {
                Some(w) => {
                    let (fix_start, fix_count) = w.fix().unwrap_or((0, 0));
                    lodestar_log::log_event!(
                        WINDOWS_PORTS_LOW,
                        family = family.netsh(),
                        available = range.available(),
                        excluded = range.excluded,
                        needed = w.needed,
                        max_sockets = max_sockets,
                        fix_start = fix_start,
                        fix_count = fix_count
                    );
                }
                None => lodestar_log::log_event!(
                    WINDOWS_PORTS_OK,
                    family = family.netsh(),
                    start = range.start,
                    end = range.end(),
                    available = range.available()
                ),
            },
            None => lodestar_log::log_event!(WINDOWS_PORTS_UNREADABLE, family = family.netsh()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DYNAMIC_EN: &str = "\r\nProtocol tcp Dynamic Port Range\r\n\
        ---------------------------------\r\n\
        Start Port      : 49152\r\n\
        Number of Ports : 16384\r\n\r\n";
    const DYNAMIC_DE: &str = "\nProtokoll tcp Dynamischer Portbereich\n\
        ---------------------------------\n\
        Startport       : 1025\n\
        Anzahl der Ports: 3976\n";
    const EXCLUDED: &str = "\r\nProtocol tcp Port Exclusion Ranges\r\n\r\n\
        Start Port    End Port\r\n\
        ----------    --------\r\n\
              5357        5357\r\n\
             50000       50059     *\r\n\
             50060       50159\r\n\
             50100       50199\r\n\r\n\
        * - Administered port exclusions.\r\n";

    #[test]
    fn parses_netsh_in_any_language() {
        assert_eq!(parse_dynamic(DYNAMIC_EN), Some((49152, 16384)));
        assert_eq!(parse_dynamic(DYNAMIC_DE), Some((1025, 3976)));
        assert_eq!(parse_dynamic(""), None);
        assert_eq!(parse_dynamic("Start Port : 49152\n"), None);
        assert_eq!(parse_dynamic("a : 1\nb : 2\nc : 3\n"), None);
        assert_eq!(parse_dynamic("Start : 60000\nCount : 10000\n"), None);
        assert_eq!(parse_dynamic("Start : 1025\nCount : 0\n"), None);
        assert_eq!(
            parse_excluded(EXCLUDED),
            vec![(5357, 5357), (50000, 50059), (50060, 50159), (50100, 50199)]
        );
        assert!(parse_excluded("70000 70001\n9 3\n").is_empty());
    }

    #[test]
    fn counts_reserved_ports_once() {
        let ex = parse_excluded(EXCLUDED);
        // 5357 is outside; 50000–50199 inside, with an overlap.
        assert_eq!(overlap(49152, 16384, &ex), 200);
        assert_eq!(overlap(1025, 3976, &ex), 0);
        assert_eq!(overlap(50050, 100, &ex), 100);
        assert_eq!(overlap(49152, 16384, &[(0, 65535)]), 16384);
        assert_eq!(overlap(49152, 16384, &[]), 0);
    }

    fn range(start: u32, count: u32, excluded: u32) -> Range {
        Range {
            start,
            count,
            excluded,
        }
    }

    #[test]
    fn the_default_range_suits_every_preset() {
        for sockets in [100, 400, 1000] {
            assert_eq!(assess(Family::V4, range(49152, 16384, 0), sockets), None);
        }
        assert_eq!(needed(400), 2848);
        assert_eq!(needed(usize::MAX), u32::MAX);
    }

    #[test]
    fn warns_when_reservations_or_a_small_range_leave_too_few() {
        // The old XP-era range suits Balanced but not Aggressive.
        assert_eq!(assess(Family::V4, range(1025, 3976, 0), 400), None);
        let w = assess(Family::V4, range(1025, 3976, 0), 1000).unwrap();
        assert_eq!(w.needed, 4048);
        assert_eq!(w.fix(), Some((49152, 16384)));
        // Hyper-V reserving most of the default range.
        let w = assess(Family::V6, range(49152, 16384, 15000), 400).unwrap();
        assert_eq!(w.fix(), Some((65536 - 18432, 18432)));
        assert_eq!(
            w.command().unwrap(),
            "netsh int ipv6 set dynamicport tcp start=47104 num=18432"
        );
        // Exactly enough doesn't warn.
        assert_eq!(assess(Family::V4, range(1025, 2848, 0), 400), None);
        assert!(assess(Family::V4, range(1025, 2847, 0), 400).is_some());
    }

    #[test]
    fn a_fix_never_goes_below_port_1025() {
        let w = assess(Family::V4, range(49152, 16384, 0), 40_000).unwrap();
        assert_eq!(w.needed, 82_048);
        assert_eq!(w.fix(), None);
        assert!(w.to_string().contains("No port range is large enough"));
        let w = assess(Family::V4, range(49152, 16384, 0), 30_000).unwrap();
        assert_eq!(w.fix(), Some((3072, 62464)));
        // Rounding up would pass port 1025: the largest range instead.
        let w = assess(Family::V4, range(49152, 16384, 0), 31_000).unwrap();
        assert_eq!(w.fix(), Some((1025, 64511)));
        let w = assess(Family::V4, range(49152, 16384, 0), 31_232).unwrap();
        assert_eq!(w.needed, 64_512);
        assert_eq!(w.fix(), None);
    }

    #[test]
    fn the_shortest_family_is_the_one_reported() {
        let v4 = (Family::V4, Some(range(49152, 16384, 15000))); // 1384 left
        let v6 = (Family::V6, Some(range(49152, 16384, 15500))); // 884 left
        let fine = (Family::V6, Some(range(49152, 16384, 0)));
        assert_eq!(worst([v4, v6], 400).unwrap().family, Family::V6);
        assert_eq!(worst([v6, v4], 400).unwrap().family, Family::V6);
        assert_eq!(worst([v4, fine], 400).unwrap().family, Family::V4);
        assert_eq!(worst([fine, v4], 400).unwrap().family, Family::V4);
        // A tie goes to the family listed first.
        let v6_same = (Family::V6, Some(range(49152, 16384, 15000)));
        assert_eq!(worst([v4, v6_same], 400).unwrap().family, Family::V4);
        assert_eq!(worst([v6_same, v4], 400).unwrap().family, Family::V6);
        // An unreadable family is skipped, not counted as narrow.
        assert_eq!(worst([(Family::V4, None), fine], 400), None);
        assert_eq!(
            worst([(Family::V4, None), v4], 400).unwrap().shortfall(),
            2848 - 1384
        );
        // No fetch pool, nothing to warn about.
        assert_eq!(worst([v4, v6], 0), None);
        assert_eq!(worst([], 400), None);
    }

    #[test]
    fn worst_trips_at_the_boundary() {
        // needed(400) = 2848: exactly that many left is enough, one fewer isn't.
        let at = |left: u32| (Family::V4, Some(range(49152, 16384, 16384 - left)));
        assert_eq!(worst([at(2848)], 400), None);
        let w = worst([at(2847)], 400).unwrap();
        assert_eq!(
            (w.shortfall(), w.needed, w.range.available()),
            (1, 2848, 2847)
        );
    }

    #[test]
    fn the_insights_detail_carries_the_fix() {
        let d = assess(Family::V6, range(49152, 16384, 15000), 400)
            .unwrap()
            .detail();
        assert_eq!(
            d,
            lodestar_api::insights::PortPressure {
                family: lodestar_api::insights::PortFamily::Ipv6,
                start: 49152,
                end: 65535,
                reserved: 15000,
                available: 1384,
                needed: 2848,
                max_sockets: 400,
                fix_start: Some(47104),
                fix_count: Some(18432),
            }
        );
        // No range is large enough: no range to set, so the fix is fewer sockets.
        let d = assess(Family::V4, range(49152, 16384, 0), 40_000)
            .unwrap()
            .detail();
        assert_eq!(d.family, lodestar_api::insights::PortFamily::Ipv4);
        assert_eq!((d.fix_start, d.fix_count), (None, None));
    }

    #[cfg(not(windows))]
    #[test]
    fn pressure_is_never_reported_off_windows() {
        assert!(ready());
        MAX_SOCKETS.store(400, Ordering::Release);
        assert_eq!(pressure(), None);
    }

    #[test]
    fn the_message_says_what_and_how_to_fix() {
        let w = assess(Family::V4, range(49152, 16384, 15000), 400).unwrap();
        assert_eq!(
            w.to_string(),
            "Windows has only 1384 ports left for outgoing IPv4 connections \
             (dynamic range 49152–65535, 15000 of them reserved by other software). \
             Lodestar fetches from up to 400 peers at once and needs about 2848. \
             Fetches may fail, and other programs may lose network access. \
             To fix it, run this in a Command Prompt opened as administrator \
             (it takes effect at once): netsh int ipv4 set dynamicport tcp \
             start=47104 num=18432. Then check again with `netsh int ipv4 show \
             excludedportrange protocol=tcp`: Hyper-V and WinNAT can reserve more \
             ports. Or lower the crawler's intensity or its max_sockets setting."
        );
    }
}
