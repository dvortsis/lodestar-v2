//! A managed companion's state record and install journal (plan
//! 2026-11-06-v06-companions §3, §4.3 CMP-5; decisions
//! `docs/decisions/2026-11-08-CMP-5.md`).
//!
//! One small JSON file per companion, `<data>/companions/<id>/state.json`:
//!
//! ```json
//! {"format":1,"installed":"5.0.2","op":{"kind":"update","version":"5.0.3","step":"staged"}}
//! ```
//!
//! - `installed`: the version that passed health and runs, or `null`
//!   during a first install.
//! - `op`: the operation under way and the last step it finished, or
//!   `null` when nothing is. It's written before each step's effect is
//!   relied on, so recovery at start-up knows what may have happened
//!   ([`crate::manager`]).
//!
//! It lives inside the companion's own folder, so an uninstall removes it
//! with everything else, and nothing outside that folder is written. The
//! layout's pointer (`current`) stays the truth for what runs
//! ([`crate::layout`]); this file says what an interrupted operation was.
//!
//! **Written atomically** the way CMP-3's config files and the pointer
//! are: a new temporary file (0600 on Unix), fsync, rename over
//! `state.json`, fsync of the folder. A crash leaves the old record or the
//! new one. It holds versions and step names only: no credential, path or
//! URL.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::extract::sync_dir;
use crate::inputs::Version;

/// The record's file name inside `<data>/companions/<id>/`.
pub const STATE_FILE: &str = "state.json";
const STATE_TMP: &str = "state.json.tmp";
const FORMAT: u32 = 1;
const MAX_STATE: u64 = 4096;

/// What an operation is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpKind {
    Install,
    Update,
    Remove,
}

/// The last step an operation finished, in the order they run. Install:
/// every step; update: no `credentials` or `files` (the companion's own
/// config and credential stay); remove: `begun`, `stopped`, `forgotten`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// Confirmed and journalled; nothing on disk yet.
    Begun,
    /// Fetched, verified and extracted into `<version>/`.
    Staged,
    /// The credential row is stored durably (CMP-3).
    Credentials,
    /// The config file is written (CMP-3).
    Files,
    /// The pointer names the new version.
    Committed,
    /// Started, and it passed health.
    Healthy,
    /// Configured through its API (CMP-8b): the commit point. Recovery
    /// rolls an operation that got here forward; anything before, back.
    Configured,
    /// Remove: it's stopped.
    Stopped,
    /// Remove: its credential rows are deleted.
    Forgotten,
}

impl Step {
    /// A stable code for logs and the API.
    pub fn code(self) -> &'static str {
        match self {
            Step::Begun => "begun",
            Step::Staged => "staged",
            Step::Credentials => "credentials",
            Step::Files => "files",
            Step::Committed => "committed",
            Step::Healthy => "healthy",
            Step::Configured => "configured",
            Step::Stopped => "stopped",
            Step::Forgotten => "forgotten",
        }
    }
}

/// An operation under way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Op {
    pub kind: OpKind,
    /// The version it installs; `None` for a removal.
    pub version: Option<Version>,
    pub step: Step,
}

/// A managed companion's record.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Record {
    pub installed: Option<Version>,
    pub op: Option<Op>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpFile {
    kind: OpKind,
    #[serde(default)]
    version: Option<String>,
    step: Step,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordFile {
    format: u32,
    installed: Option<String>,
    op: Option<OpFile>,
}

/// Why the record couldn't be read or written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum StateError {
    /// `state.json` isn't a record this build reads. It's left as it is.
    Bad,
    Io(io::ErrorKind),
}

impl StateError {
    pub fn code(&self) -> &'static str {
        match self {
            StateError::Bad => "bad_state",
            StateError::Io(_) => "io",
        }
    }
}

impl fmt::Display for StateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "companion state: {}", self.code())
    }
}

impl std::error::Error for StateError {}

impl From<io::Error> for StateError {
    fn from(e: io::Error) -> StateError {
        StateError::Io(e.kind())
    }
}

fn version(s: &str) -> Result<Version, StateError> {
    Version::parse(s)
        .ok()
        .filter(|v| v.as_str() == s)
        .ok_or(StateError::Bad)
}

/// Reads the record in `dir` (the companion's folder); `None` if there's
/// none. A link, an oversized file or anything malformed is
/// [`StateError::Bad`].
pub fn load(dir: &Path) -> Result<Option<Record>, StateError> {
    let path = dir.join(STATE_FILE);
    let meta = match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        other => other?,
    };
    if !meta.is_file() || meta.len() > MAX_STATE {
        return Err(StateError::Bad);
    }
    let mut text = String::new();
    File::open(&path)?
        .take(MAX_STATE)
        .read_to_string(&mut text)
        .map_err(|_| StateError::Bad)?;
    let file: RecordFile = serde_json::from_str(&text).map_err(|_| StateError::Bad)?;
    if file.format != FORMAT {
        return Err(StateError::Bad);
    }
    let op = match file.op {
        None => None,
        Some(o) => Some(Op {
            kind: o.kind,
            version: o.version.as_deref().map(version).transpose()?,
            step: o.step,
        }),
    };
    if op
        .as_ref()
        .is_some_and(|o| o.version.is_none() != (o.kind == OpKind::Remove))
    {
        return Err(StateError::Bad);
    }
    Ok(Some(Record {
        installed: file.installed.as_deref().map(version).transpose()?,
        op,
    }))
}

/// Writes `record` into `dir` atomically.
pub fn save(dir: &Path, record: &Record) -> Result<(), StateError> {
    let bytes = serde_json::to_vec(&RecordFile {
        format: FORMAT,
        installed: record.installed.as_ref().map(|v| v.as_str().to_owned()),
        op: record.op.as_ref().map(|o| OpFile {
            kind: o.kind,
            version: o.version.as_ref().map(|v| v.as_str().to_owned()),
            step: o.step,
        }),
    })
    .map_err(|_| StateError::Bad)?;
    let tmp = dir.join(STATE_TMP);
    match fs::remove_file(&tmp) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    let mut o = OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(&tmp)?;
    f.write_all(&bytes)?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, dir.join(STATE_FILE))?;
    // The rename has happened; as CMP-3's writer, a failed folder fsync
    // isn't reported as an unwritten record.
    let _ = sync_dir(dir);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_round_trips_and_strangers_are_refused() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(load(d.path()).unwrap(), None);
        let r = Record {
            installed: Some(Version::parse("5.0.2").unwrap()),
            op: Some(Op {
                kind: OpKind::Update,
                version: Some(Version::parse("5.0.3").unwrap()),
                step: Step::Staged,
            }),
        };
        save(d.path(), &r).unwrap();
        assert_eq!(
            fs::read_to_string(d.path().join(STATE_FILE)).unwrap(),
            r#"{"format":1,"installed":"5.0.2","op":{"kind":"update","version":"5.0.3","step":"staged"}}"#
        );
        assert_eq!(load(d.path()).unwrap(), Some(r));
        assert!(!d.path().join(STATE_TMP).exists());
        for bad in [
            r#"{"format":2,"installed":null,"op":null}"#,
            r#"{"format":1,"installed":"../x","op":null}"#,
            r#"{"format":1,"installed":null,"op":{"kind":"install","step":"begun"}}"#,
            r#"{"format":1,"installed":null,"op":{"kind":"remove","version":"1","step":"begun"}}"#,
            r#"{"format":1,"installed":null,"op":null,"extra":1}"#,
        ] {
            fs::write(d.path().join(STATE_FILE), bad).unwrap();
            assert_eq!(load(d.path()), Err(StateError::Bad), "{bad}");
        }
    }
}
