//! Companion config files written before a managed companion's first
//! start (plan `docs/plan/2026-11-06-v06-companions.md` CMP-3;
//! architecture §18.10): qBittorrent's `qBittorrent.conf` and Prowlarr's
//! `config.xml`, with the credential and a `127.0.0.1` bind address.
//!
//! - **Only our keys.** A writer sets only the keys listed here. An
//!   existing file keeps everything else byte for byte; a duplicate of one
//!   of our keys is dropped, so each appears exactly once.
//! - **Malformed files are refused**, never overwritten blindly: a file
//!   that isn't UTF-8, holds NUL, a lone CR, a line continuation, a
//!   percent-encoded key, an unterminated section header, a DOCTYPE, or
//!   isn't one well-formed `<Config>` element.
//! - **No injection.** Values are closed types (an IP literal, a checked
//!   username, base64, hex), and XML is written by `quick-xml`, never by
//!   string formatting.
//! - **Atomic and private.** A temporary file in the same folder, fsync,
//!   rename (and on Unix an fsync of the folder). Mode `0600` on Unix; on
//!   Windows the file inherits the companion folder's ACL. A symlink or
//!   other non-regular file at the path is refused.
//!
//! qBittorrent's `WebUI\Password_PBKDF2` is its own format:
//! `@ByteArray(<salt>:<hash>)`, both standard base64, PBKDF2-HMAC-SHA512
//! with 100,000 iterations over a 16-byte salt from the OS RNG.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;

use lodestar_integrations::secret::Secret;
use quick_xml::events::{BytesEnd, BytesStart, BytesText, Event};
use quick_xml::{Reader, Writer};
use sha2::Sha512;
use zeroize::{Zeroize, Zeroizing};

use crate::credentials::{STANDARD, base64, hex, random_bytes, validate_api_key};
use crate::inputs::InputError;

/// Where companions listen: loopback only (§18.10).
pub const BIND_ADDRESS: &str = "127.0.0.1";
/// qBittorrent's PBKDF2 iteration count.
pub const QBITTORRENT_ITERATIONS: u32 = 100_000;
/// The WebUI username Lodestar sets unless told otherwise.
pub const DEFAULT_USERNAME: &str = "lodestar";
/// An existing config file larger than this is refused.
const MAX_EXISTING: u64 = 1 << 20;

/// Why a config file wasn't written. Carries no file content or secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    Io {
        op: &'static str,
        kind: io::ErrorKind,
    },
    /// The path is a symlink, folder or other non-regular file.
    NotAFile,
    /// The existing file is over 1 MiB.
    TooLarge,
    /// The existing file can't be safely updated. `line` is 1-based, when
    /// known.
    Malformed {
        reason: &'static str,
        line: Option<usize>,
    },
    /// A value we were given was refused.
    Rejected(InputError),
    /// The OS RNG failed.
    Random,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Io { op, kind } => write!(f, "couldn't {op} the config file: {kind}"),
            ConfigError::NotAFile => f.write_str("the config path isn't a regular file"),
            ConfigError::TooLarge => f.write_str("the existing config file is too large"),
            ConfigError::Malformed { reason, line } => {
                write!(f, "the existing config file is malformed: {reason}")?;
                match line {
                    Some(n) => write!(f, " (line {n})"),
                    None => Ok(()),
                }
            }
            ConfigError::Rejected(e) => write!(f, "a config value was refused: it is {e}"),
            ConfigError::Random => f.write_str("the system's random generator failed"),
        }
    }
}

impl std::error::Error for ConfigError {}

fn malformed(reason: &'static str, line: Option<usize>) -> ConfigError {
    ConfigError::Malformed { reason, line }
}

/// A qBittorrent WebUI username: 1–64 of `A–Z a–z 0–9 . _ -`, so it needs
/// no escaping in `qBittorrent.conf`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Username(String);

