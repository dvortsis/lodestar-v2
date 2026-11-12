//! A `.torrent` the user hands Lodestar themselves: the upload route
//! (UP-0) and, later, the watch folder (UP-1). Plan
//! `docs/plan/2026-11-05-v07-m7.md` §5 and §4.3 UP-0; architecture §3,
//! §18.7; `docs/decisions/2026-11-12-UP-0.md`.
//!
//! - **Read** ([`read`]): the file is parsed with
//!   [`TorrentFile::parse_with`] under [`LIMITS`], within
//!   [`MAX_BYTES`]. The info dict is then read again from its own raw
//!   span with no encoding hint, exactly as BEP 9 delivers it: the file's
//!   top-level `encoding` key isn't covered by the hash, so its sender
//!   can't choose how a name decodes. The key is that dict's primary hash
//!   (its SHA-1, or for a v2-only file its truncated SHA-256).
//! - **Claims:** only what's outside the info dict, as the torrent cache
//!   keeps them (SRC-13): `announce`/`announce-list` through
//!   X-claim-trackers' guard plus the cache's query, fragment and
//!   passkey rules; `comment` as the description; `creation date` as the
//!   provenance's publication time when it's believable. `created by`
//!   has no claim field and is dropped; `url-list` and every other
//!   top-level key are dropped, never fetched (§18.7). **A torrent marked
//!   `private` (BEP 27) keeps no tracker and no comment:** a private
//!   tracker's announce address holds the uploader's passkey.
//! - **Private:** a `private=1` info dict is stored with the store's
//!   `flags::PRIVATE` (BEP 27, from the dict itself) and as
//!   `private_source`, since its hash may be known nowhere else. So every
//!   §18.7 gate leaves it out under its own rule: it never reaches the
//!   DHT, a tracker, a scrape, a cache lookup, a refetch, a feed or an
//!   export.
//! - **Judged** ([`task`]): on the blocking pool, through
//!   [`torrent_cache::judge_sealed`], exactly as a cache's verified file
//!   is (the floor's hash list first, then the full filter). A held
//!   verdict is never stored; what may be stored or held goes to the
//!   writer's lease-free entry, which judges it again at commit. Only the
//!   info dict is passed on: the store keeps it (minus `pieces`) only when
//!   the info-dict archive is on, as for a BEP 9 fetch. The rest of the
//!   file is dropped once it's read.
//!
//! Nothing here logs, and no [`Debug`] shows a name, a hash or any text
//! from the file.
//!
//! [`torrent_cache::judge_sealed`]: crate::torrent_cache::judge_sealed

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use lodestar_filter::Sealer;
use lodestar_metainfo::{Info, InfoHash, Limits, ParseOptions, TorrentFile};
use lodestar_sources::claims::{self, Claims, Confidence, Provenance};
use lodestar_store::{Preparer, SourceId};
use tokio::sync::{mpsc, oneshot};

use crate::torrent_cache::{Judged, Refusal, ToCommit, judge_sealed};

/// The built-in source uploads are stored under: what a torrent's
/// provenance names.
pub const SOURCE: &str = "upload";

/// The largest file taken, in bytes: 8 MiB (plan §5, a §2.26 number the
/// runner may raise to 32 MiB). The route refuses a longer body before
/// reading it; [`read`] refuses one too.
pub const MAX_BYTES: usize = 8 * 1024 * 1024;

/// `lodestar-metainfo`'s limits for an uploaded file: the torrent
/// cache's (a whole file within [`MAX_BYTES`], half the default bencode
/// values and files), which fits any torrent an 8 MiB file can hold.
pub const LIMITS: Limits = Limits {
    max_torrent_bytes: MAX_BYTES,
    max_items: 1_000_000,
    max_files: 100_000,
    ..Limits::DEFAULT
};

/// The longest tracker address kept, as the torrent cache's.
pub const MAX_TRACKER_URL: usize = 512;
/// A path segment this long, all letters and digits, is taken for a
/// passkey: a tracker address with one isn't kept (the cache's rule).
pub const PASSKEY_MIN: usize = 16;
/// Creation dates outside 2001-07-01 to 2100 aren't believed.
pub const MIN_CREATED: i64 = 993_945_600;
pub const MAX_CREATED: i64 = 4_102_444_800;

