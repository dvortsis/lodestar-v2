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

/// §20.4: under 50 µs per torrent on the reference machine (§3). Run with
/// `cargo test --release -p lodestar-filter --test props -- --ignored`;
/// `LODESTAR_BUDGET_US` raises the bound on slower machines.
#[test]
#[ignore]
fn budget() {
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
    let budget: u128 = std::env::var("LODESTAR_BUDGET_US")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(50);
    let rounds = 2_000;
    let start = Instant::now();
    for _ in 0..rounds {
        assert!(core.verdict(std::hint::black_box(&item)).is_none());
    }
    let per = start.elapsed() / rounds;
    println!("{per:?} per 30-file torrent (core)");
    assert!(per.as_micros() < budget, "{per:?}");

    // The whole engine with every pack on, reading the classification.
    let mut engine = lodestar_filter::Engine::new(core);
    engine.packs = lodestar_filter::PackSwitches::all();
    let class = lodestar_classify::classify(item.name, item.files);
    let facts = lodestar_filter::Facts {
        class: &class,
        size: 45_000_000_000,
        piece_len: 1 << 22,
        flags: 0,
        discovered_at: 0,
        now: 0,
    };
    let start = Instant::now();
    for _ in 0..rounds {
        let v = engine.judge(std::hint::black_box(&item), Some(&facts), 0);
        assert!(v.decision.is_none());
    }
    let per = start.elapsed() / rounds;
    println!("{per:?} per 30-file torrent (engine, packs on)");
    assert!(per.as_micros() < budget, "{per:?}");
}
