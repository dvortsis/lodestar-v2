//! The pack manifest (§20.6 "signed, versioned"; RP-0): every pack rule's
//! versions, compiled in, so the signed release manifest (§12.2) covers
//! them as it covers the rest of the binary.
//!
//! - **A version is bumped** whenever what a rule matches or its action
//!   changes. A pack's version is the set of its rules' versions.
//! - **Each version records its direction** against the one before
//!   ([`Direction`]); a rule's first version `widens` (against nothing).
//!   Accepting anything but `widens` or `action-heavier` is weakening
//!   (owner §1.8.8), and so is a version whose shadow results disagree
//!   with its record ([`super::upgrade::disagrees`]).
//! - **The previous version's code is kept** beside the new one while an
//!   install may still have it applied: `Ctx::matches` holds the code of
//!   the version this build ships ([`Code::Current`]), and an older
//!   version's code moves to a function ([`Code::Fn`]). A version two
//!   releases old is dropped from the code ([`Version::code`] `None`) only
//!   once no supported upgrade path can still have it applied; its record
//!   (number, direction, action) stays, so a skipped step's direction is
//!   still known.
//! - **Every version has a changelog entry**, `## <RULE>@<version>`, in its
//!   pack's `crates/filter/data/packs/<pack>/CHANGELOG.md` ([`changelog`]).

use std::sync::{Arc, OnceLock};

use super::{Ctx, Pack, PackRule};
use crate::core::Action;

/// Rules added after RP-0 (owner §2.39, RP-UC): not part of any
/// install's behaviour before, so seeding never records them. On a store
/// seeded before they shipped they're pending, in shadow, until accepted
/// or until their pack is turned on (which records them); a fresh store
/// records every rule at its current version, so they apply there.
pub const ADDED_AFTER_RP0: &[PackRule] = &[
    PackRule::Mw6,
    PackRule::Uc1,
    PackRule::Uc2,
    PackRule::Uc3,
    PackRule::Sp3,
];

/// What a version changed against the one before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Matches more, with the same action. A first version widens.
    Widens,
    /// Matches less.
    Narrows,
    /// Matches the same, with a heavier action.
    ActionHeavier,
    /// Matches the same, with a lighter action.
    ActionLighter,
    /// Anything else, or steps of different directions combined.
    Mixed,
}

impl Direction {
    pub const ALL: [Direction; 5] = [
        Direction::Widens,
        Direction::Narrows,
        Direction::ActionHeavier,
        Direction::ActionLighter,
        Direction::Mixed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Direction::Widens => "widens",
            Direction::Narrows => "narrows",
            Direction::ActionHeavier => "action-heavier",
            Direction::ActionLighter => "action-lighter",
            Direction::Mixed => "mixed",
        }
    }

    pub fn from_name(name: &str) -> Option<Direction> {
        Direction::ALL.into_iter().find(|d| d.as_str() == name)
    }

    /// Whether accepting a version of this direction strengthens
    /// filtering, so it needs no password when its shadow results agree.
    pub fn strengthens(self) -> bool {
        matches!(self, Direction::Widens | Direction::ActionHeavier)
    }

    /// Two steps in a row: the same direction stays, anything else is
    /// `mixed`, which counts as weakening. Conservative on purpose: a
    /// widening then a heavier action asks for the password, since no
    /// single record says both.
    pub fn then(self, next: Direction) -> Direction {
        if self == next { self } else { Direction::Mixed }
    }
}

/// A version's matching code.
#[derive(Clone, Copy)]
pub enum Code {
    /// The arm for the rule in `Ctx::matches`: the version this build
    /// ships. Only a rule's newest version has it.
    Current,
    /// A version kept beside a newer one, or one a test defines.
    Fn(fn(&Ctx<'_>) -> bool),
}

impl std::fmt::Debug for Code {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Code::Current => f.write_str("Current"),
            Code::Fn(_) => f.write_str("Fn"),
        }
    }
}

/// One version of a pack rule.
#[derive(Debug, Clone)]
pub struct Version {
    pub version: u32,
    /// Against the version before it.
    pub direction: Direction,
    pub action: Action,
    /// `None` once the code is dropped from the build: only its record
    /// is kept.
    pub code: Option<Code>,
}

