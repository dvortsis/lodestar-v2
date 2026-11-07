//! CMP-3: companion credentials and the config files they're written into.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

use lodestar_companions::CompanionId;
use lodestar_companions::config_files::{
    ConfigError, PasswordHash, Username, prowlarr_config, qbittorrent_conf, write_prowlarr,
    write_qbittorrent,
};
use lodestar_companions::credentials::{
    CredentialKey, Field, accept_pasted, generate, is_secret_row, open, replace_temporary_password,
    reseal_rows,
};
use lodestar_integrations::secret::{Secret, SecretError, SecretStore};
use proptest::prelude::*;
use quick_xml::Reader;
use quick_xml::events::Event;

const KEY: &str = "0123456789abcdef0123456789abcdef";
const PASSWORD: &str = "correct-horse-battery-staple-0123456789AB";
const SALT: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
/// `base64(pbkdf2_hmac('sha512', PASSWORD, SALT, 100000))`, computed
/// independently with Python:
/// `base64.b64encode(hashlib.pbkdf2_hmac('sha512', b'correct-horse-battery-staple-0123456789AB', bytes(range(16)), 100000))`.
const HASH_B64: &str =
    "T5Bv4SU5QnrDJkAskXMzynKx1uD/ik4P907oojzlYs/Tg7l6ZUdYB3IwB3vCgFwJYD/EaP0IwWqe3CrUVpBy0g==";

fn key(id: &str, field: Field) -> CredentialKey {
    CredentialKey::new(CompanionId::parse(id).unwrap(), field)
}

fn fixed_hash() -> &'static PasswordHash {
    static HASH: OnceLock<PasswordHash> = OnceLock::new();
    HASH.get_or_init(|| PasswordHash::with_salt(&Secret::new(PASSWORD), SALT))
}

fn hash_value() -> String {
    format!("\"@ByteArray(AAECAwQFBgcICQoLDA0ODw==:{HASH_B64})\"")
}

/// A fresh folder under the system temp dir, removed on drop.
struct TestDir(PathBuf);

