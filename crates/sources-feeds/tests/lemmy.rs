//! Lemmy API v3 post lists: synthetic answers in the shape
//! `GET /api/v3/post/list` returns (no real post is copied), the caps,
//! and property tests: never panics, never exceeds a cap, uses a post
//! whole or not at all. The last test runs the fuzz target's checks over
//! a long mutated corpus, for machines without cargo-fuzz.

use lodestar_metainfo::{InfoHash, InfoHashV2, hidden_char};
use lodestar_sources_feeds::lemmy::{
    Feed, LIMITS, Limits, MAX_CURSOR, MAX_ERROR, MAX_NAME, MAX_REF, MAX_TITLE, MAX_TORRENTS,
    Skipped, Stop, parse, parse_each,
};
use proptest::prelude::*;

const A: &str = "0123456789abcdef0123456789abcdef01234567";
const B: &str = "89abcdef0123456789abcdef0123456789abcdef";
const V2: &str = "1220aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn v1(hex: &str) -> Option<InfoHash> {
    Some(hex.parse().unwrap())
}

/// One `PostView`, with the fields Lemmy 0.19 sends and a few it adds.
fn view(id: u64, post: &str, community: &str) -> String {
    format!(
        r#"{{"post":{{"id":{id},"name":"Post {id}","nsfw":false,"removed":false,"deleted":false,
"published":"2026-11-01T12:00:00.123456Z","ap_id":"https://lemmy.example/post/{id}",
"locked":false,"language_id":0,"featured_local":false{post}}},
"creator":{{"id":7,"name":"uploader","actor_id":"https://lemmy.example/u/uploader","banned":false}},
"community":{{"id":3,"name":"linux","title":"Linux ISOs","nsfw":false,"removed":false,
"deleted":false,"actor_id":"https://lemmy.example/c/linux"{community}}},
"counts":{{"score":5,"comments":2}},"subscribed":"NotSubscribed","saved":false,"my_vote":null}}"#
    )
}

