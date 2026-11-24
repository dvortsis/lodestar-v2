//! Announce definitions and the matcher (IRC-1).

use lodestar_integrations::Secret;
use lodestar_sources_irc::announce::{
    self, Announce, CAPTURES, Definition, DefinitionError, MAX_DEFINITIONS, MAX_NAME, Matcher,
    MatcherError, Outcome, PatternError, Reject,
};
use lodestar_sources_irc::line::{self, Message};
use proptest::prelude::*;

const TEMPLATE: &str = include_str!("../data/announce/template.def");

const DEF: &str = "\
lodestar-announce = 1
id = example
name = Example Tracker
network = irc.example.net
channel = #announce
channel = #Announce-2
announcer = Announcer
announcer = Backup[bot]
pattern = ^New: (?P<name>.+?) \\[(?P<category>[^\\]]+)\\] (?P<size>[0-9.,]+ ?[KMGT]i?B) id=(?P<id>[0-9]+)$
pattern = ^Fresh (?P<name>\\S+) id=(?P<id>[0-9]+)$
torrent-url = https://example.net/dl/{id}/{passkey}/{name}.torrent
";

fn def() -> Definition {
    Definition::parse(DEF).expect("valid definition")
}

fn msg(raw: &str) -> Message {
    line::parse(raw.as_bytes())
        .expect("valid line")
        .expect("not empty")
}

fn announce(m: &Matcher, raw: &str) -> Announce {
    match m.read(&msg(raw)) {
        Outcome::Announce(a) => a,
        other => panic!("not an announce: {other:?}"),
    }
}

fn matcher() -> Matcher {
    Matcher::new(vec![def()]).unwrap()
}

#[test]
fn the_template_parses_and_matches() {
    let d = Definition::parse(TEMPLATE).expect("template.def is valid");
    assert_eq!(d.id(), "example-tracker");
    assert_eq!(d.network(), "irc.example-tracker.test");
    assert!(d.needs_passkey());
    let m = Matcher::new(vec![d]).unwrap();
    let a = announce(
        &m,
        ":ExampleBot!bot@services PRIVMSG #example-announce :New Torrent: Some.Show.S01E02.1080p.WEB.h264-GRP Category: TV/HD Size: 1.37 GiB - https://example-tracker.test/torrents/12345",
    );
    assert_eq!(a.name, "Some.Show.S01E02.1080p.WEB.h264-GRP");
    assert_eq!(a.category.as_deref(), Some("TV/HD"));
    assert_eq!(a.size, Some(1_471_026_298));
    assert_eq!(a.torrent_id.as_deref(), Some("12345"));
}

#[test]
fn reads_an_announce() {
    let m = matcher();
    let a = announce(
        &m,
        ":Announcer!a@h PRIVMSG #announce :New: Film.2024.2160p.UHD.BluRay-GRP [Movies/UHD] 61.2 GiB id=987",
    );
    assert_eq!(
        a,
        Announce {
            definition: "example".into(),
            name: "Film.2024.2160p.UHD.BluRay-GRP".into(),
            size: Some(65_712_999_628),
            category: Some("Movies/UHD".into()),
            torrent_id: Some("987".into()),
        }
    );
    // The second pattern; channel and nick under RFC 1459 case mapping.
    let a = announce(
        &m,
        ":BACKUP{BOT}!b@h NOTICE #announce-2 :Fresh Some.Album-2024-GRP id=5",
    );
    assert_eq!(a.name, "Some.Album-2024-GRP");
    assert_eq!(a.size, None);
    assert_eq!(a.category, None);
}