/// Uploads waiting for the judge. Small: each holds a whole file.
const QUEUE: usize = 4;

/// An uploaded file, read: its key, the info dict's raw bytes and the
/// claims from outside it. Its `Debug` shows none of them.
pub struct Local {
    pub key: InfoHash,
    pub info_raw: Vec<u8>,
    pub claims: Claims,
    pub provenance: Provenance,
    /// The info dict says `private=1` (BEP 27).
    pub private: bool,
}

impl std::fmt::Debug for Local {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Local")
            .field("private", &self.private)
            .finish_non_exhaustive()
    }
}

/// The body isn't a `.torrent` within [`MAX_BYTES`] and [`LIMITS`]. It
/// carries no detail: the cause is never echoed from the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Malformed;

/// Reads an uploaded file. Sync and free of I/O, so it's fuzzed directly
/// (`fuzz/fuzz_targets/local_torrent.rs`).
///
/// # Errors
/// [`Malformed`] when the body is over [`MAX_BYTES`] or doesn't parse as
/// a `.torrent` within [`LIMITS`].
pub fn read(body: &[u8]) -> Result<Local, Malformed> {
    if body.len() > MAX_BYTES {
        return Err(Malformed);
    }
    let torrent = TorrentFile::parse_with(body, &LIMITS).map_err(|_| Malformed)?;
    let info_raw = TorrentFile::raw_info(body, &LIMITS)
        .map_err(|_| Malformed)?
        .to_vec();
    // As BEP 9 delivers it: no hint from the unhashed `encoding` key.
    let info = Info::parse_with(
        &info_raw,
        &ParseOptions {
            limits: LIMITS,
            encoding_hint: None,
        },
    )
    .map_err(|_| Malformed)?;
    let private = info.private;
    Ok(Local {
        key: info.primary_hash(),
        info_raw,
        claims: claims_of(&torrent, private),
        provenance: Provenance {
            external_ref: None,
            published_at: torrent
                .creation_date
                .filter(|d| (MIN_CREATED..=MAX_CREATED).contains(d)),
            // The fields outside the info dict aren't covered by the hash.
            confidence: Confidence::Medium,
        },
        private,
    })
}

/// The fields outside the info dict as claims: the trackers and the
/// comment, neither for a private torrent. `created by` has no claim
/// field; `url-list` is never kept.
fn claims_of(torrent: &TorrentFile, private: bool) -> Claims {
    let mut c = Claims::default();
    if !private {
        for url in torrent.announce.iter().flatten() {
            if c.trackers.len() >= claims::MAX_TRACKERS {
                break;
            }
            if let Some(url) = public_tracker(url)
                && !c.trackers.contains(&url)
            {
                c.trackers.push(url);
            }
        }
        c.description = torrent.comment.clone().filter(|s| !s.is_empty());
    }
    c.clamp();
    c
}

/// `raw` as the address that was checked, when it's a public tracker:
/// X-claim-trackers' guard ([`lodestar_integrations::net::
/// public_tracker_url`]), and no query, fragment or passkey-shaped path
/// segment, as the torrent cache keeps them. The writer's claim check
/// runs on it again before it's stored.
fn public_tracker(raw: &str) -> Option<String> {
    if raw.len() > MAX_TRACKER_URL {
        return None;
    }
    let kept = lodestar_integrations::net::public_tracker_url(raw)?;
    // The guard's form is `scheme://host[:port]` and then the path, with
    // no `@` and nothing but the host and port before the first `/`.
    let rest = kept.split_once("://")?.1;
    let path = rest.find('/').map_or("", |i| &rest[i..]);
    let ok = !kept.contains(['?', '#'])
        && !path
            .split('/')
            .any(|s| s.len() >= PASSKEY_MIN && s.bytes().all(|b| b.is_ascii_alphanumeric()))
        && kept.len() <= MAX_TRACKER_URL;
    ok.then_some(kept)
}

