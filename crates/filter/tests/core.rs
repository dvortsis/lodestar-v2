//! The child-safety core (§20.5, §20.8). Rule logic is tested with neutral
//! stand-in words (`lexa` for a sexual term, `rolea` for a child role,
//! `codex` for a code term), so no indicative text is needed. The shipped
//! lists are tested with hashed fixtures and a plain-text precision corpus
//! of legitimate titles.

use lodestar_classify::{ContentType, FileInfo};
use lodestar_filter::{
    Action, Core, CoreRule, CsList, Evidence, Field, Fixture, Item, Lists, Locked, Origin,
    Switches, TermSet, normalize::Token, payload_sig, term_hash,
};

fn stand_ins() -> Lists {
    Lists {
        sexual: TermSet::of(&["lexa"]),
        child_roles: TermSet::of(&["rolea"]),
        codes: TermSet::of(&["codex"]),
    }
}

fn core() -> Core {
    Core::new(stand_ins(), CsList::default(), Switches::default())
}

fn files(paths: &[(&str, u64)]) -> Vec<FileInfo> {
    paths
        .iter()
        .map(|(p, s)| FileInfo::new(p.split('/').map(str::to_owned).collect(), *s))
        .collect()
}

const H1: [u8; 20] = [1; 20];
const H2: [u8; 20] = [2; 20];

fn rule(core: &Core, name: &str, paths: &[(&str, u64)], t: ContentType) -> Option<CoreRule> {
    let f = files(paths);
    let item = Item {
        hashes: &[H2],
        name,
        files: &f,
        content_type: t,
    };
    core.verdict(&item).map(|h| h.rule)
}

fn name_rule(core: &Core, name: &str) -> Option<CoreRule> {
    rule(core, name, &[("a.mkv", 1)], ContentType::Video)
}

#[test]
fn floor_hashes() {
    let mut c = core();
    c.cs.add_hash(H1, Origin::Imported);
    let f = files(&[("a.mkv", 1)]);
    let item = |hashes: &'static [[u8; 20]]| Item {
        hashes,
        name: "Anything",
        files: &f,
        content_type: ContentType::Movie,
    };
    let hit = c.verdict(&item(&[H2, H1])).unwrap();
    assert_eq!((hit.rule, hit.action), (CoreRule::Cs1b, Action::Drop));
    assert_eq!(hit.evidence, vec![Evidence::Hash(Origin::Imported)]);
    // Your own confirmation outranks the import.
    c.cs.add_hash(H1, Origin::Confirmed);
    assert_eq!(c.verdict(&item(&[H1])).unwrap().rule, CoreRule::Cs1a);
    c.cs.add_hash(H1, Origin::Imported);
    assert_eq!(c.cs.hash(&H1), Some(Origin::Confirmed));
    assert!(c.verdict(&item(&[H2])).is_none());
    assert!(c.cs.remove_hash(&H1));
    assert!(c.verdict(&item(&[H1])).is_none());
}

#[test]
fn floor_payload_only_when_specific() {
    let mut c = core();
    let specific = files(&[("x/a.mkv", 734_003_201), ("x/b.srt", 81_233)]);
    let vague = files(&[("a.txt", 1 << 20)]);
    c.cs.add_sig(payload_sig(&specific).unwrap().sig);
    c.cs.add_sig(payload_sig(&vague).unwrap().sig);
    let renamed = files(&[("other/name.mkv", 734_003_201), ("other/s.srt", 81_233)]);
    let check = |f: &[FileInfo]| {
        c.verdict(&Item {
            hashes: &[H2],
            name: "Renamed",
            files: f,
            content_type: ContentType::Movie,
        })
        .map(|h| h.rule)
    };
    assert_eq!(check(&renamed), Some(CoreRule::Cs8a));
    assert_eq!(check(&files(&[("b.txt", 1 << 20)])), None);
}

#[test]
fn floor_cannot_be_switched_off() {
    let mut s = Switches::default();
    for r in CoreRule::ALL {
        assert!(s.is_on(r), "{} on by default", r.id());
        assert_eq!(r.floor(), r.action() == Action::Drop);
        assert!(r.action().protective());
        assert_eq!(CoreRule::from_id(r.id()), Some(r));
        if r.floor() {
            assert_eq!(s.set(r, false), Err(Locked(r)));
            assert!(s.is_on(r));
        } else {
            s.set(r, false).unwrap();
            assert!(!s.is_on(r));
        }
    }
    assert_eq!(s.off().count(), 6);
}

