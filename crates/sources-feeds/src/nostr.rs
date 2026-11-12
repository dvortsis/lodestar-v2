//! Nostr relay messages (NIP-01) and torrent events (NIP-35, kind 2003),
//! for the Nostr discovery source (architecture §18.2 row 21, plan
//! `docs/plan/2026-11-06-v08.md` §5.3).
//!
//! [`parse`] is a pure function from one relay message (one WebSocket text
//! frame) to a [`Message`], or a [`Refused`] reason. It reads with the
//! bounded [`json`](crate::json) reader, never `serde_json`: the message is
//! cut off at [`Limits::max_bytes`] before it's read, nesting past the
//! four levels an `EVENT` has is refused, and every event field is checked
//! for shape. An `EVENT`'s id is recomputed as the SHA-256 of NIP-01's
//! canonical serialisation `[0,pubkey,created_at,kind,tags,content]`, and
//! an event whose id doesn't match is refused ([`Refused::IdMismatch`]).
//! The signature's shape is checked here; the BIP-340 check itself is
//! NOS-1's.
//!
//! [`release`] maps a kind 2003 event to the claims it makes
//! ([`Release`]): the key only from its `x` tag, and every other field
//! cut to the claims' limits (`lodestar_sources::claims`). Nothing here
//! panics, and nothing allocates more than the message's own text.
//!
//! The revisions followed are recorded in `docs/decisions/2026-11-12-NOS-0.md`.

pub use crate::common::Stop;
use crate::common::title;
use crate::json::{self, R, Reader, Walk};
use lodestar_metainfo::{InfoHash, hidden_char};
use sha2::{Digest, Sha256};

/// Bytes in a kept name: the claims' name limit.
pub const MAX_NAME: usize = 1024;
/// Kept files per release: the claims' file limit.
pub const MAX_FILES: usize = 10_000;
/// Bytes in a kept file path: the claims' path limit.
pub const MAX_PATH: usize = 4096;
/// Kept trackers per release: the claims' tracker limit.
pub const MAX_TRACKERS: usize = 64;
/// Bytes in a kept tracker URL: the claims' tracker limit.
pub const MAX_TRACKER: usize = 2048;
/// Kept external ids per release: the claims' id limit.
pub const MAX_IDS: usize = 32;
/// Bytes in an id's scheme or value: the claims' id-part limit.
pub const MAX_ID_PART: usize = 128;
/// Bytes in a kept description: the claims' description limit.
pub const MAX_DESCRIPTION: usize = 64 * 1024;
/// Kept tags (`t`, `tcat:`, `newznab:`) per release.
pub const MAX_KEPT_TAGS: usize = 64;
/// Bytes in a kept tag.
pub const MAX_TAG: usize = 128;
/// Bytes in a subscription id (NIP-01: at most 64 characters).
pub const MAX_SUB_ID: usize = 64;
/// Bytes kept of a relay's `NOTICE`, `CLOSED` or `OK` text. Longer text
/// is cut; it's only ever logged.
pub const MAX_REASON: usize = 256;
/// The NIP-35 torrent event kind.
pub const KIND_TORRENT: u16 = 2003;

/// Caps on one message. The defaults are [`LIMITS`]; why each value:
/// `docs/decisions/2026-11-12-NOS-0.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Bytes in one relay message; a longer one is refused unread.
    pub max_bytes: usize,
    /// Tags in one event.
    pub max_tags: usize,
    /// Strings in one tag.
    pub max_tag_items: usize,
}

/// The caps for a real relay (plan §5.3: messages ≤ 128 KB, tags ≤ 512).
pub const LIMITS: Limits = Limits {
    max_bytes: 128 * 1024,
    max_tags: 512,
    max_tag_items: 16,
};

/// A signed Nostr event whose shape and id are checked. The signature is
/// not yet verified (NOS-1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// The SHA-256 of the canonical serialisation, as recomputed.
    pub id: [u8; 32],
    /// The x-only secp256k1 public key.
    pub pubkey: [u8; 32],
    pub created_at: u64,
    pub kind: u16,
    pub tags: Vec<Vec<String>>,
    pub content: String,
    /// The BIP-340 signature over `id`, unchecked here.
    pub sig: [u8; 64],
}

