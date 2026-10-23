//! The log folder as the API reads it (architecture §12.1, §10.10.12):
//! which folder, the files in it, their badges, and their content as a
//! download.
//!
//! - **Which folder:** the one the file opened in this run. If file
//!   logging hasn't opened, the configured folder, only if it's an existing
//!   directory (not a link) that, on Unix, is Lodestar's account's and
//!   isn't group- or world-writable. Never created or changed from here.
//! - **Names are untrusted** (`names.rs`), and so is `index.json`: it
//!   supplies only flags, looked up by a validated name. An entry that
//!   names anything else, or has the wrong types, only loses its badge. An
//!   index that can't be read makes every file count as having details.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use flate2::bufread::MultiGzDecoder;
use lodestar_api::logs::LogFormat;
use serde_json::Value as Json;

use super::file::{Flags, INDEX};
use super::names::{LogName, open_regular};
use super::read::{self, read_line};
use super::view::{self, REDACT, UNREADABLE};
use super::zip::Content;

/// The folder the API reads, or why there's none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub dir: PathBuf,
}

/// The folder: the runtime's, else a configured one that passes the
/// checks. `Err` names why there's none.
pub fn folder(opened: Option<PathBuf>, configured: &Path) -> Result<Folder, &'static str> {
    if let Some(dir) = opened {
        return Ok(Folder { dir });
    }
    let meta = match std::fs::symlink_metadata(configured) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err("not_opened"),
        Err(_) => return Err("unreadable"),
    };
    if !meta.file_type().is_dir() {
        return Err("not_a_folder");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != rustix::process::geteuid().as_raw() || meta.mode() & 0o022 != 0 {
            return Err("not_private");
        }
    }
    #[cfg(windows)]
    if !windows_private(configured) {
        return Err("not_private");
    }
    Ok(Folder {
        dir: configured.to_path_buf(),
    })
}

/// Whether a configured folder's owner and DACL keep it private
/// (`windows::acl::log_folder`), cached per folder for the run: the check
/// starts PowerShell. Never changes the folder.
#[cfg(windows)]
fn windows_private(dir: &Path) -> bool {
    use std::collections::HashMap as Map;
    use std::sync::Mutex;
    static CHECKED: Mutex<Option<Map<PathBuf, bool>>> = Mutex::new(None);
    let mut checked = CHECKED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(&private) = checked.as_ref().and_then(|m| m.get(dir)) {
        return private;
    }
    let private = crate::windows::acl::log_folder(dir).is_ok();
    checked
        .get_or_insert_with(Map::new)
        .insert(dir.to_path_buf(), private);
    private
}

/// `index.json`, read as untrusted input.
#[derive(Debug, Clone, Default)]
pub struct IndexView {
    pub readable: bool,
    files: HashMap<String, Flags>,
}

impl IndexView {
    /// A file's flags; every file has details when the index is
    /// unreadable (fail closed).
    pub fn flags(&self, name: &LogName) -> Flags {
        match self.readable {
            false => Flags {
                details: true,
                opt_in: true,
            },
            // With its `.jsonl`/`.gz` other self's: a file whose badge
            // moved while it was compressed keeps it.
            true => {
                let get = |n: &LogName| self.files.get(&n.file_name()).copied().unwrap_or_default();
                let f = get(name);
                match name.sibling().map(|o| get(&o)) {
                    Some(o) => Flags {
                        details: f.details || o.details,
                        opt_in: f.opt_in || o.opt_in,
                    },
                    None => f,
                }
            }
        }
    }
}

/// The most of `index.json` read.
const INDEX_MAX: u64 = 4 << 20;

