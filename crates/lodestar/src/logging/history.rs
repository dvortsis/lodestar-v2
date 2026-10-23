//! `GET /logs`: history, newest first. The ring buffer by sequence, then
//! the files: the current one read backwards, then the rotated ones by
//! their names' times.
//!
//! - **The join:** the cursor carries `below`, the lowest sequence the
//!   ring covered. A file line with this run's id at or above it is
//!   skipped (it was in the ring, or purged from it), so there's no gap or
//!   duplicate however the ring moved between pages. Lines without an id
//!   (older builds) are older than any with one.
//! - **The cursor** is opaque base64url: a version, the epoch, a
//!   fingerprint of the filters, and the position (a ring sequence, or a
//!   validated file name, an uncompressed byte offset and a fingerprint of
//!   the current file's first line, to find it again after a rotation).
//!   Each field is checked on the way back in; another filter's cursor or
//!   a bad one is `bad_cursor`.
//! - **Bounded work:** at most [`READ_CAP`] bytes read or inflated per
//!   request; hitting it returns a partial page with `more` and a cursor.

use std::sync::Arc;

use lodestar_api::logs::{HistoryQuery, LogHistory, LogLine, LogsError, parse_rfc3339};
use sha2::{Digest, Sha256};

use super::Runtime;
use super::entry::Entry;
use super::files::{self, Listed};
use super::names::{LogName, open_regular};
use super::read;
use super::view::{self, matches, ring_line};

/// Bytes read or inflated per request at most.
pub const READ_CAP: u64 = 64 << 20;

/// Where a page starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pos {
    /// Ring entries below this sequence.
    Ring { before: u64 },
    /// File lines before `offset` in `name`.
    File {
        below: u64,
        name: LogName,
        offset: u64,
        /// The current file's first-line fingerprint (0 for others).
        head: u64,
    },
}

const VERSION: u8 = 1;

fn fingerprint(q: &HistoryQuery) -> [u8; 8] {
    let d =
        Sha256::digest(format!("{}|{:?}|{:?}", q.filter.describe(), q.since, q.until).as_bytes());
    d[..8].try_into().expect("8")
}

fn epoch_bytes(epoch: &str) -> [u8; 8] {
    u64::from_str_radix(epoch, 16).unwrap_or(0).to_be_bytes()
}

pub fn encode(epoch: &str, fp: [u8; 8], pos: &Pos) -> String {
    let mut b = vec![VERSION];
    b.extend_from_slice(&epoch_bytes(epoch));
    b.extend_from_slice(&fp);
    match pos {
        Pos::Ring { before } => {
            b.push(0);
            b.extend_from_slice(&before.to_be_bytes());
        }
        Pos::File {
            below,
            name,
            offset,
            head,
        } => {
            let n = name.file_name();
            b.push(1);
            b.extend_from_slice(&below.to_be_bytes());
            b.push(n.len() as u8);
            b.extend_from_slice(n.as_bytes());
            b.extend_from_slice(&offset.to_be_bytes());
            b.extend_from_slice(&head.to_be_bytes());
        }
    }
    base64url(&b)
}

