//! The ephemeral-port check (architecture §11): every outbound TCP
//! connection takes a local port from Windows' dynamic range, and Hyper-V,
//! WSL or Docker Desktop can reserve blocks inside it. When what's left is
//! narrow for the configured fetch sockets, fetches fail with "address in
//! use" and other programs on the PC can lose network access. The check
//! runs once at start and logs the problem with its fix.
//!
//! The parsing and the threshold are plain functions, tested on every OS;
//! only [`query`] runs `netsh`.

use std::fmt;

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
                 (it takes effect at once): {cmd}. Or lower the crawler's \
                 intensity or its max_sockets setting."
            ),
            None => f.write_str(
                "No port range is large enough: lower the crawler's intensity \
                 or its max_sockets setting.",
            ),
        }
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

/// Runs `netsh` from System32 (never from the search path) without a
/// console window.
#[cfg(windows)]
fn netsh(args: &[&str]) -> Option<String> {
    let out = super::system32_command("netsh.exe")?
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Checks both families and logs a warning for each narrow one.
#[cfg(windows)]
pub fn check(max_sockets: usize) {
    for family in [Family::V4, Family::V6] {
        match query(family) {
            Some(range) => {
                if let Some(w) = assess(family, range, max_sockets) {
                    tracing::warn!("ports: {w}");
                } else {
                    tracing::debug!(
                        "ports: {} dynamic range {}–{}, {} available",
                        family.netsh(),
                        range.start,
                        range.end(),
                        range.available()
                    );
                }
            }
            None => tracing::debug!(
                "ports: couldn't read the {} dynamic port range",
                family.netsh()
            ),
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
             start=47104 num=18432. Or lower the crawler's intensity or its \
             max_sockets setting."
        );
    }
}
