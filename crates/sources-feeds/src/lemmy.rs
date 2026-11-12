//! Lemmy API v3 post lists (architecture §18.2, source #22; plan
//! 2026-11-06-v08 §5, LEM-0): the `GetPostsResponse` that
//! `GET /api/v3/post/list` returns.
//!
//! The safety rules are [`torznab`](crate::torznab)'s. It's a pure function
//! from bytes to posts with no I/O. It reads one post at a time with the
//! bounded [`json`](crate::json) reader (never `serde_json`), stops at a
//! structural cap with a [`Stop`] reason and keeps the posts it finished,
//! and skips (and counts) a post that breaks a field cap, has the wrong
//! shape or has no usable magnet. A half-read post is never used. It never
//! panics.
//!
//! The shape, matched by key:
//!
//! ```text
//! {"posts": [{"post": {"id", "name", "url", "body", "nsfw", "removed",
//!                      "deleted", "published", "ap_id", …},
//!             "community": {"name", "nsfw", "actor_id", "removed",
//!                           "deleted", …},
//!             "creator": {"name", "actor_id", …}, …}, …],
//!  "next_page": "…"}
//! ```
//!
//! An error answer is `{"error": "<code>"}` ([`Page::error`]). The parser
//! targets v3 by path and checks this shape ([`Page::recognised`]); it
//! never reads the software version, so a Lemmy 1.x that still serves v3
//! works.
//!
//! - **Keys** come only from magnet URIs in `post.url` and `post.body`,
//!   read with the magnet parser in [`common`](crate::common). A magnet
//!   `url` is read whole; the body is Markdown and is scanned. An
//!   `http(s)` link to a `.torrent` gives no key (plan Q-r12) and the post
//!   counts as `no_hash`. A post can carry several magnets; each distinct
//!   key is one [`Torrent`], the url's first.
//! - **Removed and deleted posts** are skipped and counted in
//!   [`Page::removed`], as are posts in a removed or deleted community. A
//!   removal read cleanly counts there even when another field is bad.
//! - **A repeated `post`, `community` or `creator` object** skips the post
//!   as malformed: the second could carry a flag the first lacks.
//! - **Adult:** [`Post::adult`] is `post.nsfw` or `community.nsfw`. Both
//!   flags must be present as booleans, or the post is skipped as
//!   malformed, so a reshaped answer can't drop the flag unseen. Repeated
//!   keys of these flags, and of `removed` and `deleted`, are OR-ed; other
//!   repeated keys keep the first value.
//! - **Caps:** a string over [`Limits::max_text`] (64 KiB, the body cap)
//!   skips the post; more than [`Limits::max_posts`] (50, Lemmy's own page
//!   limit) posts end the read with [`Stop::Items`].

use crate::common::{
    Bad, external_ref, is_magnet, magnet_hashes, percent_decode, prepare, rfc3339, title,
};
pub use crate::common::{Hashes, MAX_NAME, MAX_REF, MAX_TITLE, Skipped, Stop, Summary};
use crate::json::{self, R, Reader, TextError, Walk};

/// Distinct torrents kept per post; the rest are left out
/// ([`Post::torrents_truncated`]).
pub const MAX_TORRENTS: usize = 16;
/// Bytes in a kept `next_page` cursor.
pub const MAX_CURSOR: usize = 1024;
/// Bytes in a kept error code.
pub const MAX_ERROR: usize = 64;

/// Caps on one parse. The defaults are [`LIMITS`]; why each value:
/// `docs/decisions/2026-11-12-LEM-0.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Bytes read; the rest is ignored ([`Stop::Bytes`]).
    pub max_bytes: usize,
    /// Open objects and arrays at once ([`Stop::Depth`]).
    pub max_depth: usize,
    /// Values in the document, containers included ([`Stop::Elements`]).
    pub max_values: usize,
    /// Posts, kept or skipped ([`Stop::Items`]).
    pub max_posts: usize,
    /// Raw bytes of a string the parser reads; over it the post is skipped.
    pub max_text: usize,
}

/// The caps for a real page.
pub const LIMITS: Limits = Limits {
    max_bytes: 8 * 1024 * 1024,
    max_depth: 16,
    max_values: 200_000,
    max_posts: 50,
    max_text: 64 * 1024,
};

/// One torrent a post links by magnet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Torrent {
    pub hashes: Hashes,
    /// The magnet's `dn`, cleaned like a title.
    pub name: Option<String>,
}

/// A community or a person, as the instance states it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Actor {
    /// At most [`MAX_NAME`] bytes, hidden characters removed.
    pub name: Option<String>,
    /// The ActivityPub id, through [`external_ref`].
    pub actor_id: Option<String>,
}