pub fn decode(s: &str, epoch: &str, fp: [u8; 8]) -> Option<Pos> {
    let b = unbase64url(s)?;
    let take8 = |at: usize| -> Option<u64> {
        Some(u64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?))
    };
    if b.first() != Some(&VERSION) || b.get(1..9)? != epoch_bytes(epoch) || b.get(9..17)? != fp {
        return None;
    }
    match b.get(17)? {
        0 if b.len() == 26 => Some(Pos::Ring { before: take8(18)? }),
        1 => {
            let below = take8(18)?;
            let n = *b.get(26)? as usize;
            let name = std::str::from_utf8(b.get(27..27 + n)?).ok()?;
            let name = LogName::parse(name)?;
            let offset = take8(27 + n)?;
            let head = take8(35 + n)?;
            (b.len() == 43 + n).then_some(Pos::File {
                below,
                name,
                offset,
                head,
            })
        }
        _ => None,
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn base64url(b: &[u8]) -> String {
    let mut out = String::with_capacity(b.len() * 4 / 3 + 3);
    for c in b.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        let chars = c.len() + 1;
        for i in 0..chars {
            out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

fn unbase64url(s: &str) -> Option<Vec<u8>> {
    if s.len() > MAX_CURSOR || s.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    for c in s.as_bytes().chunks(4) {
        let mut n = 0u32;
        for (i, ch) in c.iter().enumerate() {
            let v = B64.iter().position(|x| x == ch)? as u32;
            n |= v << (18 - 6 * i);
        }
        let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&bytes[..c.len() - 1]);
    }
    // Canonical only: the encoding of what was decoded.
    (base64url(&out) == s).then_some(out)
}

/// The longest cursor (`/logs` refuses longer ones first).
const MAX_CURSOR: usize = 512;

/// A file's first-line fingerprint, to know the current file again.
fn head_of(dir: &std::path::Path, name: &LogName) -> u64 {
    let Ok(mut f) = open_regular(&name.path(dir)) else {
        return 0;
    };
    match read::first_line(&mut f, name.gz()) {
        Ok(Some(line)) => fingerprint_of(&line),
        _ => 0,
    }
}

fn fingerprint_of(line: &[u8]) -> u64 {
    let d = Sha256::digest(line);
    u64::from_be_bytes(d[..8].try_into().expect("8"))
}

/// This run's sequence in an id, if it's ours.
fn our_seq(epoch: &str, id: &str) -> Option<u64> {
    let (e, seq) = id.split_once('-')?;
    (e == epoch).then(|| seq.parse().ok())?
}

pub fn page(
    rt: &Runtime,
    folder: Option<&std::path::Path>,
    q: &HistoryQuery,
) -> Result<LogHistory, LogsError> {
    page_capped(rt, folder, q, READ_CAP)
}

/// [`page`] with another read cap (tests of partial pages).
pub fn page_capped(
    rt: &Runtime,
    folder: Option<&std::path::Path>,
    q: &HistoryQuery,
    cap: u64,
) -> Result<LogHistory, LogsError> {
    let mask = rt.mask_now();
    let epoch = rt.epoch().to_string();
    let fp = fingerprint(q);
    let ring = rt.ring();
    let (head, _) = ring.bounds();
    let pos = match &q.cursor {
        None => Pos::Ring { before: head + 1 },
        Some(c) => decode(c, &epoch, fp).ok_or(LogsError::BadCursor)?,
    };
    let in_time = |ms: i64| q.since.is_none_or(|s| ms >= s) && q.until.is_none_or(|u| ms <= u);
    let mut lines: Vec<LogLine> = Vec::new();
    let mut unreadable = 0u64;
    let shown = |e: &Entry| -> Option<LogLine> {
        if !in_time(e.millis) {
            return None;
        }
        ring_line(e, mask, &epoch).filter(|l| matches(l, &q.filter))
    };
    let (below, resume) = match pos {
        Pos::Ring { before } => {
            if before > head + 1 {
                return Err(LogsError::BadCursor);
            }
            let (got, next): (Vec<Arc<Entry>>, Result<u64, u64>) =
                ring.before(before, q.limit, |e| shown(e).is_some());
            lines.extend(got.iter().filter_map(|e| shown(e)));
            match next {
                Ok(next) => {
                    let cursor = encode(&epoch, fp, &Pos::Ring { before: next });
                    return Ok(LogHistory {
                        lines,
                        more: true,
                        next_cursor: Some(cursor),
                        unreadable,
                    });
                }
                // The lowest sequence the walk looked at: lines below it
                // were evicted (even during the walk) and are read from the
                // files; lines from it up came from the ring.
                Err(covered) => (covered, None),
            }
        }
        Pos::File {
            below,
            name,
            offset,
            head,
        } => (below, Some((name, offset, head))),
    };
    let Some(dir) = folder else {
        return Ok(LogHistory {
            lines,
            more: false,
            next_cursor: None,
            unreadable,
        });
    };
    // A folder that can't be listed (gone, unmounted) has no files: the
    // ring's lines still come back.
    let list = || -> Vec<Listed> {
        files::list(dir)
            .unwrap_or_default()
            .into_iter()
            .filter(|l| l.regular)
            .collect()
    };
    let mut listed = list();
    // The current file as listed, to know it again if it rotates before
    // it's opened.
    let listed_head = head_of(dir, &LogName::Current);
    let mut relisted = false;
    // Where to start: the newest file, or where the cursor left off.
    let (mut idx, mut end) = match resume {
        None => (0, None),
        Some((name, offset, head)) => locate(dir, &mut listed, &name, offset, head),
    };
    let mut cost = 0u64;
    while idx < listed.len() {
        let mut name = listed[idx].name.clone();
        // A rotated file from before `since` holds nothing newer than it,
        // and neither does any older one.
        if let (Some(since), Some(at)) = (q.since, name.rotated_at()) {
            if parse_rfc3339(&at).is_some_and(|t| t < since) {
                break;
            }
        }
        let opened = files::open_listed(dir, &name);
        let Ok((opened_as, mut f)) = opened else {
            unreadable += 1;
            idx += 1;
            end = None;
            continue;
        };
        name = opened_as;
        let head_fp = match name {
            LogName::Current => read::first_line(&mut f, false)
                .ok()
                .flatten()
                .map_or(0, |line| fingerprint_of(&line)),
            _ => 0,
        };
        // The current file rotated between the listing and here: list
        // again and read the file it became, from where we were.
        if name == LogName::Current && head_fp != listed_head && !relisted {
            relisted = true;
            listed = list();
            (idx, end) = locate(
                dir,
                &mut listed,
                &LogName::Current,
                end.unwrap_or(u64::MAX),
                listed_head,
            );
            continue;
        }
        // A file that can't be read is counted and passed over, never an
        // error: one damaged file mustn't block every page that reaches it.
        let len = match &name {
            LogName::Current => {
                let n = f.metadata().map(|m| m.len()).unwrap_or(0);
                read::whole_lines(&mut f, n)
            }
            n => read::plain_len(&mut f, n.gz()),
        };
        let Ok(len) = len else {
            unreadable += 1;
            idx += 1;
            end = None;
            continue;
        };
        let start = end.map_or(len, |e: u64| e.min(len));
        let Ok(mut r) = read::backward(f, name.gz(), start) else {
            unreadable += 1;
            idx += 1;
            end = None;
            continue;
        };
        let mut at = start;
        loop {
            // Lines already inflated cost nothing more: out first.
            if !r.buffered() && cost + r.cost() >= cap {
                let pos = Pos::File {
                    below,
                    name: name.clone(),
                    offset: at,
                    head: head_fp,
                };
                return Ok(LogHistory {
                    lines,
                    more: true,
                    next_cursor: Some(encode(&epoch, fp, &pos)),
                    unreadable,
                });
            }
            let raw = match r.prev() {
                Ok(Some(raw)) => raw,
                Ok(None) => break,
                Err(_) => {
                    unreadable += 1;
                    break;
                }
            };
            at = raw.start;
            let Some(bytes) = raw.bytes else {
                unreadable += 1;
                continue;
            };
            let mut line = match view::file_line(&bytes, Some(mask)) {
                Err(()) => {
                    unreadable += 1;
                    continue;
                }
                Ok(None) => continue,
                Ok(Some(line)) => line,
            };
            // As the ring cuts its lines: 16 KiB at most each.
            view::cut(&mut line);
            if line
                .id
                .as_deref()
                .and_then(|id| our_seq(&epoch, id))
                .is_some_and(|seq| seq >= below)
            {
                continue;
            }
            if !parse_rfc3339(&line.ts).is_some_and(in_time) || !matches(&line, &q.filter) {
                continue;
            }
            lines.push(line);
            if lines.len() == q.limit {
                let pos = Pos::File {
                    below,
                    name: name.clone(),
                    offset: at,
                    head: head_fp,
                };
                return Ok(LogHistory {
                    lines,
                    more: true,
                    next_cursor: Some(encode(&epoch, fp, &pos)),
                    unreadable,
                });
            }
        }
        cost += r.cost();
        idx += 1;
        end = None;
    }
    Ok(LogHistory {
        lines,
        more: false,
        next_cursor: None,
        unreadable,
    })
}

/// Finds where a cursor left off: the same file (its `.gz` if it's been
/// compressed since); the current file again by its first line, or the
/// rotated file it became; else the next older file from its start.
fn locate(
    dir: &std::path::Path,
    listed: &mut Vec<Listed>,
    name: &LogName,
    offset: u64,
    head: u64,
) -> (usize, Option<u64>) {
    match name {
        LogName::Current => {
            let current = listed.iter().position(|l| l.name == LogName::Current);
            if let Some(i) = current
                && head_of(dir, &LogName::Current) == head
            {
                return (i, Some(offset));
            }
            // Rotated since: it's one of the newest rotated files.
            let first_rotated = current.map_or(0, |i| i + 1);
            for (i, l) in listed.iter().enumerate().skip(first_rotated).take(3) {
                if head != 0 && head_of(dir, &l.name) == head {
                    return (i, Some(offset));
                }
            }
            (first_rotated, None)
        }
        LogName::Rotated { stamp, n, .. } => {
            let same = listed.iter().position(|l| match &l.name {
                LogName::Rotated { stamp: s, n: m, .. } => s == stamp && m == n,
                LogName::Current => false,
            });
            let older = listed
                .iter()
                .position(|l| LogName::newest_first(&l.name, name) == std::cmp::Ordering::Greater)
                .unwrap_or(listed.len());
            match same {
                Some(i) => (i, Some(offset)),
                // Not listed, but there (a scan can miss a file being
                // compressed): read it, then go on to the older ones.
                None if files::open_listed(dir, name).is_ok() => {
                    listed.insert(
                        older,
                        Listed {
                            name: name.clone(),
                            // History reads neither.
                            size: 0,
                            modified: None,
                            regular: true,
                        },
                    );
                    (older, Some(offset))
                }
                None => (older, None),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_round_trip_and_refuse_tampering() {
        let epoch = "00112233aabbccdd";
        let fp = [7u8; 8];
        for pos in [
            Pos::Ring { before: 42 },
            Pos::File {
                below: 5,
                name: LogName::parse("lodestar-20261023T101500.123Z-2.jsonl.gz").unwrap(),
                offset: 1 << 33,
                head: 9,
            },
            Pos::File {
                below: 0,
                name: LogName::Current,
                offset: 0,
                head: 0,
            },
        ] {
            let c = encode(epoch, fp, &pos);
            assert!(c.len() <= 512 && c.is_ascii());
            assert_eq!(decode(&c, epoch, fp), Some(pos.clone()));
            assert_eq!(decode(&c, "ffffffffffffffff", fp), None, "another run");
            assert_eq!(decode(&c, epoch, [8; 8]), None, "other filters");
            assert_eq!(decode(&format!("{c}A"), epoch, fp), None);
            assert_eq!(decode(&c[..c.len() - 1], epoch, fp), None);
        }
        // A file name that isn't one of ours, in an otherwise good cursor.
        let mut b = vec![VERSION];
        b.extend_from_slice(&epoch_bytes(epoch));
        b.extend_from_slice(&fp);
        b.push(1);
        b.extend_from_slice(&0u64.to_be_bytes());
        let evil = b"../../../etc/passwd";
        b.push(evil.len() as u8);
        b.extend_from_slice(evil);
        b.extend_from_slice(&[0; 16]);
        assert_eq!(decode(&base64url(&b), epoch, fp), None);
        for junk in ["", "%%%", "AAAA", &"A".repeat(600)] {
            assert_eq!(decode(junk, epoch, fp), None, "{junk}");
        }
    }

    #[test]
    fn a_file_compressed_since_the_listing_is_its_gz() {
        let dir = tempfile::tempdir().unwrap();
        let plain = LogName::parse("lodestar-20261023T101500.123Z.jsonl").unwrap();
        std::fs::write(plain.compressed().unwrap().path(dir.path()), "x").unwrap();
        let (name, _) = files::open_listed(dir.path(), &plain).unwrap();
        assert_eq!(name, plain.compressed().unwrap());
        assert!(files::open_listed(dir.path(), &LogName::Current).is_err());
    }

    #[test]
    fn base64url_round_trips() {
        for n in 0..40 {
            let b: Vec<u8> = (0..n).map(|i| (i * 37 + 11) as u8).collect();
            assert_eq!(unbase64url(&base64url(&b)).unwrap(), b);
        }
        assert_eq!(base64url(b"\xfb\xff"), "-_8");
    }
}