/// One relay-to-client message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// `["EVENT", <sub>, <event>]`.
    Event { sub: String, event: Box<Event> },
    /// `["OK", <event id>, <accepted>, <reason>]`.
    Ok {
        id: [u8; 32],
        accepted: bool,
        reason: String,
    },
    /// `["EOSE", <sub>]`: stored events end here.
    Eose { sub: String },
    /// `["CLOSED", <sub>, <reason>]`: the relay ended the subscription.
    Closed { sub: String, reason: String },
    /// `["NOTICE", <text>]`.
    Notice { text: String },
}

/// Which event field a shape fault is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Id,
    Pubkey,
    CreatedAt,
    Kind,
    Tags,
    Content,
    Sig,
}

/// Why a message was refused. Nothing from a refused message is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// Longer than [`Limits::max_bytes`]; not read.
    TooLarge,
    /// Not UTF-8.
    NotUtf8,
    /// Not JSON, nested too deep or too many values ([`Stop`]).
    Json(Stop),
    /// JSON, but not a relay message's layout (wrong element count or
    /// type, an over-long subscription id).
    Shape,
    /// A message type this client doesn't know (`COUNT`, `REQ`, …).
    Unknown,
    /// `AUTH` (NIP-42): recognised and refused; the client doesn't
    /// authenticate.
    Auth,
    /// An event field is missing, repeated or the wrong shape
    /// (lower-case hex of the right length, an integer in range).
    Event(Field),
    /// More than [`Limits::max_tags`] tags, or a tag with more than
    /// [`Limits::max_tag_items`] strings or none.
    Tags,
    /// The id isn't the SHA-256 of the canonical serialisation.
    IdMismatch,
    /// An event key is written with an escape. Its decoded form could be
    /// a field another reader would take, so two readers could see two
    /// events; refused like a repeated key.
    EscapedKey,
}

impl From<Stop> for Refused {
    fn from(s: Stop) -> Self {
        Refused::Json(s)
    }
}

/// One relay message. See the module docs.
pub fn parse(msg: &[u8], limits: &Limits) -> Result<Message, Refused> {
    if msg.len() > limits.max_bytes {
        return Err(Refused::TooLarge);
    }
    let text = std::str::from_utf8(msg).map_err(|_| Refused::NotUtf8)?;
    let jl = json::Limits {
        // ["EVENT", sub, {"tags": [[…]]}]
        max_depth: 4,
        // The message, its three elements, an event's seven fields and
        // its tags with their strings, with room for unknown fields.
        max_values: 64
            + limits
                .max_tags
                .saturating_mul(limits.max_tag_items.saturating_add(1)),
        max_array: limits.max_tag_items,
        // The message is already capped, and decoding only shrinks text.
        max_text: limits.max_bytes,
    };
    let mut j = Reader::new(text, jl);
    let mut out: Result<Option<Message>, Refused> = Ok(None);
    let mut st = St::Start;
    j.array(|j, _| {
        if out.is_err() {
            return j.skip().map(|_| ());
        }
        match element(j, &mut st, limits)? {
            Ok(Some(m)) => out = Ok(Some(m)),
            Ok(None) => {}
            Err(e) => out = Err(e),
        }
        Ok(())
    })?;
    j.end()?;
    out?.ok_or(Refused::Shape)
}

/// Where a message is, element by element.
enum St {
    Start,
    Event,
    EventSub(String),
    Ok,
    OkId([u8; 32]),
    OkAccepted([u8; 32], bool),
    Eose,
    Closed,
    ClosedSub(String),
    Notice,
    /// The message is complete; another element is a [`Refused::Shape`].
    Done,
}

