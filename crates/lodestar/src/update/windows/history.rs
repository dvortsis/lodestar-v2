//! The install history on Windows (X-win-upd-history): `<data>\bin\
//! history.json`, in the Unix engine's format (`update/history.rs`, which
//! is Unix-only), so the same file reads the same on either.
//!
//! **The service writes it, never the update helper.** Each time the
//! service looks (at its start, at every status, at each check-loop wake
//! and before a hand-over), the helper's last answer
//! (`<data>\updates\result.json`) is turned into at most one entry. The helper gains no file and no right
//! (§2.47 Q13). The answer is display-only input that this account can
//! write too, so it's checked before anything is kept: the version must
//! parse, the outcome must be one that ends an install, and the time must
//! be plausible. An entry holds a version, a result, a time and a reason
//! code: no path, account or setting.

use std::io;

use lodestar_log::log_event;
use lodestar_update::semver::Version;
use serde::{Deserialize, Serialize};

use super::msi::Answer;
use crate::update::layout::{Bin, Step, parse_version};
use crate::update::log_events::UPDATE_STATE_UNSAVED;

pub const FILE: &str = "history.json";
/// The helper answer last recorded (its request id and outcome), Windows
/// only: the answer stays in `result.json` after it's recorded and is
/// looked at again and again, and a cleared history mustn't get it back.
/// Keyed by id and outcome, not time, so no clock change can hide a new
/// answer.
pub const SEEN: &str = "history-seen.json";
/// Entries kept, as on Unix.
pub const KEEP: usize = 20;
/// How far ahead of this clock an answer's time may be (a clock moved
/// back since): anything later is taken for damage.
const AHEAD: u64 = 86_400;

/// `history.json`'s `result`, as on Unix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Installed,
    RolledBack,
    Failed,
    Skipped,
}

impl Outcome {
    pub fn code(self) -> &'static str {
        match self {
            Outcome::Installed => "installed",
            Outcome::RolledBack => "rolled_back",
            Outcome::Failed => "failed",
            Outcome::Skipped => "skipped",
        }
    }
}

/// One update, field for field as on Unix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub version: String,
    pub from: String,
    #[serde(default)]
    pub level: Option<String>,
    pub at: u64,
    pub result: Outcome,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub lost_torrents: Option<u64>,
    /// What it came from, so a second look doesn't add it again: on Unix a
    /// rollback journal's start, here the helper's answer time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct History {
    pub entries: Vec<Entry>,
}

