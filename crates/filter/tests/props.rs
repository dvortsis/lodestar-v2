//! Hostile input (§6 "Hostile-input limits"): any text, any file list.

use std::time::Instant;

use lodestar_classify::{ContentType, FileInfo};
use lodestar_filter::{Core, CsList, Item, Lists, Switches, normalize};
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

/// §20.4: under 50 µs per torrent. Run with
/// `cargo test --release -p lodestar-filter --test props -- --ignored`.
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
    let rounds = 2_000;
    let start = Instant::now();
    for _ in 0..rounds {
        assert!(core.verdict(std::hint::black_box(&item)).is_none());
    }
    let per = start.elapsed() / rounds;
    println!("{per:?} per 30-file torrent");
    assert!(per.as_micros() < 50, "{per:?}");
}