impl Username {
    pub fn parse(s: &str) -> Result<Username, InputError> {
        if s.is_empty() || s.len() > 64 {
            return Err(InputError::Length);
        }
        if !s
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
        {
            return Err(InputError::Character);
        }
        Ok(Username(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for Username {
    fn default() -> Username {
        Username(DEFAULT_USERNAME.to_owned())
    }
}

/// qBittorrent's stored form of a WebUI password.
pub struct PasswordHash {
    salt: [u8; 16],
    hash: [u8; 64],
}

impl PasswordHash {
    /// Hashes `password` with a fresh salt from the OS RNG.
    pub fn new(password: &Secret) -> Result<PasswordHash, ConfigError> {
        let salt = random_bytes::<16>().map_err(|_| ConfigError::Random)?;
        Ok(PasswordHash::derive(password, *salt))
    }

    /// The test-only seam: a fixed salt, so golden output is exact.
    #[cfg(any(test, feature = "testing"))]
    pub fn with_salt(password: &Secret, salt: [u8; 16]) -> PasswordHash {
        PasswordHash::derive(password, salt)
    }

    fn derive(password: &Secret, salt: [u8; 16]) -> PasswordHash {
        let mut hash = [0u8; 64];
        pbkdf2::pbkdf2_hmac::<Sha512>(
            password.expose().as_bytes(),
            &salt,
            QBITTORRENT_ITERATIONS,
            &mut hash,
        );
        PasswordHash { salt, hash }
    }

    /// `@ByteArray(<salt b64>:<hash b64>)`.
    pub fn to_qbittorrent(&self) -> String {
        format!(
            "@ByteArray({}:{})",
            base64(&self.salt, STANDARD, true),
            base64(&self.hash, STANDARD, true)
        )
    }
}

impl fmt::Debug for PasswordHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PasswordHash(…)")
    }
}

/// qBittorrent's keys, as logical paths (`/` and `\` alike), with the
/// spelling written into `[Preferences]` when a key is missing.
const QBIT_KEYS: [(&str, &str); 3] = [
    ("Preferences\\WebUI\\Address", "WebUI\\Address"),
    (
        "Preferences\\WebUI\\Password_PBKDF2",
        "WebUI\\Password_PBKDF2",
    ),
    ("Preferences\\WebUI\\Username", "WebUI\\Username"),
];

/// `qBittorrent.conf` with our three keys set, from the existing file's
/// text if there is one (Qt's INI format: sections as key prefixes,
/// `[General]` for the root). Qt matches keys case-sensitively on Unix and
/// case-insensitively on Windows, so a key or section that differs from
/// ours only in case is refused: on either platform we'd write the wrong
/// one.
pub fn qbittorrent_conf(
    existing: Option<&str>,
    username: &Username,
    hash: &PasswordHash,
) -> Result<String, ConfigError> {
    let values = [
        BIND_ADDRESS.to_owned(),
        format!("\"{}\"", hash.to_qbittorrent()),
        username.as_str().to_owned(),
    ];
    let (bom, text) = split_bom(existing.unwrap_or(""));
    if text.contains('\0') {
        return Err(malformed("contains NUL", None));
    }
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };

    let mut out: Vec<String> = Vec::new();
    let mut written = [false; 3];
    let mut section = String::new();
    // Where missing keys go: after the last entry of the first
    // `[Preferences]`.
    let mut insert_at: Option<usize> = None;
    let mut in_first_prefs = false;
    let mut seen_prefs = false;

    for (n, raw) in text.split_inclusive('\n').enumerate() {
        let line_no = Some(n + 1);
        let body = raw.strip_suffix('\n').unwrap_or(raw);
        let body = body.strip_suffix('\r').unwrap_or(body);
        if body.contains('\r') {
            return Err(malformed("a lone carriage return", line_no));
        }
        let t = ascii_trim(body);
        // Qt reads `\` and the character after it as a pair, so an odd
        // run of them at the end of a line joins it to the next one
        // (outside a `;` comment). `\\` at the end is an escaped
        // backslash, as in a Windows path.
        let trailing = body.bytes().rev().take_while(|&c| c == b'\\').count();
        if trailing % 2 == 1 && !t.starts_with(';') {
            return Err(malformed("a line continuation", line_no));
        }
        // A quoted value may run on past the newline; refuse any line with
        // an unbalanced unescaped `"` rather than track it.
        if !t.starts_with(';') && unescaped_quotes(body) % 2 == 1 {
            return Err(malformed("a quoted value spanning lines", line_no));
        }
        if let Some(rest) = t.strip_prefix('[') {
            let name = rest
                .split_once(']')
                .map(|(name, _)| name)
                .ok_or(malformed("an unterminated section header", line_no))?;
            if name.contains('%') && name != "%General" {
                return Err(malformed("a percent-encoded section name", line_no));
            }
            let name = ascii_trim(name);
            section = if name.eq_ignore_ascii_case("general") {
                String::new()
            } else {
                normalize(name)
            };
            if section != "Preferences" && section.eq_ignore_ascii_case("preferences") {
                return Err(malformed(
                    "a section differing from qBittorrent's in case",
                    line_no,
                ));
            }
            in_first_prefs = !seen_prefs && section == "Preferences";
            seen_prefs |= in_first_prefs;
            out.push(raw.to_owned());
            if in_first_prefs {
                insert_at = Some(out.len());
            }
            continue;
        }
        if t.is_empty() || t.starts_with(';') || !t.contains('=') {
            out.push(raw.to_owned());
            continue;
        }
        let key_text = ascii_trim(body.split_once('=').map_or(body, |(k, _)| k));
        if key_text.contains('%') {
            return Err(malformed("a percent-encoded key", line_no));
        }
        let logical = if section.is_empty() {
            normalize(key_text)
        } else {
            format!("{section}\\{}", normalize(key_text))
        };
        if QBIT_KEYS
            .iter()
            .any(|(k, _)| *k != logical && k.eq_ignore_ascii_case(&logical))
        {
            return Err(malformed(
                "a key differing from qBittorrent's in case",
                line_no,
            ));
        }
        match QBIT_KEYS.iter().position(|(k, _)| *k == logical) {
            Some(i) if written[i] => {} // a duplicate: dropped
            Some(i) => {
                written[i] = true;
                out.push(format!("{key_text}={}{eol}", values[i]));
            }
            None => out.push(raw.to_owned()),
        }
        if in_first_prefs {
            insert_at = Some(out.len());
        }
    }

    let missing: Vec<String> = (0..3)
        .filter(|&i| !written[i])
        .map(|i| format!("{}={}{eol}", QBIT_KEYS[i].1, values[i]))
        .collect();
    if !missing.is_empty() {
        // The last line may lack its newline.
        if let Some(last) = out.last_mut()
            && !last.ends_with('\n')
        {
            last.push_str(eol);
        }
        match insert_at {
            Some(at) => {
                out.splice(at..at, missing);
            }
            None => {
                if out.iter().any(|l| !ascii_trim(l).is_empty()) {
                    out.push(eol.to_owned());
                }
                out.push(format!("[Preferences]{eol}"));
                out.extend(missing);
            }
        }
    }
    Ok(format!("{bom}{}", out.concat()))
}

/// `/` as `\`: Qt reads both as the group separator.
fn normalize(s: &str) -> String {
    ascii_trim(s).replace('/', "\\")
}

/// `"` not escaped by a `\` (Qt reads `\` and the next character as a
/// pair).
fn unescaped_quotes(s: &str) -> usize {
    let mut n = 0;
    let mut bytes = s.bytes();
    while let Some(c) = bytes.next() {
        match c {
            b'\\' => {
                bytes.next();
            }
            b'"' => n += 1,
            _ => {}
        }
    }
    n
}

/// Qt trims only ASCII whitespace around sections, keys and values.
fn ascii_trim(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_ascii_whitespace())
}

