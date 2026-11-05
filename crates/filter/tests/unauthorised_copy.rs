//! RP-UC (owner §2.39, `docs/decisions/2026-11-04-X-legal-docs.md`): the
//! `unauthorised-copy` pack (UC-1 to UC-3) and MW-6.
//!
//! - **Upgrades:** a store an earlier build set up has the new pack off,
//!   with an explicit row, until the user turns it on; a fresh store has it
//!   on and applied; MW-6 in a malware pack that's already on stays in
//!   shadow until accepted.
//! - **Precision:** every name in the release parser's golden corpus is
//!   judged by the four rules, against the corpus's own hand labels, and
//!   the per-rule numbers are printed (`--nocapture`) for the PR. The
//!   lawful names ([`LAWFUL`]) are never held by any pack rule.

use lodestar_classify::{FileInfo, classify};
use lodestar_filter::packs::manifest::Manifest;
use lodestar_filter::packs::upgrade::{self, DEFAULTS_KEY, FRESH_KEY, KNOWN_KEY};
use lodestar_filter::packs::{Ctx, Pack, PackRule};
use lodestar_filter::{
    Action, Core, CsList, Engine, Facts, Item, Lists, RuleRef, Switches, TermSet,
};
use lodestar_release_parser::{Flag, Source};
use lodestar_store::{Store, StoreConfig};

fn fresh_store() -> Store {
    Store::open_in_memory(StoreConfig::default()).unwrap()
}

/// A store an RP-DEF build set up: its defaults written for the five
/// packs it knew, as `apply_pack_defaults` left them, and nothing for
/// `unauthorised-copy`.
fn rp_def_store(on: &[&str]) -> Store {
    let mut s = fresh_store();
    s.update_settings_text(&[(DEFAULTS_KEY, "1")], &[FRESH_KEY])
        .unwrap();
    for p in upgrade::DEFAULTS_PACKS {
        s.set_pack_switch(p.as_str(), on.contains(&p.as_str()))
            .unwrap();
    }
    let seed = upgrade::seed_record(&Manifest::builtin()).encode();
    s.set_settings_text(&[
        (upgrade::APPLIED_KEYS[0], seed.as_str()),
        (upgrade::APPLIED_KEYS[1], seed.as_str()),
        (upgrade::SEEDED_KEY, "1"),
    ])
    .unwrap();
    s
}

fn load(store: &Store) -> Engine {
    let lists = Lists {
        sexual: TermSet::of(&["lexa"]),
        child_roles: TermSet::of(&["rolea"]),
        codes: TermSet::of(&["codex"]),
    };
    let mut e = Engine::new(Core::new(lists, CsList::default(), Switches::default()));
    e.manifest = Manifest::builtin();
    let notes = e.load_rules(&store.rule_set().unwrap());
    assert!(notes.is_empty(), "{notes:?}");
    e
}

fn files(paths: &[(&str, u64)]) -> Vec<FileInfo> {
    paths
        .iter()
        .map(|(p, s)| FileInfo::new(p.split('/').map(str::to_owned).collect(), *s))
        .collect()
}

/// The decision, and the pack rules in shadow.
fn judge(
    e: &Engine,
    name: &str,
    paths: &[(&str, u64)],
) -> (Option<(RuleRef, Action)>, Vec<PackRule>) {
    let f = files(paths);
    let class = classify(name, &f);
    let facts = Facts {
        class: &class,
        size: f.iter().map(|x| x.size).sum(),
        piece_len: 1 << 20,
        flags: 0,
        discovered_at: 0,
        now: 0,
    };
    let item = Item {
        hashes: &[[7; 20]],
        name,
        files: &f,
        content_type: class.content_type,
    };
    let (v, shadow) = e.judge_shadowed(&item, Some(&facts), 0);
    (
        v.decision.map(|d| (d.rule, d.action)),
        shadow.iter().map(|s| s.rule).collect(),
    )
}