impl TestDir {
    fn new() -> TestDir {
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "lodestar-cmp3-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        TestDir(p)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---- qBittorrent ----

#[test]
fn qbittorrent_golden_new_file() {
    let out = qbittorrent_conf(None, &Username::default(), fixed_hash()).unwrap();
    assert_eq!(
        out,
        format!(
            "[Preferences]\nWebUI\\Address=127.0.0.1\nWebUI\\Password_PBKDF2={}\nWebUI\\Username=lodestar\n",
            hash_value()
        )
    );
}

/// qBittorrent's own documented default (`adminadmin`, as its wiki and
/// shipped configs give it) verifies with our derivation, so qBittorrent
/// accepts what we write.
#[test]
fn qbittorrent_accepts_the_format() {
    // Salt `ARQ77eY1NUZaQsuDHbIMCA==` decoded.
    let salt = [
        0x01, 0x14, 0x3b, 0xed, 0xe6, 0x35, 0x35, 0x46, 0x5a, 0x42, 0xcb, 0x83, 0x1d, 0xb2, 0x0c,
        0x08,
    ];
    let h = PasswordHash::with_salt(&Secret::new("adminadmin"), salt);
    assert_eq!(
        h.to_qbittorrent(),
        "@ByteArray(ARQ77eY1NUZaQsuDHbIMCA==:0WMRkYTUWVT9wVvdDtHAjU9b3b7uB8NR1Gur2hmQCvCDpm39Q+PsJRJPaCU51dEiz+dTzh8qbPsL8WkFljQYFQ==)"
    );
    assert_eq!(
        fixed_hash().to_qbittorrent(),
        format!("@ByteArray(AAECAwQFBgcICQoLDA0ODw==:{HASH_B64})")
    );
}

#[test]
fn qbittorrent_updates_only_its_keys() {
    let existing = "[Application]\nFileLogger\\Enabled=true\n\n[BitTorrent]\nSession\\Port=6881\n\n[Preferences]\nGeneral\\Locale=en\nwebui\\address=*\nWebUI\\Port=8080\n; a comment\n\n[LegalNotice]\nAccepted=true\n";
    let out = qbittorrent_conf(Some(existing), &Username::default(), fixed_hash()).unwrap();
    assert_eq!(
        out,
        format!(
            "[Application]\nFileLogger\\Enabled=true\n\n[BitTorrent]\nSession\\Port=6881\n\n[Preferences]\nGeneral\\Locale=en\nwebui\\address=127.0.0.1\nWebUI\\Port=8080\nWebUI\\Password_PBKDF2={}\nWebUI\\Username=lodestar\n; a comment\n\n[LegalNotice]\nAccepted=true\n",
            hash_value()
        )
    );
    // Running it again changes nothing.
    assert_eq!(
        qbittorrent_conf(Some(&out), &Username::default(), fixed_hash()).unwrap(),
        out
    );
}

#[test]
fn qbittorrent_finds_its_keys_however_qt_spells_them() {
    // Root keys with the section as a prefix, `/` separators, a
    // `[Preferences\WebUI]` group, a duplicate and CRLF line ends.
    let existing = "[General]\r\nPreferences/WebUI/Address=0.0.0.0\r\n[Preferences\\WebUI]\r\nUsername=admin\r\n[Preferences]\r\nWebUI\\Username=other\r\nWebUI\\Password_PBKDF2=\"@ByteArray(old)\"\r\n";
    let out = qbittorrent_conf(Some(existing), &Username::default(), fixed_hash()).unwrap();
    assert_eq!(
        out,
        format!(
            "[General]\r\nPreferences/WebUI/Address=127.0.0.1\r\n[Preferences\\WebUI]\r\nUsername=lodestar\r\n[Preferences]\r\nWebUI\\Password_PBKDF2={}\r\n",
            hash_value()
        )
    );
}

#[test]
fn an_escaped_trailing_backslash_is_not_a_continuation() {
    let existing = "[BitTorrent]\nSession\\DefaultSavePath=C:\\\\Downloads\\\\\n; ends in one \\\n";
    let out = qbittorrent_conf(Some(existing), &Username::default(), fixed_hash()).unwrap();
    assert!(out.starts_with(existing), "{out}");
}

#[test]
fn qbittorrent_adds_a_section_to_a_file_without_one() {
    let out = qbittorrent_conf(
        Some("[BitTorrent]\nSession\\Port=6881"),
        &Username::parse("me").unwrap(),
        fixed_hash(),
    )
    .unwrap();
    assert_eq!(
        out,
        format!(
            "[BitTorrent]\nSession\\Port=6881\n\n[Preferences]\nWebUI\\Address=127.0.0.1\nWebUI\\Password_PBKDF2={}\nWebUI\\Username=me\n",
            hash_value()
        )
    );
}

#[test]
fn qbittorrent_refuses_malformed_files() {
    for (bad, line) in [
        (
            "[Preferences]\nWebUI\\Port=8080\\\nWebUI\\Address=*\n",
            Some(2),
        ),
        (
            "[Preferences]\nWebUI\\Port=8080\rWebUI\\Address=*\n",
            Some(2),
        ),
        ("[Preferences\nWebUI\\Address=*\n", Some(1)),
        ("[Preferences]\nWebUI%5CAddress=*\n", Some(2)),
        ("[Pref%65rences]\nWebUI\\Address=*\n", Some(1)),
        ("[Preferences]\nWebUI\\Address=*\0\n", None),
    ] {
        match qbittorrent_conf(Some(bad), &Username::default(), fixed_hash()) {
            Err(ConfigError::Malformed { line: l, .. }) => assert_eq!(l, line, "{bad:?}"),
            other => panic!("{bad:?}: {other:?}"),
        }
    }
}

#[test]
fn usernames_are_checked() {
    for bad in ["", "a b", "a\nb", "[x]", "a=b", "ä", &"x".repeat(65)] {
        assert!(Username::parse(bad).is_err(), "{bad:?}");
    }
    assert_eq!(
        Username::parse("Admin_1.x-y").unwrap().as_str(),
        "Admin_1.x-y"
    );
}

// ---- Prowlarr ----

const PROWLARR_EXISTING: &str = "<Config>\n  <BindAddress>*</BindAddress>\n  <Port>9696</Port>\n  <SslPort>6969</SslPort>\n  <EnableSsl>False</EnableSsl>\n  <LaunchBrowser>True</LaunchBrowser>\n  <ApiKey>ffffffffffffffffffffffffffffffff</ApiKey>\n  <AuthenticationMethod>None</AuthenticationMethod>\n  <Branch>master</Branch>\n  <UrlBase></UrlBase>\n  <InstanceName>Prowl &amp; co</InstanceName>\n  <!-- kept -->\n</Config>";

#[test]
fn prowlarr_golden_new_file() {
    let out = prowlarr_config(None, &Secret::new(KEY)).unwrap();
    assert_eq!(
        out.expose(),
        format!(
            "<Config>\n  <BindAddress>127.0.0.1</BindAddress>\n  <ApiKey>{KEY}</ApiKey>\n</Config>\n"
        )
    );
}

#[test]
fn prowlarr_updates_only_its_keys() {
    let out = prowlarr_config(Some(PROWLARR_EXISTING), &Secret::new(KEY)).unwrap();
    let want = PROWLARR_EXISTING
        .replace("<BindAddress>*", "<BindAddress>127.0.0.1")
        .replace("ffffffffffffffffffffffffffffffff", KEY);
    assert_eq!(out.expose(), want);
    assert_eq!(
        prowlarr_config(Some(out.expose()), &Secret::new(KEY))
            .unwrap()
            .expose(),
        want
    );
}

#[test]
fn prowlarr_adds_missing_keys_and_drops_duplicates() {
    let out = prowlarr_config(
        Some("<?xml version=\"1.0\"?>\n<Config>\n  <Port>9696</Port>\n  <ApiKey/>\n  <ApiKey>old</ApiKey>\n</Config>\n"),
        &Secret::new(KEY),
    )
    .unwrap();
    assert_eq!(
        out.expose(),
        format!(
            "<?xml version=\"1.0\"?>\n<Config>\n  <Port>9696</Port>\n  <ApiKey>{KEY}</ApiKey>\n  \n  <BindAddress>127.0.0.1</BindAddress>\n</Config>\n"
        )
    );
    let out = prowlarr_config(Some("<Config/>"), &Secret::new(KEY)).unwrap();
    assert_eq!(
        out.expose(),
        format!(
            "<Config>\n  <BindAddress>127.0.0.1</BindAddress>\n  <ApiKey>{KEY}</ApiKey>\n</Config>"
        )
    );
}

#[test]
fn prowlarr_refuses_malformed_files_and_bad_keys() {
    for bad in [
        "",
        "<Other></Other>",
        "<Config></Config><Config></Config>",
        "<Config><Port>1</Port>",
        "<Config><ApiKey><x/></ApiKey></Config>",
        "<Config></Port></Config>",
        "junk<Config></Config>",
        "<!DOCTYPE Config [<!ENTITY x \"y\">]><Config></Config>",
        "<Config><!-- a -- b --></Config>",
    ] {
        assert!(
            matches!(
                prowlarr_config(Some(bad), &Secret::new(KEY)),
                Err(ConfigError::Malformed { .. })
            ),
            "{bad:?}"
        );
    }
    for bad in [
        "short",
        "0123456789abcdef0123456789abcde<",
        "</ApiKey><x>0123456789abcdef0123",
    ] {
        assert!(matches!(
            prowlarr_config(None, &Secret::new(bad)),
            Err(ConfigError::Rejected(_))
        ));
    }
}

// ---- Files ----

#[test]
fn files_are_written_atomically_and_privately() {
    let dir = TestDir::new();
    let qbit = dir.path().join("qBittorrent.conf");
    std::fs::write(&qbit, "[BitTorrent]\nSession\\Port=6881\n").unwrap();
    write_qbittorrent(&qbit, &Username::default(), &Secret::new(PASSWORD)).unwrap();
    let text = std::fs::read_to_string(&qbit).unwrap();
    assert!(text.starts_with("[BitTorrent]\nSession\\Port=6881\n\n[Preferences]\n"));
    assert!(text.contains("WebUI\\Password_PBKDF2=\"@ByteArray("));
    assert!(!text.contains(PASSWORD));

    let prowlarr = dir.path().join("config.xml");
    write_prowlarr(&prowlarr, &Secret::new(KEY)).unwrap();
    assert!(std::fs::read_to_string(&prowlarr).unwrap().contains(KEY));

    // No temporary file is left behind.
    let mut names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["config.xml", "qBittorrent.conf"]);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for p in [&qbit, &prowlarr] {
            let mode = std::fs::metadata(p).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "{p:?}");
        }
    }
}