/// One post that links at least one torrent. Every field is a claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Post {
    /// The instance's own post id: only grows, so LEM-1's cursor.
    pub id: u64,
    /// 1 to [`MAX_TORRENTS`] distinct keys, `url` first, then `body` in
    /// order.
    pub torrents: Vec<Torrent>,
    /// More distinct magnets than [`MAX_TORRENTS`].
    pub torrents_truncated: bool,
    /// `post.name`: at most [`MAX_TITLE`] bytes, hidden characters removed.
    pub title: Option<String>,
    /// `post.nsfw` or `community.nsfw`: a `content-type:adult` claim.
    pub adult: bool,
    /// `post.published` in Unix seconds (RFC 3339 with a zone only).
    pub published: Option<i64>,
    /// `post.ap_id`, through [`external_ref`]: the external reference.
    pub link: Option<String>,
    pub community: Actor,
    pub creator: Actor,
}

/// How a parse went, and the paging the answer gave.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Page {
    /// `summary.error` is always `None`; see [`Page::error`].
    pub summary: Summary,
    /// Removed or deleted posts, or posts in a removed or deleted
    /// community, skipped. Not in `summary.skipped`.
    pub removed: u32,
    /// The root is an object with a `posts` array: a v3 post list.
    pub recognised: bool,
    /// `next_page`: ASCII letters, digits and `_-`, at most [`MAX_CURSOR`]
    /// bytes.
    pub next_page: Option<String>,
    /// The highest `post.id` among posts read whole, kept or skipped, so a
    /// cursor can move past posts that will never be kept.
    pub newest_id: Option<u64>,
    /// A Lemmy error answer's code (`rate_limit_error`, …): ASCII
    /// lower-case letters, digits and `_`, at most [`MAX_ERROR`] bytes.
    pub error: Option<String>,
}

/// Every kept post and the page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Feed {
    pub posts: Vec<Post>,
    pub page: Page,
}

/// Parses `body` with [`LIMITS`].
pub fn parse(body: &[u8]) -> Feed {
    let mut posts = Vec::new();
    let page = parse_each(body, &LIMITS, |p| posts.push(p));
    Feed { posts, page }
}

/// Parses `body`, handing each kept post to `emit` as soon as it ends.
pub fn parse_each(body: &[u8], limits: &Limits, mut emit: impl FnMut(Post)) -> Page {
    let mut page = Page::default();
    let bytes = crate::common::Limits {
        max_bytes: limits.max_bytes,
        ..crate::common::LIMITS
    };
    let text = prepare(body, &bytes, &mut page.summary);
    let reader = json::Limits {
        max_depth: limits.max_depth,
        max_values: limits.max_values,
        // Nothing here reads a long array but `posts`, which is capped by
        // count; skipped values are bounded by `max_values`.
        max_array: usize::MAX,
        max_text: limits.max_text,
    };
    let mut p = Parser {
        j: Reader::new(text, reader),
        l: limits,
        posts: 0,
        page,
    };
    if let Err(stop) = p.document(&mut emit) {
        p.page.summary.stop.get_or_insert(stop);
    }
    p.page
}

/// The post being read.
#[derive(Debug, Default)]
struct Draft {
    bad: Option<Bad>,
    id: Option<u64>,
    name: Option<String>,
    url: Option<String>,
    body: Option<String>,
    nsfw: Option<bool>,
    removed: Option<bool>,
    deleted: Option<bool>,
    published: Option<i64>,
    ap_id: Option<String>,
    seen_post: bool,
    seen_community: bool,
    seen_creator: bool,
    community: Actor,
    community_nsfw: Option<bool>,
    community_gone: bool,
    creator: Actor,
}

struct Parser<'a, 'l> {
    j: Reader<'a>,
    l: &'l Limits,
    posts: usize,
    page: Page,
}

impl<'a> Walk<'a> for Parser<'a, '_> {
    fn json(&mut self) -> &mut Reader<'a> {
        &mut self.j
    }
}

impl Parser<'_, '_> {
    // --- Values the parser keeps -------------------------------------

    /// A string within [`Limits::max_text`], decoded; `null` is `None`.
    /// One over the cap, with a bad escape, or of another type spoils the
    /// post.
    fn text(&mut self, d: &mut Draft) -> R<Option<String>> {
        match self.j.peek() {
            Some(b'"') => match self.j.text()? {
                Ok(s) => Ok(Some(s)),
                Err(e) => {
                    d.spoil(match e {
                        TextError::OverCap => Bad::OverCap,
                        TextError::Malformed => Bad::Malformed,
                    });
                    Ok(None)
                }
            },
            Some(b'n') => self.j.skip().map(|_| None),
            _ => {
                d.spoil(Bad::Malformed);
                self.j.skip().map(|_| None)
            }
        }
    }

