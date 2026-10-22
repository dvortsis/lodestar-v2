//! Hostile input (§6 "Hostile-input limits"): any text, any file list.

use std::time::Instant;

use lodestar_classify::{ContentType, FileInfo};
use lodestar_filter::{Core, CsList, Fixture, Item, Lists, Switches, normalize};
use proptest::prelude::*;

fn shipped() -> Core {
    Core::new(
        Lists::shipped().unwrap(),
        CsList::default(),
        Switches::default(),
    )
}

proptest! {
    #[test]
    fn fixtures_round_trip(
        name in "\\PC{0,200}",
        paths in prop::collection::vec("\\PC{0,60}", 0..20),
        ct in 0u8..12,
    ) {
        let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
        let f = Fixture::of(&name, &paths, ContentType::from_code(ct).unwrap());
        let bytes = f.encode();
        prop_assert_eq!(Fixture::decode(&bytes), Some(f));
        // Any truncation or trailing byte is refused, never misread.
        for cut in 0..bytes.len() {
            prop_assert_eq!(Fixture::decode(&bytes[..cut]), None);
        }
        let mut longer = bytes.clone();
        longer.push(0);
        prop_assert_eq!(Fixture::decode(&longer), None);
    }

    #[test]
    fn fixture_decode_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        let _ = Fixture::decode(&bytes);
        let mut v1 = vec![1u8, 0, 0xff, 0xff];
        v1.extend_from_slice(&bytes);
        prop_assert_eq!(Fixture::decode(&v1), None, "file count over the cap");
    }

    #[test]
    fn tokens_are_bounded(text in "\\PC{0,600}") {
        let mut out = Vec::new();
        normalize::tokens(&text, &mut out);
        prop_assert!(out.len() <= normalize::MAX_TOKENS);
        let words = text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).count();
        // Each word adds its parts, its whole form and one alternative.
        prop_assert!(out.iter().all(|t| (t.pos as usize) <= words * 32 + 1));
    }

    #[test]
    fn core_never_panics(
        name in "\\PC{0,300}",
        paths in prop::collection::vec(("\\PC{0,80}", 0u64..u64::MAX), 0..40),
        adult in any::<bool>(),
    ) {
        let core = shipped();
        let files: Vec<FileInfo> = paths
            .into_iter()
            .map(|(p, s)| FileInfo::new(p.split('/').map(str::to_owned).collect(), s))
            .collect();
        let item = Item {
            hashes: &[[0; 20]],
            name: &name,
            files: &files,
            content_type: if adult { ContentType::Adult } else { ContentType::Video },
        };
        let verdict = core.verdict(&item);
        let hits = core.hits(&item);
        prop_assert_eq!(verdict, hits.into_iter().find(|h| h.on));
    }
}

/// 10,000 rules of the kinds people write: keywords, phrases, release
/// groups, keywords narrowed by type or another word, alternatives, and
/// a few structural rules no word narrows. None matches the sample, so
/// every rule is considered.
fn ten_thousand_rules() -> Vec<lodestar_filter::UserRule> {
    let word = |i: usize, salt: &str| format!("{salt}{}q{i}", ["zor", "vel", "mik", "tas"][i % 4]);
    (0..10_000)
        .map(|i| {
            let q = match i % 100 {
                0..=53 => word(i, "k"),
                54..=73 => format!("\"{} {}\"", word(i, "a"), word(i, "b")),
                74..=83 => format!("group:{}", word(i, "g")),
                84..=93 => format!("{} type:movie -{}", word(i, "c"), word(i, "d")),
                94..=98 => format!("{} | {}", word(i, "e"), word(i, "f")),
                _ => format!("size:>{}TB files:>{}", i % 7 + 2, 50_000 + i),
            };
            lodestar_filter::UserRule {
                id: i as u32 + 1,
                query: lodestar_search::parse(&q).unwrap(),
                action: lodestar_filter::Action::Hide,
                enabled: true,
                shadow_until: None,
                unless: None,
            }
        })
        .collect()
}

/// §20.8 and the owner's budget (§1.6.6): under 50 µs per torrent at
/// 10,000 rules. Ignored in debug test runs; CI runs it in release (the
/// `filter budget` job): `cargo test --release -p lodestar-filter --test
/// props -- --ignored budget`.
#[test]
#[ignore]
fn budget() {
    const BUDGET_US: u128 = 50;
    let core = shipped();
    let files: Vec<FileInfo> = (0..30)
        .map(|i| {
            FileInfo::new(
                vec![
                    "Show.Name.S01.1080p.WEB-DL.DDP5.1.H.264-GROUP".into(),
                    format!(
                        "Show.Name.S01E{i:02}.Episode.Title.1080p.WEB-DL.DDP5.1.H.264-GROUP.mkv"
                    ),
                ],
                1_500_000_000 + i,
            )
        })
        .collect();
    let item = Item {
        hashes: &[[7; 20]],
        name: "Show.Name.S01.1080p.WEB-DL.DDP5.1.H.264-GROUP",
        files: &files,
        content_type: ContentType::Tv,
    };
    // Warm up (the first judgment also builds the rule index), then the
    // median of seven batches, so one noisy batch on a shared runner
    // doesn't decide. Each batch's mean is printed for the CI log.
    let time = |what: &str, f: &dyn Fn()| {
        for _ in 0..200 {
            f();
        }
        let mut batches: Vec<_> = (0..7)
            .map(|_| {
                let start = Instant::now();
                for _ in 0..300 {
                    f();
                }
                start.elapsed() / 300
            })
            .collect();
        println!("{what}: batches {batches:?}");
        batches.sort();
        batches[3]
    };
    let per = time("core", &|| {
        assert!(core.verdict(std::hint::black_box(&item)).is_none());
    });
    println!("{per:?} per 30-file torrent (core)");
    assert!(per.as_micros() < BUDGET_US, "core: {per:?}");

    // The whole engine: the core, every pack, and 10,000 of your rules,
    // reading the classification as ingest does.
    let mut engine = lodestar_filter::Engine::new(core);
    engine.packs = lodestar_filter::PackSwitches::all();
    engine.rules = ten_thousand_rules().into();
    let class = lodestar_classify::classify(item.name, item.files);
    let facts = lodestar_filter::Facts {
        class: &class,
        size: 45_000_000_000,
        piece_len: 1 << 22,
        flags: 0,
        discovered_at: 0,
        now: 0,
    };
    let per = time("engine", &|| {
        let v = engine.judge(std::hint::black_box(&item), Some(&facts), 0);
        assert!(v.decision.is_none());
    });
    println!("{per:?} per 30-file torrent (engine, packs on, 10,000 rules)");
    assert!(per.as_micros() < BUDGET_US, "engine: {per:?}");
}