/// Reads the next element of a message in state `st`: the message once
/// its last element is read, or why it's refused.
fn element(
    j: &mut Reader<'_>,
    st: &mut St,
    limits: &Limits,
) -> R<Result<Option<Message>, Refused>> {
    let now = std::mem::replace(st, St::Done);
    Ok(match now {
        St::Start => {
            if j.peek() != Some(b'"') {
                j.skip()?;
                return Ok(Err(Refused::Shape));
            }
            let Ok(k) = j.text()? else {
                return Ok(Err(Refused::Shape));
            };
            *st = match k.as_str() {
                "EVENT" => St::Event,
                "OK" => St::Ok,
                "EOSE" => St::Eose,
                "CLOSED" => St::Closed,
                "NOTICE" => St::Notice,
                "AUTH" => return Ok(Err(Refused::Auth)),
                _ => return Ok(Err(Refused::Unknown)),
            };
            Ok(None)
        }
        St::Event => sub_id(j)?.map(|s| {
            *st = St::EventSub(s);
            None
        }),
        St::EventSub(sub) => event(j, limits)?.map(|e| {
            Some(Message::Event {
                sub,
                event: Box::new(e),
            })
        }),
        St::Eose => sub_id(j)?.map(|sub| Some(Message::Eose { sub })),
        St::Closed => sub_id(j)?.map(|s| {
            *st = St::ClosedSub(s);
            None
        }),
        St::ClosedSub(sub) => reason(j)?.map(|reason| Some(Message::Closed { sub, reason })),
        St::Notice => reason(j)?.map(|text| Some(Message::Notice { text })),
        St::Ok => match hex::<32>(j)? {
            Some(id) => {
                *st = St::OkId(id);
                Ok(None)
            }
            None => Err(Refused::Shape),
        },
        St::OkId(id) => match j.peek() {
            Some(b't' | b'f') => {
                let accepted = j.peek() == Some(b't');
                j.skip()?;
                *st = St::OkAccepted(id, accepted);
                Ok(None)
            }
            _ => {
                j.skip()?;
                Err(Refused::Shape)
            }
        },
        St::OkAccepted(id, accepted) => reason(j)?.map(|reason| {
            Some(Message::Ok {
                id,
                accepted,
                reason,
            })
        }),
        St::Done => {
            j.skip()?;
            Err(Refused::Shape)
        }
    })
}

/// A subscription id: a string of 1–[`MAX_SUB_ID`] bytes with no hidden
/// characters (it's ours, echoed back, and may be logged).
fn sub_id(j: &mut Reader<'_>) -> R<Result<String, Refused>> {
    if j.peek() != Some(b'"') {
        j.skip()?;
        return Ok(Err(Refused::Shape));
    }
    Ok(match j.text()? {
        Ok(s) if !s.is_empty() && s.len() <= MAX_SUB_ID && !s.chars().any(hidden_char) => Ok(s),
        _ => Err(Refused::Shape),
    })
}

/// A relay's free text, hidden characters removed and cut to
/// [`MAX_REASON`] bytes.
fn reason(j: &mut Reader<'_>) -> R<Result<String, Refused>> {
    if j.peek() != Some(b'"') {
        j.skip()?;
        return Ok(Err(Refused::Shape));
    }
    Ok(match j.text()? {
        Ok(s) => Ok(cut(s.chars().filter(|&c| !hidden_char(c)), MAX_REASON)),
        Err(_) => Err(Refused::Shape),
    })
}

/// The characters of `it`, up to `max` bytes, cut on a character boundary.
fn cut(it: impl Iterator<Item = char>, max: usize) -> String {
    let mut out = String::new();
    for c in it {
        if out.len() + c.len_utf8() > max {
            break;
        }
        out.push(c);
    }
    out
}

/// A string of exactly `2 * N` lower-case hex digits, as NIP-01 requires.
/// `None` for anything else, read past.
fn hex<const N: usize>(j: &mut Reader<'_>) -> R<Option<[u8; N]>> {
    if j.peek() != Some(b'"') {
        j.skip()?;
        return Ok(None);
    }
    Ok(match j.text()? {
        Ok(s) => unhex::<N>(&s),
        Err(_) => None,
    })
}

fn unhex<const N: usize>(s: &str) -> Option<[u8; N]> {
    let b = s.as_bytes();
    if b.len() != 2 * N {
        return None;
    }
    let nib = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    let mut out = [0u8; N];
    for (o, p) in out.iter_mut().zip(b.chunks_exact(2)) {
        *o = (nib(p[0])? << 4) | nib(p[1])?;
    }
    Some(out)
}

fn to_hex(b: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len() * 2);
    for &x in b {
        s.push(char::from(H[usize::from(x >> 4)]));
        s.push(char::from(H[usize::from(x & 15)]));
    }
    s
}

/// A non-negative JSON integer, written without a fraction or exponent.
fn uint(j: &mut Reader<'_>) -> R<Option<u64>> {
    if !matches!(j.peek(), Some(b'0'..=b'9')) {
        j.skip()?;
        return Ok(None);
    }
    let n = j.num()?;
    Ok(if n.bytes().all(|b| b.is_ascii_digit()) {
        n.parse().ok()
    } else {
        None
    })
}