pub fn read_index(dir: &Path) -> IndexView {
    let unreadable = IndexView::default();
    let file = match open_regular(&dir.join(INDEX)) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return IndexView {
                readable: true,
                files: HashMap::new(),
            };
        }
        Err(_) => return unreadable,
    };
    let mut bytes = Vec::new();
    if file.take(INDEX_MAX + 1).read_to_end(&mut bytes).is_err() || bytes.len() as u64 > INDEX_MAX {
        return unreadable;
    }
    let Ok(Json::Object(root)) = serde_json::from_slice::<Json>(&bytes) else {
        return unreadable;
    };
    let mut files = HashMap::new();
    if let Some(Json::Object(entries)) = root.get("files") {
        for (name, v) in entries {
            let Some(parsed) = LogName::parse(name) else {
                continue;
            };
            let Json::Object(f) = v else { continue };
            let flag = |k: &str| match f.get(k) {
                None => Some(false),
                Some(Json::Bool(b)) => Some(*b),
                Some(_) => None,
            };
            if let (Some(details), Some(opt_in)) = (flag("details"), flag("opt_in")) {
                files.insert(parsed.file_name(), Flags { details, opt_in });
            }
        }
    }
    IndexView {
        readable: true,
        files,
    }
}

/// A file in the folder by a log's name.
#[derive(Debug, Clone)]
pub struct Listed {
    pub name: LogName,
    pub size: u64,
    pub modified: Option<SystemTime>,
    /// A plain file: anything else (a link with a log's name) is never
    /// read or followed.
    pub regular: bool,
}

/// Every entry named like a log, newest first.
///
/// The folder is scanned twice and the two joined: a scan can miss a file
/// whose compression finishes during it (its new `.gz` lands behind the
/// scan, and its `.jsonl` goes before the scan reaches it), and
/// compressions are one at a time and far slower than a scan, so a file
/// one scan missed the other has.
pub fn list(dir: &Path) -> io::Result<Vec<Listed>> {
    let mut out = scan(dir)?;
    out.extend(scan(dir)?);
    out.sort_by(|a, b| LogName::newest_first(&a.name, &b.name));
    // While a rotated file is compressed, its `.jsonl` and `.jsonl.gz`
    // are both there for a moment: one file, read once (the `.gz`, which
    // sorts first and holds the same lines at the same offsets). And each
    // scan lists most files once more.
    out.dedup_by(|later, kept| match (&later.name, &kept.name) {
        (LogName::Rotated { stamp: a, n: x, .. }, LogName::Rotated { stamp: b, n: y, .. }) => {
            a == b && x == y
        }
        (LogName::Current, LogName::Current) => true,
        _ => false,
    });
    Ok(out)
}

fn scan(dir: &Path) -> io::Result<Vec<Listed>> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir)? {
        let Ok(e) = e else { continue };
        let Some(name) = e.file_name().to_str().and_then(LogName::parse) else {
            continue;
        };
        // Not followed: a link reports itself.
        let Ok(meta) = e.metadata() else { continue };
        out.push(Listed {
            name,
            size: meta.len(),
            modified: meta.modified().ok(),
            regular: meta.file_type().is_file(),
        });
    }
    Ok(out)
}

/// The first readable line's time (at most 64 KiB read, decompressed).
pub fn first_ts(dir: &Path, name: &LogName) -> Option<String> {
    let mut f = open_regular(&name.path(dir)).ok()?;
    let line = read::first_line(&mut f, name.gz()).ok()??;
    let v: Json = serde_json::from_slice(&line).ok()?;
    let ts = v.get("ts")?.as_str()?;
    lodestar_api::logs::parse_rfc3339(ts).map(|_| ts.to_string())
}

