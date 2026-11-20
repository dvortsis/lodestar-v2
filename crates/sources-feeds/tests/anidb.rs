//! AniDB's titles dump and anime record (ENR-anidb): golden answers,
//! caps, hostile input and property tests. Every answer here is written
//! by hand in AniDB's documented shape; ids and titles are made up.

use lodestar_sources_feeds::anidb::{
    ANIME_LIMITS, Aid, AnimeType, ApiError, DUMP_LIMITS, Limits, Stop, TitleKind, XmlLimits, gzip,
    inflate, parse_anime, parse_titles,
};
use proptest::prelude::*;

const DUMP: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<animetitles>
  <anime aid="9001">
    <title type="main" xml:lang="x-jat">Hoshi no Kakera</title>
    <title type="official" xml:lang="en">Star Fragments</title>
    <title type="syn" xml:lang="en">Fragments of Stars</title>
    <title type="short" xml:lang="en">SF</title>
  </anime>
  <anime aid="9002">
    <title type="main" xml:lang="x-jat">Umi &amp; Sora</title>
    <title type="official" xml:lang="ja">&#28023;&#12392;&#31354;</title>
  </anime>
</animetitles>
"#;

const RECORD: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<anime id="9001" restricted="false">
  <type>TV Series</type>
  <episodecount>12</episodecount>
  <startdate>2004-04-01</startdate>
  <enddate>2004-06-24</enddate>
  <titles>
    <title xml:lang="x-jat" type="main">Hoshi no Kakera</title>
    <title xml:lang="en" type="official">Star Fragments</title>
  </titles>
  <creators><name id="1" type="Direction">SENTINEL Person</name></creators>
  <description>SENTINEL description</description>
  <tags><tag id="2"><name>SENTINEL tag</name></tag></tags>
  <episodes><episode id="3"><title xml:lang="en">SENTINEL episode</title></episode></episodes>
</anime>
"#;

#[test]
fn the_dump_reads_ids_and_titles() {
    let (all, page) = parse_titles(DUMP.as_bytes(), &DUMP_LIMITS);
    assert!(page.whole(), "{page:?}");
    assert_eq!(page.kept, 2);
    assert_eq!(all[0].aid, Aid(9001));
    let t: Vec<(TitleKind, &str, &str)> = all[0]
        .titles
        .iter()
        .map(|t| (t.kind, t.lang.as_str(), t.text.as_str()))
        .collect();
    assert_eq!(
        t,
        [
            (TitleKind::Main, "x-jat", "Hoshi no Kakera"),
            (TitleKind::Official, "en", "Star Fragments"),
            (TitleKind::Synonym, "en", "Fragments of Stars"),
            (TitleKind::Short, "en", "SF"),
        ]
    );
    // Entities and character references resolve; nothing else does.
    assert_eq!(all[1].titles[0].text, "Umi & Sora");
    assert_eq!(all[1].titles[1].text, "海と空");
    assert_eq!(all[1].restricted, None);
}

#[test]
fn the_dump_is_read_gzipped_too() {
    let (all, page) = parse_titles(&gzip(DUMP.as_bytes()), &DUMP_LIMITS);
    assert!(page.whole());
    assert_eq!(all.len(), 2);
}

#[test]
fn a_record_reads_only_what_the_plugin_needs() {
    for body in [RECORD.as_bytes().to_vec(), gzip(RECORD.as_bytes())] {
        let (a, page) = parse_anime(&body, &ANIME_LIMITS);
        assert!(page.whole(), "{page:?}");
        let a = a.unwrap();
        assert_eq!(a.aid, Aid(9001));
        assert_eq!(a.restricted, Some(false));
        assert_eq!(a.kind, Some(AnimeType::TvSeries));
        assert_eq!((a.start_year, a.end_year), (Some(2004), Some(2004)));
        assert_eq!(a.titles.len(), 2, "an episode's title isn't the anime's");
        assert!(!format!("{a:?}").contains("SENTINEL"));
    }
}

#[test]
fn restricted_and_types_read_strictly() {
    let rec = |attrs: &str, inner: &str| format!("<anime id=\"5\"{attrs}>{inner}</anime>");
    let (a, p) = parse_anime(
        rec(" restricted=\"true\"", "<type>Movie</type>").as_bytes(),
        &ANIME_LIMITS,
    );
    assert!(p.whole());
    let a = a.unwrap();
    assert_eq!((a.restricted, a.kind), (Some(true), Some(AnimeType::Movie)));
    let (a, p) = parse_anime(
        rec("", "<type>Hologram</type><startdate>?</startdate>").as_bytes(),
        &ANIME_LIMITS,
    );
    assert!(
        p.whole(),
        "an unknown type or date is no value, not a fault"
    );
    let a = a.unwrap();
    assert_eq!(
        (a.restricted, a.kind, a.start_year),
        (None, Some(AnimeType::Unknown), None)
    );
    // A marker that isn't true or false spoils it.
    let (a, p) = parse_anime(rec(" restricted=\"yes\"", "").as_bytes(), &ANIME_LIMITS);
    assert!(a.is_none() && !p.whole() && p.skipped.malformed == 1);
}

