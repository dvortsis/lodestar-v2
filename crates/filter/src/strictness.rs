//! How strict a set of blocklist subscriptions is (owner's decision §2.9,
//! §20.13): the share of the address space they block, the torrents they
//! drop, what kind of lists they are, and a warning level with
//! plain-language text (§10.14) for the first-run wizard and Settings.
//!
//! Subscriptions are off by default; this is what the user sees before
//! choosing. The IP share that matters is the share of the **public**
//! IPv4 space, because the reserved ranges some lists include hold no DHT
//! nodes: FireHOL level 1 covers 14% of all IPv4 but 0.5% of public IPv4.
//! BT_BlockLists blocks whole provider allocations and covers about 49% of
//! all IPv4 (41% of public IPv4), which comes out **severe**.

use crate::lists::{Format, IpSet};

/// The warning a set of lists gets, mildest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    None,
    Notice,
    Strong,
    Severe,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::None => "none",
            Level::Notice => "notice",
            Level::Strong => "strong",
            Level::Severe => "severe",
        }
    }
}

/// What a list blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Category {
    /// Anti-P2P and monitoring peers.
    AntiP2p,
    /// Networks seen attacking or abusing.
    Abuse,
    /// Reserved and unroutable ranges (bogons).
    Reserved,
    /// Peers or nodes, purpose not known (a list added by URL).
    Peers,
    /// A vetted child-abuse hash list that joins the locked floor.
    ChildAbuse,
    /// Torrents by hash.
    Content,
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Category::AntiP2p => "anti_p2p",
            Category::Abuse => "abuse",
            Category::Reserved => "reserved",
            Category::Peers => "peers",
            Category::ChildAbuse => "child_abuse",
            Category::Content => "content",
        }
    }
}

/// IP figures measured on a catalog list, for the wizard, where nothing
/// has been fetched yet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Measured {
    /// Share of all IPv4 addresses.
    pub v4_share: f64,
    /// Share of public IPv4 addresses ([`IpSet::v4_public_coverage`]).
    pub v4_public_share: f64,
    pub v4_ranges: usize,
    pub v6_ranges: usize,
}

/// A list in the one-click catalog. Every entry was checked to be live
/// and in the stated format before it was added; check them again (and
/// measure them again) before each release.
#[derive(Debug)]
pub struct CatalogEntry {
    pub key: &'static str,
    pub name: &'static str,
    pub url: &'static str,
    pub format: Format,
    pub about: &'static str,
    pub categories: &'static [Category],
    /// Measured 2026-10-22, merged ranges.
    pub measured: Measured,
}

pub const CATALOG: &[CatalogEntry] = &[
    CatalogEntry {
        key: "bt-blocklists",
        name: "BT_BlockLists (Naunter)",
        url: "https://raw.githubusercontent.com/Naunter/BT_BlockLists/master/bt_blocklists.gz",
        format: Format::P2p,
        about: "Combined P2P blocklist of anti-P2P, monitoring and known-bad peer \
                ranges, popular with BitTorrent clients. Very broad: it blocks whole \
                provider allocations and covered about half of the IPv4 space when \
                checked, which noticeably shrinks the DHT the crawler can see. About \
                34 MB expanded.",
        categories: &[Category::AntiP2p, Category::Abuse, Category::Reserved],
        measured: Measured {
            v4_share: 0.4924,
            v4_public_share: 0.4111,
            v4_ranges: 393_709,
            v6_ranges: 0,
        },
    },
    CatalogEntry {
        key: "firehol-level1",
        name: "FireHOL level 1",
        url: "https://raw.githubusercontent.com/firehol/blocklist-ipsets/master/firehol_level1.netset",
        format: Format::Cidr,
        about: "Networks that are attacking or abusing right now, chosen for the \
                fewest false positives. Small.",
        categories: &[Category::Abuse, Category::Reserved],
        measured: Measured {
            v4_share: 0.1423,
            v4_public_share: 0.004953,
            v4_ranges: 3_919,
            v6_ranges: 0,
        },
    },
    CatalogEntry {
        key: "firehol-level2",
        name: "FireHOL level 2",
        url: "https://raw.githubusercontent.com/firehol/blocklist-ipsets/master/firehol_level2.netset",
        format: Format::Cidr,
        about: "Addresses seen attacking in the last 48 hours. Broader than level 1.",
        categories: &[Category::Abuse],
        measured: Measured {
            v4_share: 0.000008,
            v4_public_share: 0.000010,
            v4_ranges: 18_677,
            v6_ranges: 0,
        },
    },
];

