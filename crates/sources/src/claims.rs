//! What a source asserts about a key (architecture §18.1, §18.5).
//!
//! Claims are hints: a name, a file list, trackers, external IDs, a
//! description and a publisher, exactly as the source stated them.
//! Verified metadata (the BEP 9 info dict) always wins over them. They
//! come from untrusted feeds, so every claim is bounded: [`Claims::clamp`]
//! cuts each field to the limits below, and the [`Emitter`] applies it to
//! every event, so nothing past the contract boundary ever sees more.
//! Clamping trims; it never rejects, because a long name is still a
//! useful hint.
//!
//! The child-safety filter still runs on every claim before storage
//! (§18.7, §20.1). Bounding here doesn't replace it.
//!
//! [`Emitter`]: crate::Emitter

/// Bytes in a claimed name.
pub const MAX_NAME: usize = 1024;
/// Claimed files per key.
pub const MAX_FILES: usize = 10_000;
/// Bytes in one claimed file path.
pub const MAX_PATH: usize = 4096;
/// Claimed trackers per key.
pub const MAX_TRACKERS: usize = 64;
/// Bytes in one tracker URL.
pub const MAX_TRACKER: usize = 2048;
/// External IDs per key.
pub const MAX_IDS: usize = 32;
/// Bytes in an ID's scheme (`imdb`) or value (`tt0111161`).
pub const MAX_ID_PART: usize = 128;
/// Bytes in a claimed description.
pub const MAX_DESCRIPTION: usize = 64 * 1024;
/// Bytes in a publisher identity.
pub const MAX_PUBLISHER: usize = 256;
/// Bytes in a provenance external reference (a GUID or item URL).
pub const MAX_EXTERNAL_REF: usize = 2048;
/// Bytes of text in one event's claims overall. Every other field fits
/// in about 200 KiB at its own limit, so this bounds the file list: files
/// past the budget are dropped. Without it one event could hold 40 MB.
pub const MAX_TOTAL: usize = 1024 * 1024;

/// One claimed file. Sizes are what the source says, not what's true.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedFile {
    pub path: String,
    pub size: Option<u64>,
}

/// An external ID, such as `imdb:tt0111161`, `tmdb:278` or `anidb:1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalId {
    /// Lower-case scheme, without the colon.
    pub scheme: String,
    pub value: String,
}

/// Everything a source asserted about one key. Every field is optional.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Claims {
    pub name: Option<String>,
    /// The total size the source states, which may disagree with `files`.
    pub size: Option<u64>,
    pub files: Vec<ClaimedFile>,
    pub trackers: Vec<String>,
    pub ids: Vec<ExternalId>,
    pub description: Option<String>,
    /// The uploader or channel as the source names it. A public handle
    /// from the feed, never a peer address (§18.7: no profiles of people).
    pub publisher: Option<String>,
}

impl Claims {
    /// True when the source asserted nothing.
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.size.is_none()
            && self.files.is_empty()
            && self.trackers.is_empty()
            && self.ids.is_empty()
            && self.description.is_none()
            && self.publisher.is_none()
    }

    /// Cuts every field to its limit and the whole to [`MAX_TOTAL`].
    /// Strings are cut on a character boundary; lists keep their first
    /// non-empty entries. Empty strings become `None`, and IDs with an
    /// empty part are dropped.
    pub fn clamp(&mut self) {
        clamp_opt(&mut self.name, MAX_NAME);
        self.trackers.retain(|t| !t.is_empty());
        self.trackers.truncate(MAX_TRACKERS);
        for t in &mut self.trackers {
            truncate(t, MAX_TRACKER);
        }
        for id in &mut self.ids {
            truncate(&mut id.scheme, MAX_ID_PART);
            id.scheme.make_ascii_lowercase();
            truncate(&mut id.value, MAX_ID_PART);
        }
        self.ids
            .retain(|id| !id.scheme.is_empty() && !id.value.is_empty());
        self.ids.truncate(MAX_IDS);
        clamp_opt(&mut self.description, MAX_DESCRIPTION);
        clamp_opt(&mut self.publisher, MAX_PUBLISHER);
        let mut total = self.fixed_len();
        self.files.truncate(MAX_FILES);
        let mut keep = 0;
        for f in &mut self.files {
            truncate(&mut f.path, MAX_PATH);
            total += f.path.len();
            if total > MAX_TOTAL {
                break;
            }
            keep += 1;
        }
        self.files.truncate(keep);
    }

    /// Bytes of text held, the measure [`MAX_TOTAL`] bounds.
    pub fn text_len(&self) -> usize {
        self.fixed_len() + self.files.iter().map(|f| f.path.len()).sum::<usize>()
    }

    /// Bytes of text outside the file list.
    fn fixed_len(&self) -> usize {
        let len = |s: &Option<String>| s.as_ref().map_or(0, String::len);
        len(&self.name)
            + len(&self.description)
            + len(&self.publisher)
            + self.trackers.iter().map(String::len).sum::<usize>()
            + self
                .ids
                .iter()
                .map(|i| i.scheme.len() + i.value.len())
                .sum::<usize>()
    }
}

