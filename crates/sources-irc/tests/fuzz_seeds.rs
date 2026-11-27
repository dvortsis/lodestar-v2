//! The seeds of the `irc_announce` fuzz target (X-irc-fuzz-seeds), built
//! here rather than by hand, in the target's input layout: two bytes of
//! definition length (big-endian), the definition, then one server line.
//!
//! Each seed is an ordinary, valid or near-valid input: the shipped
//! template with announce lines, the target's fixed definition (length 0,
//! so the line is read against it alone) with lines of its two shapes, and
//! two definitions at their list caps. The test checks what each seed
//! does, then that `fuzz/seeds/irc_announce/` holds exactly these bytes.
//! `LODESTAR_BLESS=1 cargo test -p lodestar-sources-irc --test fuzz_seeds`
//! rewrites the folder.

use std::path::Path;

use lodestar_sources_irc::announce::{
    Definition, MAX_ANNOUNCERS, MAX_CHANNELS, MAX_PATTERNS, Matcher, Outcome, Reject,
};
use lodestar_sources_irc::line;

const TEMPLATE: &str = include_str!("../data/announce/template.def");

/// The target's fixed definition, copied from
/// `fuzz/fuzz_targets/irc_announce.rs`; `the_fixed_definition_is_the_targets`
/// keeps the two the same.
const FIXED: &str = "\
lodestar-announce = 1
id = fixed
name = Fixed
network = irc.example.net
channel = #announce
announcer = Announcer
pattern = ^New: (?P<name>.+?) \\[(?P<category>[^\\]]*)\\] (?P<size>\\S+ ?\\S*) id=(?P<id>\\S+)$
pattern = (?P<name>.*?)(?: id=(?P<id>\\S*))?$
torrent-url = https://example.net/dl/{id}/{passkey}/{name}
";

/// Every seed is well under this, so libFuzzer mutates small inputs.
const MAX_SEED: usize = 4096;

/// The template's own announce shape, from its announcer to its channel.
const TEMPLATE_HIT: &str = ":ExampleBot!bot@services.example-tracker.test PRIVMSG \
    #example-announce :New Torrent: Some.Show.S01E02.1080p.WEB-DL Category: TV \
    Size: 1.37 GiB - https://example-tracker.test/torrents/12345";

/// What a seed's line comes to, read against its own definition when it
/// has one, else against the fixed one.
#[derive(Debug)]
enum Expect {
    /// An announce with this name.
    Announce(&'static str),
    NoMatch,
    Ignored,
    Rejected(Reject),
}

struct Seed {
    name: &'static str,
    definition: String,
    line: String,
    expect: Expect,
}

impl Seed {
    fn new(name: &'static str, definition: &str, line: &str, expect: Expect) -> Seed {
        Seed {
            name,
            definition: definition.to_owned(),
            line: line.to_owned(),
            expect,
        }
    }