/// An event object: every field present once and of the right shape,
/// the tags within the caps, and the id the canonical serialisation's.
fn event(j: &mut Reader<'_>, limits: &Limits) -> R<Result<Event, Refused>> {
    if j.peek() != Some(b'{') {
        j.skip()?;
        return Ok(Err(Refused::Shape));
    }
    let mut id = None;
    let mut pubkey = None;
    let mut created_at = None;
    let mut kind = None;
    let mut tags = None;
    let mut content = None;
    let mut sig = None;
    let mut bad: Option<Refused> = None;
    j.object(|j, key| {
        if key.contains('\\') {
            bad.get_or_insert(Refused::EscapedKey);
            return j.skip().map(|_| ());
        }
        let f = match key {
            "id" => Field::Id,
            "pubkey" => Field::Pubkey,
            "created_at" => Field::CreatedAt,
            "kind" => Field::Kind,
            "tags" => Field::Tags,
            "content" => Field::Content,
            "sig" => Field::Sig,
            _ => return j.skip().map(|_| ()),
        };
        if bad.is_some() {
            return j.skip().map(|_| ());
        }
        let ok = match f {
            Field::Id => set(&mut id, hex::<32>(j)?),
            Field::Pubkey => set(&mut pubkey, hex::<32>(j)?),
            Field::Sig => set(&mut sig, hex::<64>(j)?),
            Field::CreatedAt => set(&mut created_at, uint(j)?),
            Field::Kind => set(&mut kind, uint(j)?.and_then(|k| u16::try_from(k).ok())),
            Field::Content => {
                let c = if j.peek() == Some(b'"') {
                    j.text()?.ok()
                } else {
                    j.skip()?;
                    None
                };
                set(&mut content, c)
            }
            Field::Tags => match read_tags(j, limits)? {
                Ok(t) => set(&mut tags, Some(t)),
                Err(e) => {
                    bad = Some(e);
                    true
                }
            },
        };
        if !ok {
            bad = Some(Refused::Event(f));
        }
        Ok(())
    })?;
    if let Some(e) = bad {
        return Ok(Err(e));
    }
    let missing = [
        (id.is_none(), Field::Id),
        (pubkey.is_none(), Field::Pubkey),
        (created_at.is_none(), Field::CreatedAt),
        (kind.is_none(), Field::Kind),
        (tags.is_none(), Field::Tags),
        (content.is_none(), Field::Content),
        (sig.is_none(), Field::Sig),
    ]
    .into_iter()
    .find_map(|(m, f)| m.then_some(f));
    let (
        Some(id),
        Some(pubkey),
        Some(created_at),
        Some(kind),
        Some(tags),
        Some(content),
        Some(sig),
    ) = (id, pubkey, created_at, kind, tags, content, sig)
    else {
        return Ok(Err(Refused::Event(missing.unwrap_or(Field::Id))));
    };
    let e = Event {
        id,
        pubkey,
        created_at,
        kind,
        tags,
        content,
        sig,
    };
    if event_id(&e.pubkey, e.created_at, e.kind, &e.tags, &e.content) != e.id {
        return Ok(Err(Refused::IdMismatch));
    }
    Ok(Ok(e))
}

/// Stores `v` in an empty `slot`. False when `v` is `None` (a bad value)
/// or the slot is already full (a repeated key, which could make two
/// readers see two events).
fn set<T>(slot: &mut Option<T>, v: Option<T>) -> bool {
    match (slot.is_some(), v) {
        (false, Some(v)) => {
            *slot = Some(v);
            true
        }
        _ => false,
    }
}

/// `tags`: an array of at most `max_tags` arrays, each of 1 to
/// `max_tag_items` strings.
fn read_tags(j: &mut Reader<'_>, limits: &Limits) -> R<Result<Vec<Vec<String>>, Refused>> {
    if j.peek() != Some(b'[') {
        j.skip()?;
        return Ok(Err(Refused::Event(Field::Tags)));
    }
    let mut tags: Vec<Vec<String>> = Vec::new();
    let mut bad = None;
    j.array(|j, n| {
        if bad.is_some() || n >= limits.max_tags {
            bad.get_or_insert(Refused::Tags);
            return j.skip().map(|_| ());
        }
        if j.peek() != Some(b'[') {
            bad = Some(Refused::Event(Field::Tags));
            return j.skip().map(|_| ());
        }
        let mut tag = Vec::new();
        j.array(|j, m| {
            if bad.is_some() || m >= limits.max_tag_items {
                bad.get_or_insert(Refused::Tags);
                return j.skip().map(|_| ());
            }
            if j.peek() != Some(b'"') {
                bad = Some(Refused::Event(Field::Tags));
                return j.skip().map(|_| ());
            }
            match j.text()? {
                Ok(s) => tag.push(s),
                Err(_) => bad = Some(Refused::Event(Field::Tags)),
            }
            Ok(())
        })?;
        if tag.is_empty() {
            bad.get_or_insert(Refused::Tags);
        }
        tags.push(tag);
        Ok(())
    })?;
    Ok(match bad {
        Some(e) => Err(e),
        None => Ok(tags),
    })
}