    /// A boolean; anything else spoils the post.
    fn flag(&mut self, d: &mut Draft) -> R<Option<bool>> {
        let v = match self.j.peek() {
            Some(b't') => Some(true),
            Some(b'f') => Some(false),
            _ => None,
        };
        if v.is_none() {
            d.spoil(Bad::Malformed);
        }
        self.j.skip()?;
        Ok(v)
    }

    // --- The shape ---------------------------------------------------

    fn document(&mut self, emit: &mut dyn FnMut(Post)) -> R<()> {
        if self.j.peek() == Some(b'{') {
            let mut posts = false;
            let mut cursor = false;
            let mut error = false;
            self.object(|p, k| match k {
                "posts" if !posts && p.j.peek() == Some(b'[') => {
                    posts = true;
                    p.page.recognised = true;
                    p.array(|p, _| p.post(emit))
                }
                "next_page" if !cursor => {
                    cursor = true;
                    p.page.next_page = p.code()?.filter(|c| next_page(c));
                    Ok(())
                }
                "error" if !error => {
                    error = true;
                    p.page.error = p.code()?.filter(|c| error_code(c));
                    Ok(())
                }
                _ => p.j.skip().map(drop),
            })?;
        } else {
            self.j.skip()?;
        }
        self.j.end()
    }

    /// A short string at the root; anything else is `None`.
    fn code(&mut self) -> R<Option<String>> {
        if self.j.peek() != Some(b'"') {
            return self.j.skip().map(|_| None);
        }
        Ok(self.j.text()?.ok())
    }

    /// One element of `posts`.
    fn post(&mut self, emit: &mut dyn FnMut(Post)) -> R<()> {
        self.posts += 1;
        if self.posts > self.l.max_posts {
            return Err(Stop::Items);
        }
        if self.j.peek() != Some(b'{') {
            self.j.skip()?;
            self.page.summary.skipped.malformed += 1;
            return Ok(());
        }
        let mut d = Draft::default();
        self.object(|p, k| {
            let (seen, field): (bool, Field<'_, '_>) = match k {
                "post" => (std::mem::replace(&mut d.seen_post, true), Draft::post_field),
                "community" => (
                    std::mem::replace(&mut d.seen_community, true),
                    Draft::community_field,
                ),
                "creator" => (
                    std::mem::replace(&mut d.seen_creator, true),
                    Draft::creator_field,
                ),
                _ => return p.j.skip().map(drop),
            };
            if seen {
                // A second object could carry a flag the first lacks:
                // the post can't be read truthfully.
                d.spoil(Bad::Malformed);
                return p.j.skip().map(drop);
            }
            p.part(&mut d, field)
        })?;
        self.finish(d, emit);
        Ok(())
    }

    /// `post`, `community` or `creator`: an object whose fields `field`
    /// reads. Anything else spoils the post.
    fn part(&mut self, d: &mut Draft, field: fn(&mut Draft, &mut Self, &str) -> R<()>) -> R<()> {
        if self.j.peek() != Some(b'{') {
            d.spoil(Bad::Malformed);
            return self.j.skip().map(drop);
        }
        self.object(|p, k| field(d, p, k))
    }

    fn finish(&mut self, d: Draft, emit: &mut dyn FnMut(Post)) {
        let id = d.id;
        let result = d.finish();
        if !matches!(result, Err(Skip::Bad(_))) {
            self.page.newest_id = self.page.newest_id.max(id);
        }
        match result {
            Ok(post) => {
                self.page.summary.kept += 1;
                emit(post);
            }
            Err(Skip::Removed) => self.page.removed += 1,
            Err(Skip::Bad(b)) => match b {
                Bad::OverCap => self.page.summary.skipped.over_cap += 1,
                Bad::Malformed => self.page.summary.skipped.malformed += 1,
            },
            Err(Skip::NoHash) => self.page.summary.skipped.no_hash += 1,
            Err(Skip::BadHash) => self.page.summary.skipped.bad_hash += 1,
        }
    }
}

/// Reads one field of a `post`, `community` or `creator` object.
type Field<'a, 'l> = fn(&mut Draft, &mut Parser<'a, 'l>, &str) -> R<()>;