#[test]
fn errors_are_recognised_and_their_text_never_kept() {
    let (a, p) = parse_anime(b"<error>Banned</error>", &ANIME_LIMITS);
    assert!(a.is_none() && !p.whole());
    assert_eq!(p.error, Some(ApiError::Banned));
    let (_, p) = parse_anime(
        b"<error code=\"302\">client version missing or invalid</error>",
        &ANIME_LIMITS,
    );
    assert_eq!(p.error, Some(ApiError::Other));
    let (_, p) = parse_anime(&gzip(b"<error>banned</error>"), &ANIME_LIMITS);
    assert_eq!(p.error, Some(ApiError::Banned));
    let (_, p) = parse_anime(b"<error>Banned &unknown;</error>", &ANIME_LIMITS);
    assert_eq!(p.error, Some(ApiError::Other));
}

#[test]
fn repeats_and_bad_shapes_spoil() {
    let cases = [
        // A repeated field.
        "<anime id=\"5\"><type>Movie</type><type>OVA</type></anime>",
        "<anime id=\"5\"><titles/><titles/></anime>",
        // An element inside a field.
        "<anime id=\"5\"><titles><title type=\"main\" xml:lang=\"en\">A<b/></title></titles></anime>",
        // A title without a type, or with a bad language.
        "<anime id=\"5\"><titles><title xml:lang=\"en\">A</title></titles></anime>",
        "<anime id=\"5\"><titles><title type=\"main\" xml:lang=\"e n\">A</title></titles></anime>",
        // An empty title, and an unknown entity.
        "<anime id=\"5\"><titles><title type=\"main\" xml:lang=\"en\"> </title></titles></anime>",
        "<anime id=\"5\"><titles><title type=\"main\" xml:lang=\"en\">&ent;</title></titles></anime>",
    ];
    for c in cases {
        let (a, p) = parse_anime(c.as_bytes(), &ANIME_LIMITS);
        assert!(
            a.is_none() && !p.whole() && p.skipped.malformed == 1,
            "{c}: {p:?}"
        );
    }
    for c in [
        "<anime>x</anime>",
        "<anime id=\"0\"/>",
        "<anime id=\"01\"/>",
        "<anime id=\"x\"/>",
    ] {
        let (a, p) = parse_anime(c.as_bytes(), &ANIME_LIMITS);
        assert!(a.is_none() && p.skipped.bad_id == 1, "{c}");
    }
}

#[test]
fn hostile_documents_stop() {
    let stops = [
        (
            "<!DOCTYPE x [<!ENTITY a \"b\">]><anime id=\"1\"/>",
            Stop::Doctype,
        ),
        ("<anime id=\"1\">", Stop::Malformed),
        ("<animetitles/>", Stop::Malformed),
        ("<anime id=\"1\"/><anime id=\"2\"/>", Stop::Malformed),
        ("<anime id=\"1\"></wrong>", Stop::Malformed),
    ];
    for (doc, stop) in stops {
        let (a, p) = parse_anime(doc.as_bytes(), &ANIME_LIMITS);
        assert_eq!(p.stop, Some(stop), "{doc}");
        assert!(!p.whole() && (a.is_none() || p.stop.is_some()));
    }
    let (_, p) = parse_anime(b"\xff<anime id=\"1\"/>", &ANIME_LIMITS);
    assert_eq!(p.stop, Some(Stop::NotUtf8));
    let (_, p) = parse_titles(b"<anime aid=\"1\"/>", &DUMP_LIMITS);
    assert_eq!(p.stop, Some(Stop::Malformed), "the dump's root only");
    let deep = format!(
        "<anime id=\"1\">{}{}</anime>",
        "<a>".repeat(20),
        "</a>".repeat(20)
    );
    let (_, p) = parse_anime(deep.as_bytes(), &ANIME_LIMITS);
    assert_eq!(p.stop, Some(Stop::Depth));
}

fn tight() -> Limits {
    Limits {
        max_compressed: 4096,
        xml: XmlLimits {
            max_bytes: 4096,
            max_depth: 4,
            max_elements: 50,
            max_items: 3,
            max_attrs: 3,
            max_attr_len: 16,
            max_text: 32,
        },
        max_titles: 2,
        max_all_titles: 4,
    }
}

fn dump_of(n: usize, titles: usize) -> String {
    let mut s = String::from("<animetitles>");
    for i in 1..=n {
        s.push_str(&format!("<anime aid=\"{i}\">"));
        for j in 0..titles {
            s.push_str(&format!(
                "<title type=\"syn\" xml:lang=\"en\">T{i} {j}</title>"
            ));
        }
        s.push_str("</anime>");
    }
    s + "</animetitles>"
}