impl Version {
    /// Whether this version matches, or `None` when its code is gone.
    pub fn matches(&self, rule: PackRule, ctx: &Ctx<'_>) -> Option<bool> {
        Some(match self.code? {
            Code::Current => ctx.matches(rule),
            Code::Fn(f) => f(ctx),
        })
    }
}

/// A rule's versions, oldest first.
#[derive(Debug, Clone)]
pub struct RuleEntry {
    pub rule: PackRule,
    /// It existed before RP-0, so seeding records its version 1: the
    /// behaviour every install had then.
    pub baseline: bool,
    versions: Vec<Version>,
}

impl RuleEntry {
    pub fn new(rule: PackRule, baseline: bool, versions: Vec<Version>) -> RuleEntry {
        RuleEntry {
            rule,
            baseline,
            versions,
        }
    }

    pub fn versions(&self) -> &[Version] {
        &self.versions
    }

    /// The version this build ships.
    pub fn current(&self) -> &Version {
        self.versions
            .last()
            .expect("validated: at least one version")
    }

    pub fn get(&self, version: u32) -> Option<&Version> {
        self.versions.iter().find(|v| v.version == version)
    }

    /// A version whose code this build still has.
    pub fn retained(&self, version: u32) -> Option<&Version> {
        self.get(version).filter(|v| v.code.is_some())
    }

    /// The oldest version whose code this build still has.
    pub fn oldest_retained(&self) -> &Version {
        self.versions
            .iter()
            .find(|v| v.code.is_some())
            .expect("validated: the current version has code")
    }

    /// The direction of going from version `from` (`None`: from nothing)
    /// to version `to`, every step in between combined
    /// ([`Direction::then`]). `None` when there's no record for it: `from`
    /// isn't a version this build knows, `to` isn't newer, or a step is
    /// missing. No record counts as weakening.
    pub fn direction(&self, from: Option<u32>, to: u32) -> Option<Direction> {
        let start = match from {
            Some(f) => {
                self.get(f)?;
                f
            }
            None => 0,
        };
        if to <= start || self.get(to).is_none() {
            return None;
        }
        self.versions
            .iter()
            .filter(|v| v.version > start && v.version <= to)
            .map(|v| v.direction)
            .reduce(Direction::then)
    }
}

/// Every pack rule's versions.
#[derive(Debug, Clone)]
pub struct Manifest {
    /// By [`PackRule::index`].
    rules: Vec<RuleEntry>,
}

impl Manifest {
    /// The manifest this build ships.
    pub fn builtin() -> Arc<Manifest> {
        static BUILTIN: OnceLock<Arc<Manifest>> = OnceLock::new();
        BUILTIN
            .get_or_init(|| {
                let entries = PackRule::ALL
                    .into_iter()
                    .map(|r| {
                        RuleEntry::new(
                            r,
                            !ADDED_AFTER_RP0.contains(&r),
                            vec![Version {
                                version: 1,
                                direction: Direction::Widens,
                                action: r.action(),
                                code: Some(Code::Current),
                            }],
                        )
                    })
                    .collect();
                Arc::new(Manifest::new(entries).expect("the built-in manifest is valid"))
            })
            .clone()
    }