#[test]
fn ignores_what_is_not_an_announce() {
    let m = matcher();
    let ignored = [
        // Another nick, another channel, a private message, another
        // command, a server notice, CTCP.
        ":Someone!a@h PRIVMSG #announce :New: X [Y] 1 GB id=1",
        ":Announcer!a@h PRIVMSG #chat :New: X [Y] 1 GB id=1",
        ":Announcer!a@h PRIVMSG lode :New: X [Y] 1 GB id=1",
        ":Announcer!a@h TOPIC #announce :New: X [Y] 1 GB id=1",
        ":irc.example.net NOTICE #announce :New: X [Y] 1 GB id=1",
        ":Announcer!a@h PRIVMSG #announce :\u{1}ACTION New: X [Y] 1 GB id=1\u{1}",
        "PRIVMSG #announce :New: X [Y] 1 GB id=1",
    ];
    for raw in ignored {
        assert_eq!(m.read(&msg(raw)), Outcome::Ignored, "{raw}");
    }
    assert_eq!(
        m.read(&msg(
            ":Announcer!a@h PRIVMSG #announce :site maintenance at noon"
        )),
        Outcome::NoMatch
    );
}

#[test]
fn cleans_server_text_before_matching() {
    let m = matcher();
    // Colours, bold and a reset around the fields, a zero-width space
    // and a right-to-left override inside the name, and a bell.
    let a = announce(
        &m,
        ":Announcer!a@h PRIVMSG #announce :\u{2}New:\u{f} \u{3}04Bad\u{200b}.Name\u{202e}.GRP\u{3} [\u{3}12TV\u{3}] 700 MB id=\u{2}42\u{2}\u{7}",
    );
    assert_eq!(a.name, "Bad.Name.GRP");
    assert_eq!(a.category.as_deref(), Some("TV"));
    assert_eq!(a.size, Some(700_000_000));
    assert_eq!(a.torrent_id.as_deref(), Some("42"));
    for f in [
        Some(a.name.as_str()),
        a.category.as_deref(),
        a.torrent_id.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        assert!(!f.chars().any(|c| c.is_control() || line::is_format_char(c)));
    }
}

#[test]
fn a_url_in_a_capture_is_refused() {
    let text = DEF.replace(
        "pattern = ^Fresh (?P<name>\\S+) id=(?P<id>[0-9]+)$",
        "pattern = ^Fresh (?P<name>\\S+) id=(?P<id>[0-9]+)$\npattern = ^Link (?P<name>.+) (?P<id>[0-9]+)$",
    );
    let m = Matcher::new(vec![Definition::parse(&text).unwrap()]).unwrap();
    assert_eq!(
        m.read(&msg(
            ":Announcer!a@h PRIVMSG #announce :Link https://example.net/dl?passkey=abc 7"
        )),
        Outcome::Rejected(Reject::Url)
    );
}

#[test]
fn a_missing_id_is_refused_when_the_template_needs_it() {
    // An optional id capture: every pattern captures `id`, but this line
    // has none.
    let text = "\
lodestar-announce = 1
id = opt
name = Optional
network = irc.example.net
channel = #a
announcer = Bot
pattern = ^(?P<name>\\S+)(?: id=(?P<id>[0-9]+))?$
torrent-url = https://example.net/{id}
";
    let m = Matcher::new(vec![Definition::parse(text).unwrap()]).unwrap();
    assert_eq!(
        m.read(&msg(":Bot!b@h PRIVMSG #a :Name.Only")),
        Outcome::Rejected(Reject::TorrentId)
    );
    assert!(matches!(
        m.read(&msg(":Bot!b@h PRIVMSG #a :Name.Only id=3")),
        Outcome::Announce(_)
    ));
    // An id with characters a URL path can't take as they are is refused
    // too, rather than escaped into a different id.
    let text = text.replace("[0-9]+", "\\S+");
    let m = Matcher::new(vec![Definition::parse(&text).unwrap()]).unwrap();
    assert_eq!(
        m.read(&msg(":Bot!b@h PRIVMSG #a :Name id=../../x")),
        Outcome::Rejected(Reject::TorrentId)
    );
    // Without a template, an announce needs no id.
    let text = text.replace("torrent-url = https://example.net/{id}\n", "");
    let m = Matcher::new(vec![Definition::parse(&text).unwrap()]).unwrap();
    assert!(matches!(
        m.read(&msg(":Bot!b@h PRIVMSG #a :Name.Only")),
        Outcome::Announce(Announce {
            torrent_id: None,
            ..
        })
    ));
}

