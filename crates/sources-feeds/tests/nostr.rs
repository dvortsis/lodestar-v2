//! NIP-01 relay messages and NIP-35 events: every message type, every
//! cap refused, malformed hex, id mismatch, unknown kinds, the claims
//! mapping, golden canonical-serialisation vectors and a property test
//! that nothing panics.

use lodestar_sources_feeds::nostr::{
    self, Event, Field, KIND_TORRENT, LIMITS, Limits, MAX_SUB_ID, Message, Refused, canonical,
    event_id, npub, parse, release,
};
use proptest::prelude::*;

const PK: &str = "7e7e9c42a91bfef19fa929e5fda1b72e0ebc1a4c1141673e2794234d86addf4e";
const SIG: &str = "0000000000000000000000000000000000000000000000000000000000000000\
                   0000000000000000000000000000000000000000000000000000000000000000";

fn pk() -> [u8; 32] {
    let mut k = [0u8; 32];
    for (i, b) in k.iter_mut().enumerate() {
        *b = u8::from_str_radix(&PK[2 * i..2 * i + 2], 16).unwrap();
    }
    k
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A JSON string literal for `s`, escaped the way most clients write it.
fn js(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            '\u{8}' => o.push_str("\\b"),
            '\u{c}' => o.push_str("\\f"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn tags_json(tags: &[Vec<String>]) -> String {
    let t: Vec<String> = tags
        .iter()
        .map(|t| {
            format!(
                "[{}]",
                t.iter().map(|s| js(s)).collect::<Vec<_>>().join(",")
            )
        })
        .collect();
    format!("[{}]", t.join(","))
}

fn strs(t: &[&[&str]]) -> Vec<Vec<String>> {
    t.iter()
        .map(|t| t.iter().map(|s| s.to_string()).collect())
        .collect()
}

/// An `EVENT` message with a correct id.
fn event_msg(kind: u16, tags: &[&[&str]], content: &str) -> String {
    let tags = strs(tags);
    let id = event_id(&pk(), 1_700_000_000, kind, &tags, content);
    format!(
        r#"["EVENT","sub1",{{"id":"{}","pubkey":"{PK}","created_at":1700000000,"kind":{kind},"tags":{},"content":{},"sig":"{SIG}"}}]"#,
        hex(&id),
        tags_json(&tags),
        js(content)
    )
}

fn event_of(msg: &str) -> Event {
    match parse(msg.as_bytes(), &LIMITS) {
        Ok(Message::Event { sub, event }) => {
            assert_eq!(sub, "sub1");
            *event
        }
        other => panic!("{other:?}"),
    }
}

fn refused(msg: &str) -> Refused {
    parse(msg.as_bytes(), &LIMITS).expect_err(msg)
}

// ── golden vectors, computed independently (Python's json.dumps with
// separators=(',', ':') and ensure_ascii=False, which agrees with NIP-01
// for these characters) ──

/// created_at, kind, tags, content, the canonical form, the id.
type Vector = (
    u64,
    u16,
    Vec<Vec<String>>,
    &'static str,
    &'static str,
    &'static str,
);

#[test]
fn canonical_golden_vectors() {
    let cases: [Vector; 4] = [
        (
            1_700_000_000,
            1,
            vec![],
            "hello",
            r#"[0,"7e7e9c42a91bfef19fa929e5fda1b72e0ebc1a4c1141673e2794234d86addf4e",1700000000,1,[],"hello"]"#,
            "eeaf6019d3539958822e623fa80fea5459003a6d9bfbf79014b01fd9bb11d46b",
        ),
        (
            1_700_000_001,
            1,
            strs(&[&["t", "a\"b"]]),
            "line\nbreak \"q\" back\\slash\ttab\rcr\u{8}bs\u{c}feed / slash",
            r#"[0,"7e7e9c42a91bfef19fa929e5fda1b72e0ebc1a4c1141673e2794234d86addf4e",1700000001,1,[["t","a\"b"]],"line\nbreak \"q\" back\\slash\ttab\rcr\bbs\ffeed / slash"]"#,
            "7f27535fcf40c6bd327d851c7d46116dbe7aa502310b59dd95f283eb5c2c58de",
        ),
        (
            1_700_000_002,
            2003,
            strs(&[
                &["title", "Ubuntu 24.04 ✓"],
                &["x", "0123456789abcdef0123456789abcdef01234567"],
                &["file", "ubuntu.iso", "6114656256"],
                &["i", "imdb:tt0111161"],
                &["i", "tcat:video,movie"],
                &["t", "Linux"],
            ]),
            "émoji 🎉 \u{2028} u2028",
            "[0,\"7e7e9c42a91bfef19fa929e5fda1b72e0ebc1a4c1141673e2794234d86addf4e\",1700000002,2003,[[\"title\",\"Ubuntu 24.04 ✓\"],[\"x\",\"0123456789abcdef0123456789abcdef01234567\"],[\"file\",\"ubuntu.iso\",\"6114656256\"],[\"i\",\"imdb:tt0111161\"],[\"i\",\"tcat:video,movie\"],[\"t\",\"Linux\"]],\"émoji 🎉 \u{2028} u2028\"]",
            "bd7b912f2140c682472b269e6719ec152ad5dca2794f13c2ac844e792e568765",
        ),
        (
            0,
            0,
            strs(&[&["e"]]),
            "",
            r#"[0,"7e7e9c42a91bfef19fa929e5fda1b72e0ebc1a4c1141673e2794234d86addf4e",0,0,[["e"]],""]"#,
            "8b9b1608c99842e01322f8b6b0cd6f88cbb168d6846a4bbeb47e0cf0f6db5b62",
        ),
    ];
    for (at, kind, tags, content, want, id) in cases {
        assert_eq!(canonical(&pk(), at, kind, &tags, content), want);
        assert_eq!(hex(&event_id(&pk(), at, kind, &tags, content)), id);
    }
}

#[test]
fn other_control_characters_are_verbatim() {
    // NIP-01 lists the seven escapes; every other character is verbatim.
    let c = canonical(&pk(), 1, 1, &[], "a\u{1}b\u{7f}");
    assert!(c.ends_with(",[],\"a\u{1}b\u{7f}\"]"));
}

// ── messages ──

#[test]
fn event_round_trips_with_escapes_decoded() {
    let m = event_msg(
        1,
        &[&["t", "q\"x"], &["p", "a\\b"]],
        "hi\n\"there\" \u{1F389}",
    );
    let e = event_of(&m);
    assert_eq!(e.kind, 1);
    assert_eq!(e.created_at, 1_700_000_000);
    assert_eq!(e.pubkey, pk());
    assert_eq!(e.content, "hi\n\"there\" \u{1F389}");
    assert_eq!(e.tags, strs(&[&["t", "q\"x"], &["p", "a\\b"]]));
}

#[test]
fn unicode_escapes_in_the_wire_form_still_match() {
    // A client may write `é` or `\/`; the id covers the decoded text.
    let tags: Vec<Vec<String>> = vec![];
    let id = event_id(&pk(), 5, 1, &tags, "é/");
    let m = format!(
        r#"["EVENT","s",{{"id":"{}","pubkey":"{PK}","created_at":5,"kind":1,"tags":[],"content":"é\/","sig":"{SIG}"}}]"#,
        hex(&id)
    );
    assert!(matches!(
        parse(m.as_bytes(), &LIMITS),
        Ok(Message::Event { .. })
    ));
}

#[test]
fn whitespace_and_field_order_do_not_matter() {
    let tags: Vec<Vec<String>> = vec![];
    let id = hex(&event_id(&pk(), 5, 1, &tags, "x"));
    let m = format!(
        "[ \"EVENT\" , \"s\" ,\n {{ \"sig\":\"{SIG}\", \"content\":\"x\", \"tags\":[ ], \"kind\":1, \"created_at\":5, \"pubkey\":\"{PK}\", \"id\":\"{id}\", \"extra\":{{\"a\":[1]}} }} ]"
    );
    assert!(matches!(
        parse(m.as_bytes(), &LIMITS),
        Ok(Message::Event { .. })
    ));
}

#[test]
fn id_mismatch_is_refused() {
    let m = event_msg(1, &[], "hello");
    let forged = m.replace("\"hello\"", "\"hellp\"");
    assert_eq!(refused(&forged), Refused::IdMismatch);
}

#[test]
fn malformed_hex_is_refused() {
    let m = event_msg(1, &[], "hello");
    let upper = m.replace(PK, &PK.to_uppercase());
    assert_eq!(refused(&upper), Refused::Event(Field::Pubkey));
    let short = m.replace(PK, &PK[..62]);
    assert_eq!(refused(&short), Refused::Event(Field::Pubkey));
    let sig = m.replace(SIG, &SIG[..126]);
    assert_eq!(refused(&sig), Refused::Event(Field::Sig));
    let sig = m.replace(SIG, &format!("{}g", &SIG[..127]));
    assert_eq!(refused(&sig), Refused::Event(Field::Sig));
    let id_at = m.find("\"id\":\"").unwrap() + 6;
    let mut bad = m.clone();
    bad.replace_range(id_at..id_at + 1, "Z");
    assert_eq!(refused(&bad), Refused::Event(Field::Id));
}

#[test]
fn bad_numbers_are_refused() {
    let m = event_msg(1, &[], "hello");
    for (from, to, f) in [
        (
            "\"created_at\":1700000000",
            "\"created_at\":-1",
            Field::CreatedAt,
        ),
        (
            "\"created_at\":1700000000",
            "\"created_at\":1.5",
            Field::CreatedAt,
        ),
        (
            "\"created_at\":1700000000",
            "\"created_at\":1e9",
            Field::CreatedAt,
        ),
        (
            "\"created_at\":1700000000",
            "\"created_at\":\"1\"",
            Field::CreatedAt,
        ),
        (
            "\"created_at\":1700000000",
            "\"created_at\":18446744073709551616",
            Field::CreatedAt,
        ),
        ("\"kind\":1", "\"kind\":65536", Field::Kind),
        ("\"content\":\"hello\"", "\"content\":5", Field::Content),
    ] {
        assert_eq!(refused(&m.replacen(from, to, 1)), Refused::Event(f), "{to}");
    }
}

#[test]
fn missing_and_repeated_fields_are_refused() {
    let m = event_msg(1, &[], "hello");
    let no_sig = m.replace(&format!(",\"sig\":\"{SIG}\""), "");
    assert_eq!(refused(&no_sig), Refused::Event(Field::Sig));
    let twice = m.replace("\"kind\":1,", "\"kind\":1,\"kind\":1,");
    assert_eq!(refused(&twice), Refused::Event(Field::Kind));
}

#[test]
fn tag_shapes_are_checked() {
    let m = event_msg(1, &[&["t", "a"]], "x");
    assert_eq!(
        refused(&m.replace("[[\"t\",\"a\"]]", "[[\"t\",1]]")),
        Refused::Event(Field::Tags)
    );
    assert_eq!(
        refused(&m.replace("[[\"t\",\"a\"]]", "[\"t\"]")),
        Refused::Event(Field::Tags)
    );
    assert_eq!(
        refused(&m.replace("[[\"t\",\"a\"]]", "[[]]")),
        Refused::Tags
    );
    assert_eq!(
        refused(&m.replace("\"tags\":[[\"t\",\"a\"]]", "\"tags\":{}")),
        Refused::Event(Field::Tags)
    );
}

#[test]
fn every_cap_is_refused() {
    // Bytes.
    let big = event_msg(1, &[], &"a".repeat(LIMITS.max_bytes));
    assert_eq!(refused(&big), Refused::TooLarge);
    // Tags: the cap itself passes, one more is refused.
    let t: Vec<&[&str]> = vec![&["t", "a"]; LIMITS.max_tags];
    assert!(parse(event_msg(1, &t, "").as_bytes(), &LIMITS).is_ok());
    let t: Vec<&[&str]> = vec![&["t", "a"]; LIMITS.max_tags + 1];
    assert_eq!(refused(&event_msg(1, &t, "")), Refused::Tags);
    // Strings in a tag.
    let long: Vec<&str> = vec!["a"; LIMITS.max_tag_items];
    assert!(parse(event_msg(1, &[&long], "").as_bytes(), &LIMITS).is_ok());
    let long: Vec<&str> = vec!["a"; LIMITS.max_tag_items + 1];
    assert_eq!(refused(&event_msg(1, &[&long], "")), Refused::Tags);
    // Depth: nothing nests deeper than an event's tags.
    let deep = event_msg(1, &[], "").replace("\"tags\":[]", "\"tags\":[[[\"a\"]]]");
    assert_eq!(refused(&deep), Refused::Json(nostr::Stop::Depth));
    // Subscription id.
    let sub = format!("[\"EOSE\",\"{}\"]", "s".repeat(MAX_SUB_ID + 1));
    assert_eq!(refused(&sub), Refused::Shape);
    // Tighter caps hold the same way.
    let tight = Limits {
        max_bytes: 10,
        ..LIMITS
    };
    assert_eq!(
        parse(b"[\"NOTICE\",\"hello\"]", &tight),
        Err(Refused::TooLarge)
    );
}

#[test]
fn other_messages() {
    let ok = format!("[\"OK\",\"{}\",true,\"\"]", "ab".repeat(32));
    assert_eq!(
        parse(ok.as_bytes(), &LIMITS),
        Ok(Message::Ok {
            id: [0xab; 32],
            accepted: true,
            reason: String::new()
        })
    );
    let no = format!("[\"OK\",\"{}\",false,\"blocked: no\"]", "ab".repeat(32));
    assert!(matches!(
        parse(no.as_bytes(), &LIMITS),
        Ok(Message::Ok { accepted: false, ref reason, .. }) if reason == "blocked: no"
    ));
    assert_eq!(
        parse(br#"["EOSE","sub1"]"#, &LIMITS),
        Ok(Message::Eose { sub: "sub1".into() })
    );
    assert_eq!(
        parse(br#"["CLOSED","sub1","auth-required: x"]"#, &LIMITS),
        Ok(Message::Closed {
            sub: "sub1".into(),
            reason: "auth-required: x".into()
        })
    );
    assert_eq!(
        parse("[\"NOTICE\",\"a\\u0000b\u{202e}c\"]".as_bytes(), &LIMITS),
        Ok(Message::Notice { text: "abc".into() })
    );
    let long = format!("[\"NOTICE\",\"{}\"]", "é".repeat(400));
    match parse(long.as_bytes(), &LIMITS) {
        Ok(Message::Notice { text }) => assert!(text.len() <= nostr::MAX_REASON),
        other => panic!("{other:?}"),
    }
}

#[test]
fn auth_unknown_and_bad_layouts_are_refused() {
    assert_eq!(refused(r#"["AUTH","challenge"]"#), Refused::Auth);
    assert_eq!(refused(r#"["COUNT","s",{"count":1}]"#), Refused::Unknown);
    assert_eq!(refused(r#"["eose","s"]"#), Refused::Unknown);
    for bad in [
        "[]",
        "{}",
        "[1]",
        r#"["EOSE"]"#,
        r#"["EOSE","s","extra"]"#,
        r#"["EOSE",""]"#,
        "[\"EOSE\",\"a\\u0000\"]",
        r#"["NOTICE",1]"#,
        r#"["EVENT","s"]"#,
        r#"["EVENT","s",[]]"#,
        r#"["OK","ab",true,""]"#,
        r#"["CLOSED","s"]"#,
    ] {
        assert!(
            matches!(refused(bad), Refused::Shape | Refused::Json(_)),
            "{bad}"
        );
    }
    let ok = format!("[\"OK\",\"{}\",1,\"\"]", "ab".repeat(32));
    assert_eq!(refused(&ok), Refused::Shape);
    assert!(matches!(refused(r#"["EOSE","s"] x"#), Refused::Json(_)));
    assert!(matches!(refused(r#"["EOSE","s",]"#), Refused::Json(_)));
    assert_eq!(parse(b"\xff", &LIMITS), Err(Refused::NotUtf8));
}

// ── NIP-35 claims ──

const HASH: &str = "0123456789abcdef0123456789abcdef01234567";

fn release_of(tags: &[&[&str]], content: &str) -> Option<nostr::Release> {
    release(&event_of(&event_msg(KIND_TORRENT, tags, content)))
}

#[test]
fn torrent_event_maps_to_claims() {
    let r = release_of(
        &[
            &["title", "  Ubuntu 24.04\u{200b} Desktop "],
            &["x", HASH],
            &["file", "ubuntu.iso", "6114656256"],
            &["file", "notes\u{202e}.txt"],
            &["file", ""],
            &["tracker", "udp://tracker.example:1337/announce"],
            &["tracker", "udp://tracker.example:1337/announce"],
            &["tracker", "javascript:alert(1)"],
            &["tracker", "http://a b"],
            &["i", "imdb:tt0111161"],
            &["i", "tmdb:movie:693134"],
            &["i", "MAL:anime:9253"],
            &["i", "tcat:video,movie,4k"],
            &["i", "newznab:2045"],
            &["i", "nocolon"],
            &["t", "Linux"],
            &["t", "linux"],
            &["unknown", "x"],
        ],
        "A description\nover two lines.\u{0}",
    )
    .unwrap();
    assert_eq!(r.hash.to_string(), HASH);
    assert_eq!(r.name.as_deref(), Some("Ubuntu 24.04 Desktop"));
    assert_eq!(r.files.len(), 2);
    assert_eq!(r.files[0].path, "ubuntu.iso");
    assert_eq!(r.files[0].size, Some(6_114_656_256));
    assert_eq!(r.files[1].path, "notes.txt");
    assert_eq!(r.files[1].size, None);
    assert_eq!(r.trackers, vec!["udp://tracker.example:1337/announce"]);
    let ids: Vec<(&str, &str)> = r
        .ids
        .iter()
        .map(|i| (i.scheme.as_str(), i.value.as_str()))
        .collect();
    assert_eq!(
        ids,
        vec![
            ("imdb", "tt0111161"),
            ("tmdb", "movie:693134"),
            ("mal", "anime:9253")
        ]
    );
    assert_eq!(
        r.tags,
        vec!["tcat:video,movie,4k", "newznab:2045", "Linux", "linux"]
    );
    assert_eq!(
        r.description.as_deref(),
        Some("A description\nover two lines.")
    );
    assert_eq!(r.publisher, npub(&pk()));
    assert!(r.publisher.starts_with("npub1"));
    assert!(!r.adult);
}

#[test]
fn the_key_comes_only_from_x() {
    // No x: no release, even with a magnet in the content.
    assert!(release_of(&[&["title", "a"]], &format!("magnet:?xt=urn:btih:{HASH}")).is_none());
    // Malformed, zero, v2-length and upper-case-ok.
    assert!(release_of(&[&["x", "zz"]], "").is_none());
    assert!(release_of(&[&["x", &"0".repeat(40)]], "").is_none());
    assert!(release_of(&[&["x", &"a".repeat(64)]], "").is_none());
    // Two that disagree: ambiguous, dropped. The same twice is fine.
    let other = "1123456789abcdef0123456789abcdef01234567";
    assert!(release_of(&[&["x", HASH], &["x", other]], "").is_none());
    assert!(release_of(&[&["x", HASH], &["x", HASH]], "").is_some());
}

#[test]
fn unknown_kinds_are_ignored() {
    let e = event_of(&event_msg(1, &[&["x", HASH]], "note"));
    assert!(release(&e).is_none());
}

#[test]
fn adult_tags_and_warnings() {
    for t in ["nsfw", "NSFW", "porn", "xxx", "adult", " ns\u{200b}fw "] {
        assert!(
            release_of(&[&["x", HASH], &["t", t]], "").unwrap().adult,
            "{t}"
        );
    }
    assert!(
        release_of(&[&["x", HASH], &["content-warning", "Adult content"]], "")
            .unwrap()
            .adult
    );
    for w in ["spoilers", "violence", "adulthood themes"] {
        assert!(
            !release_of(&[&["x", HASH], &["content-warning", w]], "")
                .unwrap()
                .adult,
            "{w}"
        );
    }
    assert!(
        !release_of(&[&["x", HASH], &["t", "adults-only-not"]], "")
            .unwrap()
            .adult
    );
}

#[test]
fn kept_fields_stay_within_the_claims_limits() {
    use lodestar_sources::claims;
    assert_eq!(nostr::MAX_NAME, claims::MAX_NAME);
    assert_eq!(nostr::MAX_FILES, claims::MAX_FILES);
    assert_eq!(nostr::MAX_PATH, claims::MAX_PATH);
    assert_eq!(nostr::MAX_TRACKERS, claims::MAX_TRACKERS);
    assert_eq!(nostr::MAX_TRACKER, claims::MAX_TRACKER);
    assert_eq!(nostr::MAX_IDS, claims::MAX_IDS);
    assert_eq!(nostr::MAX_ID_PART, claims::MAX_ID_PART);
    assert_eq!(nostr::MAX_DESCRIPTION, claims::MAX_DESCRIPTION);
    assert!(npub(&pk()).len() <= claims::MAX_PUBLISHER);

    let long = "n".repeat(5000);
    let tag = "t".repeat(nostr::MAX_TAG + 1);
    let r = release_of(
        &[
            &["x", HASH],
            &["title", &long],
            &["t", &tag],
            &["i", &format!("imdb:{long}")],
            &["file", &"p".repeat(nostr::MAX_PATH + 1)],
        ],
        "",
    )
    .unwrap();
    assert_eq!(r.name.as_ref().map(String::len), Some(nostr::MAX_NAME));
    assert!(r.tags.is_empty() && r.ids.is_empty() && r.files.is_empty());
}

// ── nothing panics ──

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn arbitrary_bytes_never_panic(b in proptest::collection::vec(any::<u8>(), 0..512)) {
        if let Ok(Message::Event { event, .. }) = parse(&b, &LIMITS) {
            let _ = release(&event);
        }
    }

    #[test]
    fn mutated_events_never_panic(
        content in ".{0,64}",
        at in any::<u64>(),
        cut in 0usize..600,
        flip in 0usize..600,
    ) {
        let m = event_msg(KIND_TORRENT, &[&["x", HASH], &["title", &content]], &content);
        let m = m.replace("1700000000", &at.to_string());
        let mut b = m.into_bytes();
        if let Some(x) = b.get_mut(flip) {
            *x ^= 0x20;
        }
        b.truncate(cut.max(1));
        if let Ok(Message::Event { event, .. }) = parse(&b, &LIMITS) {
            prop_assert_eq!(
                event_id(&event.pubkey, event.created_at, event.kind, &event.tags, &event.content),
                event.id
            );
            let _ = release(&event);
        }
    }

    #[test]
    fn valid_events_round_trip(content in "\\PC{0,64}", t in "\\PC{0,32}") {
        let e = event_of(&event_msg(1, &[&["t", &t]], &content));
        prop_assert_eq!(e.content, content);
        prop_assert_eq!(&e.tags[0][1], &t);
    }
}
