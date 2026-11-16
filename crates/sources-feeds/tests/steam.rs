//! The Steam app-list parser (ENR-steam): the shape it reads, its caps,
//! and that nothing it's handed (garbage, cut-short, oversize or mutated
//! pages) panics or keeps more than the caps allow. The corpus is the
//! fuzz target's seeds, `fuzz/seeds/steam/`.

use lodestar_sources_feeds::steam::{LIMITS, Limits, MAX_NAME, Page, Stop, parse_page};
use proptest::prelude::*;

const PAGE: &[u8] = include_bytes!("../../../fuzz/seeds/steam/page.json");
const LAST: &[u8] = include_bytes!("../../../fuzz/seeds/steam/last.json");

fn bounded(p: &Page, l: &Limits) {
    assert!(p.apps.len() <= l.max_items);
    for a in &p.apps {
        assert!(a.id > 0);
        assert!(!a.name.is_empty() && a.name.len() <= MAX_NAME);
        assert!(a.name.trim() == a.name && !a.name.chars().any(char::is_control));
    }
}

#[test]
fn golden_pages() {
    let p = parse_page(PAGE, &LIMITS);
    assert!(p.whole() && p.more);
    assert_eq!(p.last_appid, Some(400));
    assert_eq!(p.apps.len(), 4);
    assert_eq!(p.apps[1].name, "Sample Saga: Part Two");
    let p = parse_page(LAST, &LIMITS);
    assert!(p.whole() && !p.more && p.apps.len() == 1);
}

#[test]
fn faults_stop_the_page() {
    let cut = parse_page(&PAGE[..PAGE.len() / 2], &LIMITS);
    assert_eq!(cut.stop, Some(Stop::Malformed));
    assert!(!cut.whole());
    let small = parse_page(
        PAGE,
        &Limits {
            max_bytes: 40,
            ..LIMITS
        },
    );
    assert_eq!(small.stop, Some(Stop::Bytes));
    assert!(!parse_page(b"[]", &LIMITS).whole());
    assert!(!parse_page(br#"{"other":1}"#, &LIMITS).whole());
    let deep = format!("{{\"response\":{}{}}}", "[".repeat(50), "]".repeat(50));
    assert_eq!(parse_page(deep.as_bytes(), &LIMITS).stop, Some(Stop::Depth));
}

#[test]
fn bad_apps_are_skipped() {
    let long = "x".repeat(MAX_NAME + 1);
    let b = format!(
        r#"{{"response":{{"apps":[{{"appid":1,"name":"{long}"}},{{"appid":2,"name":"a\u0007b"}},{{"appid":99999999999,"name":"big"}},{{"name":"no id"}},7,{{"appid":3,"name":" Fine "}}]}}}}"#
    );
    let p = parse_page(b.as_bytes(), &LIMITS);
    assert!(p.whole());
    assert_eq!(p.apps.len(), 1);
    assert_eq!(p.apps[0].name, "Fine");
    assert_eq!(p.skipped.malformed, 5);
}

proptest! {
    #[test]
    fn arbitrary_bytes_never_panic(body in proptest::collection::vec(any::<u8>(), 0..2048)) {
        bounded(&parse_page(&body, &LIMITS), &LIMITS);
    }

    #[test]
    fn mutated_seeds_stay_bounded(
        at in any::<prop::sample::Index>(),
        byte in any::<u8>(),
        cut in any::<prop::sample::Index>(),
        items in 0usize..4,
        max_values in 1usize..64,
    ) {
        let mut b = PAGE.to_vec();
        let i = at.index(b.len());
        b[i] = byte;
        b.truncate(cut.index(b.len()) + 1);
        let l = Limits { max_items: items, max_values, max_depth: 4, ..LIMITS };
        bounded(&parse_page(&b, &l), &l);
    }
}