#[test]
fn names_are_bounded() {
    let text = DEF.replace("(?P<name>\\S+)", "(?P<name>.*)");
    let m = Matcher::new(vec![Definition::parse(&text).unwrap()]).unwrap();
    assert_eq!(
        m.read(&msg(":Announcer!a@h PRIVMSG #announce :Fresh    id=1")),
        Outcome::Rejected(Reject::Name),
        "a name of spaces is empty once trimmed"
    );
    // A body is at most 510 bytes, so even Latin-1 decoding (each byte
    // of a body that isn't UTF-8 becomes one character, `é` two bytes in
    // UTF-8) makes a name of about 1,020 bytes at most, under the cap;
    // the cap is defence in depth. Check the doubled name comes through.
    let body = format!(
        ":Announcer!a@h PRIVMSG #announce :Fresh {} id=1",
        "\u{e9}".repeat(180)
    );
    let mut bytes = Vec::new();
    for c in body.chars() {
        if c == '\u{e9}' {
            bytes.push(0xE9);
        } else {
            bytes.push(c as u8);
        }
    }
    let m2 = line::parse(&bytes).unwrap().unwrap();
    match m.read(&m2) {
        Outcome::Announce(a) => {
            assert_eq!(a.name, "\u{e9}".repeat(180));
            assert!(a.name.len() <= MAX_NAME);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_torrent_url_carries_the_passkey_only_when_asked() {
    let d = def();
    let m = Matcher::new(vec![d.clone()]).unwrap();
    let a = announce(
        &m,
        ":Announcer!a@h PRIVMSG #announce :New: Odd Name/With?Chars&More [TV] 1 GB id=77",
    );
    let key = Secret::new("s3cr3t/key+");
    let url = d.torrent_url(&a, Some(&key)).expect("filled");
    assert_eq!(
        url.expose(),
        "https://example.net/dl/77/s3cr3t%2Fkey%2B/Odd%20Name%2FWith%3FChars%26More.torrent"
    );
    // Nothing that can be printed holds it.
    assert_eq!(format!("{url:?}"), "SecretUrl(<redacted>)");
    for printed in [format!("{a:?}"), format!("{d:?}"), format!("{m:?}")] {
        assert!(!printed.contains("s3cr3t"), "{printed}");
        assert!(!printed.contains("https://"), "{printed}");
    }
    // No passkey, an empty one, or an announce from another definition:
    // no URL.
    assert!(d.torrent_url(&a, None).is_none());
    assert!(d.torrent_url(&a, Some(&Secret::new(""))).is_none());
    let other = Announce {
        definition: "another".into(),
        ..a.clone()
    };
    assert!(d.torrent_url(&other, Some(&key)).is_none());
    // A template without `{passkey}` needs none.
    let open = DEF.replace("/{passkey}", "");
    let d = Definition::parse(&open).unwrap();
    assert!(!d.needs_passkey());
    assert_eq!(
        d.torrent_url(&a, None).unwrap().expose(),
        "https://example.net/dl/77/Odd%20Name%2FWith%3FChars%26More.torrent"
    );
}

#[test]
fn definition_errors() {
    let bad: &[(&str, DefinitionError)] = &[
        ("", DefinitionError::NotADefinition),
        ("# only a comment\n", DefinitionError::NotADefinition),
        ("id = x\n", DefinitionError::NotADefinition),
        (
            "lodestar-announce = 2\n",
            DefinitionError::UnsupportedVersion,
        ),
        (
            "lodestar-announce = 1\nlodestar-announce = 1\n",
            DefinitionError::Repeated("lodestar-announce"),
        ),
        (
            "lodestar-announce = 1\njust words\n",
            DefinitionError::Syntax(2),
        ),
        (
            "lodestar-announce = 1\ncolour = red\n",
            DefinitionError::UnknownKey(2),
        ),
        (
            "lodestar-announce = 1\nid = x\nid = y\n",
            DefinitionError::Repeated("id"),
        ),
        (
            "lodestar-announce = 1\nid = Bad_Id\n",
            DefinitionError::Invalid("id"),
        ),
        (
            "lodestar-announce = 1\nid = -x\n",
            DefinitionError::Invalid("id"),
        ),
        (
            "lodestar-announce = 1\nname = a\u{202e}b\n",
            DefinitionError::Invalid("name"),
        ),
        (
            "lodestar-announce = 1\nname = a\tb\n",
            DefinitionError::Invalid("name"),
        ),
        (
            "lodestar-announce = 1\nnetwork = bad host\n",
            DefinitionError::Invalid("network"),
        ),
        (
            "lodestar-announce = 1\nchannel = nochan\n",
            DefinitionError::Invalid("channel"),
        ),
        (
            "lodestar-announce = 1\nannouncer = bad.nick\n",
            DefinitionError::Invalid("announcer"),
        ),
        (
            "lodestar-announce = 1\npattern = (?P<other>x)\n",
            DefinitionError::Pattern(1, PatternError::UnknownCapture),
        ),
        (
            "lodestar-announce = 1\npattern = (x)\n",
            DefinitionError::Pattern(1, PatternError::NoName),
        ),
        (
            "lodestar-announce = 1\npattern = (?P<name>[\n",
            DefinitionError::Pattern(1, PatternError::Syntax),
        ),
        (
            "lodestar-announce = 1\ntorrent-url = http://x.example/{id}\n",
            DefinitionError::Invalid("torrent-url"),
        ),
        (
            "lodestar-announce = 1\ntorrent-url = https://x.example:0/{id}\n",
            DefinitionError::Invalid("torrent-url"),
        ),
        (
            "lodestar-announce = 1\ntorrent-url = https://x.example/{id}\ntorrent-url = https://x.example/{id}\n",
            DefinitionError::Repeated("torrent-url"),
        ),
        (
            "lodestar-announce = 1\nchannel = #a\u{2028}b\n",
            DefinitionError::Invalid("channel"),
        ),
        (
            "lodestar-announce = 1\nannouncer-host = bot@host\n",
            DefinitionError::Invalid("announcer-host"),
        ),
        (
            "lodestar-announce = 1\ntorrent-url = https://x.example/{id}/123e4567-e89b-12d3-a456-426614174000\n",
            DefinitionError::LiteralSecret,
        ),
        (
            "lodestar-announce = 1\ntorrent-url = https://0123456789abcdef0123456789abcdef.x.example/{id}\n",
            DefinitionError::LiteralSecret,
        ),
    ];
    for (text, want) in bad {
        assert_eq!(Definition::parse(text).unwrap_err(), *want, "{text:?}");
    }
    // Each required key, missing.
    for (key, line) in [
        ("id", "id = example\n"),
        ("name", "name = Example Tracker\n"),
        ("network", "network = irc.example.net\n"),
    ] {
        let text = DEF.replace(line, "");
        assert_eq!(
            Definition::parse(&text).unwrap_err(),
            DefinitionError::Missing(key)
        );
    }
    let only = |keep: &str| {
        DEF.lines()
            .filter(|l| !l.starts_with(keep))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(
        Definition::parse(&only("channel")).unwrap_err(),
        DefinitionError::Missing("channel")
    );
    assert_eq!(
        Definition::parse(&only("announcer")).unwrap_err(),
        DefinitionError::Missing("announcer")
    );
    assert_eq!(
        Definition::parse(&only("pattern")).unwrap_err(),
        DefinitionError::Missing("pattern")
    );
    // `{id}` in the template needs every pattern to capture it.
    let text = DEF.replace(
        "pattern = ^Fresh (?P<name>\\S+) id=(?P<id>[0-9]+)$",
        "pattern = ^Fresh (?P<name>\\S+)$",
    );
    assert_eq!(
        Definition::parse(&text).unwrap_err(),
        DefinitionError::Invalid("torrent-url")
    );
    // Errors print no definition text.
    let e = Definition::parse(
        "lodestar-announce = 1\ntorrent-url = https://x.example/0123456789abcdefABCDEF\n",
    )
    .unwrap_err();
    assert_eq!(e, DefinitionError::LiteralSecret);
    assert!(!e.to_string().contains("0123456789"));
}

#[test]
fn repeated_lists_are_capped_and_deduplicated() {
    let mut text = String::from(DEF);
    for i in 0..6 {
        text.push_str(&format!("channel = #more{i}\n"));
    }
    assert!(Definition::parse(&text).is_ok(), "8 channels");
    text.push_str("channel = #onetoomany\n");
    assert_eq!(
        Definition::parse(&text).unwrap_err(),
        DefinitionError::TooMany("channel")
    );
    // A channel or nick repeated under case mapping counts once.
    let text = DEF.replace("channel = #Announce-2", "channel = #ANNOUNCE");
    assert_eq!(Definition::parse(&text).unwrap().channels().len(), 1);
    // A repeat doesn't count against the cap.
    let mut text = String::from(DEF);
    for i in 0..6 {
        text.push_str(&format!("channel = #more{i}\nchannel = #MORE{i}\n"));
    }
    assert_eq!(Definition::parse(&text).unwrap().channels().len(), 8);
    let mut text = String::from(DEF);
    for _ in 0..3 {
        text.push_str("pattern = (?P<name>x)\n");
    }
    assert_eq!(
        Definition::parse(&text).unwrap_err(),
        DefinitionError::TooMany("pattern")
    );
    let mut text = String::from(DEF);
    for i in 0..7 {
        text.push_str(&format!("announcer = bot{i}\n"));
    }
    assert_eq!(
        Definition::parse(&text).unwrap_err(),
        DefinitionError::TooMany("announcer")
    );
}

#[test]
fn sizes_and_lines_are_bounded() {
    let long = "a".repeat(announce::MAX_DEFINITION_BYTES + 1);
    assert_eq!(
        Definition::parse(&long).unwrap_err(),
        DefinitionError::TooLong
    );
    let text = format!(
        "lodestar-announce = 1\n# {}\n",
        "a".repeat(announce::MAX_DEFINITION_LINE)
    );
    assert_eq!(
        Definition::parse(&text).unwrap_err(),
        DefinitionError::LineTooLong(2)
    );
    let text = format!(
        "lodestar-announce = 1\npattern = (?P<name>{})\n",
        "a".repeat(announce::MAX_PATTERN)
    );
    assert_eq!(
        Definition::parse(&text).unwrap_err(),
        DefinitionError::Pattern(1, PatternError::TooLong)
    );
    // Small source, huge program: refused by the size limit.
    let text = "lodestar-announce = 1\npattern = (?P<name>\\w{1000}\\w{1000}\\w{1000})\n";
    assert_eq!(
        Definition::parse(text).unwrap_err(),
        DefinitionError::Pattern(1, PatternError::TooBig)
    );
    // Too deeply nested.
    let text = format!(
        "lodestar-announce = 1\npattern = (?P<name>{}a{})\n",
        "(".repeat(40),
        ")".repeat(40)
    );
    assert_eq!(
        Definition::parse(&text).unwrap_err(),
        DefinitionError::Pattern(1, PatternError::Syntax)
    );
    // CRLF line ends are fine.
    assert!(Definition::parse(&DEF.replace('\n', "\r\n")).is_ok());
}

#[test]
fn matcher_limits() {
    let with_id =
        |id: &str| Definition::parse(&DEF.replace("id = example", &format!("id = {id}"))).unwrap();
    assert_eq!(
        Matcher::new(vec![with_id("a"), with_id("a")]).unwrap_err(),
        MatcherError::DuplicateId
    );
    let many: Vec<Definition> = (0..=MAX_DEFINITIONS)
        .map(|i| with_id(&format!("t{i}")))
        .collect();
    assert_eq!(
        Matcher::new(many).unwrap_err(),
        MatcherError::TooManyDefinitions
    );
    // Four patterns each: 17 definitions are 68 patterns, over 64.
    let four = DEF.to_owned()
        + "pattern = ^A (?P<name>\\S+) id=(?P<id>[0-9]+)$\npattern = ^B (?P<name>\\S+) id=(?P<id>[0-9]+)$\n";
    let defs: Vec<Definition> = (0..17)
        .map(|i| Definition::parse(&four.replace("id = example", &format!("id = t{i}"))).unwrap())
        .collect();
    assert_eq!(
        Matcher::new(defs).unwrap_err(),
        MatcherError::TooManyPatterns
    );
    let m = Matcher::new(vec![with_id("a"), with_id("b")]).unwrap();
    assert_eq!(m.definitions().len(), 2);
    assert_eq!(m.definition("b").unwrap().id(), "b");
    // The first definition for a channel and announcer decides.
    let a = announce(&m, ":Announcer!a@h PRIVMSG #announce :Fresh X id=1");
    assert_eq!(a.definition, "a");
}

#[test]
fn every_capture_name_is_documented() {
    let readme = include_str!("../data/announce/README.md");
    for c in CAPTURES {
        assert!(readme.contains(&format!("`{c}`")), "{c}");
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn definition_parse_never_panics(text in "\\PC{0,400}|(lodestar-announce = 1\n)([a-z-]{1,12} = [ -~]{0,60}\n){0,12}") {
        let _ = Definition::parse(&text);
    }

    #[test]
    fn matched_fields_are_clean_and_bounded(
        text in "[\u{2}\u{3}\u{f}\u{200b}\u{202e}\u{1}\t0-9a-zA-Z .:/\\[\\]=-]{0,200}|\\PC{0,200}",
        size in "[0-9.,]{1,12} ?(B|KB|MB|GB|GiB|TiB|tb)?",
    ) {
        let d = Definition::parse(&DEF.replace("(?P<name>\\S+)", "(?P<name>.*)")).unwrap();
        let m = Matcher::new(vec![d]).unwrap();
        for body in [format!("Fresh {text} id=1"), format!("New: {text} [{text}] {size} id=2"), text.clone()] {
            let raw = format!(":Announcer!a@h PRIVMSG #announce :{body}");
            let Ok(Some(message)) = line::parse(raw.as_bytes()) else { continue };
            if let Outcome::Announce(a) = m.read(&message) {
                prop_assert!(!a.name.is_empty() && a.name.len() <= MAX_NAME);
                for f in [Some(a.name.as_str()), a.category.as_deref(), a.torrent_id.as_deref()].into_iter().flatten() {
                    prop_assert!(!f.chars().any(|c| c.is_control() || line::is_format_char(c)));
                    prop_assert!(!f.contains("://"));
                }
                prop_assert!(a.size.is_none_or(|s| s <= announce::MAX_SIZE));
            }
        }
    }

    #[test]
    fn parse_size_never_panics(s in "\\PC{0,40}|[0-9.,]{0,30} ?[kmgtKMGTiIbB]{0,3}") {
        if let Some(n) = announce::parse_size(&s) {
            prop_assert!(n <= announce::MAX_SIZE);
        }
    }
}

#[test]
fn dot_segments_never_reach_the_url() {
    let d = def();
    let m = Matcher::new(vec![d.clone()]).unwrap();
    let key = Secret::new("k");
    // A name of dots would climb the path in `/{name}`.
    let text = DEF.replace("/{name}.torrent", "/{name}");
    let d2 = Definition::parse(&text).unwrap();
    let a = announce(&m, ":Announcer!a@h PRIVMSG #announce :Fresh .. id=5");
    assert_eq!(a.name, "..");
    assert!(d2.torrent_url(&a, Some(&key)).is_none());
    // An id of dots isn't an id.
    let text = DEF.replace("id=(?P<id>[0-9]+)", "id=(?P<id>[0-9.]+)");
    let m = Matcher::new(vec![Definition::parse(&text).unwrap()]).unwrap();
    for id in [".", ".."] {
        assert_eq!(
            m.read(&msg(&format!(
                ":Announcer!a@h PRIVMSG #announce :Fresh x id={id}"
            ))),
            Outcome::Rejected(Reject::TorrentId),
            "{id}"
        );
    }
    let a = announce(&m, ":Announcer!a@h PRIVMSG #announce :Fresh x id=1.2");
    assert_eq!(a.torrent_id.as_deref(), Some("1.2"));
}

#[test]
fn an_overlong_name_is_refused() {
    // No parsed line is long enough; a hand-built message is.
    let text = DEF.replace("(?P<name>\\S+)", "(?P<name>.*)");
    let m = Matcher::new(vec![Definition::parse(&text).unwrap()]).unwrap();
    let m2 = Message {
        tags: Vec::new(),
        source: Some("Announcer!a@h".into()),
        command: "PRIVMSG".into(),
        params: vec![
            "#announce".into(),
            format!("Fresh {} id=1", "a".repeat(MAX_NAME + 1)),
        ],
    };
    assert_eq!(m.read(&m2), Outcome::Rejected(Reject::Name));
    let m3 = Message {
        params: vec![
            "#announce".into(),
            format!("Fresh {} id=1", "a".repeat(MAX_NAME)),
        ],
        ..m2
    };
    assert!(matches!(m.read(&m3), Outcome::Announce(_)));
}

#[test]
fn announcer_hosts_narrow_who_counts() {
    let text =
        format!("{DEF}announcer-host = services.example.net\nannouncer-host = user/announcer\n");
    let d = Definition::parse(&text).unwrap();
    assert_eq!(d.announcer_hosts().len(), 2);
    let m = Matcher::new(vec![d]).unwrap();
    let line = |src: &str| format!(":{src} PRIVMSG #announce :Fresh x id=1");
    for ok in [
        "Announcer!a@services.example.net",
        "Announcer!a@SERVICES.example.NET",
        "Announcer!a@user/announcer",
    ] {
        assert!(
            matches!(m.read(&msg(&line(ok))), Outcome::Announce(_)),
            "{ok}"
        );
    }
    for no in [
        "Announcer!a@evil.example.net",
        "Announcer!a@services.example.net.evil",
        "Announcer",
    ] {
        assert_eq!(m.read(&msg(&line(no))), Outcome::Ignored, "{no}");
    }
    // Without hosts, any host will do.
    assert!(matches!(
        matcher().read(&msg(&line("Announcer!a@anywhere"))),
        Outcome::Announce(_)
    ));
    // Capped like announcers, repeats counted once.
    let mut text = String::from(DEF);
    for i in 0..8 {
        text.push_str(&format!("announcer-host = h{i}\nannouncer-host = H{i}\n"));
    }
    assert_eq!(Definition::parse(&text).unwrap().announcer_hosts().len(), 8);
    assert_eq!(
        Definition::parse(&format!("{DEF}announcer-host = *.example.net\n")).unwrap_err(),
        DefinitionError::Invalid("announcer-host")
    );
    text.push_str("announcer-host = h9\n");
    assert_eq!(
        Definition::parse(&text).unwrap_err(),
        DefinitionError::TooMany("announcer-host")
    );
}

#[test]
fn the_template_host_is_known_before_any_url() {
    assert_eq!(def().torrent_url_host(), Some("example.net"));
    let text = DEF.replace("https://example.net/", "https://dl.example.net:8443/");
    assert_eq!(
        Definition::parse(&text).unwrap().torrent_url_host(),
        Some("dl.example.net")
    );
    let text = DEF.replace(
        "torrent-url = https://example.net/dl/{id}/{passkey}/{name}.torrent\n",
        "",
    );
    assert_eq!(Definition::parse(&text).unwrap().torrent_url_host(), None);
}

#[test]
fn a_byte_order_mark_is_allowed() {
    assert!(Definition::parse(&format!("\u{FEFF}{DEF}")).is_ok());
}

#[test]
fn size_polish() {
    use announce::parse_size;
    assert_eq!(parse_size("1.2,345 GB"), None, "a dot before a group");
    assert_eq!(parse_size("0,500 GB"), Some(500_000_000), "a decimal comma");
    assert_eq!(parse_size("1,500 GB"), Some(1_500_000_000_000), "thousands");
    assert_eq!(parse_size("1.5\u{a0}GiB"), Some(1_610_612_736));
    assert_eq!(parse_size("\u{a0}2 KB\u{a0}"), Some(2000));
    // Fraction digits past the ninth still count.
    assert_eq!(parse_size("0.0000000015 TB"), Some(1500));
}