#[test]
fn every_cap_holds() {
    let l = tight();
    // Titles past the per-anime cap are passed over, and say so.
    let (all, p) = parse_titles(dump_of(1, 3).as_bytes(), &l);
    assert!(p.whole());
    assert!(all[0].truncated && all[0].titles.len() == 2);
    // Anime, titles overall, elements.
    assert_eq!(
        parse_titles(dump_of(4, 1).as_bytes(), &l).1.stop,
        Some(Stop::Items)
    );
    assert_eq!(
        parse_titles(dump_of(3, 2).as_bytes(), &l).1.stop,
        Some(Stop::Titles)
    );
    let many = format!("<animetitles>{}</animetitles>", "<x/>".repeat(60));
    assert_eq!(
        parse_titles(many.as_bytes(), &l).1.stop,
        Some(Stop::Elements)
    );
    // Bytes, compressed and inflated.
    let big = format!("<animetitles>{}</animetitles>", " ".repeat(5000));
    assert_eq!(parse_titles(big.as_bytes(), &l).1.stop, Some(Stop::Bytes));
    assert_eq!(
        parse_titles(&gzip(big.as_bytes()), &l).1.stop,
        Some(Stop::Inflated)
    );
    // Text and attributes over their caps spoil the anime.
    let long = format!(
        "<animetitles><anime aid=\"1\"><title type=\"main\" xml:lang=\"en\">{}</title></anime></animetitles>",
        "a".repeat(40)
    );
    let p = parse_titles(long.as_bytes(), &l).1;
    assert_eq!(p.skipped.over_cap, 1);
    let attrs = "<animetitles><anime aid=\"1\" a=\"1\" b=\"2\" c=\"3\"/></animetitles>";
    let p = parse_titles(attrs.as_bytes(), &l).1;
    assert_eq!(p.skipped.bad_id + p.skipped.over_cap, 1, "{p:?}");
}

#[test]
fn a_gzip_bomb_never_inflates_past_the_cap() {
    let bomb = gzip(&vec![b' '; 64 << 20]);
    assert!(bomb.len() < 1 << 20);
    let l = Limits {
        xml: XmlLimits {
            max_bytes: 1 << 20,
            ..ANIME_LIMITS.xml
        },
        ..ANIME_LIMITS
    };
    assert_eq!(inflate(&bomb, &l), Err(Stop::Inflated));
    assert_eq!(parse_anime(&bomb, &l).1.stop, Some(Stop::Inflated));
    let mut cut = gzip(RECORD.as_bytes());
    cut.truncate(cut.len() / 2);
    assert_eq!(parse_anime(&cut, &ANIME_LIMITS).1.stop, Some(Stop::Gzip));
}

#[test]
fn a_huge_record_of_unread_elements_is_read_whole() {
    // A long series: thousands of episodes, all passed over.
    let mut s =
        String::from("<anime id=\"7\" restricted=\"false\"><type>TV Series</type><episodes>");
    for i in 0..20_000 {
        s.push_str(&format!(
            "<episode id=\"{i}\"><epno>{i}</epno><title xml:lang=\"en\">E</title></episode>"
        ));
    }
    s.push_str("</episodes></anime>");
    let (a, p) = parse_anime(&gzip(s.as_bytes()), &ANIME_LIMITS);
    assert!(p.whole(), "{p:?}");
    assert!(a.unwrap().titles.is_empty());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Any bytes: no panic, and whatever is kept is clean and bounded.
    #[test]
    fn any_bytes_are_safe(body in proptest::collection::vec(any::<u8>(), 0..2048), gz in any::<bool>()) {
        let body = if gz { gzip(&body) } else { body };
        for l in [ANIME_LIMITS, tight()] {
            let (a, p) = parse_anime(&body, &l);
            if let Some(a) = a {
                prop_assert!(a.aid.0 > 0 && a.titles.len() <= l.max_titles);
                prop_assert!(p.kept == 1);
            }
            let (all, p) = parse_titles(&body, &l);
            prop_assert!(all.len() <= l.xml.max_items && p.kept as usize == all.len());
        }
    }

    /// Generated dumps round-trip their ids and titles.
    #[test]
    fn generated_dumps_round_trip(titles in proptest::collection::vec("[A-Za-z0-9 ]{1,20}", 1..20)) {
        let mut s = String::from("<animetitles><anime aid=\"42\">");
        for t in &titles {
            s.push_str(&format!("<title type=\"official\" xml:lang=\"en\">{t}</title>"));
        }
        s.push_str("</anime></animetitles>");
        let (all, p) = parse_titles(&gzip(s.as_bytes()), &DUMP_LIMITS);
        let kept: Vec<&str> = all.iter().flat_map(|a| a.titles.iter().map(|t| t.text.as_str())).collect();
        let want: Vec<&str> = titles.iter().map(|t| t.trim()).filter(|t| !t.is_empty()).collect();
        if want.len() == titles.len() {
            prop_assert!(p.whole());
            prop_assert_eq!(kept, want);
        } else {
            prop_assert!(!p.whole(), "a blank title spoils its anime");
        }
    }
}