#[test]
fn a_bad_existing_file_is_left_alone() {
    let dir = TestDir::new();
    let p = dir.path().join("config.xml");
    std::fs::write(&p, "<Config><Port>1</Port>").unwrap();
    assert!(matches!(
        write_prowlarr(&p, &Secret::new(KEY)),
        Err(ConfigError::Malformed { .. })
    ));
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        "<Config><Port>1</Port>"
    );

    std::fs::write(&p, vec![b'<'; (1 << 20) + 1]).unwrap();
    assert_eq!(
        write_prowlarr(&p, &Secret::new(KEY)),
        Err(ConfigError::TooLarge)
    );
    std::fs::write(&p, [0xff, 0xfe]).unwrap();
    assert!(matches!(
        write_prowlarr(&p, &Secret::new(KEY)),
        Err(ConfigError::Malformed { .. })
    ));

    let sub = dir.path().join("folder");
    std::fs::create_dir(&sub).unwrap();
    assert_eq!(
        write_prowlarr(&sub, &Secret::new(KEY)),
        Err(ConfigError::NotAFile)
    );
    let missing = dir.path().join("nope").join("config.xml");
    assert!(matches!(
        write_prowlarr(&missing, &Secret::new(KEY)),
        Err(ConfigError::Io { .. })
    ));
}