const CAM: (&str, &[(&str, u64)]) = (
    "Example.Movie.2024.HDCAM.x264-GRP",
    &[("Example.Movie.2024.HDCAM.x264-GRP.mkv", 1 << 30)],
);
const CRACK: (&str, &[(&str, u64)]) = (
    "Example Photo Editor 2024 v25.1 x64 Crack",
    &[("setup.exe", 2 << 30), ("Crack/patch.exe", 1 << 20)],
);

fn pack_row(s: &Store, pack: &str) -> Option<bool> {
    s.rule_set()
        .unwrap()
        .pack_switches
        .iter()
        .find(|(id, _)| id == pack)
        .map(|(_, on)| *on)
}

/// RP-UC's test from the spec: on a store an earlier build set up, the
/// new pack gets its explicit off row and stays off until turned on.
#[test]
fn an_upgraded_store_has_unauthorised_copy_off_until_turned_on() {
    let m = Manifest::builtin();
    let mut s = rp_def_store(&[]);
    assert_eq!(pack_row(&s, "unauthorised-copy"), None);
    assert!(upgrade::ensure_seeded(&mut s, &m, 0).unwrap().is_none());
    assert_eq!(pack_row(&s, "unauthorised-copy"), Some(false), "explicit");
    let all: Vec<&str> = Pack::ALL.iter().map(|p| p.as_str()).collect();
    assert_eq!(
        s.setting_text(KNOWN_KEY).unwrap().as_deref(),
        Some(all.join(" ").as_str())
    );
    let e = load(&s);
    for r in [PackRule::Uc1, PackRule::Uc2, PackRule::Uc3] {
        assert!(!e.packs.is_on(r), "{}", r.id());
    }
    assert_eq!(judge(&e, CAM.0, CAM.1).0, None);
    // Turned on (preview → apply records its current versions): applied.
    let recorded = upgrade::set_pack_switch(&mut s, &m, "unauthorised-copy", true, 0).unwrap();
    assert_eq!(recorded, [PackRule::Uc1, PackRule::Uc2, PackRule::Uc3]);
    let e = load(&s);
    for r in [PackRule::Uc1, PackRule::Uc2, PackRule::Uc3] {
        assert!(e.pack_versions.state(r).plain, "{} applies", r.id());
    }
    assert_eq!(
        judge(&e, CAM.0, CAM.1).0,
        Some((RuleRef::Pack(PackRule::Uc1), Action::Hide))
    );
    // Turned off again, then an Undo that deletes the rows: the step
    // doesn't run again, and the pack reads off.
    upgrade::set_pack_switch(&mut s, &m, "unauthorised-copy", false, 0).unwrap();
    let mut set = s.rule_set().unwrap();
    set.pack_switches.clear();
    s.replace_rule_set(&set).unwrap();
    upgrade::ensure_seeded(&mut s, &m, 0).unwrap();
    assert_eq!(pack_row(&s, "unauthorised-copy"), None, "not written again");
    assert!(!load(&s).packs.is_on(PackRule::Uc1));
}

/// A row the user already has is a choice: the step never changes it.
#[test]
fn a_row_already_there_is_kept() {
    let m = Manifest::builtin();
    let mut s = rp_def_store(&[]);
    s.set_pack_switch("unauthorised-copy", true).unwrap();
    upgrade::ensure_seeded(&mut s, &m, 0).unwrap();
    assert_eq!(pack_row(&s, "unauthorised-copy"), Some(true));
    // And a store a pre-RP-DEF build left gets every off row in step 3.
    let mut s = fresh_store();
    s.update_settings_text(&[], &[FRESH_KEY]).unwrap();
    upgrade::ensure_seeded(&mut s, &m, 0).unwrap();
    for p in Pack::ALL {
        assert_eq!(pack_row(&s, p.as_str()), Some(false), "{}", p.as_str());
    }
}