fn page(views: &[String]) -> String {
    format!(r#"{{"posts":[{}],"next_page":"Pa46c"}}"#, views.join(","))
}

fn with(body: &[u8], limits: &Limits) -> Feed {
    let mut posts = Vec::new();
    let page = parse_each(body, limits, |p| posts.push(p));
    Feed { posts, page }
}

#[test]
fn post_list() {
    let views = [
        view(
            10,
            &format!(r#","url":"magnet:?xt=urn:btih:{A}&dn=Debian+13""#),
            "",
        ),
        view(
            11,
            &format!(
                r#","url":null,"body":"Two:\n- [x](magnet:?xt=urn:btih:{B}&dn=B)\n- magnet:?xt=urn:btmh:{V2}.\n- again magnet:?xt=urn:btih:{B}""#
            ),
            "",
        ),
        // A link to a `.torrent` file gives no key (plan Q-r12).
        view(12, r#","url":"https://example.org/a.torrent""#, ""),
        view(13, r#","url":"magnet:?xt=urn:btih:nothex""#, ""),
        view(14, &format!(r#","url":"magnet:?xt=urn:btih:{A}""#), "").replacen(
            r#""removed":false"#,
            r#""removed":true"#,
            1,
        ),
        view(15, &format!(r#","body":"magnet:?xt=urn:btih:{A}""#), "").replacen(
            r#""deleted":false"#,
            r#""deleted":true"#,
            1,
        ),
    ];
    let f = parse(page(&views).as_bytes());
    let p = &f.page;
    assert_eq!(p.summary.stop, None);
    assert!(p.recognised);
    assert_eq!(p.next_page.as_deref(), Some("Pa46c"));
    assert_eq!(p.newest_id, Some(15));
    assert_eq!(p.removed, 2);
    assert_eq!(p.error, None);
    assert_eq!(p.summary.kept, 2);
    assert_eq!(
        p.summary.skipped,
        Skipped {
            no_hash: 1,
            bad_hash: 1,
            ..Skipped::default()
        }
    );

    let a = &f.posts[0];
    assert_eq!(a.id, 10);
    assert_eq!(a.title.as_deref(), Some("Post 10"));
    assert_eq!(a.torrents.len(), 1);
    assert_eq!(a.torrents[0].hashes.v1, v1(A));
    assert_eq!(a.torrents[0].name.as_deref(), Some("Debian 13"));
    assert!(!a.adult);
    assert_eq!(a.published, Some(1_793_534_400));
    assert_eq!(a.link.as_deref(), Some("https://lemmy.example/post/10"));
    assert_eq!(a.community.name.as_deref(), Some("linux"));
    assert_eq!(
        a.community.actor_id.as_deref(),
        Some("https://lemmy.example/c/linux")
    );
    assert_eq!(a.creator.name.as_deref(), Some("uploader"));

    let b = &f.posts[1];
    assert_eq!(b.torrents.len(), 2);
    assert_eq!(b.torrents[0].hashes.v1, v1(B));
    assert_eq!(b.torrents[0].name.as_deref(), Some("B"));
    assert_eq!(b.torrents[1].hashes.v1, None);
    assert_eq!(b.torrents[1].hashes.v2, Some(InfoHashV2([0xaa; 32])));
    assert!(!b.torrents_truncated);
}

#[test]
fn nsfw_from_post_or_community() {
    let m = format!(r#","url":"magnet:?xt=urn:btih:{A}""#);
    let views = [
        view(1, &m, ""),
        view(2, &m.replace(",\"url\"", ",\"nsfw\":true,\"url\""), ""),
        view(3, &m, "").replace(r#""nsfw":false,"removed""#, r#""nsfw":true,"removed""#),
    ];
    let f = parse(page(&views).as_bytes());
    let adult: Vec<bool> = f.posts.iter().map(|p| p.adult).collect();
    assert_eq!(adult, [false, true, true]);
}

#[test]
fn missing_or_odd_flags_skip_the_post() {
    let m = format!(r#","url":"magnet:?xt=urn:btih:{A}""#);
    let views = [
        // No post.nsfw.
        view(1, &m, "").replace(
            r#""nsfw":false,"removed":false,"deleted":false,"#,
            r#""removed":false,"deleted":false,"#,
        ),
        // A string where a flag goes.
        view(2, &m, "").replacen(r#""nsfw":false"#, r#""nsfw":"no""#, 1),
        // No community.
        view(3, &m, "").replace("\"community\":", "\"x\":"),
        // An id that isn't a positive integer.
        view(0, &m, ""),
        view(5, &m, ""),
        // Missing its flags, with the highest id: it doesn't move the cursor.
        view(9, &m, "").replacen(r#""removed":false,"#, "", 1),
    ];
    let f = parse(page(&views).as_bytes());
    assert_eq!(f.page.summary.kept, 1);
    assert_eq!(f.page.summary.skipped.malformed, 5);
    assert_eq!(f.posts[0].id, 5);
    // Only posts read whole and sound move the cursor.
    assert_eq!(f.page.newest_id, Some(5));
}

#[test]
fn repeated_flags_only_add_restrictions() {
    let m = format!(r#","url":"magnet:?xt=urn:btih:{A}""#);
    let views = [
        view(1, &format!(r#"{m},"nsfw":true"#), ""),
        view(2, &format!(r#"{m},"removed":true"#), ""),
        view(3, &m, r#","deleted":true"#),
    ];
    let f = parse(page(&views).as_bytes());
    assert_eq!(f.posts.len(), 1);
    assert!(f.posts[0].adult);
    assert_eq!(f.page.removed, 2);
}

#[test]
fn repeated_objects_skip_the_post() {
    let m = format!(r#","url":"magnet:?xt=urn:btih:{A}""#);
    let views = [
        view(1, &m, "").replacen(r#""counts":"#, r#""community":{"nsfw":true},"counts":"#, 1),
        view(2, &m, "").replacen(
            r#""counts":"#,
            r#""post":{"id":2,"nsfw":false,"removed":true,"deleted":false},"counts":"#,
            1,
        ),
        view(3, &m, "").replacen(r#""counts":"#, r#""creator":{"name":"other"},"counts":"#, 1),
    ];
    let f = parse(page(&views).as_bytes());
    assert!(f.posts.is_empty());
    assert_eq!(f.page.summary.skipped.malformed, 3);
}

#[test]
fn removed_wins_over_other_faults() {
    let long = format!(r#","body":"{}""#, "x".repeat(70 * 1024));
    let v = view(4, &long, "").replacen(r#""removed":false"#, r#""removed":true"#, 1);
    let community = view(5, r#","url":null"#, r#","removed":true"#);
    let f = parse(page(&[v, community]).as_bytes());
    assert_eq!(f.page.removed, 2);
    assert_eq!(f.page.summary.skipped.total(), 0);
    assert_eq!(f.page.newest_id, Some(5));
}

#[test]
fn url_magnet_is_read_whole() {
    // Characters that end a magnet in Markdown are fine in `url`.
    let url = format!(r#","url":" magnet:?dn=It's+(Here)|é&xt=urn:btih:{A} ""#);
    let f = parse(page(&[view(1, &url, "")]).as_bytes());
    assert_eq!(f.posts[0].torrents[0].hashes.v1, v1(A));
    assert_eq!(
        f.posts[0].torrents[0].name.as_deref(),
        Some("It's (Here)|é")
    );
    // A magnet inside an http(s) url isn't a key.
    let url = format!(r#","url":"https://x.example/?m=magnet:?xt=urn:btih:{A}""#);
    let f = parse(page(&[view(1, &url, "")]).as_bytes());
    assert_eq!(f.page.summary.skipped.no_hash, 1);
}

#[test]
fn dropped_fields() {
    let m = format!(r#","url":"magnet:?xt=urn:btih:{A}""#);
    let v = view(1, &m, "")
        .replacen(
            "2026-11-01T12:00:00.123456Z",
            "2026-11-01T12:00:00.123456",
            1,
        )
        .replacen(
            r#""name":"uploader""#,
            &format!(r#""name":"{}""#, "u".repeat(257)),
            1,
        );
    let body = page(&[v]).replacen(r#""next_page":"Pa46c""#, r#""next_page":"a b""#, 1);
    let f = parse(body.as_bytes());
    let p = &f.posts[0];
    assert_eq!(p.published, None);
    assert_eq!(p.creator.name, None);
    assert_eq!(f.page.next_page, None);
}

#[test]
fn error_answer() {
    let f = parse(br#"{"error":"rate_limit_error"}"#);
    assert_eq!(f.page.error.as_deref(), Some("rate_limit_error"));
    assert!(!f.page.recognised);
    assert_eq!(f.page.summary.stop, None);
    let f = parse(br#"{"error":"Not A Code"}"#);
    assert_eq!(f.page.error, None);
}

#[test]
fn not_a_post_list() {
    for body in [
        &b"[]"[..],
        br#"{"posts":{}}"#,
        br#"{"post_view":{}}"#,
        b"null",
    ] {
        let f = parse(body);
        assert!(!f.page.recognised, "{body:?}");
        assert!(f.posts.is_empty());
    }
    assert_eq!(
        parse(br#"{"posts":[]} x"#).page.summary.stop,
        Some(Stop::Malformed)
    );
}

#[test]
fn cut_short_keeps_finished_posts() {
    let m = format!(r#","url":"magnet:?xt=urn:btih:{A}""#);
    let body = page(&[view(1, &m, ""), view(2, &m, "")]);
    let cut = &body.as_bytes()[..body.len() - 60];
    let f = parse(cut);
    assert_eq!(f.page.summary.stop, Some(Stop::Malformed));
    assert_eq!(f.posts.len(), 1);
    assert_eq!(f.page.newest_id, Some(1));
}

#[test]
fn caps() {
    let m = format!(r#","url":"magnet:?xt=urn:btih:{A}""#);
    // More posts than a page holds.
    let views: Vec<String> = (1..=51).map(|i| view(i, &m, "")).collect();
    let f = parse(page(&views).as_bytes());
    assert_eq!(f.page.summary.stop, Some(Stop::Items));
    assert_eq!(f.posts.len(), LIMITS.max_posts);

    // A body over 64 KiB skips the post, not the page.
    let long = format!(
        r#","body":"magnet:?xt=urn:btih:{A} {}""#,
        "x".repeat(64 * 1024)
    );
    let f = parse(page(&[view(1, &long, ""), view(2, &m, "")]).as_bytes());
    assert_eq!(f.page.summary.skipped.over_cap, 1);
    assert_eq!(f.posts.len(), 1);
    assert_eq!(f.page.summary.stop, None);

    // Distinct magnets per post.
    let many: String = (0..MAX_TORRENTS + 3)
        .map(|i| format!("magnet:?xt=urn:btih:{:040x} ", i + 1))
        .collect();
    let f = parse(page(&[view(1, &format!(r#","body":"{many}""#), "")]).as_bytes());
    assert_eq!(f.posts[0].torrents.len(), MAX_TORRENTS);
    assert!(f.posts[0].torrents_truncated);

    // Values.
    let tight = Limits {
        max_values: 10,
        ..LIMITS
    };
    let f = with(page(&[view(1, &m, "")]).as_bytes(), &tight);
    assert_eq!(f.page.summary.stop, Some(Stop::Elements));

    // Depth.
    let deep = format!(r#"{{"posts":[{}{}]}}"#, "[".repeat(40), "]".repeat(40));
    assert_eq!(parse(deep.as_bytes()).page.summary.stop, Some(Stop::Depth));

    // Bytes.
    let tight = Limits {
        max_bytes: 100,
        ..LIMITS
    };
    let f = with(page(&[view(1, &m, "")]).as_bytes(), &tight);
    assert_eq!(f.page.summary.stop, Some(Stop::Bytes));
    assert!(f.posts.is_empty());
}

#[test]
fn hidden_characters_and_credentials() {
    let post =
        format!(r#","name":"A\u202eB\u200b","url":"magnet:?xt=urn:btih:{A}&dn=x%E2%80%AEy""#);
    let v = view(1, &post, "")
        .replacen(r#""name":"Post 1","#, "", 1)
        .replace(
            "https://lemmy.example/post/1",
            "https://user:pw@lemmy.example/post/1?token=s",
        );
    let f = parse(page(&[v]).as_bytes());
    let p = &f.posts[0];
    assert_eq!(p.title.as_deref(), Some("AB"));
    assert_eq!(p.torrents[0].name.as_deref(), Some("xy"));
    let link = p.link.as_deref().unwrap();
    assert!(!link.contains("pw") && !link.contains("token"), "{link}");
}

// --- Properties -----------------------------------------------------

/// What holds for any input: `fuzz/fuzz_targets/lemmy.rs` checks the
/// same.
fn check(f: &Feed, limits: &Limits) {
    let p = &f.page;
    let s = &p.summary;
    assert_eq!(s.kept as usize, f.posts.len());
    assert!((s.kept + s.skipped.total() + p.removed) as usize <= limits.max_posts);
    assert_eq!(s.error, None);
    assert!(p.next_page.as_ref().is_none_or(|c| c.len() <= MAX_CURSOR));
    assert!(p.error.as_ref().is_none_or(|c| c.len() <= MAX_ERROR));
    for post in &f.posts {
        assert!(post.id > 0 && p.newest_id >= Some(post.id));
        assert!((1..=MAX_TORRENTS).contains(&post.torrents.len()));
        for (i, t) in post.torrents.iter().enumerate() {
            assert!(t.hashes.key().is_some());
            assert!(
                post.torrents[..i]
                    .iter()
                    .all(|o| o.hashes.key() != t.hashes.key())
            );
            assert!(
                t.name
                    .as_ref()
                    .is_none_or(|n| !n.is_empty() && n.len() <= MAX_TITLE)
            );
            assert!(!t.name.iter().flat_map(|n| n.chars()).any(hidden_char));
        }
        assert!(
            post.title
                .as_ref()
                .is_none_or(|t| !t.is_empty() && t.len() <= MAX_TITLE)
        );
        assert!(!post.title.iter().flat_map(|t| t.chars()).any(hidden_char));
        for r in [&post.link, &post.community.actor_id, &post.creator.actor_id] {
            assert!(r.as_ref().is_none_or(|l| l.len() <= MAX_REF));
        }
        for n in [&post.community.name, &post.creator.name] {
            assert!(
                n.as_ref()
                    .is_none_or(|n| !n.is_empty() && n.len() <= MAX_NAME)
            );
            assert!(!n.iter().flat_map(|n| n.chars()).any(hidden_char));
        }
    }
}

/// Tight caps picked by one byte, as the fuzz target does.
fn small(pick: u8) -> Limits {
    Limits {
        max_bytes: 1 + usize::from(pick) * 64,
        max_depth: 1 + usize::from(pick % 8),
        max_values: 1 + usize::from(pick) * 4,
        max_posts: 1 + usize::from(pick % 4),
        max_text: usize::from(pick) * 4,
    }
}

fn fragment() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("{".to_string()),
        Just("}".to_string()),
        Just("[".to_string()),
        Just("]".to_string()),
        Just(",".to_string()),
        Just(":".to_string()),
        Just(r#""post":{"#.to_string()),
        Just(r#""community":{"nsfw":true},"#.to_string()),
        Just(r#""id":12,"nsfw":false,"removed":false,"deleted":false,"#.to_string()),
        Just(format!(r#""url":"magnet:?xt=urn:btih:{A}","#)),
        Just(format!(r#""body":"x magnet:?xt=urn:btmh:{V2}&dn=a+b y","#)),
        Just(r#""nsfw":true,"#.to_string()),
        Just(r#""next_page":"Pa1","#.to_string()),
        Just(format!(r#""name":"{}u202e{}d800","#, '\\', '\\')),
        "[a-z{}\\[\\]\":,0-9\\\\ \u{202E}\u{FEFF}]{0,12}",
    ]
}

proptest! {
    #[test]
    fn any_bytes_are_safe(body in proptest::collection::vec(any::<u8>(), 0..512)) {
        check(&parse(&body), &LIMITS);
    }

    #[test]
    fn any_fragments_are_safe(
        parts in proptest::collection::vec(fragment(), 0..60),
        pick in any::<u8>(),
    ) {
        for root in [r#"{"posts":[{"#, r#"{"posts":["#, ""] {
            let body = format!("{root}{}", parts.concat());
            check(&parse(body.as_bytes()), &LIMITS);
            let limits = small(pick);
            check(&with(body.as_bytes(), &limits), &limits);
        }
    }

    #[test]
    fn posts_round_trip(
        posts in proptest::collection::vec(
            (any::<[u8; 20]>().prop_filter("not zero", |h| *h != [0; 20]), any::<(bool, bool, bool)>()),
            0..20,
        ),
    ) {
        let views: Vec<String> = posts
            .iter()
            .enumerate()
            .map(|(i, (h, (nsfw, in_body, gone)))| {
                let hex: String = h.iter().map(|b| format!("{b:02x}")).collect();
                let field = if *in_body { "body" } else { "url" };
                let v = view(i as u64 + 1, &format!(r#","{field}":"magnet:?xt=urn:btih:{hex}""#), "");
                let v = if *nsfw { v.replacen(r#""nsfw":false"#, r#""nsfw":true"#, 1) } else { v };
                if *gone { v.replacen(r#""removed":false"#, r#""removed":true"#, 1) } else { v }
            })
            .collect();
        let f = parse(page(&views).as_bytes());
        prop_assert_eq!(f.page.summary.stop, None);
        let live: Vec<_> = posts.iter().filter(|(_, (_, _, gone))| !gone).collect();
        prop_assert_eq!(f.posts.len(), live.len());
        prop_assert_eq!(f.page.removed as usize, posts.len() - live.len());
        for (p, (h, (nsfw, _, _))) in f.posts.iter().zip(live) {
            prop_assert_eq!(p.torrents[0].hashes.v1, Some(InfoHash(*h)));
            prop_assert_eq!(p.adult, *nsfw);
        }
    }
}

/// xorshift64*: a fixed, dependency-free source of mutations.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// The fuzz target's checks over 100,000 inputs: random bytes, and seed
/// pages with bytes flipped, inserted, deleted, duplicated and spliced.
#[test]
fn mutated_corpus() {
    let m = format!(r#","url":"magnet:?xt=urn:btih:{A}""#);
    let b = format!(r#","body":"[a](magnet:?xt=urn:btih:{B}&dn=b) magnet:?xt=urn:btmh:{V2}""#);
    let seeds: Vec<Vec<u8>> = vec![
        page(&[view(1, &m, ""), view(2, &b, r#","nsfw":true"#)]).into_bytes(),
        page(&[view(3, &b, ""), view(4, r#","url":null"#, "")]).into_bytes(),
        br#"{"error":"rate_limit_error"}"#.to_vec(),
    ];
    let tokens: [&[u8]; 12] = [
        b"{",
        b"}",
        b"[",
        b"]",
        b",",
        b"\"",
        b"\\",
        b":",
        b"magnet:?xt=urn:btih:",
        b"true",
        b"\xff",
        b"1e999",
    ];
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut kept = 0;
    for n in 0..100_000 {
        let mut input = if n % 10 == 0 {
            (0..rng.below(256)).map(|_| rng.next() as u8).collect()
        } else {
            seeds[rng.below(seeds.len())].clone()
        };
        for _ in 0..1 + rng.below(8) {
            let at = rng.below(input.len() + 1);
            match rng.below(6) {
                0 if at < input.len() => input[at] ^= 1 << rng.below(8),
                1 => input.insert(at, rng.next() as u8),
                2 if at < input.len() => {
                    input.remove(at);
                }
                3 => {
                    let t = tokens[rng.below(tokens.len())];
                    input.splice(at..at, t.iter().copied());
                }
                4 => {
                    let end = (at + rng.below(64)).min(input.len());
                    let chunk = input[at..end].to_vec();
                    input.splice(at..at, chunk);
                }
                _ => {
                    let other = &seeds[rng.below(seeds.len())];
                    let from = rng.below(other.len());
                    input.truncate(at);
                    input.extend_from_slice(&other[from..]);
                }
            }
        }
        let pick = rng.next() as u8;
        let f = parse(&input);
        check(&f, &LIMITS);
        kept += f.posts.len();
        let limits = small(pick);
        check(&with(&input, &limits), &limits);
    }
    // The mutations leave many pages whole enough to reach the posts.
    eprintln!("kept {kept} posts");
    assert!(kept > 10_000, "{kept}");
}