/// What became of an upload, once judged. The writer commits what may be
/// stored after this answer, judging it again if the rules have moved on
/// meanwhile (stricter only).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Handed to the writer to store (or, already stored, to add the
    /// upload as a source).
    Accepted {
        key: InfoHash,
        /// Stored with BEP 27's private mark.
        private: bool,
    },
    /// Not a `.torrent` within the limits, or one the store can't prepare.
    Malformed,
    /// On the floor's hash list.
    Refused,
    /// The filter holds it (drop or quarantine): never stored.
    Held,
}

// The key is the user's own, but nothing here logs it.
impl std::fmt::Debug for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Outcome::Accepted { private, .. } => f
                .debug_struct("Accepted")
                .field("private", private)
                .finish_non_exhaustive(),
            Outcome::Malformed => f.write_str("Malformed"),
            Outcome::Refused => f.write_str("Refused"),
            Outcome::Held => f.write_str("Held"),
        }
    }
}

/// Uploads aren't being taken: the pipeline has stopped, or its judge
/// ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Closed;

/// One upload for the judge, with where its answer goes.
pub struct Upload {
    body: Vec<u8>,
    reply: oneshot::Sender<Outcome>,
}

impl std::fmt::Debug for Upload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Upload").finish_non_exhaustive()
    }
}

/// What became of uploads since the start. Never holds a hash or a name.
/// Commits are counted by the writer with the torrent cache's
/// (`WriterStats::torrent_cache`), since uploads share its lease-free
/// entry.
#[derive(Debug, Default)]
pub struct UploadStats {
    /// Files handed over.
    pub received: AtomicU64,
    /// Judged storable and handed to the writer.
    pub accepted: AtomicU64,
    /// Not a `.torrent` within the limits.
    pub malformed: AtomicU64,
    /// On the floor's hash list.
    pub refused: AtomicU64,
    /// Held by the filter at judging.
    pub held: AtomicU64,
}

/// Where the host hands the pipeline uploaded files
/// (`Pipeline::uploads`). Cheap to clone. Sending waits for room, so the
/// files waiting are bounded.
#[derive(Debug, Clone)]
pub struct Uploads {
    tx: mpsc::Sender<Upload>,
    stats: Arc<UploadStats>,
}

impl Uploads {
    /// Reads and judges `body`, and hands what may be stored or held to
    /// the writer.
    ///
    /// # Errors
    /// [`Closed`] when uploads aren't being taken.
    pub async fn submit(&self, body: Vec<u8>) -> Result<Outcome, Closed> {
        self.stats.received.fetch_add(1, Ordering::Relaxed);
        let (reply, answer) = oneshot::channel();
        self.tx
            .send(Upload { body, reply })
            .await
            .map_err(|_| Closed)?;
        let outcome = answer.await.map_err(|_| Closed)?;
        let counter = match outcome {
            Outcome::Accepted { .. } => &self.stats.accepted,
            Outcome::Malformed => &self.stats.malformed,
            Outcome::Refused => &self.stats.refused,
            Outcome::Held => &self.stats.held,
        };
        counter.fetch_add(1, Ordering::Relaxed);
        Ok(outcome)
    }

    pub fn stats(&self) -> &UploadStats {
        &self.stats
    }
}

/// The uploads channel: its sending half for the host, and the receiving
/// half [`task`] reads.
pub fn channel() -> (Uploads, mpsc::Receiver<Upload>) {
    let (tx, rx) = mpsc::channel(QUEUE);
    (
        Uploads {
            tx,
            stats: Arc::default(),
        },
        rx,
    )
}

