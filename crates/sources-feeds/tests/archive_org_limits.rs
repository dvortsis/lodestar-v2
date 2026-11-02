//! Every cap, malformed JSON, and how the hash sources are chosen.

use lodestar_sources_feeds::archive_org::{
    Feed, LIMITS, Limits, MAX_COLLECTIONS, MAX_PATH, Stop, parse, parse_each,
};

const HASH: &str = "0123456789abcdef0123456789abcdef01234567";
const OTHER: &str = "89abcdef0123456789abcdef0123456789abcdef";

fn doc(id: &str) -> String {
    format!(r#"{{"identifier":"{id}","btih":"{HASH}"}}"#)
}

fn search(docs: &[String]) -> String {
    format!(
        r#"{{"response":{{"numFound":9,"docs":[{}]}}}}"#,
        docs.join(",")
    )
}

fn docs(n: usize) -> Vec<String> {
    (0..n).map(|i| doc(&format!("d{i}"))).collect()
}

/// A JSON escape of `hex`: a backslash, `u` and the four digits.
fn u(hex: &str) -> String {
    format!("{}u{hex}", '\\')
}

fn with(body: &str, limits: &Limits) -> Feed {
    let mut items = Vec::new();
    let page = parse_each(body.as_bytes(), limits, |i| items.push(i));
    Feed { items, page }
}

fn ids(f: &Feed) -> Vec<&str> {
    f.items
        .iter()
        .filter_map(|i| i.identifier.as_deref())
        .collect()
}

/// A metadata answer with these `files` entries.
fn metadata(files: &[String], meta: &str) -> String {
    format!(
        r#"{{"files":[{}],"metadata":{{"identifier":"item"{meta}}}}}"#,
        files.join(",")
    )
}

fn file(name: &str) -> String {
    format!(r#"{{"name":"{name}","size":"1"}}"#)
}

fn torrent(name: &str, btih: &str) -> String {
    format!(r#"{{"name":"{name}","btih":"{btih}"}}"#)
}

#[test]
fn bytes_cap_keeps_finished_items() {
    let body = search(&docs(3));
    let cut = body.find("d2").unwrap();
    let f = with(
        &body,
        &Limits {
            max_bytes: cut,
            ..LIMITS
        },
    );
    assert_eq!(f.page.summary.stop, Some(Stop::Bytes));
    assert_eq!(ids(&f), ["d0", "d1"]);
    // A cut metadata answer has no item: the root never closed.
    let body = metadata(&[file("a"), torrent("item_archive.torrent", HASH)], "");
    let f = with(
        &body,
        &Limits {
            max_bytes: body.len() - 1,
            ..LIMITS
        },
    );
    assert_eq!((f.page.summary.stop, f.items.len()), (Some(Stop::Bytes), 0));
}

#[test]
fn utf8_and_bom() {
    let body = search(&docs(3));
    let mut bytes = body.clone().into_bytes();
    let at = body.find("d2").unwrap();
    bytes[at] = 0xff;
    let mut n = 0;
    let page = parse_each(&bytes, &LIMITS, |_| n += 1);
    assert_eq!((page.summary.stop, n), (Some(Stop::NotUtf8), 2));
    let bom = format!("\u{feff}{body}");
    let f = parse(bom.as_bytes());
    assert_eq!((f.page.summary.stop, f.items.len()), (None, 3));
}

#[test]
fn depth_cap_stops() {
    let deep = |n: usize| {
        format!(
            r#"{{"x":{}{},{}}}"#,
            "[".repeat(n),
            "]".repeat(n),
            r#""items":[]"#
        )
    };
    // The root is depth 1, so 15 arrays inside fit the default 16.
    assert_eq!(parse(deep(15).as_bytes()).page.summary.stop, None);
    assert_eq!(
        parse(deep(16).as_bytes()).page.summary.stop,
        Some(Stop::Depth)
    );
    // Inside a doc, after a finished one: that one is kept.
    let nested = format!(
        r#"{{"items":[{},{{"x":{}}}]}}"#,
        doc("a"),
        "[".repeat(100_000)
    );
    let f = parse(nested.as_bytes());
    assert_eq!(
        (f.page.summary.stop, ids(&f)),
        (Some(Stop::Depth), vec!["a"])
    );
}

#[test]
fn values_cap_stops() {
    let body = search(&docs(3));
    // The root, `response`, `numFound`, `docs` and 3 values a doc.
    let f = with(
        &body,
        &Limits {
            max_values: 4 + 3 * 2,
            ..LIMITS
        },
    );
    assert_eq!(
        (f.page.summary.stop, ids(&f)),
        (Some(Stop::Elements), vec!["d0", "d1"])
    );
    let f = with(
        &body,
        &Limits {
            max_values: 4 + 3 * 3,
            ..LIMITS
        },
    );
    assert_eq!((f.page.summary.stop, f.items.len()), (None, 3));
}

#[test]
fn items_cap_stops() {
    let mut d = docs(2);
    d.insert(1, r#"{"identifier":"no-hash"}"#.into());
    let f = with(
        &search(&d),
        &Limits {
            max_items: 2,
            ..LIMITS
        },
    );
    assert_eq!(f.page.summary.stop, Some(Stop::Items));
    assert_eq!(
        (f.page.summary.kept, f.page.summary.skipped.no_hash),
        (1, 1)
    );
    // A metadata answer is one item.
    let body = metadata(&[torrent("item_archive.torrent", HASH)], "");
    let f = with(
        &body,
        &Limits {
            max_items: 0,
            ..LIMITS
        },
    );
    assert_eq!((f.page.summary.stop, f.items.len()), (Some(Stop::Items), 0));
    let f = with(
        &body,
        &Limits {
            max_items: 1,
            ..LIMITS
        },
    );
    assert_eq!((f.page.summary.stop, f.items.len()), (None, 1));
}

#[test]
fn array_cap_skips_the_item() {
    let limits = Limits {
        max_array: 4,
        ..LIMITS
    };
    let with_subjects = |n: usize, nested: bool| {
        let list = vec!["\"s\""; n].join(",");
        let list = if nested {
            format!(r#"{{"a":[{list}]}}"#)
        } else {
            format!("[{list}]")
        };
        search(&[format!(
            r#"{{"identifier":"x","btih":"{HASH}","subject":{list}}}"#
        )])
    };
    for nested in [false, true] {
        let f = with(&with_subjects(4, nested), &limits);
        assert_eq!(f.page.summary.kept, 1);
        let f = with(&with_subjects(5, nested), &limits);
        assert_eq!(
            (f.page.summary.kept, f.page.summary.skipped.over_cap),
            (0, 1)
        );
    }
    // A field that is read, too.
    let titles = search(&[format!(
        r#"{{"btih":"{HASH}","title":["a","b","c","d","e"]}}"#
    )]);
    assert_eq!(with(&titles, &limits).page.summary.skipped.over_cap, 1);
    // Outside an item it costs only values.
    let outside = format!(r#"{{"x":[1,2,3,4,5,6],"items":[{}]}}"#, doc("a"));
    assert_eq!(with(&outside, &limits).items.len(), 1);
}

#[test]
fn text_cap_skips_the_item() {
    // The hash is 40 bytes, so it fits.
    let limits = Limits {
        max_text: 40,
        ..LIMITS
    };
    let title = |t: &str| search(&[format!(r#"{{"btih":"{HASH}","title":"{t}"}}"#)]);
    assert_eq!(with(&title(&"a".repeat(40)), &limits).page.summary.kept, 1);
    let f = with(&title(&"a".repeat(41)), &limits);
    assert_eq!(
        (f.page.summary.kept, f.page.summary.skipped.over_cap),
        (0, 1)
    );
    // The raw length counts: 7 escapes are 42 bytes for 7 characters.
    let f = with(&title(&u("0041").repeat(7)), &limits);
    assert_eq!(f.page.summary.skipped.over_cap, 1);
    // A field that isn't read is never measured.
    let long = search(&[format!(
        r#"{{"btih":"{HASH}","description":"{}"}}"#,
        "a".repeat(1000)
    )]);
    assert_eq!(with(&long, &limits).page.summary.kept, 1);
}

#[test]
fn file_caps_truncate() {
    let mut files: Vec<String> = (0..5).map(|i| file(&format!("f{i}"))).collect();
    // The torrent entry comes after the cap and is still found.
    files.push(torrent("item_archive.torrent", HASH));
    let body = metadata(&files, "");
    let f = with(
        &body,
        &Limits {
            max_files: 3,
            ..LIMITS
        },
    );
    let it = &f.items[0];
    assert_eq!((it.files.len(), it.files_truncated), (3, true));
    assert_eq!(it.hashes.v1, Some(HASH.parse().unwrap()));
    let f = with(
        &body,
        &Limits {
            max_files: 5,
            ..LIMITS
        },
    );
    assert_eq!(
        (f.items[0].files.len(), f.items[0].files_truncated),
        (5, false)
    );
    // Name bytes: each path is 2 bytes.
    let f = with(
        &body,
        &Limits {
            max_name_bytes: 7,
            ..LIMITS
        },
    );
    assert_eq!(
        (f.items[0].files.len(), f.items[0].files_truncated),
        (3, true)
    );
    // A path over MAX_PATH is left out, not cut.
    let long = metadata(
        &[
            file(&"p".repeat(MAX_PATH + 1)),
            file(&"q".repeat(MAX_PATH)),
            torrent("item_archive.torrent", HASH),
        ],
        "",
    );
    let it = &parse(long.as_bytes()).items[0];
    assert_eq!(it.files.len(), 1);
    assert_eq!(
        (it.files[0].path.len(), it.files_truncated),
        (MAX_PATH, true)
    );
}

#[test]
fn collections_are_cut() {
    let list: Vec<String> = (0..40).map(|i| format!("\"c{i}\"")).collect();
    let body = search(&[format!(
        r#"{{"btih":"{HASH}","collection":[{}]}}"#,
        list.join(",")
    )]);
    let f = parse(body.as_bytes());
    assert_eq!(f.items[0].collections.len(), MAX_COLLECTIONS);
    assert_eq!(f.items[0].collections[0], "c0");
}

#[test]
fn malformed_json_stops() {
    let ok = doc("a");
    for (body, kept) in [
        (format!(r#"{{"items":[{ok},"#), 1),
        (format!(r#"{{"items":[{ok}]}} x"#), 1),
        (format!(r#"{{"items":[{ok},{{"title":"a\qb"}}]}}"#), 1),
        (format!(r#"{{"items":[{ok},{{"size":01}}]}}"#), 1),
        (format!(r#"{{"items":[{ok},{{"x":tru}}]}}"#), 1),
        (format!(r#"{{"items":[{ok},{{"x":1.}}]}}"#), 1),
        (format!(r#"{{"items":[{ok},{{"x":"a{}b"}}]}}"#, '\u{1}'), 1),
        (format!(r#"{{"items":[{ok}],}}"#), 1),
        (format!(r#"{{"items":[{ok},]}}"#), 1),
        (format!(r#"{{'items':[{ok}]}}"#), 0),
        (String::new(), 0),
    ] {
        let f = parse(body.as_bytes());
        assert_eq!(
            (f.page.summary.stop, f.items.len()),
            (Some(Stop::Malformed), kept),
            "{body}"
        );
    }
    // Valid JSON that isn't an answer has no items and no stop.
    for body in ["[]", "null", r#"{"items":7,"response":[]}"#, " 1e5 "] {
        assert_eq!(
            parse(body.as_bytes()).page.summary,
            Default::default(),
            "{body}"
        );
    }
}

#[test]
fn bad_fields_skip_or_drop() {
    // A lone surrogate in a field that is read is malformed; a doc that
    // isn't an object too.
    let body = search(&[
        format!(r#"{{"btih":"{HASH}","title":"\ud800"}}"#),
        "7".into(),
        doc("ok"),
    ]);
    let f = parse(body.as_bytes());
    assert_eq!((f.page.summary.skipped.malformed, ids(&f)), (2, vec!["ok"]));
    // The first value of a field wins; a repeat costs nothing.
    let dup = search(&[format!(
        r#"{{"btih":"{HASH}","identifier":"first","identifier":"second","btih":"bad"}}"#
    )]);
    assert_eq!(ids(&parse(dup.as_bytes())), ["first"]);
    // Keys are compared as written.
    let escaped = search(&[format!(r#"{{"b{}ih":"{HASH}"}}"#, u("0074"))]);
    assert_eq!(parse(escaped.as_bytes()).page.summary.skipped.no_hash, 1);
}

#[test]
fn only_v1_hashes() {
    for bad in [
        "a".repeat(64),
        "0".repeat(40),
        HASH[1..].to_string(),
        format!("{HASH}0"),
        "".into(),
    ] {
        let body = search(&[format!(r#"{{"btih":"{bad}"}}"#)]);
        assert_eq!(
            parse(body.as_bytes()).page.summary.skipped.bad_hash,
            1,
            "{bad}"
        );
    }
    let body = search(&[r#"{"btih":7}"#.into(), r#"{"btih":null}"#.into()]);
    assert_eq!(parse(body.as_bytes()).page.summary.skipped.bad_hash, 2);
    let upper = search(&[format!(r#"{{"btih":"{}"}}"#, HASH.to_uppercase())]);
    assert_eq!(parse(upper.as_bytes()).page.summary.kept, 1);
}

#[test]
fn metadata_hash_sources() {
    let hash_of = |body: String| {
        let f = parse(body.as_bytes());
        (
            f.items.first().and_then(|i| i.hashes.v1),
            f.page.summary.skipped,
        )
    };
    let h = |s: &str| Some(s.parse().unwrap());
    // The first `.torrent` with a `btih` when none is the archive's.
    let (got, _) = hash_of(metadata(
        &[torrent("a.torrent", OTHER), torrent("b.torrent", HASH)],
        "",
    ));
    assert_eq!(got, h(OTHER));
    // The archive's wins; the name compares exactly.
    let (got, _) = hash_of(metadata(
        &[
            torrent("a.torrent", OTHER),
            torrent("item_archive.TORRENT", HASH),
        ],
        "",
    ));
    assert_eq!(got, h(OTHER));
    // A bad archive btih skips the item, even with a good other source.
    let (got, skipped) = hash_of(metadata(
        &[
            torrent("item_archive.torrent", "nope"),
            torrent("b.torrent", HASH),
        ],
        &format!(r#","btih":"{HASH}""#),
    ));
    assert_eq!((got, skipped.bad_hash), (None, 1));
    // `btih` on a file that isn't a `.torrent` is no source, and the file
    // is listed.
    let (got, skipped) = hash_of(metadata(&[torrent("x.mp4", HASH)], ""));
    assert_eq!((got, skipped.no_hash), (None, 1));
    // The `metadata` object's `btih` when no `.torrent` has one; a
    // `.torrent` without a `btih` is just a file.
    let body = metadata(&[file("plain.torrent")], &format!(r#","btih":"{HASH}""#));
    let f = parse(body.as_bytes());
    assert_eq!(f.items[0].hashes.v1, h(HASH));
    assert_eq!(f.items[0].files[0].path, "plain.torrent");
    // A dark item: no files, no hash.
    let (got, skipped) = hash_of(r#"{"is_dark":true,"metadata":{"identifier":"x"}}"#.into());
    assert_eq!((got, skipped.no_hash), (None, 1));
}