/// NIP-01's event id: the SHA-256 of
/// `[0,<pubkey>,<created_at>,<kind>,<tags>,<content>]` with no whitespace,
/// strings escaped by [`escape`], and integers in decimal.
pub fn event_id(
    pubkey: &[u8; 32],
    created_at: u64,
    kind: u16,
    tags: &[Vec<String>],
    content: &str,
) -> [u8; 32] {
    let mut h = Sha256::new();
    serialise(
        &mut |b: &[u8]| h.update(b),
        pubkey,
        created_at,
        kind,
        tags,
        content,
    );
    h.finalize().into()
}

/// The canonical serialisation itself, for the golden vectors.
pub fn canonical(
    pubkey: &[u8; 32],
    created_at: u64,
    kind: u16,
    tags: &[Vec<String>],
    content: &str,
) -> String {
    let mut out = Vec::new();
    serialise(
        &mut |b: &[u8]| out.extend_from_slice(b),
        pubkey,
        created_at,
        kind,
        tags,
        content,
    );
    // Only whole strings and ASCII are written, so this is UTF-8.
    String::from_utf8(out).unwrap_or_default()
}

fn serialise(
    w: &mut dyn FnMut(&[u8]),
    pubkey: &[u8; 32],
    created_at: u64,
    kind: u16,
    tags: &[Vec<String>],
    content: &str,
) {
    w(b"[0,\"");
    w(to_hex(pubkey).as_bytes());
    w(b"\",");
    w(created_at.to_string().as_bytes());
    w(b",");
    w(kind.to_string().as_bytes());
    w(b",[");
    for (i, tag) in tags.iter().enumerate() {
        if i > 0 {
            w(b",");
        }
        w(b"[");
        for (k, s) in tag.iter().enumerate() {
            if k > 0 {
                w(b",");
            }
            escape(w, s);
        }
        w(b"]");
    }
    w(b"],");
    escape(w, content);
    w(b"]");
}

/// A JSON string as NIP-01 serialises it: quoted, with `\n`, `"`, `\`,
/// `\r`, `\t`, `\b` and `\f` escaped as shown, and every other character
/// verbatim.
fn escape(w: &mut dyn FnMut(&[u8]), s: &str) {
    w(b"\"");
    let b = s.as_bytes();
    let mut run = 0;
    for (i, &c) in b.iter().enumerate() {
        let e: &[u8] = match c {
            b'\n' => b"\\n",
            b'"' => b"\\\"",
            b'\\' => b"\\\\",
            b'\r' => b"\\r",
            b'\t' => b"\\t",
            0x08 => b"\\b",
            0x0c => b"\\f",
            _ => continue,
        };
        w(&b[run..i]);
        w(e);
        run = i + 1;
    }
    w(&b[run..]);
    w(b"\"");
}

/// One file a release lists, as its publisher states it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    /// Not empty, at most [`MAX_PATH`] bytes, hidden characters removed.
    pub path: String,
    pub size: Option<u64>,
}

/// An external id from an `i` tag: `imdb:tt0111161` is scheme `imdb`,
/// value `tt0111161`; `tmdb:movie:693134` is scheme `tmdb`, value
/// `movie:693134`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Id {
    /// Lower-case ASCII letters and digits, 1–[`MAX_ID_PART`] bytes.
    pub scheme: String,
    /// 1–[`MAX_ID_PART`] bytes, no whitespace or hidden characters.
    pub value: String,
}

