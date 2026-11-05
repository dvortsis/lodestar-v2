//! `Page::skipped_newest`: the newest date among skipped items.

use lodestar_sources_feeds::archive_org::{LIMITS, parse_each};

const HASH: &str = "0123456789abcdef0123456789abcdef01234567";

fn skipped_newest(docs: &[&str]) -> (Option<i64>, u32) {
    let body = format!(r#"{{"response":{{"docs":[{}]}}}}"#, docs.join(","));
    let mut kept = 0;
    let page = parse_each(body.as_bytes(), &LIMITS, |_| kept += 1);
    (page.skipped_newest, kept)
}

#[test]
fn the_newest_date_of_several_skipped_items_wins() {
    let (t, kept) = skipped_newest(&[
        r#"{"identifier":"a","publicdate":"2026-11-03T09:00:00Z"}"#,
        r#"{"identifier":"b","publicdate":"2026-11-03T10:00:00Z"}"#,
        r#"{"identifier":"c","publicdate":"2026-11-03T08:00:00Z"}"#,
    ]);
    assert_eq!((t, kept), (Some(1_793_700_000), 0));
}

#[test]
fn addeddate_stands_in_for_a_missing_publicdate() {
    let (t, _) = skipped_newest(&[r#"{"identifier":"a","addeddate":"2026-11-03T09:00:00Z"}"#]);
    assert_eq!(t, Some(1_793_696_400));
}

#[test]
fn a_bad_hash_is_skipped_with_its_date() {
    let (t, kept) =
        skipped_newest(&[r#"{"identifier":"a","btih":"zz","publicdate":"2026-11-03T09:00:00Z"}"#]);
    assert_eq!((t, kept), (Some(1_793_696_400), 0));
}

#[test]
fn kept_items_and_undated_skips_do_not_count() {
    let kept_doc =
        format!(r#"{{"identifier":"k","btih":"{HASH}","publicdate":"2026-11-03T11:00:00Z"}}"#);
    let (t, kept) = skipped_newest(&[&kept_doc, r#"{"identifier":"u"}"#]);
    assert_eq!((t, kept), (None, 1));
}