/// Opens a listed file: a rotated `.jsonl` compressed since the listing
/// is its `.gz`, with the same lines at the same offsets.
pub fn open_listed(dir: &Path, name: &LogName) -> io::Result<(LogName, std::fs::File)> {
    match open_regular(&name.path(dir)) {
        Ok(f) => Ok((name.clone(), f)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => match name.compressed() {
            Some(gz) => {
                let f = open_regular(&gz.path(dir))?;
                Ok((gz, f))
            }
            None => Err(e),
        },
        Err(e) => Err(e),
    }
}

/// A reader of `name`'s bytes as written (decompressed), up to its
/// length now: the current file to its last whole line.
pub fn open_plain(dir: &Path, name: &LogName) -> io::Result<Box<dyn BufRead + Send>> {
    open_capped(dir, name, read::MAX_PLAIN)
}

fn open_capped(dir: &Path, name: &LogName, cap: u64) -> io::Result<Box<dyn BufRead + Send>> {
    let (name, mut file) = open_listed(dir, name)?;
    Ok(match name {
        LogName::Current => {
            let len = file.metadata()?.len();
            let end = read::whole_lines(&mut file, len)?;
            use std::io::Seek;
            file.rewind()?;
            Box::new(BufReader::with_capacity(64 << 10, file.take(end)))
        }
        // Untrusted when read back: never inflated past the most a log
        // file holds (a bomb ends there, as history ends it).
        LogName::Rotated { gz: true, .. } => Box::new(BufReader::with_capacity(
            64 << 10,
            Capped {
                inner: MultiGzDecoder::new(BufReader::with_capacity(64 << 10, file)),
                left: cap,
            },
        )),
        LogName::Rotated { gz: false, .. } => Box::new(BufReader::with_capacity(64 << 10, file)),
    })
}

/// A decompressed file read up to `left` bytes; one that holds more is an
/// error there, so a download fails and an archive member says it was cut
/// short, rather than ending as if the file ended.
struct Capped<R> {
    inner: R,
    left: u64,
}

impl<R: Read> Read for Capped<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.left == 0 {
            return match self.inner.read(&mut [0u8; 1])? {
                0 => Ok(0),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "a log file larger than any Lodestar writes",
                )),
            };
        }
        let max = buf
            .len()
            .min(usize::try_from(self.left).unwrap_or(usize::MAX));
        let n = self.inner.read(&mut buf[..max])?;
        self.left -= n as u64;
        Ok(n)
    }
}

/// A download's file name: ASCII, from the typed name.
pub fn download_name(name: &LogName, format: LogFormat, redact: bool) -> String {
    format!(
        "{}{}.{}",
        name.stem(),
        if redact { "-redacted" } else { "" },
        match format {
            LogFormat::Jsonl => "jsonl",
            LogFormat::Text => "txt",
        }
    )
}

/// A file's content as a download: as written (JSONL, unredacted), or
/// line by line through the redaction rules, or as text.
pub fn content(r: Box<dyn BufRead + Send>, format: LogFormat, redact: bool) -> Content {
    match (format, redact) {
        (LogFormat::Jsonl, false) => Box::new(Raw { r }),
        _ => Box::new(Lines {
            r,
            format,
            redact,
            line: Vec::new(),
            done: false,
        }),
    }
}

const CHUNK: usize = 64 << 10;

struct Raw {
    r: Box<dyn BufRead + Send>,
}

impl Iterator for Raw {
    type Item = io::Result<Vec<u8>>;

    fn next(&mut self) -> Option<io::Result<Vec<u8>>> {
        let mut buf = vec![0u8; CHUNK];
        match self.r.read(&mut buf) {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some(Ok(buf))
            }
            Err(e) => Some(Err(e)),
        }
    }
}

struct Lines {
    r: Box<dyn BufRead + Send>,
    format: LogFormat,
    redact: bool,
    line: Vec<u8>,
    done: bool,
}

impl Lines {
    /// One line converted, or `None` to leave it out.
    fn convert(&self, over: bool) -> Option<String> {
        if over {
            return Some(UNREADABLE.to_string());
        }
        let parsed = view::file_line(&self.line, self.redact.then_some(REDACT));
        match (parsed, self.format, self.redact) {
            (Ok(None), _, _) => None,
            (Ok(Some(l)), LogFormat::Jsonl, _) => Some(view::to_json(&l)),
            (Ok(Some(l)), LogFormat::Text, _) => Some(view::to_text(&l)),
            // Redacted output never passes through bytes it couldn't parse.
            (Err(()), _, true) => Some(UNREADABLE.to_string()),
            (Err(()), _, false) => Some(view::raw_text(&self.line)),
        }
    }
}

impl Iterator for Lines {
    type Item = io::Result<Vec<u8>>;