enum Skip {
    Removed,
    Bad(Bad),
    NoHash,
    BadHash,
}

/// OR-s a repeated flag in, so a repeat can only add a restriction.
fn or(slot: &mut Option<bool>, v: Option<bool>) {
    if let Some(v) = v {
        *slot = Some(slot.unwrap_or(false) | v);
    }
}

impl Draft {
    fn spoil(&mut self, bad: Bad) {
        self.bad.get_or_insert(bad);
    }

    fn post_field(&mut self, p: &mut Parser<'_, '_>, key: &str) -> R<()> {
        match key {
            "id" if self.id.is_none() => {
                if matches!(p.j.peek(), Some(b'0'..=b'9')) {
                    self.id = id(p.j.num()?);
                } else {
                    p.j.skip()?;
                }
                if self.id.is_none() {
                    self.spoil(Bad::Malformed);
                }
            }
            "name" if self.name.is_none() => self.name = p.text(self)?,
            "url" if self.url.is_none() => self.url = p.text(self)?,
            "body" if self.body.is_none() => self.body = p.text(self)?,
            "ap_id" if self.ap_id.is_none() => self.ap_id = p.text(self)?,
            "published" if self.published.is_none() => {
                self.published = p.text(self)?.as_deref().and_then(rfc3339);
            }
            "nsfw" => {
                let v = p.flag(self)?;
                or(&mut self.nsfw, v);
            }
            "removed" => {
                let v = p.flag(self)?;
                or(&mut self.removed, v);
            }
            "deleted" => {
                let v = p.flag(self)?;
                or(&mut self.deleted, v);
            }
            _ => {
                p.j.skip()?;
            }
        }
        Ok(())
    }

    fn community_field(&mut self, p: &mut Parser<'_, '_>, key: &str) -> R<()> {
        match key {
            "name" if self.community.name.is_none() => {
                self.community.name = p.text(self)?.as_deref().and_then(name);
            }
            "actor_id" if self.community.actor_id.is_none() => {
                self.community.actor_id = p.text(self)?.as_deref().and_then(external_ref);
            }
            "nsfw" => {
                let v = p.flag(self)?;
                or(&mut self.community_nsfw, v);
            }
            "removed" | "deleted" => {
                self.community_gone |= p.flag(self)? == Some(true);
            }
            _ => {
                p.j.skip()?;
            }
        }
        Ok(())
    }