fn split_bom(s: &str) -> (&'static str, &str) {
    match s.strip_prefix('\u{feff}') {
        Some(rest) => ("\u{feff}", rest),
        None => ("", s),
    }
}

/// Writes `qBittorrent.conf` at `path`: an existing file is updated, a
/// missing one created. The folder must exist.
pub fn write_qbittorrent(
    path: &Path,
    username: &Username,
    password: &Secret,
) -> Result<(), ConfigError> {
    let existing = read_existing(path)?;
    let text = qbittorrent_conf(
        existing.as_deref().map(String::as_str),
        username,
        &PasswordHash::new(password)?,
    )?;
    write_atomic(path, text.as_bytes())
}

/// Prowlarr's elements, children of `<Config>`, in the order they're
/// added when missing.
const PROWLARR_KEYS: [&str; 2] = ["BindAddress", "ApiKey"];

/// `config.xml` with `BindAddress` and `ApiKey` set, from the existing
/// file's text if there is one. The result holds the key, so it's a
/// [`Secret`].
pub fn prowlarr_config(existing: Option<&str>, api_key: &Secret) -> Result<Secret, ConfigError> {
    validate_api_key(api_key.expose()).map_err(ConfigError::Rejected)?;
    let values = [BIND_ADDRESS, api_key.expose()];
    // A new file is an empty `<Config>` with our elements added.
    let (bom, text) = split_bom(existing.unwrap_or("<Config>\n</Config>\n"));
    let mut reader = Reader::from_str(text);
    reader.config_mut().check_comments = true;
    let mut w = Writer::new(Vec::with_capacity(text.len() + 160));
    xml(w.get_mut().write_all(bom.as_bytes()))?;

    let mut depth = 0usize;
    let mut root_done = false;
    let mut written = [false; 2];
    // Inside one of our elements: its contents are replaced.
    let mut skipping = false;
    let mut ends_in_newline = true;
    loop {
        let event = reader
            .read_event()
            .map_err(|_| malformed("not well-formed XML", None))?;
        if skipping {
            match event {
                Event::End(_) => {
                    skipping = false;
                    depth -= 1;
                    continue;
                }
                Event::Text(_) | Event::CData(_) | Event::GeneralRef(_) | Event::Comment(_) => {
                    continue;
                }
                Event::Eof => return Err(malformed("an unclosed element", None)),
                _ => return Err(malformed("markup inside ApiKey or BindAddress", None)),
            }
        }
        match event {
            Event::Eof => break,
            Event::DocType(_) => return Err(malformed("a DOCTYPE", None)),
            Event::Start(_) | Event::Empty(_) if depth == 0 && root_done => {
                return Err(malformed("more than one root element", None));
            }
            Event::Start(e) if depth == 0 => {
                check_root(&e)?;
                xml(w.write_event(Event::Start(e)))?;
                depth = 1;
                ends_in_newline = false;
            }
            Event::Empty(e) if depth == 0 => {
                // `<Config/>`: expanded, with our elements inside.
                check_root(&e)?;
                xml(w.write_event(Event::Start(e)))?;
                close_root(&mut w, &written, &values, false)?;
                root_done = true;
            }
            Event::Start(e) if depth == 1 && our_key(&e).is_some() => {
                replace(&mut w, &e, &mut written, &values)?;
                depth += 1;
                skipping = true;
            }
            Event::Empty(e) if depth == 1 && our_key(&e).is_some() => {
                replace(&mut w, &e, &mut written, &values)?;
            }
            Event::Start(e) | Event::Empty(e) if is_ours_or_root(&e) => {
                // Prowlarr reads each key from `Descendants(key)` and resets
                // it unless there's exactly one, so ours may appear only as
                // children of the root.
                return Err(malformed("a nested Config, ApiKey or BindAddress", None));
            }
            Event::Start(e) => {
                depth += 1;
                xml(w.write_event(Event::Start(e)))?;
            }
            Event::End(e) => {
                depth -= 1;
                if depth == 0 {
                    close_root(&mut w, &written, &values, ends_in_newline)?;
                    root_done = true;
                    continue;
                }
                xml(w.write_event(Event::End(e)))?;
                ends_in_newline = false;
            }
            Event::Text(t) => {
                if depth == 0 && !t.chars().all(|c| c.is_ascii_whitespace()) {
                    return Err(malformed("text outside <Config>", None));
                }
                if depth == 1 {
                    ends_in_newline = t.ends_with('\n');
                }
                xml(w.write_event(Event::Text(t)))?;
            }
            Event::GeneralRef(_) | Event::CData(_) if depth == 0 => {
                return Err(malformed("text outside <Config>", None));
            }
            other => {
                if depth == 1 {
                    ends_in_newline = false;
                }
                xml(w.write_event(other))?;
            }
        }
    }
    if !root_done || depth != 0 {
        return Err(malformed("no complete <Config> element", None));
    }
    into_secret(Zeroizing::new(w.into_inner()))
}