/// What a NIP-35 event claims. Every field is a claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// From the `x` tag, the only source of the key.
    pub hash: InfoHash,
    /// The event's id, for dedupe and provenance.
    pub event: [u8; 32],
    pub created_at: u64,
    /// `title`, at most [`MAX_NAME`] bytes.
    pub name: Option<String>,
    /// `file` tags, at most [`MAX_FILES`].
    pub files: Vec<File>,
    /// `tracker` tags with a tracker scheme, at most [`MAX_TRACKERS`].
    /// Untrusted URLs: `net::tracker::public_tracker_url` checks each
    /// before any announce.
    pub trackers: Vec<String>,
    /// Typed `i` tags, at most [`MAX_IDS`].
    pub ids: Vec<Id>,
    /// `t` tags and the `tcat:` and `newznab:` category `i` tags, at most
    /// [`MAX_KEPT_TAGS`] of [`MAX_TAG`] bytes.
    pub tags: Vec<String>,
    /// `content`, hidden characters other than line breaks and tabs
    /// removed, at most [`MAX_DESCRIPTION`] bytes.
    pub description: Option<String>,
    /// The publisher's key as a NIP-19 `npub`.
    pub publisher: String,
    /// An adult `t` tag or NIP-36 content warning: the claim
    /// `content-type:adult`.
    pub adult: bool,
}

/// The adult terms (plan §5.3).
const ADULT: [&str; 4] = ["nsfw", "porn", "xxx", "adult"];

fn adult_term(s: &str) -> bool {
    ADULT.iter().any(|a| s.eq_ignore_ascii_case(a))
}

/// The claims a kind [`KIND_TORRENT`] event makes; `None` for another
/// kind, or one with no `x` tag, a malformed one or two that disagree.
pub fn release(e: &Event) -> Option<Release> {
    if e.kind != KIND_TORRENT {
        return None;
    }
    let mut hash: Option<InfoHash> = None;
    let mut r = Release {
        hash: InfoHash([0; 20]),
        event: e.id,
        created_at: e.created_at,
        name: None,
        files: Vec::new(),
        trackers: Vec::new(),
        ids: Vec::new(),
        tags: Vec::new(),
        description: None,
        publisher: npub(&e.pubkey),
        adult: false,
    };
    for t in &e.tags {
        let (Some(name), v) = (t.first(), t.get(1)) else {
            continue;
        };
        let Some(v) = v else { continue };
        match name.as_str() {
            "x" => {
                let h = info_hash(v)?;
                if hash.is_some_and(|k| k != h) {
                    return None;
                }
                hash = Some(h);
            }
            "title" if r.name.is_none() => {
                r.name = title(v).map(|s| cut(s.chars(), MAX_NAME));
            }
            "file" if r.files.len() < MAX_FILES => {
                let path: String = v.chars().filter(|&c| !hidden_char(c)).collect();
                if !path.is_empty() && path.len() <= MAX_PATH {
                    let size = t.get(2).and_then(|s| decimal(s));
                    r.files.push(File { path, size });
                }
            }
            "tracker" if r.trackers.len() < MAX_TRACKERS => {
                if tracker_ok(v) && !r.trackers.contains(v) {
                    r.trackers.push(v.clone());
                }
            }
            "i" => {
                if let Some(cat) = category(v) {
                    push_tag(&mut r.tags, cat);
                } else if let Some(id) = external_id(v)
                    && r.ids.len() < MAX_IDS
                    && !r.ids.contains(&id)
                {
                    r.ids.push(id);
                }
            }
            "t" => {
                let shown: String = v.chars().filter(|&c| !hidden_char(c)).collect();
                r.adult |= adult_term(shown.trim());
                push_tag(&mut r.tags, v);
            }
            "content-warning" => {
                let shown: String = v.chars().filter(|&c| !hidden_char(c)).collect();
                r.adult |= shown.split(|c: char| !c.is_alphanumeric()).any(adult_term);
            }
            _ => {}
        }
    }
    r.hash = hash?;
    let d = cut(
        e.content
            .chars()
            .filter(|&c| c == '\n' || c == '\t' || !hidden_char(c)),
        MAX_DESCRIPTION,
    );
    let d = d.trim();
    r.description = (!d.is_empty()).then(|| d.to_owned());
    Some(r)
}

/// A v1 info hash: 40 hex digits, not all zero.
fn info_hash(s: &str) -> Option<InfoHash> {
    if s.len() != 40 {
        return None;
    }
    let h: InfoHash = s.parse().ok()?;
    (h.0 != [0; 20]).then_some(h)
}