    fn bytes(&self) -> Vec<u8> {
        let d = self.definition.as_bytes();
        let mut out = u16::try_from(d.len())
            .expect("a definition under 64 KiB")
            .to_be_bytes()
            .to_vec();
        out.extend_from_slice(d);
        out.extend_from_slice(self.line.as_bytes());
        out
    }
}

/// A definition with every list at its cap: 8 channels, 8 announcers, 8
/// announcer hosts and 4 patterns. With `folded`, each list also repeats
/// entries in another case (RFC 1459 case mapping for channels and nicks,
/// ASCII case for hosts), which the parser folds rather than counting past
/// the cap.
fn at_caps(folded: bool) -> String {
    let mut lines = vec![
        "lodestar-announce = 1".to_owned(),
        format!("id = caps{}", if folded { "-folded" } else { "" }),
        "name = At the caps".to_owned(),
        "network = irc.caps.test".to_owned(),
    ];
    lines.extend((0..7).map(|i| format!("channel = #announce-{i}")));
    lines.push("channel = #Caps[Main]".to_owned());
    lines.extend((0..7).map(|i| format!("announcer = Bot{i}")));
    lines.push("announcer = Caps[Bot]".to_owned());
    lines.extend((0..8).map(|i| format!("announcer-host = bot{i}.caps.test")));
    if folded {
        // `[]\~` are the upper case of `{}|^`.
        for l in [
            "channel = #caps{main}",
            "channel = #ANNOUNCE-0",
            "announcer = caps{bot}",
            "announcer = BOT1",
            "announcer-host = BOT0.CAPS.TEST",
        ] {
            lines.push(l.to_owned());
        }
    }
    for l in [
        r"pattern = ^\[(?P<category>[^\]]+)\] (?P<name>\S+) \((?P<size>[0-9.]+ ?[KMGT]i?B)\) #(?P<id>[0-9]+)$",
        r"pattern = ^(?P<name>\S+) in (?P<category>\S+) #(?P<id>[0-9]+)$",
        r"pattern = ^(?P<name>\S+) (?P<size>[0-9]+) #(?P<id>[0-9]+)$",
        r"pattern = ^(?P<name>.+) #(?P<id>[A-Za-z0-9._-]+)$",
        "torrent-url = https://caps.test/get/{id}?key={passkey}&n={name}",
    ] {
        lines.push(l.to_owned());
    }
    lines.join("\n") + "\n"
}

fn seeds() -> Vec<Seed> {
    use Expect::*;
    let caps = at_caps(false);
    let folded = at_caps(true);
    let crlf_bom = format!("\u{feff}{}", TEMPLATE.replace('\n', "\r\n"));
    let fixed = |text: &str| format!(":Announcer!a@announce.example.net PRIVMSG #announce :{text}");
    vec![
        // The shipped template, with a line that matches it.
        Seed::new(
            "template-announce",
            TEMPLATE,
            TEMPLATE_HIT,
            Announce("Some.Show.S01E02.1080p.WEB-DL"),
        ),
        // The same as a NOTICE to the channel in upper case, with a CRLF
        // ending and mIRC bold and colour, which the matcher strips.
        Seed::new(
            "template-notice-formatted",
            TEMPLATE,
            ":ExampleBot!bot@services.example-tracker.test NOTICE #EXAMPLE-ANNOUNCE \
             :\u{2}New Torrent:\u{2} \u{3}04,01Some.Film.2026.2160p\u{3} Category: Movies \
             Size: 58.2 GB - https://example-tracker.test/torrents/9\r",
            Announce("Some.Film.2026.2160p"),
        ),
        // Its announcer and channel, but chatter the pattern doesn't match.
        Seed::new(
            "template-no-match",
            TEMPLATE,
            ":ExampleBot!bot@services.example-tracker.test PRIVMSG #example-announce \
             :Site maintenance tonight, announces may pause",
            NoMatch,
        ),
        // An announce-shaped line from a nick that isn't the announcer.
        Seed::new(
            "template-other-nick",
            TEMPLATE,
            &TEMPLATE_HIT.replace(":ExampleBot!bot@", ":SomeUser!user@"),
            Ignored,
        ),
        // The template saved with CRLF endings and a byte-order mark.
        Seed::new(
            "template-crlf-bom",
            &crlf_bom,
            TEMPLATE_HIT,
            Announce("Some.Show.S01E02.1080p.WEB-DL"),
        ),
        // No definition: the fixed one's first pattern, with a size.
        Seed::new(
            "fixed-with-size",
            "",
            &fixed("New: Some.Album.2026.FLAC [Music] 412.7 MiB id=a1b2c3"),
            Announce("Some.Album.2026.FLAC"),
        ),
        // Its first pattern, with a size in plain bytes.
        Seed::new(
            "fixed-size-bytes",
            "",
            &fixed("New: Some.Book.2026.EPUB [Books] 1048576 B id=77"),
            Announce("Some.Book.2026.EPUB"),
        ),
        // Its second pattern: no size, no category.
        Seed::new(
            "fixed-no-size",
            "",
            &fixed("Some.Release.2026.720p.HDTV id=4096"),
            Announce("Some.Release.2026.720p.HDTV"),
        ),
        // Its second pattern without an id, which the template's `{id}`
        // refuses.
        Seed::new(
            "fixed-no-id",
            "",
            &fixed("Some.Release.2026"),
            Rejected(Reject::TorrentId),
        ),
        // Message tags, as an IRCv3 server sends them, escapes included,
        // and a size with a decimal comma.
        Seed::new(
            "fixed-tags",
            "",
            &format!(
                "@time=2026-11-27T08:00:00.000Z;msgid=abc123;+example.test/note=a\\sb\\:c {}",
                fixed("New: Some.Game.2026-GROUP [Games] 7,5 GB id=x_1.2")
            ),
            Announce("Some.Game.2026-GROUP"),
        ),
        // A CTCP ACTION, which is never an announce.
        Seed::new(
            "fixed-action",
            "",
            &fixed("\u{1}ACTION New: Some.Release [TV] 1 GB id=1\u{1}"),
            Ignored,
        ),
        // Another command to the same channel.
        Seed::new(
            "fixed-topic",
            "",
            ":Announcer!a@announce.example.net TOPIC #announce :New: Some.Release [TV] 1 GB id=1",
            Ignored,
        ),
        // A server numeric, whose source is a server, not a nick.
        Seed::new(
            "fixed-numeric",
            "",
            ":irc.example.net 332 me #announce :Announces only, no chat",
            Ignored,
        ),
        // Every list at its cap, with a line from the last announcer and
        // host to the last channel, matching the first pattern.
        Seed::new(
            "caps",
            &caps,
            ":Caps[Bot]!bot@bot7.caps.test PRIVMSG #Caps[Main] \
             :[TV] Some.Show.S02E01.1080p (2.5 GiB) #31337",
            Announce("Some.Show.S02E01.1080p"),
        ),
        // The caps with entries repeated in another case, and a line that
        // reaches the fourth pattern through the folded names.
        Seed::new(
            "caps-folded",
            &folded,
            ":caps{bot}!bot@BOT7.caps.test NOTICE #caps{main} :Some Film (2026) 2160p #rel-2026.1",
            Announce("Some Film (2026) 2160p"),
        ),
    ]
}

fn outcome(seed: &Seed) -> Outcome {
    let def = if seed.definition.is_empty() {
        FIXED
    } else {
        &seed.definition
    };
    let def = Definition::parse(def).unwrap_or_else(|e| panic!("{}: {e}", seed.name));
    let msg = line::parse(seed.line.as_bytes())
        .unwrap_or_else(|e| panic!("{}: {e}", seed.name))
        .expect("not blank");
    Matcher::new(vec![def]).expect("one definition").read(&msg)
}

#[test]
fn each_seed_does_what_it_says() {
    for seed in seeds() {
        let got = outcome(&seed);
        let ok = match (&seed.expect, &got) {
            (Expect::Announce(name), Outcome::Announce(a)) => a.name == *name,
            (Expect::NoMatch, Outcome::NoMatch) | (Expect::Ignored, Outcome::Ignored) => true,
            (Expect::Rejected(r), Outcome::Rejected(g)) => r == g,
            _ => false,
        };
        assert!(ok, "{}: expected {:?}, got {got:?}", seed.name, seed.expect);
    }
}

#[test]
fn the_caps_seeds_are_at_the_caps() {
    for folded in [false, true] {
        let d = Definition::parse(&at_caps(folded)).expect("valid at the caps");
        assert_eq!(d.channels().len(), MAX_CHANNELS);
        assert_eq!(d.announcers().len(), MAX_ANNOUNCERS);
        assert_eq!(d.announcer_hosts().len(), MAX_ANNOUNCERS);
        // One more pattern is refused, so the four are at the cap.
        let over = at_caps(folded) + "pattern = (?P<name>.+)\n";
        assert!(Definition::parse(&over).is_err());
        assert_eq!(MAX_PATTERNS, 4);
    }
}

#[test]
fn the_fixed_definition_is_the_targets() {
    let target = include_str!("../../../fuzz/fuzz_targets/irc_announce.rs");
    let open = "const FIXED: &str = \"\\\n";
    let start = target.find(open).expect("FIXED") + open.len();
    let body = &target[start..start + target[start..].find("\";\n").expect("its end")];
    // The target's text is a Rust string literal: unescape `\\` as this
    // file's is.
    assert_eq!(body.replace("\\\\", "\\"), FIXED);
}

#[test]
fn the_fuzz_seeds_are_current() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/seeds/irc_announce");
    let seeds = seeds();
    for s in &seeds {
        assert!(s.bytes().len() < MAX_SEED, "{} is too big", s.name);
    }
    if std::env::var_os("LODESTAR_BLESS").is_some() {
        std::fs::create_dir_all(&dir).unwrap();
        for e in std::fs::read_dir(&dir).unwrap() {
            std::fs::remove_file(e.unwrap().path()).unwrap();
        }
        for s in &seeds {
            std::fs::write(dir.join(s.name), s.bytes()).unwrap();
        }
        return;
    }
    let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e} (write with LODESTAR_BLESS=1)", dir.display()))
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    on_disk.sort();
    let mut names: Vec<String> = seeds.iter().map(|s| s.name.to_owned()).collect();
    names.sort();
    assert_eq!(
        on_disk, names,
        "the folder's files (rewrite with LODESTAR_BLESS=1)"
    );
    for s in &seeds {
        let file = std::fs::read(dir.join(s.name)).unwrap();
        assert!(
            file == s.bytes(),
            "{} is stale (rewrite with LODESTAR_BLESS=1)",
            s.name
        );
    }
}