/// A fresh store has the pack on, with UC-1 to UC-3 and MW-6 applied at
/// their current versions, not pending.
#[test]
fn a_fresh_store_has_it_on_and_applied() {
    let m = Manifest::builtin();
    let mut s = fresh_store();
    assert!(upgrade::ensure_seeded(&mut s, &m, 0).unwrap().is_some());
    assert_eq!(pack_row(&s, "unauthorised-copy"), Some(true));
    assert!(s.setting_text(KNOWN_KEY).unwrap().is_some());
    let e = load(&s);
    for r in [PackRule::Mw6, PackRule::Uc1, PackRule::Uc2, PackRule::Uc3] {
        assert!(e.packs.is_on(r), "{}", r.id());
        assert!(e.pack_versions.state(r).plain, "{} applies", r.id());
    }
    assert_eq!(
        judge(&e, CAM.0, CAM.1).0,
        Some((RuleRef::Pack(PackRule::Uc1), Action::Hide))
    );
    // Cracked software: MW-6 and UC-2 both hold it, so either alone does.
    let (d, _) = judge(&e, CRACK.0, CRACK.1);
    assert!(
        matches!(
            d,
            Some((RuleRef::Pack(PackRule::Mw6 | PackRule::Uc2), Action::Hide))
        ),
        "{d:?}"
    );
    upgrade::set_pack_switch(&mut s, &m, "unauthorised-copy", false, 0).unwrap();
    let e = load(&s);
    assert_eq!(
        judge(&e, CRACK.0, CRACK.1).0,
        Some((RuleRef::Pack(PackRule::Mw6), Action::Hide)),
        "turning unauthorised-copy off doesn't restore cracked software"
    );
    assert_eq!(judge(&e, CAM.0, CAM.1).0, None);
}

/// MW-6 in a malware pack an earlier build had on is pending, in shadow:
/// it decides nothing until accepted (RP-3, RP-6).
#[test]
fn mw6_is_in_shadow_on_an_upgraded_store_with_malware_on() {
    let m = Manifest::builtin();
    let mut s = rp_def_store(&["malware"]);
    upgrade::ensure_seeded(&mut s, &m, 0).unwrap();
    let e = load(&s);
    assert!(e.packs.is_on(PackRule::Mw6) && e.packs.is_on(PackRule::Mw1));
    let st = e.pack_versions.state(PackRule::Mw6);
    assert!(st.pending() && st.apply.is_empty());
    let (d, shadow) = judge(&e, CRACK.0, CRACK.1);
    assert_eq!(d, None, "nothing applies before acceptance");
    assert_eq!(shadow, [PackRule::Mw6]);
    // Accepted: it applies.
    upgrade::accept(&mut s, &m, &[PackRule::Mw6], 0).unwrap();
    let e = load(&s);
    assert_eq!(
        judge(&e, CRACK.0, CRACK.1).0,
        Some((RuleRef::Pack(PackRule::Mw6), Action::Hide))
    );
}

// ---------------------------------------------------------------------
// Precision.