pub fn catalog(key: &str) -> Option<&'static CatalogEntry> {
    CATALOG.iter().find(|e| e.key == key)
}

/// What's known about one list.
#[derive(Debug, Clone)]
pub enum Facts {
    /// The ranges of an IP list that has been fetched.
    Ips(IpSet),
    /// The number of torrents a content list blocks.
    Hashes(u64),
    /// A catalog list's last measurement.
    Measured(Measured),
    /// Never fetched: nothing is known yet.
    Unknown,
}

/// One list in the candidate set.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub name: String,
    /// An IP list (else a content list).
    pub ip: bool,
    /// Marked as part of the locked floor.
    pub floor: bool,
    pub categories: Vec<Category>,
    pub facts: Facts,
}

impl Candidate {
    /// A catalog list, from its measurement.
    pub fn from_catalog(e: &CatalogEntry) -> Candidate {
        Candidate {
            name: e.name.to_owned(),
            ip: e.format.is_ip(),
            floor: false,
            categories: e.categories.to_vec(),
            facts: Facts::Measured(e.measured),
        }
    }
}

/// One list's figures, as the assessment shows them.
#[derive(Debug, Clone, PartialEq)]
pub struct ListFigures {
    pub name: String,
    pub ip: bool,
    pub floor: bool,
    pub categories: Vec<Category>,
    pub v4_share: Option<f64>,
    pub v4_public_share: Option<f64>,
    pub v6_ranges: Option<usize>,
    pub hashes: Option<u64>,
    /// Figures from a catalog measurement, not the list itself.
    pub measured: bool,
}

/// The assessment of a candidate set of lists.
#[derive(Debug, Clone, PartialEq)]
pub struct Strictness {
    /// Share of all IPv4 addresses blocked (0–1).
    pub v4_share: f64,
    /// Share of public IPv4 addresses blocked (0–1): the warning reads this.
    pub v4_public_share: f64,
    pub v6_ranges: usize,
    /// Share of IPv6 global unicast blocked (0–1).
    pub v6_share: f64,
    /// Torrents blocked by hash in the pack layer.
    pub hashes: u64,
    /// Torrents blocked by hash in the locked floor.
    pub floor_hashes: u64,
    pub categories: Vec<Category>,
    pub level: Level,
    /// Some figures are a catalog measurement, or a lower bound because
    /// lists measured separately may overlap.
    pub estimate: bool,
    /// Lists never fetched, so what they block isn't known.
    pub unknown: Vec<String>,
    /// The plain-language warning, one sentence or two per point.
    pub text: String,
    pub lists: Vec<ListFigures>,
}

/// IP shares at or above these get a notice, a strong warning or a severe
/// one. §20.13 warns above 10% of IPv4; the strong level starts there.
pub const NOTICE_SHARE: f64 = 0.01;
pub const STRONG_SHARE: f64 = 0.10;
pub const SEVERE_SHARE: f64 = 0.25;

fn share_level(share: f64) -> Level {
    if share >= SEVERE_SHARE {
        Level::Severe
    } else if share >= STRONG_SHARE {
        Level::Strong
    } else if share >= NOTICE_SHARE {
        Level::Notice
    } else {
        Level::None
    }
}

/// A share as people read it: "41%", "2.3%", "under 0.1%".
pub fn percent(share: f64) -> String {
    let p = share * 100.0;
    if p <= 0.0 {
        "0%".to_owned()
    } else if p < 0.1 {
        "under 0.1%".to_owned()
    } else if p < 10.0 {
        format!("{p:.1}%")
    } else if p < 99.5 {
        format!("{p:.0}%")
    } else {
        "almost all".to_owned()
    }
}