    fn creator_field(&mut self, p: &mut Parser<'_, '_>, key: &str) -> R<()> {
        match key {
            "name" if self.creator.name.is_none() => {
                self.creator.name = p.text(self)?.as_deref().and_then(name);
            }
            "actor_id" if self.creator.actor_id.is_none() => {
                self.creator.actor_id = p.text(self)?.as_deref().and_then(external_ref);
            }
            _ => {
                p.j.skip()?;
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<Post, Skip> {
        // A removal read cleanly wins over any other fault, so LEM-1's
        // health counts moderation as moderation.
        if self.removed == Some(true) || self.deleted == Some(true) || self.community_gone {
            return Err(Skip::Removed);
        }
        if let Some(b) = self.bad {
            return Err(Skip::Bad(b));
        }
        // The shape: a post with its id and flags, in a community with
        // its nsfw flag. Missing any of these, the answer isn't one we
        // can read safely.
        let (Some(id), Some(nsfw), Some(removed), Some(deleted), Some(community_nsfw)) = (
            self.id,
            self.nsfw,
            self.removed,
            self.deleted,
            self.community_nsfw,
        ) else {
            return Err(Skip::Bad(Bad::Malformed));
        };
        debug_assert!(!removed && !deleted);
        let mut m = Magnets::default();
        // `url` is one URL, not Markdown: a magnet there is read whole.
        if let Some(url) = self.url.as_deref().map(str::trim).filter(|u| is_magnet(u)) {
            m.add(url);
        }
        if let Some(body) = &self.body {
            m.scan(body);
        }
        if m.torrents.is_empty() {
            return Err(if m.seen { Skip::BadHash } else { Skip::NoHash });
        }
        Ok(Post {
            id,
            torrents: m.torrents,
            torrents_truncated: m.truncated,
            title: self.name.as_deref().and_then(title),
            adult: nsfw || community_nsfw,
            published: self.published,
            link: self.ap_id.as_deref().and_then(external_ref),
            community: self.community,
            creator: self.creator,
        })
    }
}

/// The magnets found in a post's text so far.
#[derive(Default)]
struct Magnets {
    torrents: Vec<Torrent>,
    truncated: bool,
    /// Some text started like a magnet.
    seen: bool,
}

impl Magnets {
    /// Every magnet URI in `text`: from `magnet:` up to whitespace, a
    /// character that ends a Markdown or HTML link, or a non-ASCII byte,
    /// less trailing sentence punctuation. Linear in `text`.
    fn scan(&mut self, text: &str) {
        let b = text.as_bytes();
        let mut i = 0;
        while i < b.len() {
            // `m` is ASCII, so `i` is a character boundary.
            if !b[i].eq_ignore_ascii_case(&b'm') || !text.get(i..).is_some_and(is_magnet) {
                i += 1;
                continue;
            }
            let len = b[i..]
                .iter()
                .position(|&c| ends_magnet(c))
                .unwrap_or(b.len() - i);
            // Everything up to `i + len` is ASCII, so this slice can't
            // split a character.
            let uri = text
                .get(i..i + len)
                .unwrap_or_default()
                .trim_end_matches(['.', ',', ';', ':', '!', '?']);
            i += len.max(1);
            self.add(uri);
        }
    }

    /// One magnet URI, kept when it gives a new key within the cap.
    fn add(&mut self, uri: &str) {
        self.seen = true;
        let Some(hashes) = magnet_hashes(uri) else {
            return;
        };
        if self.torrents.iter().any(|t| t.hashes.key() == hashes.key()) {
            return;
        }
        if self.torrents.len() >= MAX_TORRENTS {
            self.truncated = true;
            return;
        }
        self.torrents.push(Torrent {
            hashes,
            name: dn(uri),
        });
    }
}

fn ends_magnet(c: u8) -> bool {
    !c.is_ascii()
        || c.is_ascii_whitespace()
        || c.is_ascii_control()
        || b"()[]<>{}\"'`|\\".contains(&c)
}

/// A magnet's first `dn`, `+` read as a space, cleaned like a title.
fn dn(uri: &str) -> Option<String> {
    let query = uri.split_once('?')?.1;
    let raw = query.split('&').find_map(|p| {
        p.get(..3)
            .filter(|k| k.eq_ignore_ascii_case("dn="))
            .map(|_| &p[3..])
    })?;
    title(&percent_decode(&raw.replace('+', " "))?)
}

/// A post id: a positive integer as written, no sign, fraction or
/// exponent.
fn id(s: &str) -> Option<u64> {
    let ok = !s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit());
    ok.then(|| s.parse().ok()).flatten().filter(|&n| n > 0)
}

/// A kept community or person name.
fn name(s: &str) -> Option<String> {
    title(s).filter(|n| n.len() <= MAX_NAME)
}

fn next_page(s: &str) -> bool {
    (1..=MAX_CURSOR).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn error_code(s: &str) -> bool {
    (1..=MAX_ERROR).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: &str = "0123456789abcdef0123456789abcdef01234567";

    fn scan(s: &str) -> Magnets {
        let mut m = Magnets::default();
        m.scan(s);
        m
    }

    #[test]
    fn magnets_in_markdown() {
        let m = scan(&format!(
            "Get it [here](magnet:?xt=urn:btih:{H}&dn=A+B%20C). Or magnet:?xt=urn:btih:{H}."
        ));
        assert_eq!(m.torrents.len(), 1);
        assert_eq!(m.torrents[0].name.as_deref(), Some("A B C"));
        assert!(!m.truncated);
    }

    #[test]
    fn trailing_punctuation_and_case() {
        let m = scan(&format!("MAGNET:?XT=URN:BTIH:{}, done", H.to_uppercase()));
        assert_eq!(m.torrents.len(), 1);
    }

    #[test]
    fn bad_magnet_is_seen() {
        let m = scan("magnet:?xt=urn:btih:nothex");
        assert!(m.seen && m.torrents.is_empty());
        let m = scan("no magnets, just a magnetic personality");
        assert!(!m.seen);
    }

    #[test]
    fn ends_before_non_ascii() {
        let m = scan(&format!("magnet:?xt=urn:btih:{H}é"));
        assert_eq!(m.torrents.len(), 1);
    }

    #[test]
    fn ids() {
        assert_eq!(id("42"), Some(42));
        assert_eq!(id("0"), None);
        assert_eq!(id("1.5"), None);
        assert_eq!(id("-3"), None);
        assert_eq!(id("99999999999999999999999"), None);
    }

    #[test]
    fn codes() {
        assert!(next_page("Pa46c_x-1"));
        assert!(!next_page("a b"));
        assert!(!next_page(""));
        assert!(error_code("rate_limit_error"));
        assert!(!error_code("Rate"));
    }
}