    fn next(&mut self) -> Option<io::Result<Vec<u8>>> {
        if self.done {
            return None;
        }
        let mut out = Vec::with_capacity(CHUNK + 1024);
        while out.len() < CHUNK {
            let mut line = std::mem::take(&mut self.line);
            let read = read_line(&mut self.r, &mut line);
            self.line = line;
            match read {
                Err(e) => return Some(Err(e)),
                Ok(None) => {
                    self.done = true;
                    break;
                }
                Ok(Some((_, over))) => {
                    if self.line.is_empty() && !over {
                        continue;
                    }
                    if let Some(l) = self.convert(over) {
                        out.extend_from_slice(l.as_bytes());
                        out.push(b'\n');
                    }
                }
            }
        }
        (!out.is_empty()).then_some(Ok(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A configured folder another account owns isn't read, whatever its
    /// index says (Windows: by its owner and DACL, checked once a run).
    #[cfg(windows)]
    #[test]
    fn a_folder_owned_by_another_account_is_refused() {
        // Setting another owner takes an administrator (CI is one).
        if std::env::var_os("CI").is_none() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let mine = tmp.path().join("mine");
        std::fs::create_dir(&mine).unwrap();
        assert!(folder(None, &mine).is_ok());
        let theirs = tmp.path().join("theirs");
        std::fs::create_dir(&theirs).unwrap();
        std::fs::write(
            theirs.join("index.json"),
            r#"{"files":{},"restricted":true}"#,
        )
        .unwrap();
        let root = std::env::var_os("SystemRoot").unwrap();
        let out = std::process::Command::new(Path::new(&root).join("System32").join("icacls.exe"))
            .arg(&theirs)
            .args(["/setowner", "*S-1-5-32-545"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert_eq!(folder(None, &theirs), Err("not_private"));
        // Checked once a run: the answer is cached.
        assert_eq!(folder(None, &theirs), Err("not_private"));
    }

    #[test]
    fn a_file_being_compressed_is_listed_once() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "lodestar.jsonl",
            "lodestar-20261023T101500.123Z.jsonl",
            "lodestar-20261023T101500.123Z.jsonl.gz",
            "lodestar-20261023T101500.123Z.jsonl.gz.part",
            "lodestar-20251023T101500.123Z.jsonl.gz",
        ] {
            std::fs::write(dir.path().join(name), "").unwrap();
        }
        let names: Vec<String> = list(dir.path())
            .unwrap()
            .iter()
            .map(|l| l.name.file_name())
            .collect();
        assert_eq!(
            names,
            [
                "lodestar.jsonl",
                "lodestar-20261023T101500.123Z.jsonl.gz",
                "lodestar-20251023T101500.123Z.jsonl.gz",
            ]
        );
    }

    #[test]
    fn a_hostile_index_only_loses_badges() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let good = "lodestar-20261023T101500.123Z.jsonl.gz";
        std::fs::write(
            d.join(INDEX),
            format!(
                r#"{{"files":{{"../../etc/passwd":{{"details":true}},"{good}":{{"details":true,"opt_in":false}},"lodestar.jsonl":{{"details":"yes"}},"lodestar-x.jsonl":7}},"alive_at":1}}"#
            ),
        )
        .unwrap();
        let ix = read_index(d);
        assert!(ix.readable);
        assert!(ix.flags(&LogName::parse(good).unwrap()).details);
        assert!(!ix.flags(&LogName::Current).details, "wrong type: no badge");
        assert_eq!(ix.files.len(), 1);
        // Unreadable: every file has details.
        std::fs::write(d.join(INDEX), "{not json").unwrap();
        let ix = read_index(d);
        assert!(!ix.readable);
        assert!(ix.flags(&LogName::Current).details && ix.flags(&LogName::Current).opt_in);
        std::fs::write(d.join(INDEX), "[1]").unwrap();
        assert!(!read_index(d).readable);
        std::fs::remove_file(d.join(INDEX)).unwrap();
        assert!(read_index(d).readable, "none yet is an empty index");
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_index_is_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = dir.path().join("elsewhere.json");
        std::fs::write(&elsewhere, r#"{"files":{}}"#).unwrap();
        std::os::unix::fs::symlink(&elsewhere, dir.path().join(INDEX)).unwrap();
        assert!(!read_index(dir.path()).readable);
    }

    #[cfg(unix)]
    #[test]
    fn which_folder() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("logs");
        assert_eq!(folder(None, &logs), Err("not_opened"));
        std::fs::create_dir(&logs).unwrap();
        std::fs::set_permissions(&logs, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(folder(None, &logs).unwrap().dir, logs);
        std::fs::set_permissions(&logs, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(folder(None, &logs), Err("not_private"));
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&logs, &link).unwrap();
        assert_eq!(folder(None, &link), Err("not_a_folder"));
        assert_eq!(
            folder(Some(link.clone()), &logs).unwrap().dir,
            link,
            "the runtime's own"
        );
    }

    #[test]
    fn downloads_convert_line_by_line() {
        let text = format!(
            "{}\nnot json at all\n\n{}\n",
            r#"{"id":"0011223344556677-1","ts":"2026-10-23T10:15:00.000Z","level":"info","component":"fetch","event":"x.y","msg":"m","fields":{"name":{"kind":"name","value":"Fixture.Name"}}}"#,
            r#"{"ts":"2026-10-23T10:15:00.000Z","level":"info","component":"filters","event":"filters.change_detail","msg":"m","fields":{"change":{"kind":"filter_changes","value":"term"}}}"#,
        );
        let run = |format, redact| {
            let r: Box<dyn BufRead + Send> = Box::new(io::Cursor::new(text.clone().into_bytes()));
            String::from_utf8(
                content(r, format, redact)
                    .collect::<io::Result<Vec<Vec<u8>>>>()
                    .unwrap()
                    .concat(),
            )
            .unwrap()
        };
        assert_eq!(run(LogFormat::Jsonl, false), text, "as written");
        let red = run(LogFormat::Jsonl, true);
        assert!(
            !red.contains("Fixture.Name") && !red.contains("term"),
            "{red}"
        );
        assert_eq!(red.lines().count(), 2, "{red}");
        assert!(red.lines().nth(1).unwrap() == UNREADABLE, "{red}");
        let t = run(LogFormat::Text, false);
        assert!(
            t.contains("Fixture.Name") && t.contains("not json at all"),
            "{t}"
        );
        assert!(
            t.starts_with("2026-10-23T10:15:00.000Z  INFO x.y: m name=Fixture.Name"),
            "{t}"
        );
        assert_eq!(
            download_name(&LogName::Current, LogFormat::Text, true),
            "lodestar-redacted.txt"
        );
    }

    #[test]
    fn a_compressed_file_is_read_whole_bounded_and_found_after_compression() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let plain = LogName::parse("lodestar-20261023T101500.123Z.jsonl").unwrap();
        let gz = plain.compressed().unwrap();
        // Compressed after the listing: the `.jsonl` asked for is its `.gz`.
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        e.write_all(&b"0123456789\n".repeat(100_000)).unwrap();
        std::fs::write(gz.path(dir.path()), e.finish().unwrap()).unwrap();
        let mut all = Vec::new();
        open_plain(dir.path(), &plain)
            .unwrap()
            .read_to_end(&mut all)
            .unwrap();
        assert_eq!(all.len(), 1_100_000);
        // Never inflated past the cap: what's past it is an error, not an
        // end.
        let mut r = open_capped(dir.path(), &gz, 4096).unwrap();
        let mut capped = vec![0u8; 8192];
        let mut got = 0;
        let e = loop {
            match r.read(&mut capped[got..]) {
                Ok(0) => panic!("ended as if whole"),
                Ok(n) => got += n,
                Err(e) => break e,
            }
        };
        assert_eq!(got, 4096);
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        // Exactly at the cap is whole.
        let mut all = Vec::new();
        open_capped(dir.path(), &gz, 1_100_000)
            .unwrap()
            .read_to_end(&mut all)
            .unwrap();
        assert_eq!(all.len(), 1_100_000);
    }
}