/// How much the source's word is worth, as the plugin judges it. Ranking
/// treats every claim as weaker than verified metadata whatever this says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Confidence {
    /// Scraped or user-submitted text with no checks.
    #[default]
    Low,
    /// A curated feed or an indexer's own record.
    Medium,
    /// The publisher's own official list.
    High,
}

/// Where a discovery came from, beyond the source id the [`Emitter`]
/// stamps on it.
///
/// [`Emitter`]: crate::Emitter
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Provenance {
    /// The source's own reference: a feed item GUID or its page URL.
    pub external_ref: Option<String>,
    /// When the source says the item was published, in Unix seconds. The
    /// time Lodestar saw it is [`Discovery::at`](crate::Discovery::at).
    pub published_at: Option<i64>,
    pub confidence: Confidence,
}

impl Provenance {
    pub fn clamp(&mut self) {
        clamp_opt(&mut self.external_ref, MAX_EXTERNAL_REF);
    }
}

/// Cuts `s` to at most `max` bytes on a character boundary.
pub(crate) fn truncate(s: &mut String, max: usize) {
    if s.len() > max {
        let mut end = max;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
}

fn clamp_opt(s: &mut Option<String>, max: usize) {
    if let Some(v) = s {
        truncate(v, max);
        if v.is_empty() {
            *s = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_respects_char_boundaries() {
        let mut s = "aé".repeat(10); // 3 bytes per pair
        truncate(&mut s, 4);
        assert_eq!(s, "aéa");
        let mut short = String::from("ok");
        truncate(&mut short, 4);
        assert_eq!(short, "ok");
    }

    #[test]
    fn clamp_bounds_every_field() {
        let long = "x".repeat(MAX_DESCRIPTION * 2);
        let mut c = Claims {
            name: Some(long.clone()),
            size: Some(1),
            files: vec![
                ClaimedFile {
                    path: long.clone(),
                    size: None
                };
                MAX_FILES + 5
            ],
            trackers: vec![long.clone(); MAX_TRACKERS + 5],
            ids: vec![
                ExternalId {
                    scheme: "IMDB".into(),
                    value: long.clone()
                };
                MAX_IDS + 5
            ],
            description: Some(long.clone()),
            publisher: Some(long),
        };
        c.clamp();
        assert_eq!(c.name.as_ref().unwrap().len(), MAX_NAME);
        assert!(c.files.iter().all(|f| f.path.len() == MAX_PATH));
        assert!(c.text_len() <= MAX_TOTAL);
        assert!(!c.files.is_empty() && c.files.len() < MAX_FILES);
        assert_eq!(c.trackers.len(), MAX_TRACKERS);
        assert_eq!(c.ids.len(), MAX_IDS);
        assert_eq!(c.ids[0].scheme, "imdb");
        assert_eq!(c.ids[0].value.len(), MAX_ID_PART);
        assert_eq!(c.description.as_ref().unwrap().len(), MAX_DESCRIPTION);
        assert_eq!(c.publisher.as_ref().unwrap().len(), MAX_PUBLISHER);
    }

    #[test]
    fn small_file_lists_are_kept_whole() {
        let mut c = Claims {
            files: vec![
                ClaimedFile {
                    path: "a/b.mkv".into(),
                    size: Some(1)
                };
                MAX_FILES + 1
            ],
            ..Claims::default()
        };
        c.clamp();
        assert_eq!(c.files.len(), MAX_FILES);
    }

    #[test]
    fn clamp_drops_empty_parts() {
        let mut c = Claims {
            name: Some(String::new()),
            trackers: vec![String::new()],
            ids: vec![ExternalId {
                scheme: "tmdb".into(),
                value: String::new(),
            }],
            ..Claims::default()
        };
        c.clamp();
        assert!(c.is_empty());
    }
}