#[test]
fn switched_off_rules_still_report_for_previews() {
    let mut c = core();
    c.switches.set(CoreRule::Cs2a, false).unwrap();
    let f = files(&[("a.mkv", 1)]);
    let item = Item {
        hashes: &[H2],
        name: "Title 15yo lexa",
        files: &f,
        content_type: ContentType::Video,
    };
    assert!(c.verdict(&item).is_none());
    let hits = c.hits(&item);
    assert_eq!(hits.len(), 1);
    assert_eq!((hits[0].rule, hits[0].on), (CoreRule::Cs2a, false));
}

#[test]
fn minor_ages() {
    let c = core();
    for name in [
        "Title 15yo lexa",
        "Title 15 y/o lexa",
        "Title 15-year-old lexa",
        "Title 15 years old something lexa",
        "lexa Title aged 15",
        "Title age 9 lexa",
        // Evasions: look-alike letters, invisible characters, leet.
        "Title 15yo l\u{0435}xa",
        "Title 15yo le\u{200B}xa",
        "Title 15yo l3xa",
        "Title 15yo l.e.x.a",
    ] {
        assert_eq!(name_rule(&c, name), Some(CoreRule::Cs2a), "{name}");
    }
    for name in [
        // CSX-4: adult ages.
        "Title 18yo lexa",
        "Title 19 years old lexa",
        // CSX-3: ratings are not ages.
        "Title PEGI 16 yo lexa",
        // No age marker, or too far apart.
        "Title 15 lexa",
        "Title 15yo a b c d e f g h lexa",
        "Title S01E15 lexa",
    ] {
        assert_eq!(name_rule(&c, name), None, "{name}");
    }
    // Eight words apart still counts.
    assert_eq!(
        name_rule(&c, "Title 15yo a b c d e f lexa"),
        Some(CoreRule::Cs2a)
    );
}

#[test]
fn codes() {
    let c = core();
    assert_eq!(name_rule(&c, "Title codex"), Some(CoreRule::Cs3a));
    let in_files = rule(
        &c,
        "Plain Title",
        &[("Folder codex/a.jpg", 1), ("b.jpg", 1)],
        ContentType::Images,
    );
    assert_eq!(in_files, Some(CoreRule::Cs3b));
}

#[test]
fn roles_and_educational_context() {
    let c = core();
    assert_eq!(name_rule(&c, "rolea Title lexa"), Some(CoreRule::Cs4a));
    // CSX-2 cancels the name's evidence outside Adult…
    assert_eq!(name_rule(&c, "rolea lexa documentary"), None);
    assert_eq!(name_rule(&c, "Title 15yo lexa education"), None);
    // …but never inside Adult…
    let adult = rule(
        &c,
        "rolea lexa documentary",
        &[("a.mp4", 1)],
        ContentType::Adult,
    );
    assert_eq!(adult, Some(CoreRule::Cs4a));
    // …never in file names…
    let in_file = rule(
        &c,
        "Health Class Documentary",
        &[("ep1.mkv", 1), ("extras/rolea lexa.mkv", 1)],
        ContentType::Tv,
    );
    assert_eq!(in_file, Some(CoreRule::Cs4a));
    let age_in_file = rule(
        &c,
        "Puberty Documentary",
        &[("extras/15yo lexa.mkv", 1)],
        ContentType::Tv,
    );
    assert_eq!(age_in_file, Some(CoreRule::Cs2a));
    // …and never CS-3.
    assert_eq!(name_rule(&c, "codex documentary"), Some(CoreRule::Cs3a));
}

#[test]
fn looser_windows_inside_adult() {
    let c = core();
    let far_role = "rolea a b c d e f g h i lexa";
    let far_age = "15yo a b c d e f g h i lexa";
    assert_eq!(name_rule(&c, far_role), None);
    assert_eq!(name_rule(&c, far_age), None);
    let adult = |n| rule(&c, n, &[("a.mp4", 1)], ContentType::Adult);
    assert_eq!(adult(far_role), Some(CoreRule::Cs5a));
    assert_eq!(adult(far_age), Some(CoreRule::Cs5b));
}

#[test]
fn evidence_says_where() {
    let c = core();
    let f = files(&[("a.mkv", 1), ("b/15yo x lexa.mkv", 1)]);
    let hit = c
        .verdict(&Item {
            hashes: &[H2],
            name: "Plain",
            files: &f,
            content_type: ContentType::Video,
        })
        .unwrap();
    assert_eq!(
        hit.evidence,
        vec![
            Evidence::MinorAge {
                field: Field::File(1),
                pos: 1
            },
            Evidence::Sexual {
                field: Field::File(1),
                pos: 4
            },
        ]
    );
}