#[cfg(unix)]
#[test]
fn a_symlink_is_refused() {
    let dir = TestDir::new();
    let target = dir.path().join("elsewhere.xml");
    std::fs::write(&target, "<Config></Config>").unwrap();
    let link = dir.path().join("config.xml");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert_eq!(
        write_prowlarr(&link, &Secret::new(KEY)),
        Err(ConfigError::NotAFile)
    );
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "<Config></Config>"
    );
}

// ---- Sealing and rotation ----

#[test]
fn rotation_reseals_companion_rows_and_they_still_open() {
    let dir = TestDir::new();
    let mut store = SecretStore::open_or_create(dir.path()).unwrap();
    let qbit = key("qbittorrent", Field::WebUiPassword);
    let prowlarr = key("prowlarr", Field::ApiKey);
    let a = generate(&store, &qbit).unwrap();
    let b = generate(&store, &prowlarr).unwrap();
    let other = store.seal("source:x", &Secret::new("x")).unwrap();
    let rows = [
        (a.row.key.clone(), a.row.value.clone()),
        (b.row.key.clone(), b.row.value.clone()),
        ("sources.secret.x/api_key".to_owned(), other),
    ];
    let view = || rows.iter().map(|(k, v)| (k.as_str(), v));

    // Nothing to do before a rotation.
    assert!(reseal_rows(&store, view()).unwrap().is_empty());

    store.rotate().unwrap();
    let resealed = reseal_rows(&store, view()).unwrap();
    assert_eq!(
        resealed.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(),
        [qbit.row_key(), prowlarr.row_key()]
    );
    store.retire_old_keys().unwrap();
    let store = SecretStore::open_or_create(dir.path()).unwrap();
    for (k, issued, row) in [(&qbit, &a, &resealed[0]), (&prowlarr, &b, &resealed[1])] {
        assert_eq!(
            open(&store, k, &row.value).unwrap().expose(),
            issued.secret.expose()
        );
        // The old row no longer opens: without the reseal it'd be lost.
        assert!(matches!(
            open(&store, k, &issued.row.value),
            Err(lodestar_companions::credentials::CredentialError::Sealed(
                SecretError::UnknownKey(_)
            ))
        ));
    }
}

#[test]
fn a_malformed_companion_secret_row_stops_the_reseal() {
    let store = SecretStore::from_key(&[1; 32]);
    let sealed = store.seal("x", &Secret::new("x")).unwrap();
    assert!(is_secret_row("companions.BAD.secret.x"));
    assert!(reseal_rows(&store, [("companions.BAD.secret.x", &sealed)]).is_err());
}

// ---- Never logged ----

struct Capture(Mutex<Vec<String>>);

impl lodestar_log::Sink for Capture {
    fn record(&self, record: &lodestar_log::Record<'_>) {
        self.0.lock().unwrap().push(format!(
            "{record:?} {}",
            record.text(lodestar_log::Mask::NONE, None)
        ));
    }
}