fn is_ours_or_root(e: &BytesStart<'_>) -> bool {
    our_key(e).is_some() || e.name().into_inner() == "Config"
}

/// A root with attributes (a namespace above all) isn't one Prowlarr finds.
fn check_root(e: &BytesStart<'_>) -> Result<(), ConfigError> {
    if e.name().into_inner() == "Config" && e.attributes().next().is_none() {
        Ok(())
    } else {
        Err(malformed("the root element isn't <Config>", None))
    }
}

type XmlWriter = Writer<Vec<u8>>;

fn our_key(e: &BytesStart<'_>) -> Option<usize> {
    PROWLARR_KEYS
        .iter()
        .position(|k| e.name().into_inner() == *k)
}

/// Our element in place of the existing one; a duplicate is dropped.
fn replace(
    w: &mut XmlWriter,
    e: &BytesStart<'_>,
    written: &mut [bool; 2],
    values: &[&str; 2],
) -> Result<(), ConfigError> {
    if let Some(i) = our_key(e)
        && !written[i]
    {
        written[i] = true;
        element(w, PROWLARR_KEYS[i], values[i])?;
    }
    Ok(())
}

/// Adds whichever of our elements are missing, then `</Config>`.
fn close_root(
    w: &mut XmlWriter,
    written: &[bool; 2],
    values: &[&str; 2],
    ends_in_newline: bool,
) -> Result<(), ConfigError> {
    let mut first = true;
    for i in (0..2).filter(|&i| !written[i]) {
        let lead = if first && ends_in_newline {
            "  "
        } else {
            "\n  "
        };
        first = false;
        xml(w.write_event(Event::Text(BytesText::from_escaped(lead))))?;
        element(w, PROWLARR_KEYS[i], values[i])?;
    }
    if !first {
        xml(w.write_event(Event::Text(BytesText::from_escaped("\n"))))?;
    }
    xml(w.write_event(Event::End(BytesEnd::new("Config"))))
}