/// Names that must never be hidden: public-domain and Creative Commons
/// films, Internet Archive items, Linux images, lawful patches and
/// updates, and title words the parser reads as a weak source. Each with
/// the files it would have.
const LAWFUL: &[(&str, &[(&str, u64)])] = &[
    (
        "Nanny Cam 1080p x264-GRP",
        &[("Nanny Cam 1080p x264-GRP.mkv", 2 << 30)],
    ),
    ("Little Ts 720p", &[("Little Ts 720p.mkv", 900 << 20)]),
    ("Dash Cam Footage 2019 1080p", &[("dashcam.mp4", 3 << 30)]),
    ("The Cam Girl Next Door 1080p WEB", &[("film.mkv", 2 << 30)]),
    (
        "Big.Buck.Bunny.2008.1080p.BluRay.x264-GRP",
        &[("bbb.mkv", 900 << 20)],
    ),
    (
        "Big Buck Bunny 60fps 4K",
        &[("bbb_sunflower_2160p_60fps_normal.mp4", 600 << 20)],
    ),
    (
        "Sintel.2010.2160p.WEB-DL.x265-GRP",
        &[("sintel.mkv", 2 << 30)],
    ),
    (
        "Tears.of.Steel.2012.1080p.BluRay.x264-GRP",
        &[("tos.mkv", 1 << 30)],
    ),
    (
        "Elephants Dream 2006 1080p WEB-DL-GRP",
        &[("ed.mkv", 700 << 20)],
    ),
    (
        "Cosmos.Laundromat.2015.1080p.WEBRip.x264-GRP",
        &[("cl.mkv", 700 << 20)],
    ),
    (
        "Spring.2019.1080p.WEB-DL.x264-GRP",
        &[("spring.mkv", 500 << 20)],
    ),
    (
        "Night.of.the.Living.Dead.1968.1080p.BluRay.x264-GRP",
        &[("notld.mkv", 8 << 30)],
    ),
    ("Nosferatu.1922.720p.BluRay.x264-GRP", &[("n.mkv", 4 << 30)]),
    (
        "Plan.9.from.Outer.Space.1957.DVDRip.XviD-GRP",
        &[("p9.avi", 700 << 20)],
    ),
    (
        "Steamboat.Willie.1928.1080p.WEB-DL-GRP",
        &[("sw.mkv", 200 << 20)],
    ),
    (
        "His.Girl.Friday.1940.DVDRip.x264-GRP",
        &[
            ("His.Girl.Friday.1940.DVDRip.x264-GRP.mkv", 1 << 30),
            ("his-girl-friday-1940_meta.xml", 2_000),
            ("his-girl-friday-1940_files.xml", 4_000),
        ],
    ),
    (
        "CarnivalOfSouls",
        &[
            ("CarnivalOfSouls.mp4", 700 << 20),
            ("CarnivalOfSouls_meta.xml", 2_000),
            ("CarnivalOfSouls_files.xml", 4_000),
            ("__ia_thumb.jpg", 20_000),
        ],
    ),
    (
        "ubuntu-24.04.1-desktop-amd64.iso",
        &[("ubuntu-24.04.1-desktop-amd64.iso", 6 << 30)],
    ),
    (
        "debian-12.7.0-amd64-netinst.iso",
        &[("debian-12.7.0-amd64-netinst.iso", 650 << 20)],
    ),
    (
        "Fedora-Workstation-Live-x86_64-40",
        &[("Fedora-Workstation-Live-x86_64-40-1.14.iso", 2 << 30)],
    ),
    (
        "Firefox.120.0.1.Patch",
        &[("firefox-120.0.1.patch.exe", 50 << 20)],
    ),
    (
        "LibreOffice 24.2.5 Update x64",
        &[("LibreOffice_24.2.5_Win_x86-64.msi", 350 << 20)],
    ),
    (
        "Blender 4.2.0 Windows x64 Patch",
        &[("blender-4.2.0-windows-x64.msi", 300 << 20)],
    ),
    ("Crack Attack v1.2 x64 Patch", &[("setup.exe", 50 << 20)]),
    (
        "John the Ripper 1.9.0 Jumbo password cracker",
        &[("john.zip", 40 << 20)],
    ),
    (
        "Cracked.Eggs.Documentary.2019.1080p.WEB",
        &[("eggs.mkv", 2 << 30)],
    ),
    ("The Crack 2023 1080p WEB", &[("crack.mkv", 2 << 30)]),
    (
        "Keygen Church - Album 2021 FLAC",
        &[("01 - Song.flac", 30 << 20), ("02 - Song.flac", 30 << 20)],
    ),
    (
        "Linux Mint 22 Cinnamon 64-bit",
        &[("linuxmint-22-cinnamon-64bit.iso", 3 << 30)],
    ),
];