#[test]
fn no_plaintext_reaches_logs_or_debug_output() {
    static SINK: OnceLock<Capture> = OnceLock::new();
    let sink = SINK.get_or_init(|| Capture(Mutex::new(Vec::new())));
    let _ = lodestar_log::set_sink(sink);

    let dir = TestDir::new();
    let store = SecretStore::from_key(&[3; 32]);
    let qbit = key("qbittorrent", Field::WebUiPassword);
    let prowlarr = key("prowlarr", Field::ApiKey);
    let mut plaintext = Vec::new();
    let mut shown = Vec::new();

    let a = generate(&store, &qbit).unwrap();
    let b = generate(&store, &prowlarr).unwrap();
    let pasted = accept_pasted(&store, &qbit, Secret::new("Tmp4Pass9x")).unwrap();
    let change = replace_temporary_password(&store, &qbit, &pasted.value).unwrap();
    let again = generate(&store, &prowlarr).unwrap();
    let refused = accept_pasted(&store, &prowlarr, Secret::new("REFUSED-SECRET-VALUE"));
    let hash = PasswordHash::new(&a.secret).unwrap();
    let qpath = dir.path().join("qBittorrent.conf");
    let ppath = dir.path().join("config.xml");
    write_qbittorrent(&qpath, &Username::default(), &change.next.secret).unwrap();
    write_prowlarr(&ppath, &again.secret).unwrap();
    let bad_xml = prowlarr_config(
        Some(&format!(
            "<Config><ApiKey>{}</ApiKey>",
            again.secret.expose()
        )),
        &b.secret,
    );

    for s in [
        &a.secret,
        &b.secret,
        &change.temporary,
        &change.next.secret,
        &again.secret,
    ] {
        plaintext.push(s.expose().to_owned());
    }
    plaintext.push("REFUSED-SECRET-VALUE".to_owned());
    shown.push(format!(
        "{a:?} {b:?} {pasted:?} {change:?} {again:?} {hash:?}"
    ));
    shown.push(format!("{refused:?} {}", refused.as_ref().unwrap_err()));
    shown.push(format!("{bad_xml:?} {}", bad_xml.as_ref().unwrap_err()));
    shown.push(std::fs::read_to_string(&qpath).unwrap());
    shown.extend(sink.0.lock().unwrap().iter().cloned());

    for p in &plaintext {
        for s in &shown {
            assert!(!s.contains(p.as_str()), "plaintext found in {s:?}");
        }
    }
}

// ---- Property tests ----

/// The `(logical key, value)` entries of a Qt INI text, in order.
fn ini_entries(text: &str) -> Vec<(String, String)> {
    let mut section = String::new();
    let mut out = Vec::new();
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let trim = |s: &str| s.trim_matches(|c: char| c.is_ascii_whitespace()).to_owned();
    for line in text.lines() {
        let t = trim(line);
        let t = t.as_str();
        if let Some(rest) = t.strip_prefix('[') {
            let name = rest.split_once(']').unwrap().0;
            section = if name.eq_ignore_ascii_case("general") {
                String::new()
            } else {
                format!("{}\\", trim(name).replace('/', "\\").to_ascii_lowercase())
            };
        } else if let Some((k, v)) = t.split_once('=')
            && !t.starts_with(';')
            && !t.starts_with('#')
        {
            let k = trim(k).replace('/', "\\").to_ascii_lowercase();
            out.push((format!("{section}{k}"), v.to_owned()));
        }
    }
    out
}

const OURS: [&str; 3] = [
    "preferences\\webui\\address",
    "preferences\\webui\\password_pbkdf2",
    "preferences\\webui\\username",
];

fn ini_line() -> impl Strategy<Value = String> {
    prop_oneof![
        Just(String::new()),
        "; [a-zA-Z0-9 =]{0,12}".prop_map(|s| s),
        "\\[(Preferences|BitTorrent|Application|General|Core)\\]".prop_map(|s| s),
        "(WebUI\\\\(Address|Port|Username|Password_PBKDF2|Locale)|Session\\\\Port|Accepted)=[a-zA-Z0-9*.,\" ]{0,12}"
            .prop_map(|s| s),
    ]
}

/// A mini Prowlarr config: child elements with simple text.
fn xml_children() -> impl Strategy<Value = Vec<(String, String)>> {
    prop::collection::vec(
        (
            "(Port|ApiKey|BindAddress|Branch|LogLevel|UrlBase)",
            "[a-zA-Z0-9 *.]{0,10}",
        ),
        0..8,
    )
}