    /// A manifest from its entries, checked: one entry per rule; versions
    /// from 1 up, strictly ascending; a first version that widens; the
    /// newest with code, and only the newest with [`Code::Current`].
    pub fn new(entries: Vec<RuleEntry>) -> Result<Manifest, String> {
        let mut rules: Vec<Option<RuleEntry>> = vec![None; PackRule::COUNT];
        for e in entries {
            let id = e.rule.id();
            let Some((first, rest)) = e.versions.split_first() else {
                return Err(format!("{id} has no version"));
            };
            if first.version == 0 || first.direction != Direction::Widens {
                return Err(format!("{id}'s first version must be 1 or more and widen"));
            }
            let mut last = first.version;
            for v in rest {
                if v.version <= last {
                    return Err(format!("{id}'s versions must ascend"));
                }
                last = v.version;
            }
            let n = e.versions.len();
            for (i, v) in e.versions.iter().enumerate() {
                let newest = i + 1 == n;
                match v.code {
                    None if newest => return Err(format!("{id}'s newest version has no code")),
                    Some(Code::Current) if !newest => {
                        return Err(format!("{id}@{} can't use the current code", v.version));
                    }
                    _ => {}
                }
            }
            let slot = &mut rules[e.rule.index()];
            if slot.is_some() {
                return Err(format!("{id} is listed twice"));
            }
            *slot = Some(e);
        }
        let rules = rules
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| "every pack rule needs an entry".to_owned())?;
        Ok(Manifest { rules })
    }

    pub fn entry(&self, rule: PackRule) -> &RuleEntry {
        &self.rules[rule.index()]
    }

    /// A pack's version: its rules' current versions.
    pub fn pack_version(&self, pack: Pack) -> Vec<(PackRule, u32)> {
        PackRule::ALL
            .into_iter()
            .filter(|r| r.pack() == pack)
            .map(|r| (r, self.entry(r).current().version))
            .collect()
    }
}

/// A pack's changelog, compiled in: one `## <RULE>@<version>` entry per
/// version, newest first.
pub fn changelog(pack: Pack) -> &'static str {
    match pack {
        Pack::Malware => include_str!("../../data/packs/malware/CHANGELOG.md"),
        Pack::Lures => include_str!("../../data/packs/lures/CHANGELOG.md"),
        Pack::Fakes => include_str!("../../data/packs/fakes/CHANGELOG.md"),
        Pack::Spam => include_str!("../../data/packs/spam/CHANGELOG.md"),
        Pack::Scams => include_str!("../../data/packs/scams/CHANGELOG.md"),
        Pack::UnauthorisedCopy => {
            include_str!("../../data/packs/unauthorised-copy/CHANGELOG.md")
        }
    }
}