// The shipped lists.

fn shipped() -> Core {
    Core::new(
        Lists::shipped().unwrap(),
        CsList::default(),
        Switches::default(),
    )
}

#[test]
fn shipped_lists_load() {
    let l = Lists::shipped().unwrap();
    assert!(l.sexual.len() >= 20);
    assert!(l.child_roles.len() >= 10);
    // Code terms come only from your imports.
    assert!(l.codes.is_empty());
}

fn tok(pos: u32, hash: u64) -> Token {
    Token {
        pos,
        hash,
        num: None,
    }
}

fn num(pos: u32, n: u16) -> Token {
    Token {
        pos,
        hash: term_hash(&n.to_string()),
        num: Some(n),
    }
}

#[test]
fn shipped_must_match_fixtures() {
    let c = shipped();
    // Hashed fixtures: every shipped term must fire its rules.
    for sexual in c.lists().sexual.iter() {
        let age = Fixture {
            name: vec![num(0, 15), tok(1, term_hash("yo")), tok(2, sexual)],
            files: vec![],
            content_type: ContentType::Video,
        };
        let rules: Vec<CoreRule> = c.fixture_hits(&age).iter().map(|h| h.rule).collect();
        assert_eq!(rules, vec![CoreRule::Cs2a]);
        for role in c.lists().child_roles.iter() {
            let pair = Fixture {
                name: vec![tok(0, role), tok(3, sexual)],
                files: vec![],
                content_type: ContentType::Video,
            };
            let rules: Vec<CoreRule> = c.fixture_hits(&pair).iter().map(|h| h.rule).collect();
            assert_eq!(rules, vec![CoreRule::Cs4a]);
        }
    }
}

#[test]
fn shipped_precision_corpus() {
    // Legitimate titles prone to false positives (§20.8) stay unblocked.
    let c = shipped();
    let corpus: &[(&str, &[&str], ContentType)] = &[
        (
            "Sex Education S02E03 – The School Trip",
            &["Sex.Education.S02E03.1080p.NF.WEB-DL.mkv"],
            ContentType::Tv,
        ),
        (
            "Teen Titans Go! S05E01",
            &["Teen.Titans.Go.S05E01.mkv"],
            ContentType::Tv,
        ),
        (
            "Halo 3 [PEGI 16]",
            &["setup.exe", "data.bin"],
            ContentType::Game,
        ),
        (
            "Lolita (1962)",
            &["Lolita.1962.1080p.BluRay.mkv"],
            ContentType::Movie,
        ),
        (
            "Lolita - Vladimir Nabokov",
            &["Lolita.epub"],
            ContentType::Ebook,
        ),
        (
            "Coming of Age (documentary)",
            &["Coming.of.Age.2019.mkv"],
            ContentType::Movie,
        ),
        (
            "Sex and the City (2008)",
            &["Sex.and.the.City.2008.mkv"],
            ContentType::Movie,
        ),
        ("Kids (1995)", &["Kids.1995.1080p.mkv"], ContentType::Movie),
        (
            "The Kids Are All Right (2010)",
            &["The.Kids.Are.All.Right.2010.mkv"],
            ContentType::Movie,
        ),
        (
            "Big Mouth S01 Complete",
            &["Big.Mouth.S01E01.mkv", "Big.Mouth.S01E02.mkv"],
            ContentType::Tv,
        ),
        (
            "Somesite.24.01.15.Jane.Doe.18.Years.Old.XXX.1080p",
            &["somesite.24.01.15.jane.doe.18.years.old.xxx.mp4"],
            ContentType::Adult,
        ),
        (
            "Talking to Kids About Sex - A Parenting Course",
            &["01 intro.mp4"],
            ContentType::Video,
        ),
        (
            "Puberty and Sex Education for Parents (Health Class)",
            &["lesson01.mp4", "lesson02.mp4"],
            ContentType::Video,
        ),
    ];
    for (name, paths, t) in corpus {
        let f: Vec<FileInfo> = paths
            .iter()
            .map(|p| FileInfo::new(vec![(*p).to_owned()], 1_000_000))
            .collect();
        let hits = c.hits(&Item {
            hashes: &[H2],
            name,
            files: &f,
            content_type: *t,
        });
        assert!(hits.is_empty(), "{name}: {hits:?}");
    }
}