#[test]
fn lawful_names_are_never_held() {
    let m = Manifest::builtin();
    let mut held = Vec::new();
    for (name, paths) in LAWFUL {
        let f = files(paths);
        let class = classify(name, &f);
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
            files: &f,
            content_type: class.content_type,
        };
        let ctx = Ctx::new(&item, &facts, &[]);
        for r in PackRule::ALL {
            if m.entry(r).current().matches(r, &ctx) == Some(true) {
                held.push(format!("{name} held by {}", r.id()));
            }
        }
    }
    assert!(held.is_empty(), "{held:#?}");
}

/// One golden-corpus entry: the name, its file, and its hand labels.
struct Golden {
    file: &'static str,
    name: String,
    labels: Vec<(String, String)>,
}

impl Golden {
    /// Files of the kind the labels describe, as a torrent of it would
    /// carry: an installer for software, a video for film and TV, tracks
    /// for music, nothing otherwise. Chosen from the hand labels, never
    /// from the parser, so the type the rules see is the corpus's.
    fn files(&self) -> Vec<FileInfo> {
        let has = |k: &str| self.label(k).is_some();
        let one = |p: String, s: u64| FileInfo::new(vec![p], s);
        if has("version")
            || has("platform")
            || has("arch")
            || self.label("flags").is_some_and(|f| f.contains("cracked"))
        {
            vec![one("setup.exe".into(), 200 << 20)]
        } else if has("artist") || has("album") || has("bitrate") || has("sample") {
            vec![
                one("01 - Track.flac".into(), 30 << 20),
                one("02 - Track.flac".into(), 30 << 20),
            ]
        } else if has("res") || has("vcodec") || has("season") || has("episode") || has("source") {
            vec![one(format!("{}.mkv", self.name), 2 << 30)]
        } else {
            Vec::new()
        }
    }

