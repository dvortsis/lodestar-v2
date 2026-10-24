//! `<data>/bin/history.json` (§12.2): the last 20 updates, outside the
//! store, so a restore can't erase it. The supervisor appends a rollback's
//! outcome (keyed by its journal, so a repeated step doesn't add it
//! twice); the updater (part 5) appends installs and clears it.

use serde::{Deserialize, Serialize};

use super::layout::Bin;

pub const FILE: &str = "history.json";
/// Entries kept.
pub const KEEP: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Installed,
    RolledBack,
    Failed,
    Skipped,
}

/// One update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub version: String,
    pub from: String,
    /// The release's level, when known (`fix`, `feature`, `major`).
    #[serde(default)]
    pub level: Option<String>,
    pub at: u64,
    pub result: Outcome,
    #[serde(default)]
    pub reason: Option<String>,
    /// Torrents a restore loses; `None` when it couldn't be counted.
    #[serde(default)]
    pub lost_torrents: Option<u64>,
    /// The rollback journal this came from (its start time), so repeating
    /// a journal step doesn't add the entry again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct History {
    pub entries: Vec<Entry>,
}

/// Appends `entry`, newest last, keeping [`KEEP`]. An entry from a journal
/// already recorded is not added again.
pub fn append(bin: &Bin, entry: Entry) -> std::io::Result<()> {
    // A damaged file is started again rather than blocking a rollback.
    let mut history: History = bin.read(FILE).ok().flatten().unwrap_or_default();
    if entry.journal.is_some()
        && history
            .entries
            .iter()
            .any(|e| e.journal == entry.journal && e.version == entry.version)
    {
        return Ok(());
    }
    history.entries.push(entry);
    let excess = history.entries.len().saturating_sub(KEEP);
    history.entries.drain(..excess);
    bin.write(FILE, &history)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(v: &str, journal: Option<u64>) -> Entry {
        Entry {
            version: v.into(),
            from: "0.5.2".into(),
            level: None,
            at: 1,
            result: Outcome::RolledBack,
            reason: Some("failed".into()),
            lost_torrents: None,
            journal,
        }
    }

    #[test]
    fn keeps_twenty_and_never_repeats_a_journal() {
        let data = tempfile::tempdir().unwrap();
        let bin = Bin::open(data.path()).unwrap();
        append(&bin, entry("0.6.0", Some(7))).unwrap();
        append(&bin, entry("0.6.0", Some(7))).unwrap();
        let h: History = bin.read(FILE).unwrap().unwrap();
        assert_eq!(h.entries.len(), 1);
        for i in 0..30 {
            append(&bin, entry(&format!("0.7.{i}"), None)).unwrap();
        }
        let h: History = bin.read(FILE).unwrap().unwrap();
        assert_eq!(h.entries.len(), KEEP);
        assert_eq!(h.entries.last().unwrap().version, "0.7.29");
        // A damaged file doesn't block the append.
        std::fs::write(bin.path(FILE), "not json").unwrap();
        append(&bin, entry("0.8.0", None)).unwrap();
        let h: History = bin.read(FILE).unwrap().unwrap();
        assert_eq!(h.entries.len(), 1);
    }
}