/// A rule version's changelog entry: the text under its heading, up to
/// the next heading.
pub fn changelog_entry(rule: PackRule, version: u32) -> Option<&'static str> {
    let text = changelog(rule.pack());
    let heading = format!("## {}@{version}", rule.id());
    // By byte offsets, so `\r\n` line ends read the same.
    let end = text.match_indices(&heading).find_map(|(at, _)| {
        let starts_line = at == 0 || text.as_bytes()[at - 1] == b'\n';
        let rest = &text[at + heading.len()..];
        let ends_heading = rest.is_empty() || rest.starts_with([' ', '\r', '\n']);
        (starts_line && ends_heading).then_some(at + heading.len())
    })?;
    let rest = &text[end..];
    let body = &rest[rest.find('\n').map_or(rest.len(), |n| n + 1)..];
    let stop = body.find("\n## ").map_or(body.len(), |i| i + 1);
    Some(body[..stop].trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Item;
    use crate::matcher::Facts;
    use lodestar_classify::{FileInfo, classify};

    #[test]
    fn the_builtin_manifest_matches_the_rules() {
        let m = Manifest::builtin();
        for r in PackRule::ALL {
            let e = m.entry(r);
            assert_eq!(e.rule, r);
            assert_eq!(
                e.baseline,
                !ADDED_AFTER_RP0.contains(&r),
                "{} existed before RP-0, or was added after it",
                r.id()
            );
            assert_eq!(e.current().action, r.action(), "{}", r.id());
            assert!(matches!(e.current().code, Some(Code::Current)));
            assert_eq!(e.direction(None, 1), Some(Direction::Widens));
        }
        assert_eq!(
            m.pack_version(Pack::Scams),
            [(PackRule::Sc1, 1), (PackRule::Sc2, 1)]
        );
    }

    #[test]
    fn every_version_has_a_changelog_entry() {
        let m = Manifest::builtin();
        for r in PackRule::ALL {
            for v in m.entry(r).versions() {
                let entry = changelog_entry(r, v.version);
                assert!(
                    entry.is_some_and(|e| !e.is_empty()),
                    "{}@{} needs an entry in {}'s CHANGELOG.md",
                    r.id(),
                    v.version,
                    r.pack().as_str()
                );
                assert!(
                    entry.unwrap().contains(v.direction.as_str()),
                    "{}@{}'s entry names its direction",
                    r.id(),
                    v.version
                );
            }
        }
        assert_eq!(changelog_entry(PackRule::Mw1, 99), None);
        assert_eq!(
            changelog_entry(PackRule::Mw1, 1).unwrap().lines().count(),
            1
        );
        assert!(
            !changelog_entry(PackRule::Sc1, 1)
                .unwrap()
                .contains("SC-2@1")
        );
    }

    #[test]
    fn directions_combine_and_missing_records_are_none() {
        use Direction::*;
        let v = |version, direction| Version {
            version,
            direction,
            action: Action::Hide,
            code: None,
        };
        let mut versions = vec![
            v(1, Widens),
            v(2, Widens),
            v(4, Narrows),
            v(5, ActionHeavier),
        ];
        versions[3].code = Some(Code::Current);
        let e = RuleEntry::new(PackRule::Mw2, true, versions);
        assert_eq!(e.direction(Some(1), 2), Some(Widens));
        assert_eq!(e.direction(None, 2), Some(Widens));
        assert_eq!(e.direction(Some(2), 4), Some(Narrows));
        assert_eq!(e.direction(Some(1), 5), Some(Mixed));
        assert_eq!(e.direction(Some(3), 5), None, "no record of version 3");
        assert_eq!(e.direction(Some(5), 4), None, "not newer");
        assert_eq!(e.direction(Some(5), 5), None);
        assert_eq!(e.direction(Some(4), 6), None);
        assert_eq!(e.oldest_retained().version, 5);
        assert!(e.retained(4).is_none() && e.get(4).is_some());
        for d in Direction::ALL {
            assert_eq!(Direction::from_name(d.as_str()), Some(d));
            assert_eq!(d.then(d), d);
        }
        assert!(Widens.strengthens() && ActionHeavier.strengthens());
        assert!(!Narrows.strengthens() && !ActionLighter.strengthens() && !Mixed.strengthens());
    }

    #[test]
    fn bad_manifests_are_refused() {
        let one = |r, version, direction, code| {
            RuleEntry::new(
                r,
                true,
                vec![Version {
                    version,
                    direction,
                    action: Action::Hide,
                    code,
                }],
            )
        };
        let all = |except: PackRule| -> Vec<RuleEntry> {
            PackRule::ALL
                .into_iter()
                .filter(|r| *r != except)
                .map(|r| one(r, 1, Direction::Widens, Some(Code::Current)))
                .collect()
        };
        assert!(Manifest::new(all(PackRule::Mw1)).is_err(), "MW-1 missing");
        let mut twice = all(PackRule::Mw1);
        twice.push(one(
            PackRule::Mw2,
            1,
            Direction::Widens,
            Some(Code::Current),
        ));
        assert!(Manifest::new(twice).is_err());
        for bad in [
            one(PackRule::Mw1, 0, Direction::Widens, Some(Code::Current)),
            one(PackRule::Mw1, 1, Direction::Narrows, Some(Code::Current)),
            one(PackRule::Mw1, 1, Direction::Widens, None),
        ] {
            let mut m = all(PackRule::Mw1);
            m.push(bad);
            assert!(Manifest::new(m).is_err());
        }
        let mut m = all(PackRule::Mw1);
        m.push(RuleEntry::new(
            PackRule::Mw1,
            true,
            vec![
                Version {
                    version: 1,
                    direction: Direction::Widens,
                    action: Action::Hide,
                    code: Some(Code::Current),
                },
                Version {
                    version: 2,
                    direction: Direction::Widens,
                    action: Action::Hide,
                    code: Some(Code::Current),
                },
            ],
        ));
        assert!(Manifest::new(m).is_err(), "only the newest is current");
        let mut m = all(PackRule::Mw1);
        m.push(one(
            PackRule::Mw1,
            1,
            Direction::Widens,
            Some(Code::Current),
        ));
        assert!(Manifest::new(m).is_ok());
    }

    /// A deterministic set of torrents over every pack rule's evidence and
    /// guards: the per-rule golden below hashes which of them each rule
    /// matches.
    fn golden_set() -> Vec<(String, Vec<FileInfo>)> {
        const NAME: &[&str] = &[
            "movie",
            "2019",
            "1080p",
            "x265",
            "free",
            "robux",
            "spotify",
            "netflix",
            "bitcoin",
            "wallet",
            "recovery",
            "generator",
            "hack",
            "premium",
            "trainer",
            "offline",
            "mod",
            "sample",
            "trailer",
            "vbucks",
            "fortnite",
            "www.site.com",
            "[site.org]",
            "codec",
            "setup",
            "album",
            "s01e02",
            "discography",
            "complete",
        ];
        const FILE: &[(&str, u64)] = &[
            ("movie.mkv", 4 << 30),
            ("movie.mp4.exe", 2 << 20),
            ("codec setup.exe", 2 << 20),
            ("player.exe", 2 << 20),
            ("tiny.mkv", 90_000),
            ("screensaver.scr", 1 << 20),
            ("password.txt", 90),
            ("how to open.txt", 90),
            ("unlock survey.txt", 90),
            ("human verification unlock.txt", 90),
            ("visit site.url", 90),
            ("movie.rar", 4 << 30),
            ("movie.r00", 50 << 20),
            ("movie.sfv", 300),
            ("movie.nfo", 2_000),
            ("song.flac", 30 << 20),
            ("setup.exe", 40 << 20),
            ("VIDEO_TS/VTS_01_1.VOB", 1 << 30),
            ("pad.bin", 0),
            ("readme.txt", 300),
        ];
        // A small LCG: the same set on every machine.
        let mut seed: u64 = 0x005E_ED0F_5EED;
        let mut next = |n: usize| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((seed >> 33) as usize) % n
        };
        let mut out = Vec::new();
        for _ in 0..3_000 {
            let words = 1 + next(6);
            let name: Vec<&str> = (0..words).map(|_| NAME[next(NAME.len())]).collect();
            let files = 1 + next(4);
            let mut f: Vec<FileInfo> = (0..files)
                .map(|_| {
                    let (p, s) = FILE[next(FILE.len())];
                    FileInfo::new(p.split('/').map(str::to_owned).collect(), s)
                })
                .collect();
            if next(9) == 0 {
                f.clear();
            }
            let mut name = name.join(" ");
            if next(15) == 0 {
                name = std::iter::repeat_n(name.as_str(), 60)
                    .collect::<Vec<_>>()
                    .join(" ");
            }
            out.push((name, f));
        }
        // Names for the rules added after RP-0 (MW-6, UC-1 to UC-3), after
        // the generated set, so the rules before them keep their golden.
        const EXTRA: &[(&str, &[(&str, u64)])] = &[
            ("Example Editor v2.1 x64 Crack", &[("setup.exe", 2 << 30)]),
            ("Example.Suite.v16.0.Keygen-GRP", &[("setup.exe", 2 << 30)]),
            (
                "Example Tool v4.1 Pre-Activated",
                &[("setup.exe", 80 << 20)],
            ),
            ("Example.Browser.120.0.1.Patch", &[("update.exe", 50 << 20)]),
            ("Crack Attack v1.2 x64 Patch", &[("setup.exe", 50 << 20)]),
            ("Example.Movie.2024.HDCAM.x264-GRP", &[("m.mkv", 1 << 30)]),
            ("Example Movie (2024) [TS] 720p", &[("m.mkv", 1 << 30)]),
            ("Example.Movie.2024.CAM.x264-GRP", &[("m.mkv", 1 << 30)]),
            ("Nanny Cam 1080p x264-GRP", &[("m.mkv", 1 << 30)]),
            ("Little Ts 720p", &[("m.mkv", 1 << 30)]),
            (
                "Example.Movie.2019.1080p.BluRay.x264-GRP",
                &[("m.mkv", 8 << 30)],
            ),
            (
                "Example.Show.S02E03.1080p.WEB-DL.H.264-GRP",
                &[("m.mkv", 2 << 30)],
            ),
            (
                "Big.Buck.Bunny.2008.1080p.BluRay.x264-GRP",
                &[("m.mkv", 1 << 30)],
            ),
            ("Example Movie 2019 1080p BluRay", &[("m.mkv", 8 << 30)]),
        ];
        for (name, files) in EXTRA {
            let f = files
                .iter()
                .map(|(p, s)| FileInfo::new(vec![(*p).to_owned()], *s))
                .collect();
            out.push(((*name).to_owned(), f));
        }
        out
    }

    /// Per rule, version and action, how many of [`golden_set`] it matches and a
    /// hash of which. A change to what a rule matches fails this unless
    /// its version is bumped (and the new version's golden added): the
    /// golden of today's behaviour that RP-1's corpus takes over.
    const GOLDEN: &[(&str, usize, u64)] = &[
        ("MW-4@1 drop", 16, 13510458297458882951),
        ("MW-1@1 hide", 318, 18405588938268816894),
        ("MW-2@1 hide", 199, 5420927143327199823),
        ("MW-3@1 hide", 114, 17286647990777949854),
        ("MW-5@1 hide", 128, 624893996585892302),
        ("MW-6@1 hide", 3, 8239104331045227003),
        ("LU-1@1 hide", 6, 5791013990083080710),
        ("LU-4@1 hide", 587, 15362597008036224271),
        ("LU-2@1 demote", 85, 18213498744997554530),
        ("LU-3@1 demote", 100, 935730709647423198),
        ("FK-4@1 drop", 42, 2688972397374616067),
        ("FK-1@1 hide", 195, 1162936391390153166),
        ("FK-5@1 demote", 29, 8441971907697171463),
        ("FK-2@1 flag", 16, 1128617146480757237),
        ("SP-2@1 hide", 162, 11973947260632497437),
        ("SP-1@1 demote", 426, 419072724506069972),
        // Every other item of the set, those marked flooded.
        ("SP-3@1 demote", 1507, 2971618462628662722),
        ("SC-1@1 hide", 351, 370022691562877752),
        ("SC-2@1 hide", 287, 15705590736820052948),
        ("UC-1@1 hide", 3, 3719771000408991908),
        ("UC-2@1 hide", 3, 8239104331045227003),
        ("UC-3@1 hide", 2, 1690303002812331472),
    ];

    #[test]
    fn rules_match_as_their_version_says() {
        let set = golden_set();
        let m = Manifest::builtin();
        let mut got: Vec<(String, usize, u64)> = PackRule::ALL
            .into_iter()
            .map(|r| {
                (
                    format!(
                        "{}@{} {}",
                        r.id(),
                        m.entry(r).current().version,
                        m.entry(r).current().action.as_str()
                    ),
                    0,
                    FNV,
                )
            })
            .collect();
        for (i, (name, files)) in set.iter().enumerate() {
            let class = classify(name, files);
            let facts = Facts {
                class: &class,
                size: 0,
                piece_len: 0,
                flags: 0,
                discovered_at: 0,
                now: 0,
            };
            let item = Item {
                hashes: &[[0; 20]],
                name,
                files,
                content_type: class.content_type,
            };
            // SP-3 matches where the writer counted a flood: every other
            // item here, which leaves every other rule as it is.
            let flood = (i % 2 == 0).then_some(crate::flood::Level::Flag);
            let ctx = Ctx::new(&item, &facts, &[]).with_flood(flood);
            // SC-1 ∪ SC-2 is SC-1 as it was before RP-0 split them.
            assert_eq!(
                ctx.scam(|_| true),
                ctx.matches(PackRule::Sc1) || ctx.matches(PackRule::Sc2),
                "{name:?}"
            );
            for (r, g) in PackRule::ALL.into_iter().zip(&mut got) {
                if m.entry(r).current().matches(r, &ctx) == Some(true) {
                    g.1 += 1;
                    for b in (i as u32).to_le_bytes() {
                        g.2 = (g.2 ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
                    }
                }
            }
        }
        let got: Vec<(&str, usize, u64)> =
            got.iter().map(|(k, n, h)| (k.as_str(), *n, *h)).collect();
        for (r, n, _) in &got {
            assert!(*n > 0, "{r} matches none of the golden set: widen the set");
        }
        assert_eq!(
            got, GOLDEN,
            "a pack rule's matching changed: bump its version in the manifest, \
             add a changelog entry, and record the new version's golden"
        );
    }

    const FNV: u64 = 0xcbf2_9ce4_8422_2325;
}
