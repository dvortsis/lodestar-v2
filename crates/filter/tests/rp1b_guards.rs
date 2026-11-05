//! RP-1b: MW-2@2's music-production guard and FK-2@2's short-episode
//! guard, measured on the release parser's golden corpus and the pack
//! corpora. The numbers are printed (`--nocapture`) for the PR.
//!
//! - **Lawful names:** every golden-corpus name, with the files a torrent
//!   of its kind carries, is held by neither rule. Version 1 holds none
//!   either, so this shows neither over-filters, not what the guards do.
//! - **Version 1's part is unchanged on real names:** with a lure added
//!   to each one (an installer beside the media for MW-2, each video
//!   shrunk under the 40 MiB tiny-video threshold for FK-2), version 2
//!   holds exactly what version 1 holds. This is by construction: the
//!   corpus gives music names two tracks in the root and video names one
//!   file, so neither guard can apply. The guards themselves are measured
//!   by the pack corpora and the precision items.
//! - **The pack corpora:** each rule's must-match examples are all held
//!   and its must-not-match ones and precision items all pass.

use lodestar_classify::{FileInfo, classify};
use lodestar_filter::corpus::{Corpus, Expect};
use lodestar_filter::packs::manifest::Manifest;
use lodestar_filter::packs::{Ctx, PackRule};
use lodestar_filter::{Facts, Item};

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

/// One golden-corpus name and its hand labels.
struct Golden {
    name: String,
    labels: Vec<String>,
}

impl Golden {
    fn has(&self, key: &str) -> bool {
        self.labels.iter().any(|k| k == key)
    }

    /// Files of the kind the labels describe, as `unauthorised_copy.rs`
    /// picks them: an installer for software, tracks for music, one video
    /// for film and TV, nothing otherwise.
    fn files(&self) -> Vec<FileInfo> {
        let one = |p: String, s: u64| FileInfo::new(vec![p], s);
        if self.has("version") || self.has("platform") || self.has("arch") {
            vec![one("setup.exe".into(), 200 << 20)]
        } else if self.has("artist") || self.has("album") || self.has("bitrate") {
            vec![
                one("01 - Track.flac".into(), 30 << 20),
                one("02 - Track.flac".into(), 30 << 20),
            ]
        } else if self.has("res")
            || self.has("vcodec")
            || self.has("season")
            || self.has("episode")
            || self.has("source")
        {
            vec![one(format!("{}.mkv", self.name), 2 << 30)]
        } else {
            Vec::new()
        }
    }
}

fn golden() -> Vec<Golden> {
    let mut out: Vec<Golden> = Vec::new();
    for (_, text) in CORPUS {
        for line in text.lines() {
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with(' ') {
                if let (Some(g), Some((k, _))) = (out.last_mut(), line.trim().split_once(':')) {
                    g.labels.push(k.trim().to_owned());
                }
            } else {
                out.push(Golden {
                    name: line.to_owned(),
                    labels: Vec::new(),
                });
            }
        }
    }
    out
}

/// Whether version `version` of `rule` holds the name with these files.
fn holds(rule: PackRule, version: u32, name: &str, files: &[FileInfo]) -> bool {
    let m = Manifest::builtin();
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
    let ctx = Ctx::new(&item, &facts, &[]);
    let v = m.entry(rule).retained(version).expect("a kept version");
    v.matches(rule, &ctx) == Some(true)
}

/// The lure each rule exists for, added to a name's own files.
fn lured(rule: PackRule, files: &[FileInfo]) -> Vec<FileInfo> {
    let mut f = files.to_vec();
    match rule {
        PackRule::Mw2 => f.push(FileInfo::new(vec!["setup.exe".into()], 2 << 20)),
        _ => {
            for x in &mut f {
                if x.name().ends_with(".mkv") {
                    x.size = 30 << 20;
                }
            }
        }
    }
    f
}

#[test]
fn precision_on_the_parser_corpus_and_the_pack_corpora() {
    let corpus = golden();
    assert!(corpus.len() > 300, "the corpus reads: {}", corpus.len());
    let report = match Corpus::builtin() {
        Ok(c) => c.run(&Manifest::builtin()),
        Err(e) => panic!("the shipped corpus won't parse: {e}"),
    };
    println!(
        "RP-1b on the release-parser golden corpus ({} names) and the pack corpora:",
        corpus.len()
    );
    for rule in [PackRule::Mw2, PackRule::Fk2] {
        let (mut lawful, mut lured_v1, mut lured_v2) = (Vec::new(), 0, 0);
        for g in &corpus {
            let f = g.files();
            if holds(rule, 2, &g.name, &f) {
                lawful.push(g.name.clone());
            }
            let l = lured(rule, &f);
            let (one, two) = (holds(rule, 1, &g.name, &l), holds(rule, 2, &g.name, &l));
            assert_eq!(one, two, "{}: {:?}", rule.id(), g.name);
            lured_v1 += usize::from(one);
            lured_v2 += usize::from(two);
        }
        let rr = report.rule(rule).expect("reported");
        let ok = |e: Expect| rr.items.iter().filter(|i| i.expect == e && i.pass).count();
        let items: Vec<_> = report
            .precision
            .iter()
            .filter(|p| p.guards.contains(&rule))
            .collect();
        let clean = items
            .iter()
            .filter(|p| !p.held.iter().any(|(r, _)| *r == rule))
            .count();
        println!(
            "  {}@{}: golden names held {} of {} (precision on lawful names {}); \
             with a lure, held {} by @1 and {} by @2; pack corpus match {}/{}, \
             miss {}/{}; precision items passed {}/{}",
            rule.id(),
            rr.version,
            lawful.len(),
            corpus.len(),
            if lawful.is_empty() {
                "100%"
            } else {
                "below 100%"
            },
            lured_v1,
            lured_v2,
            ok(Expect::Match),
            rr.count(Expect::Match),
            ok(Expect::Miss),
            rr.count(Expect::Miss),
            clean,
            items.len()
        );
        assert!(lawful.is_empty(), "{}: {lawful:#?}", rule.id());
        assert!(lured_v2 > 0, "{} holds no lured name", rule.id());
        assert_eq!(ok(Expect::Match), rr.count(Expect::Match));
        assert_eq!(ok(Expect::Miss), rr.count(Expect::Miss));
        assert_eq!(clean, items.len());
    }
}