fn count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn names(lists: &[&str]) -> String {
    match lists {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// Assesses a candidate set of lists.
pub fn assess(candidates: &[Candidate]) -> Strictness {
    let mut fetched = Vec::new();
    let mut measured_v4 = (0.0f64, 0.0f64);
    let mut measured_v6 = 0;
    let mut estimate = false;
    let mut unknown = Vec::new();
    let mut hashes = 0;
    let mut floor_hashes = 0;
    let mut categories = Vec::new();
    let mut lists = Vec::new();
    for c in candidates {
        let mut f = ListFigures {
            name: c.name.clone(),
            ip: c.ip,
            floor: c.floor,
            categories: c.categories.clone(),
            v4_share: None,
            v4_public_share: None,
            v6_ranges: None,
            hashes: None,
            measured: false,
        };
        match &c.facts {
            Facts::Ips(set) => {
                f.v4_share = Some(set.v4_coverage());
                f.v4_public_share = Some(set.v4_public_coverage());
                f.v6_ranges = Some(set.v6_ranges());
                fetched.push(set);
            }
            Facts::Measured(m) => {
                f.v4_share = Some(m.v4_share);
                f.v4_public_share = Some(m.v4_public_share);
                f.v6_ranges = Some(m.v6_ranges);
                f.measured = true;
                measured_v4.0 = measured_v4.0.max(m.v4_share);
                measured_v4.1 = measured_v4.1.max(m.v4_public_share);
                measured_v6 = measured_v6.max(m.v6_ranges);
                estimate = true;
            }
            Facts::Hashes(n) => {
                f.hashes = Some(*n);
                if c.floor {
                    floor_hashes += n;
                } else {
                    hashes += n;
                }
            }
            Facts::Unknown => unknown.push(c.name.clone()),
        }
        for cat in &c.categories {
            if !categories.contains(cat) {
                categories.push(*cat);
            }
        }
        lists.push(f);
    }
    categories.sort();
    let union = IpSet::union(fetched.iter().copied());
    // Lists measured separately may overlap, so the largest share is a
    // lower bound of their union.
    let v4_share = union.v4_coverage().max(measured_v4.0);
    let v4_public_share = union.v4_public_coverage().max(measured_v4.1);
    let v6_ranges = union.v6_ranges().max(measured_v6);
    let v6_share = union.v6_coverage();

    let ip_lists: Vec<&str> = candidates
        .iter()
        .filter(|c| c.ip && !matches!(c.facts, Facts::Unknown))
        .map(|c| c.name.as_str())
        .collect();
    let content: Vec<&str> = candidates
        .iter()
        .filter(|c| !c.ip && !c.floor && !matches!(c.facts, Facts::Unknown))
        .map(|c| c.name.as_str())
        .collect();
    let floor: Vec<&str> = candidates
        .iter()
        .filter(|c| !c.ip && c.floor)
        .map(|c| c.name.as_str())
        .collect();

    let mut level = Level::None;
    let mut text = Vec::new();
    if !ip_lists.is_empty() {
        let l = share_level(v4_public_share).max(share_level(v6_share));
        level = level.max(l);
        let subject = if ip_lists.len() == 1 {
            ip_lists[0].to_owned()
        } else {
            "Together, these lists".to_owned()
        };
        let verb = if ip_lists.len() == 1 {
            "blocks"
        } else {
            "block"
        };
        let at_least = if estimate { "at least " } else { "" };
        let p = percent(v4_public_share);
        text.push(match share_level(v4_public_share) {
            Level::Severe => format!(
                "{subject} {verb} {at_least}{p} of public IPv4 addresses. Lodestar won't talk to \
                 any of them, so about {p} of the DHT goes out of the crawler's reach and it will \
                 find far fewer torrents. Choose a narrower list unless you need this one."
            ),
            Level::Strong => format!(
                "{subject} {verb} {at_least}{p} of public IPv4 addresses, so about that share of \
                 the DHT goes out of the crawler's reach and it will find noticeably fewer \
                 torrents. A narrower list keeps more of the DHT in reach."
            ),
            Level::Notice => format!(
                "{subject} {verb} {at_least}{p} of public IPv4 addresses, a small share of the DHT."
            ),
            Level::None => format!(
                "{subject} {verb} {at_least}{p} of public IPv4 addresses, which costs the crawler \
                 almost nothing."
            ),
        });
        if v6_ranges > 0 {
            let ranges = if v6_ranges == 1 {
                "range is"
            } else {
                "ranges are"
            };
            text.push(format!(
                "{} IPv6 {ranges} blocked too ({} of global IPv6 addresses).",
                count(v6_ranges as u64),
                percent(v6_share)
            ));
        }
    }
    if hashes > 0 {
        level = level.max(Level::Notice);
        text.push(format!(
            "{} torrents on {} are dropped by hash. Your exceptions and overrides can still \
             release single torrents.",
            count(hashes),
            names(&content)
        ));
    }
    if !floor.is_empty() {
        level = level.max(Level::Strong);
        let n = if floor_hashes > 0 {
            format!("its {} torrents", count(floor_hashes))
        } else {
            "its torrents".to_owned()
        };
        text.push(format!(
            "{} joins the locked floor: {n} stay blocked beyond any override, and entries its \
             publisher later removes stay blocked until you remove them in the app. Only choose \
             this for a vetted child-abuse hash list.",
            names(&floor)
        ));
    }
    if !unknown.is_empty() {
        level = level.max(Level::Notice);
        let n: Vec<&str> = unknown.iter().map(String::as_str).collect();
        let (has, it, blocks, refresh) = if unknown.len() == 1 {
            ("hasn't", "it", "blocks", "its first refresh")
        } else {
            ("haven't", "they", "block", "their first refresh")
        };
        text.push(format!(
            "{} {has} been fetched yet, so what {it} {blocks} isn't known until {refresh}.",
            names(&n)
        ));
    }
    if candidates.is_empty() {
        text.push(
            "No lists are chosen, so nothing is blocked beyond Lodestar's own filters.".into(),
        );
    }
    Strictness {
        v4_share,
        v4_public_share,
        v6_ranges,
        v6_share,
        hashes,
        floor_hashes,
        categories,
        level,
        estimate,
        unknown,
        text: text.join(" "),
        lists,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lists::IpRange;

    fn ips(ranges: &[(u32, u32)]) -> Candidate {
        Candidate {
            name: "List".into(),
            ip: true,
            floor: false,
            categories: vec![Category::Peers],
            facts: Facts::Ips(IpSet::new(ranges.iter().map(|&(a, b)| IpRange::V4(a, b)))),
        }
    }

    /// `n` /8 blocks of public space, from 11.0.0.0 (up to 89).
    fn slash8s(n: u32) -> Candidate {
        ips(&[(11 << 24, ((11 + n) << 24) - 1)])
    }

    #[test]
    fn bt_blocklists_is_severe() {
        let s = assess(&[Candidate::from_catalog(catalog("bt-blocklists").unwrap())]);
        assert_eq!(s.level, Level::Severe);
        assert!((s.v4_share - 0.49).abs() < 0.01, "{}", s.v4_share);
        assert!(s.estimate);
        assert!(
            s.text
                .starts_with("BT_BlockLists (Naunter) blocks at least 41% of public IPv4")
        );
        assert!(
            s.text.contains("goes out of the crawler's reach"),
            "{}",
            s.text
        );
        // FireHOL level 1 is mostly reserved space: little of the DHT.
        let s = assess(&[Candidate::from_catalog(catalog("firehol-level1").unwrap())]);
        assert_eq!(s.level, Level::None);
        assert!(s.v4_share > 0.14 && s.v4_public_share < 0.01);
        assert!(s.text.contains("0.5% of public IPv4"), "{}", s.text);
        // Together: the broader one decides.
        let both: Vec<Candidate> = ["bt-blocklists", "firehol-level1", "firehol-level2"]
            .iter()
            .map(|k| Candidate::from_catalog(catalog(k).unwrap()))
            .collect();
        let s = assess(&both);
        assert_eq!(s.level, Level::Severe);
        assert!(
            s.text
                .starts_with("Together, these lists block at least 41%")
        );
        assert_eq!(
            s.categories,
            [Category::AntiP2p, Category::Abuse, Category::Reserved]
        );
    }

    #[test]
    fn levels_follow_the_public_share() {
        // A /8 is 1/256 of IPv4, about 0.45% of the public space.
        let public = 1.0 / 256.0 / (1.0 - 0.1385);
        for (n, level) in [
            (1, Level::None),
            (3, Level::Notice),
            (24, Level::Strong),
            (60, Level::Severe),
        ] {
            let s = assess(&[slash8s(n)]);
            assert!((s.v4_public_share - public * f64::from(n)).abs() < 0.001);
            assert_eq!(s.level, level, "{n} /8s: {}", s.text);
            assert!(!s.estimate);
        }
        // Reserved space alone costs nothing: 224.0.0.0/3.
        let s = assess(&[ips(&[(0xe000_0000, u32::MAX)])]);
        assert!(s.v4_share > 0.12);
        assert_eq!(s.v4_public_share, 0.0);
        assert_eq!(s.level, Level::None);
        // Overlapping fetched lists are counted once.
        let s = assess(&[slash8s(30), slash8s(30)]);
        assert_eq!(s.level, Level::Strong);
        assert!(
            s.text.starts_with("Together, these lists block 14%"),
            "{}",
            s.text
        );
    }

    #[test]
    fn hashes_floor_ipv6_and_unknown_lists() {
        let content = |floor: bool, n: u64| Candidate {
            name: if floor {
                "Vetted".into()
            } else {
                "Hashes".into()
            },
            ip: false,
            floor,
            categories: vec![if floor {
                Category::ChildAbuse
            } else {
                Category::Content
            }],
            facts: Facts::Hashes(n),
        };
        let s = assess(&[content(false, 12_345)]);
        assert_eq!((s.level, s.hashes), (Level::Notice, 12_345));
        assert!(
            s.text
                .starts_with("12,345 torrents on Hashes are dropped by hash.")
        );
        let s = assess(&[content(false, 3), content(true, 1_000)]);
        assert_eq!((s.level, s.floor_hashes), (Level::Strong, 1_000));
        assert!(
            s.text
                .contains("Vetted joins the locked floor: its 1,000 torrents")
        );
        let v6 = Candidate {
            facts: Facts::Ips(IpSet::new([IpRange::V6(
                0x2001 << 112,
                (0x2002 << 112) - 1,
            )])),
            ..ips(&[])
        };
        let s = assess(&[v6]);
        assert_eq!(s.v6_ranges, 1);
        assert!(
            s.text.contains("1 IPv6 range is blocked too (under 0.1%"),
            "{}",
            s.text
        );
        let s = assess(&[Candidate {
            facts: Facts::Unknown,
            name: "New list".into(),
            ..ips(&[])
        }]);
        assert_eq!(s.level, Level::Notice);
        assert_eq!(s.unknown, ["New list"]);
        assert!(s.text.contains(
            "New list hasn't been fetched yet, so what it blocks isn't known until its first \
             refresh."
        ));
        let s = assess(&[
            Candidate {
                facts: Facts::Unknown,
                name: "One".into(),
                ..ips(&[])
            },
            Candidate {
                facts: Facts::Unknown,
                name: "Two".into(),
                ..ips(&[])
            },
        ]);
        assert!(
            s.text.contains(
                "haven't been fetched yet, so what they block isn't known until their \
                           first refresh."
            ),
            "{}",
            s.text
        );
        let s = assess(&[]);
        assert_eq!(s.level, Level::None);
        assert!(s.text.starts_with("No lists are chosen"));
    }

    #[test]
    fn percentages_read_naturally() {
        assert_eq!(percent(0.0), "0%");
        assert_eq!(percent(0.0004), "under 0.1%");
        assert_eq!(percent(0.0234), "2.3%");
        assert_eq!(percent(0.4111), "41%");
        assert_eq!(percent(0.999), "almost all");
        assert_eq!(count(1_234_567), "1,234,567");
        assert_eq!(count(12), "12");
    }

    #[test]
    fn the_catalog_is_well_formed() {
        for e in CATALOG {
            assert!(catalog(e.key).is_some());
            let m = e.measured;
            assert!(m.v4_public_share <= 1.0 && m.v4_share <= 1.0, "{}", e.key);
            assert!(!e.categories.is_empty(), "{}", e.key);
        }
    }
}