/// A file size: decimal digits that fit a `u64`.
fn decimal(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// A tracker URL with a tracker scheme, within [`MAX_TRACKER`] bytes, with
/// no whitespace or hidden characters. Only the shape: whether it may be
/// announced to is checked where it's used.
fn tracker_ok(s: &str) -> bool {
    let lower =
        |p: &str| s.get(..p.len()).is_some_and(|h| h.eq_ignore_ascii_case(p)) && s.len() > p.len();
    s.len() <= MAX_TRACKER
        && ["udp://", "http://", "https://", "wss://"]
            .iter()
            .any(|p| lower(p))
        && !s.chars().any(|c| c.is_whitespace() || hidden_char(c))
}

/// A `tcat:` or `newznab:` category `i` tag, as a tag.
fn category(v: &str) -> Option<&str> {
    let (scheme, rest) = v.split_once(':')?;
    (["tcat", "newznab"]
        .iter()
        .any(|p| scheme.eq_ignore_ascii_case(p))
        && !rest.is_empty())
    .then_some(v)
}

/// A typed `i` tag, split at its first colon.
fn external_id(v: &str) -> Option<Id> {
    let (scheme, value) = v.split_once(':')?;
    let ok_scheme = (1..=MAX_ID_PART).contains(&scheme.len())
        && scheme.bytes().all(|b| b.is_ascii_alphanumeric());
    let ok_value = (1..=MAX_ID_PART).contains(&value.len())
        && !value.chars().any(|c| c.is_whitespace() || hidden_char(c));
    (ok_scheme && ok_value).then(|| Id {
        scheme: scheme.to_ascii_lowercase(),
        value: value.to_owned(),
    })
}

/// Keeps a tag: hidden characters removed, trimmed, not empty, within
/// [`MAX_TAG`] bytes (a longer one is dropped, not cut), not already kept.
fn push_tag(tags: &mut Vec<String>, v: &str) {
    if tags.len() >= MAX_KEPT_TAGS {
        return;
    }
    let t: String = v.chars().filter(|&c| !hidden_char(c)).collect();
    let t = t.trim();
    if !t.is_empty() && t.len() <= MAX_TAG && !tags.iter().any(|k| k == t) {
        tags.push(t.to_owned());
    }
}

/// A public key as a NIP-19 `npub` (bech32, BIP-173).
pub fn npub(key: &[u8; 32]) -> String {
    const CHARSET: &[u8; 32] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
    const HRP: &[u8] = b"npub";
    // 8-bit to 5-bit groups, padded.
    let mut data = Vec::with_capacity(52 + 6);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &b in key {
        acc = (acc << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            data.push(((acc >> bits) & 31) as u8);
        }
    }
    if bits > 0 {
        data.push(((acc << (5 - bits)) & 31) as u8);
    }
    let polymod = |v: &[u8]| {
        const GEN: [u32; 5] = [
            0x3b6a_57b2,
            0x2650_8e6d,
            0x1ea1_19fa,
            0x3d42_33dd,
            0x2a14_62b3,
        ];
        let mut chk = 1u32;
        for &x in v {
            let top = chk >> 25;
            chk = ((chk & 0x01ff_ffff) << 5) ^ u32::from(x);
            for (i, g) in GEN.iter().enumerate() {
                if (top >> i) & 1 == 1 {
                    chk ^= g;
                }
            }
        }
        chk
    };
    let mut v: Vec<u8> = HRP.iter().map(|c| c >> 5).collect();
    v.push(0);
    v.extend(HRP.iter().map(|c| c & 31));
    v.extend_from_slice(&data);
    v.extend_from_slice(&[0; 6]);
    let m = polymod(&v) ^ 1;
    for i in 0..6 {
        data.push(((m >> (5 * (5 - i))) & 31) as u8);
    }
    let mut s = String::with_capacity(4 + 1 + data.len());
    s.push_str("npub1");
    for d in data {
        s.push(char::from(CHARSET[usize::from(d)]));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npub_matches_nip19() {
        let k = unhex::<32>("7e7e9c42a91bfef19fa929e5fda1b72e0ebc1a4c1141673e2794234d86addf4e")
            .unwrap();
        assert_eq!(
            npub(&k),
            "npub10elfcs4fr0l0r8af98jlmgdh9c8tcxjvz9qkw038js35mp4dma8qzvjptg"
        );
    }

    #[test]
    fn hex_is_lower_case_only() {
        assert!(unhex::<1>("ab").is_some());
        assert!(unhex::<1>("AB").is_none());
        assert!(unhex::<1>("a").is_none());
        assert!(unhex::<1>("zz").is_none());
    }
}