/// What the judge hands on: an outcome, and the commit for the writer
/// when there is one.
fn judge_one(
    engine: &lodestar_filter::Engine,
    preparer: &mut Preparer,
    sealer: Option<&Sealer>,
    source: SourceId,
    body: Vec<u8>,
    now: i64,
) -> (Outcome, Option<ToCommit>) {
    let read = read(&body);
    // Only the info dict goes on: the rest of the file is dropped here.
    drop(body);
    let Ok(local) = read else {
        return (Outcome::Malformed, None);
    };
    let judged = judge_sealed(engine, preparer, sealer, local.key, &local.info_raw, now);
    let outcome = match &judged {
        Judged::Store(_) => Outcome::Accepted {
            key: local.key,
            private: local.private,
        },
        Judged::Held { .. } => Outcome::Held,
        Judged::Refused(Refusal::Blocked) => return (Outcome::Refused, None),
        // `read` derived the key from this dict, so a mismatch can't
        // happen; it and a dict that won't prepare are malformed.
        Judged::Refused(Refusal::Mismatch | Refusal::Invalid) => {
            return (Outcome::Malformed, None);
        }
    };
    let held = matches!(judged, Judged::Held { .. });
    let commit = ToCommit {
        source,
        judged,
        claims: local.claims,
        provenance: local.provenance,
        at: now,
        // Kept for a hold only: the writer judges it again when the rules
        // changed before its commit.
        info_raw: if held { local.info_raw } else { Vec::new() },
        key: local.key,
        // BEP 27: its hash may be known nowhere else (§18.7).
        private_source: local.private,
    };
    (outcome, Some(commit))
}