/// The entry the helper's `answer` records, or `None` for one that
/// doesn't end an install (nothing to do, a refusal, a busy Installer, a
/// blocked install with retries left, a rollback still under way) or that
/// doesn't check out. `running` is this build's version; `now` the clock.
pub fn entry_for(answer: &Answer, running: &Version, now: u64) -> Option<Entry> {
    let version = parse_version(&answer.version)?;
    if !super::msi::valid_id(&answer.id) || answer.at == 0 || answer.at > now.saturating_add(AHEAD)
    {
        return None;
    }
    let (result, reason) = match answer.outcome.as_str() {
        // Installed, but the old program runs until Windows restarts, and
        // the helper judges it after that: not the end yet.
        "installed" if answer.restart => return None,
        "installed" => (Outcome::Installed, None),
        "install_failed" => (Outcome::Failed, Some("install_failed")),
        // The new version failed its health check, and the old one is back.
        "rolled_back" => (Outcome::RolledBack, Some("selftest")),
        "rollback_halted" => (Outcome::Failed, Some("rollback_halted")),
        "install_interrupted" => (Outcome::Failed, Some("install_interrupted")),
        _ => return None,
    };
    Some(Entry {
        // As this build prints it, not as the file spelled it.
        version: version.to_string(),
        // Updated from: this build, while it's older than the answer's
        // (still the old one, or back to it). Otherwise unknown: the answer
        // doesn't say what it replaced, and a later build may have been
        // installed by hand since.
        from: if *running < version {
            running.to_string()
        } else {
            String::new()
        },
        level: None,
        at: answer.at,
        result,
        reason: reason.map(String::from),
        lost_torrents: None,
        journal: Some(answer.at),
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Seen {
    id: String,
    outcome: String,
}

/// Records the helper's `answer` unless it was recorded before (even if
/// the history was cleared since); the entry when it was added. A failed
/// write names its file.
pub fn record(
    bin: &Bin,
    answer: &Answer,
    running: &Version,
    now: u64,
    fault: &dyn Fn(Step) -> io::Result<()>,
) -> Result<Option<Entry>, (&'static str, io::Error)> {
    let Some(entry) = entry_for(answer, running, now) else {
        return Ok(None);
    };
    // Damaged, it's taken as none: the entry's own check in `append`
    // still keeps one from being added twice.
    if bin
        .read::<Seen>(SEEN)
        .ok()
        .flatten()
        .is_some_and(|s| s.id == answer.id && s.outcome == answer.outcome)
    {
        return Ok(None);
    }
    let added = append(bin, entry.clone(), fault).map_err(|e| (FILE, e))?;
    let seen = Seen {
        id: answer.id.clone(),
        outcome: answer.outcome.clone(),
    };
    bin.write_acknowledged(SEEN, &seen, fault)
        .map_err(|e| (SEEN, e))?;
    Ok(added.then_some(entry))
}

/// The portable build's own install or rollback (`portable.rs`), which
/// finish in this process: no answer, so the journal's `attempt` id (or,
/// in a journal from before it, `key`, the new program's SHA-256 in hex)
/// stands for it. It's the same at every repeat of the step, so a repeat
/// adds nothing, and each install attempt is its own entry. `now` is the
/// clock. Best effort, as the record must never stop the step: a failed
/// write is logged.
pub fn record_portable(
    bin: &Bin,
    from: &str,
    to: &str,
    result: Outcome,
    attempt: Option<u64>,
    key: &str,
    now: i64,
) {
    let Some(version) = parse_version(to) else {
        return;
    };
    let entry = Entry {
        version: version.to_string(),
        from: parse_version(from)
            .map(|v| v.to_string())
            .unwrap_or_default(),
        level: None,
        at: u64::try_from(now).unwrap_or(0),
        result,
        // The journal doesn't keep why the self-test failed.
        reason: None,
        lost_torrents: None,
        journal: attempt.or_else(|| key.get(..16).and_then(|k| u64::from_str_radix(k, 16).ok())),
    };
    if let Err(e) = append(bin, entry, &|_| Ok(())) {
        log_event!(
            UPDATE_STATE_UNSAVED,
            file = FILE,
            error = lodestar_log::Error::io(&e)
        );
    }
}

/// The history, or `Err` when the file is there but unreadable.
pub fn read(bin: &Bin) -> io::Result<History> {
    Ok(bin.read::<History>(FILE)?.unwrap_or_default())
}

/// Appends `entry`, newest last, keeping [`KEEP`]; `Ok(false)` when it's
/// already there (the same answer looked at again). A damaged file is
/// started again: it's display only, and must never stop the record.
pub fn append(bin: &Bin, entry: Entry, fault: &dyn Fn(Step) -> io::Result<()>) -> io::Result<bool> {
    let mut history = read(bin).unwrap_or_default();
    if history.entries.iter().any(|e| {
        e.journal == entry.journal && e.version == entry.version && e.result == entry.result
    }) {
        return Ok(false);
    }
    history.entries.push(entry);
    let excess = history.entries.len().saturating_sub(KEEP);
    history.entries.drain(..excess);
    bin.write_acknowledged(FILE, &history, fault)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_700_000_000;

    fn answer(outcome: &str, version: &str, at: u64) -> Answer {
        serde_json::from_value(serde_json::json!({
            "format": 1, "id": "ab".repeat(16), "version": version,
            "outcome": outcome, "at": at,
        }))
        .unwrap()
    }

    fn bin() -> (tempfile::TempDir, Bin) {
        let dir = tempfile::tempdir().unwrap();
        let bin = Bin::open(dir.path()).unwrap();
        (dir, bin)
    }

    fn ok(_: Step) -> io::Result<()> {
        Ok(())
    }

    #[test]
    fn each_ending_becomes_one_entry_and_the_rest_none() {
        let running = Version::new(0, 5, 3);
        for (outcome, result, reason) in [
            ("installed", Outcome::Installed, None),
            ("install_failed", Outcome::Failed, Some("install_failed")),
            ("rolled_back", Outcome::RolledBack, Some("selftest")),
            ("rollback_halted", Outcome::Failed, Some("rollback_halted")),
            (
                "install_interrupted",
                Outcome::Failed,
                Some("install_interrupted"),
            ),
        ] {
            let e = entry_for(&answer(outcome, "0.5.4", NOW), &running, NOW).unwrap();
            assert_eq!(
                (e.result, e.reason.as_deref()),
                (result, reason),
                "{outcome}"
            );
            assert_eq!((e.version.as_str(), e.from.as_str()), ("0.5.4", "0.5.3"));
            assert_eq!((e.at, e.journal), (NOW, Some(NOW)));
        }
        for outcome in [
            "nothing",
            "install_blocked",
            "installer_busy",
            "rollback_pending",
            "handed_off",
            "copy_refused",
            "refused",
            "anything_else",
        ] {
            assert_eq!(
                entry_for(&answer(outcome, "0.5.4", NOW), &running, NOW),
                None
            );
        }
        // Installed and now running: what it replaced isn't known.
        let e = entry_for(&answer("installed", "0.5.3", NOW), &running, NOW).unwrap();
        assert_eq!(e.from, "");
    }

    #[test]
    fn an_answer_that_doesnt_check_out_records_nothing() {
        let running = Version::new(0, 5, 3);
        for (version, at) in [
            ("not a version", NOW),
            ("../../0.5.4", NOW),
            ("0.5.4", 0),
            ("0.5.4", NOW + AHEAD + 1),
        ] {
            assert_eq!(
                entry_for(&answer("installed", version, at), &running, NOW),
                None,
                "{version} {at}"
            );
        }
        // A clock moved back by less than a day is still believed.
        assert!(entry_for(&answer("installed", "0.5.4", NOW + AHEAD), &running, NOW).is_some());
    }

    #[test]
    fn appends_once_keeps_twenty_and_restarts_a_damaged_file() {
        let (_dir, bin) = bin();
        let running = Version::new(0, 5, 3);
        let e = entry_for(&answer("installed", "0.5.4", NOW), &running, NOW).unwrap();
        assert!(append(&bin, e.clone(), &ok).unwrap());
        // The same answer again adds nothing.
        assert!(!append(&bin, e, &ok).unwrap());
        assert_eq!(read(&bin).unwrap().entries.len(), 1);
        // The same answer's rollback, later, is its own entry.
        let rb = entry_for(&answer("rolled_back", "0.5.4", NOW + 60), &running, NOW).unwrap();
        assert!(append(&bin, rb, &ok).unwrap());
        for i in 0..30 {
            let a = answer("install_failed", "0.5.4", NOW + 100 + i);
            assert!(append(&bin, entry_for(&a, &running, NOW).unwrap(), &ok).unwrap());
        }
        let h = read(&bin).unwrap();
        assert_eq!(h.entries.len(), KEEP);
        assert_eq!(h.entries.last().unwrap().at, NOW + 129);
        // Damaged: read as an error, and written over by the next entry.
        std::fs::write(bin.path(FILE), b"{not json").unwrap();
        assert!(read(&bin).is_err());
        let a = answer("installed", "0.5.5", NOW + 500);
        assert!(append(&bin, entry_for(&a, &running, NOW).unwrap(), &ok).unwrap());
        assert_eq!(read(&bin).unwrap().entries.len(), 1);
    }

    fn answer_id(outcome: &str, version: &str, at: u64, id: &str) -> Answer {
        let mut a = answer(outcome, version, at);
        a.id = id.into();
        a
    }

    #[test]
    fn an_answer_recorded_once_isnt_again_after_a_clear() {
        let (_dir, bin) = bin();
        let running = Version::new(0, 5, 3);
        let a = answer("installed", "0.5.4", NOW);
        assert!(record(&bin, &a, &running, NOW, &ok).unwrap().is_some());
        // Looked at again (every status), and again after a clear: once.
        assert!(record(&bin, &a, &running, NOW, &ok).unwrap().is_none());
        std::fs::remove_file(bin.path(FILE)).unwrap();
        assert!(record(&bin, &a, &running, NOW, &ok).unwrap().is_none());
        assert!(!bin.path(FILE).exists());
        // The same request's next outcome is new; so is another request's,
        // even with an earlier time (a clock set back).
        let rb = answer("rolled_back", "0.5.4", NOW + 60);
        assert!(record(&bin, &rb, &running, NOW, &ok).unwrap().is_some());
        let next = answer_id("install_failed", "0.5.5", NOW - 3600, &"cd".repeat(16));
        assert!(record(&bin, &next, &running, NOW, &ok).unwrap().is_some());
        assert_eq!(read(&bin).unwrap().entries.len(), 2);
    }

    #[test]
    fn a_restart_still_to_come_a_later_build_or_a_bad_id_records_nothing_wrong() {
        let running = Version::new(0, 5, 3);
        // 3010: the old program runs until Windows restarts.
        let mut a = answer("installed", "0.5.4", NOW);
        a.restart = true;
        assert_eq!(entry_for(&a, &running, NOW), None);
        // Recorded late, after a later build was installed by hand: what
        // it replaced is unknown, never the later build.
        let e = entry_for(
            &answer("installed", "0.5.4", NOW),
            &Version::new(0, 6, 0),
            NOW,
        );
        assert_eq!(e.unwrap().from, "");
        for id in ["", "short", &"g".repeat(32), &"a".repeat(65)] {
            assert_eq!(
                entry_for(&answer_id("installed", "0.5.4", NOW, id), &running, NOW),
                None,
                "{id}"
            );
        }
    }

    #[test]
    fn a_failed_write_names_its_file() {
        let (_dir, bin) = bin();
        let running = Version::new(0, 5, 3);
        std::fs::create_dir(bin.path(SEEN)).unwrap();
        let a = answer("installed", "0.5.4", NOW);
        let (file, _) = record(&bin, &a, &running, NOW, &ok).unwrap_err();
        assert_eq!(file, SEEN);
    }

    /// The portable build: each attempt is its own entry (a build that
    /// rolled back without being skipped, installed and rolled back
    /// again), and a repeat of one attempt's step adds none.
    #[test]
    fn each_portable_attempt_is_its_own_entry() {
        let (_dir, bin) = bin();
        let key = "ab".repeat(32);
        for attempt in [Some(1), Some(1), Some(2), None, None] {
            record_portable(
                &bin,
                "0.5.4",
                "0.5.5",
                Outcome::RolledBack,
                attempt,
                &key,
                NOW as i64,
            );
        }
        assert_eq!(read(&bin).unwrap().entries.len(), 3);
    }

    #[test]
    fn a_failed_write_is_an_error_and_keeps_the_file() {
        let (_dir, bin) = bin();
        let running = Version::new(0, 5, 3);
        let e = entry_for(&answer("installed", "0.5.4", NOW), &running, NOW).unwrap();
        let fail = |_: Step| Err(io::Error::other("injected"));
        assert!(append(&bin, e, &fail).is_err());
        assert!(!bin.path(FILE).exists());
    }

    /// The Unix engine's file, read here, and this one's written as the
    /// Unix engine writes it.
    #[test]
    fn the_file_is_the_unix_engines() {
        let unix = r#"{"entries":[{"version":"0.5.0","from":"0.4.9","level":"fix","at":1,"result":"rolled_back","reason":"selftest","lost_torrents":3,"journal":7},{"version":"0.5.1","from":"image","level":null,"at":2,"result":"installed","reason":null,"lost_torrents":null}]}"#;
        let h: History = serde_json::from_str(unix).unwrap();
        assert_eq!(h.entries[0].result, Outcome::RolledBack);
        assert_eq!(h.entries[1].journal, None);
        assert_eq!(serde_json::to_string(&h).unwrap(), unix);
    }
}