fn xml_children_of(text: &str) -> Vec<(String, String)> {
    let mut r = Reader::from_str(text);
    let mut out = Vec::new();
    let mut depth = 0;
    let mut name = String::new();
    loop {
        match r.read_event().unwrap() {
            Event::Start(e) => {
                depth += 1;
                if depth == 2 {
                    name = e.name().into_inner().to_owned();
                    out.push((name.clone(), String::new()));
                }
            }
            Event::Empty(e) if depth == 1 => {
                out.push((e.name().into_inner().to_owned(), String::new()));
            }
            Event::Text(t) if depth == 2 => {
                out.last_mut().unwrap().1 = t.xml10_content().into_owned();
            }
            Event::End(_) => depth -= 1,
            Event::Eof => break,
            _ => {}
        }
    }
    let _ = name;
    out
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn qbittorrent_output_parses_with_our_keys_once(lines in prop::collection::vec(ini_line(), 0..20)) {
        let existing = lines.join("\n");
        let out = qbittorrent_conf(Some(&existing), &Username::default(), fixed_hash()).unwrap();
        let entries = ini_entries(&out);
        let want = ["127.0.0.1".to_owned(), hash_value(), "lodestar".to_owned()];
        for (k, v) in OURS.iter().zip(&want) {
            let found: Vec<_> = entries.iter().filter(|(e, _)| e == k).collect();
            prop_assert_eq!(found.len(), 1, "{}", out);
            prop_assert_eq!(&found[0].1, v);
        }
        // Everything else is unchanged and in order.
        let others = |text: &str| ini_entries(text).into_iter().filter(|(k, _)| !OURS.contains(&k.as_str())).collect::<Vec<_>>();
        prop_assert_eq!(others(&out), others(&existing));
    }

    #[test]
    fn qbittorrent_takes_any_bytes_or_refuses(text in prop_oneof![
        "(?s).{0,200}",
        "(\\[|\\]|=|;|%|\\\\|/|\r|\n|\u{feff}| |Preferences|General|WebUI|Address|Username|Password_PBKDF2|x){0,60}",
    ]) {
        if let Ok(out) = qbittorrent_conf(Some(&text), &Username::default(), fixed_hash()) {
            // Whatever it accepts, it can read back with our keys once.
            let entries = ini_entries(&out);
            for k in OURS {
                prop_assert_eq!(entries.iter().filter(|(e, _)| e == k).count(), 1, "{:?}", out);
            }
        }
    }

    #[test]
    fn usernames_are_refused_or_round_trip(s in "(?s).{0,70}") {
        if let Ok(u) = Username::parse(&s) {
            let out = qbittorrent_conf(None, &u, fixed_hash()).unwrap();
            let entries = ini_entries(&out);
            prop_assert!(entries.contains(&(OURS[2].to_owned(), s.clone())));
            prop_assert_eq!(entries.len(), 3);
        }
    }

    #[test]
    fn prowlarr_output_parses_with_our_keys_once(children in xml_children()) {
        let mut existing = String::from("<Config>");
        for (n, v) in &children {
            existing.push_str(&format!("\n  <{n}>{v}</{n}>"));
        }
        existing.push_str("\n</Config>");
        let out = prowlarr_config(Some(&existing), &Secret::new(KEY)).unwrap();
        let got = xml_children_of(out.expose());
        for (name, value) in [("BindAddress", "127.0.0.1"), ("ApiKey", KEY)] {
            let found: Vec<_> = got.iter().filter(|(n, _)| n == name).collect();
            prop_assert_eq!(found.len(), 1);
            prop_assert_eq!(found[0].1.as_str(), value);
        }
        let others = |v: Vec<(String, String)>| v.into_iter().filter(|(n, _)| n != "ApiKey" && n != "BindAddress").collect::<Vec<_>>();
        prop_assert_eq!(others(got), others(children));
    }

    #[test]
    fn prowlarr_takes_any_text_or_refuses(text in prop_oneof![
        "(?s).{0,200}",
        "(<|>|/|=|\"|&|;|!|-|\\?| |\n|Config|ApiKey|BindAddress|Port|amp|x){0,60}",
    ]) {
        if let Ok(out) = prowlarr_config(Some(&text), &Secret::new(KEY)) {
            let got = xml_children_of(out.expose());
            prop_assert_eq!(got.iter().filter(|(n, _)| n == "ApiKey").count(), 1);
        }
    }

    #[test]
    fn api_keys_are_refused_or_round_trip(s in "(?s).{0,40}") {
        if let Ok(out) = prowlarr_config(None, &Secret::new(s.clone())) {
            let got = xml_children_of(out.expose());
            prop_assert!(got.contains(&("ApiKey".to_owned(), s.clone())));
        }
    }
}