/// Judges each upload ([`judge_one`], on the blocking pool, under the
/// filter's current rules), answers it, and passes what may be stored or
/// held to the writer's lease-free entry as source `source`. The answer
/// is sent once the writer has taken the commit. Ends when the channel
/// or the writer closes, or a judgment panics (every later upload is
/// then [`Closed`], and nothing is stored from the one that panicked).
pub(crate) async fn task(
    mut uploads: mpsc::Receiver<Upload>,
    out: mpsc::Sender<ToCommit>,
    mut preparer: Preparer,
    filter: Arc<lodestar_filter::Handle>,
    sealer: Option<Arc<Sealer>>,
    source: SourceId,
) {
    while let Some(Upload { body, reply }) = uploads.recv().await {
        let engine = filter.current();
        let sealer = sealer.clone();
        let now = crate::unix_now();
        let Ok((p, outcome, commit)) = tokio::task::spawn_blocking(move || {
            let (outcome, commit) =
                judge_one(&engine, &mut preparer, sealer.as_deref(), source, body, now);
            (preparer, outcome, commit)
        })
        .await
        else {
            return;
        };
        preparer = p;
        if let Some(commit) = commit
            && out.send(commit).await.is_err()
        {
            return;
        }
        // The uploader may have gone; the commit stands either way.
        let _ = reply.send(outcome);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_bencode::{OwnedValue as Value, to_vec as encode};

    fn dict(pairs: Vec<(&str, Value)>) -> Value {
        Value::Dict(
            pairs
                .into_iter()
                .map(|(k, v)| (k.as_bytes().to_vec(), v))
                .collect(),
        )
    }

    fn bytes(s: &str) -> Value {
        Value::Bytes(s.as_bytes().to_vec())
    }

    fn info(name: &str, private: bool) -> Value {
        let mut pairs = vec![
            ("name", bytes(name)),
            ("piece length", Value::Int(16384)),
            ("length", Value::Int(1000)),
            ("pieces", Value::Bytes(vec![7; 20])),
        ];
        if private {
            pairs.push(("private", Value::Int(1)));
        }
        dict(pairs)
    }

    fn file(info: Value, extra: Vec<(&str, Value)>) -> Vec<u8> {
        let mut pairs = vec![("info", info)];
        pairs.extend(extra);
        encode(&dict(pairs))
    }

    #[test]
    fn the_limits_are_the_caches() {
        assert_eq!(LIMITS.max_torrent_bytes, MAX_BYTES);
        assert_eq!(MAX_BYTES, 8 << 20);
    }

    #[test]
    fn reads_the_key_from_the_raw_info_dict() {
        let raw_info = encode(&info("Some.Name", false));
        let body = file(info("Some.Name", false), vec![]);
        let l = read(&body).unwrap();
        assert_eq!(l.info_raw, raw_info);
        assert_eq!(l.key, Info::parse(&raw_info).unwrap().primary_hash());
        assert!(!l.private);
        assert!(l.claims.is_empty());
    }

    #[test]
    fn keeps_public_trackers_and_the_comment_and_drops_web_seeds() {
        let body = file(
            info("Some.Name", false),
            vec![
                ("announce", bytes("udp://tracker.example:6969/announce")),
                (
                    "announce-list",
                    Value::List(vec![Value::List(vec![
                        bytes("udp://tracker.example:6969/announce"),
                        bytes("http://192.168.1.1/announce"),
                        bytes("http://localhost/announce"),
                        bytes("https://t.example/abcdefghijklmnop1234/announce"),
                        bytes("https://t.example/announce?passkey=x"),
                        bytes("https://other.example/announce"),
                    ])]),
                ),
                ("comment", bytes("a comment")),
                ("created by", bytes("maker 1.0")),
                ("creation date", Value::Int(1_600_000_000)),
                ("url-list", bytes("https://seed.example/file")),
            ],
        );
        let l = read(&body).unwrap();
        assert_eq!(
            l.claims.trackers,
            [
                "udp://tracker.example:6969/announce",
                "https://other.example/announce"
            ]
        );
        assert_eq!(l.claims.description.as_deref(), Some("a comment"));
        assert_eq!(l.provenance.published_at, Some(1_600_000_000));
        assert_eq!(l.provenance.confidence, Confidence::Medium);
        let text = format!("{:?}", l.claims);
        assert!(!text.contains("seed.example"), "url-list is dropped");
        assert!(!text.contains("maker"), "created by has no claim field");
    }

    #[test]
    fn a_private_torrent_keeps_no_tracker_or_comment() {
        let body = file(
            info("Some.Name", true),
            vec![
                ("announce", bytes("https://tracker.example/announce")),
                ("comment", bytes("from the tracker")),
            ],
        );
        let l = read(&body).unwrap();
        assert!(l.private);
        assert!(l.claims.trackers.is_empty());
        assert!(l.claims.description.is_none());
    }

    #[test]
    fn an_unbelievable_creation_date_is_dropped() {
        for d in [0, MIN_CREATED - 1, MAX_CREATED + 1, -5] {
            let body = file(
                info("Some.Name", false),
                vec![("creation date", Value::Int(d))],
            );
            assert_eq!(read(&body).unwrap().provenance.published_at, None);
        }
    }

    #[test]
    fn the_encoding_key_doesnt_choose_how_the_name_decodes() {
        // A name in GBK with a top-level hint: the dict is read without it.
        let name = Value::Bytes(vec![0xc4, 0xe3, 0xba, 0xc3]);
        let inner = dict(vec![
            ("name", name),
            ("piece length", Value::Int(16384)),
            ("length", Value::Int(1)),
            ("pieces", Value::Bytes(vec![1; 20])),
        ]);
        let raw = encode(&inner);
        let hinted = file(inner, vec![("encoding", bytes("GBK"))]);
        let l = read(&hinted).unwrap();
        assert_eq!(l.info_raw, raw);
        let plain = Info::parse(&raw).unwrap();
        let got = Info::parse(&l.info_raw).unwrap();
        assert_eq!(got.name, plain.name);
    }

    #[test]
    fn malformed_bodies_are_refused_without_detail() {
        let too_big = vec![b'd'; MAX_BYTES + 1];
        for body in [
            &b""[..],
            b"not bencode",
            b"le",
            b"de",
            b"d4:infoi1ee",
            b"d4:infod4:name1:xee",
            &too_big,
        ] {
            assert_eq!(read(body).unwrap_err(), Malformed);
        }
    }

    #[test]
    fn debug_shows_no_text_from_the_file() {
        let body = file(
            info("Secret.Name", false),
            vec![("comment", bytes("secret comment"))],
        );
        let l = read(&body).unwrap();
        let shown = format!("{l:?}");
        assert!(!shown.contains("Secret") && !shown.contains("secret"));
        let o = Outcome::Accepted {
            key: l.key,
            private: false,
        };
        assert_eq!(format!("{o:?}"), "Accepted { private: false, .. }");
    }
}