    fn label(&self, key: &str) -> Option<&str> {
        self.labels
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

const CORPUS: &[(&str, &str)] = &[
    (
        "anime",
        include_str!("../../release-parser/tests/golden/anime.txt"),
    ),
    (
        "books_other",
        include_str!("../../release-parser/tests/golden/books_other.txt"),
    ),
    (
        "holdout",
        include_str!("../../release-parser/tests/golden/holdout.txt"),
    ),
    (
        "movies",
        include_str!("../../release-parser/tests/golden/movies.txt"),
    ),
    (
        "music",
        include_str!("../../release-parser/tests/golden/music.txt"),
    ),
    (
        "software_games",
        include_str!("../../release-parser/tests/golden/software_games.txt"),
    ),
    (
        "tv",
        include_str!("../../release-parser/tests/golden/tv.txt"),
    ),
];

fn golden() -> Vec<Golden> {
    let mut out: Vec<Golden> = Vec::new();
    for (file, text) in CORPUS {
        for line in text.lines() {
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with(' ') {
                if let (Some(g), Some((k, v))) = (out.last_mut(), line.trim().split_once(':')) {
                    g.labels.push((k.trim().to_owned(), v.trim().to_owned()));
                }
            } else {
                out.push(Golden {
                    file,
                    name: line.to_owned(),
                    labels: Vec::new(),
                });
            }
        }
    }
    out
}

/// Per rule: the names it holds and, of them, the ones the corpus's
/// labels agree are what it's for; and the labelled ones it missed.
#[derive(Default)]
struct Tally {
    held: Vec<String>,
    wrong: Vec<String>,
    missed: Vec<String>,
}

/// What a rule is for, by the corpus's hand labels (never the parser's
/// output): UC-1 a pre-release source; MW-6 and UC-2 the cracked flag;
/// UC-3 a disc or web source with a release group.
fn labelled(rule: PackRule, g: &Golden) -> bool {
    let source = g.label("source");
    match rule {
        PackRule::Uc1 => matches!(source, Some("cam" | "telesync" | "telecine" | "screener")),
        PackRule::Mw6 | PackRule::Uc2 => g.label("flags").is_some_and(|f| f.contains("cracked")),
        PackRule::Uc3 => {
            g.label("group").is_some()
                && matches!(
                    source,
                    Some("bluray" | "remux" | "dvd" | "web-dl" | "webrip")
                )
        }
        _ => false,
    }
}

const RULES: [PackRule; 4] = [PackRule::Uc1, PackRule::Uc2, PackRule::Uc3, PackRule::Mw6];

#[test]
fn precision_on_the_parser_corpus() {
    let m = Manifest::builtin();
    let corpus = golden();
    assert!(corpus.len() > 300, "the corpus reads: {}", corpus.len());
    let mut tally: Vec<Tally> = RULES.iter().map(|_| Tally::default()).collect();
    for g in &corpus {
        let f = g.files();
        let class = classify(&g.name, &f);
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
            name: &g.name,
            files: &f,
            content_type: class.content_type,
        };
        let ctx = Ctx::new(&item, &facts, &[]);
        for (r, t) in RULES.iter().zip(&mut tally) {
            let hit = m.entry(*r).current().matches(*r, &ctx) == Some(true);
            let want = labelled(*r, g);
            let tag = format!("{}: {} ({:?})", g.file, g.name, class.content_type);
            if hit {
                t.held.push(tag.clone());
                if !want {
                    t.wrong.push(tag);
                }
            } else if want {
                t.missed.push(tag);
            }
        }
    }
    println!(
        "RP-UC precision on the release-parser golden corpus ({} names):",
        corpus.len()
    );
    for (r, t) in RULES.iter().zip(&tally) {
        let right = t.held.len() - t.wrong.len();
        println!(
            "  {}: holds {}, {} as labelled (precision {}), misses {} labelled",
            r.id(),
            t.held.len(),
            right,
            if t.held.is_empty() {
                "n/a".to_owned()
            } else {
                format!("{:.1}%", 100.0 * right as f64 / t.held.len() as f64)
            },
            t.missed.len()
        );
        for w in &t.wrong {
            println!("    not as labelled: {w}");
        }
        for w in &t.missed {
            println!("    missed: {w}");
        }
    }
    // UC-1, UC-2 and MW-6 are the high-precision tiers: everything they
    // hold is what the labels say. UC-3 is medium precision by design
    // (owner §2.39): what it holds that the labels don't call a disc or
    // web rip with a group is listed above, and stays under a tenth.
    for (r, t) in RULES.iter().zip(&tally) {
        assert!(!t.held.is_empty(), "{} holds nothing in the corpus", r.id());
        if *r == PackRule::Uc3 {
            assert!(
                t.wrong.len() * 10 < t.held.len(),
                "{}: {:#?}",
                r.id(),
                t.wrong
            );
        } else {
            assert!(t.wrong.is_empty(), "{}: {:#?}", r.id(), t.wrong);
        }
    }
    // MW-6 and UC-2 are the same test.
    assert_eq!(tally[1].held, tally[3].held);
}

/// The labels the precision test reads are the corpus's own, and the
/// parser agrees with them on the fields the rules read, so a wrong
/// number above is the rule's, not the parser's.
#[test]
fn the_rules_read_fields_the_parser_gets_right() {
    let corpus = golden();
    for g in &corpus {
        let class = classify(&g.name, &[]);
        let r = &class.title.release;
        if let Some(s) = g.label("source")
            && matches!(s, "cam" | "telesync" | "telecine" | "screener")
        {
            assert_eq!(r.source.map(Source::as_str), Some(s), "{}", g.name);
        }
        if g.label("flags").is_some_and(|f| f.contains("cracked")) {
            assert!(r.flags.contains(&Flag::Cracked), "{}", g.name);
        }
    }
}