fn element(w: &mut XmlWriter, name: &str, value: &str) -> Result<(), ConfigError> {
    xml(w.write_event(Event::Start(BytesStart::new(name))))?;
    xml(w.write_event(Event::Text(BytesText::new(value))))?;
    xml(w.write_event(Event::End(BytesEnd::new(name))))
}

fn xml(r: io::Result<()>) -> Result<(), ConfigError> {
    r.map_err(|e| ConfigError::Io {
        op: "encode",
        kind: e.kind(),
    })
}

fn into_secret(mut bytes: Zeroizing<Vec<u8>>) -> Result<Secret, ConfigError> {
    let taken = std::mem::take(&mut *bytes);
    match String::from_utf8(taken) {
        Ok(s) => Ok(Secret::new(s)),
        Err(e) => {
            e.into_bytes().zeroize();
            Err(malformed("not UTF-8", None))
        }
    }
}

/// Writes Prowlarr's `config.xml` at `path`: an existing file is updated,
/// a missing one created. The folder must exist.
pub fn write_prowlarr(path: &Path, api_key: &Secret) -> Result<(), ConfigError> {
    let existing = read_existing(path)?;
    let text = prowlarr_config(existing.as_deref().map(String::as_str), api_key)?;
    write_atomic(path, text.expose().as_bytes())
}

fn io_err(op: &'static str) -> impl Fn(io::Error) -> ConfigError {
    move |e| ConfigError::Io { op, kind: e.kind() }
}

/// The existing file's text, `None` when there's none. It may hold an old
/// credential, so the buffer is zeroed on drop.
fn read_existing(path: &Path) -> Result<Option<Zeroizing<String>>, ConfigError> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_err("inspect")(e)),
    };
    if !meta.file_type().is_file() {
        return Err(ConfigError::NotAFile);
    }
    if meta.len() > MAX_EXISTING {
        return Err(ConfigError::TooLarge);
    }
    let mut buf = Zeroizing::new(Vec::new());
    File::open(path)
        .map_err(io_err("open"))?
        .take(MAX_EXISTING + 1)
        .read_to_end(&mut buf)
        .map_err(io_err("read"))?;
    if buf.len() as u64 > MAX_EXISTING {
        return Err(ConfigError::TooLarge);
    }
    match String::from_utf8(std::mem::take(&mut *buf)) {
        Ok(s) => Ok(Some(Zeroizing::new(s))),
        Err(e) => {
            e.into_bytes().zeroize();
            Err(malformed("not UTF-8", None))
        }
    }
}

/// Temp file in the same folder, fsync, rename; private permissions.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), ConfigError> {
    let name = path.file_name().ok_or(ConfigError::NotAFile)?;
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    let suffix = random_bytes::<8>().map_err(|_| ConfigError::Random)?;
    let tmp = dir.join(format!(".{}.{}.tmp", name.to_string_lossy(), hex(&*suffix)));
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(&tmp).map_err(io_err("create"))?;
    let result = file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(io_err("write"))
        .and_then(|()| {
            drop(file);
            fs::rename(&tmp, path).map_err(io_err("replace"))
        });
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result?;
    // Best effort, like the key file's: the rename has happened, so the
    // file is in place and an error here would wrongly say it isn't.
    #[cfg(unix)]
    let _ = File::open(dir).and_then(|d| d.sync_all());
    Ok(())
}
