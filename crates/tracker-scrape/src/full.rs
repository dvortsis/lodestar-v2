//! BEP 48 full scrape: the reply to a scrape request with no `info_hash`,
//! which lists every torrent the tracker knows (architecture §18.2 rank
//! 16).
//!
//! The reply is the usual scrape dictionary, `d5:filesd<20-byte hash><entry>
//! …ee`, but it can run to many megabytes, so [`Parser`] is a push parser:
//! it takes the body in chunks as they arrive and never holds more than
//! one dictionary key, a capped `failure reason` and one batch of hashes.
//! Memory stays flat however large the reply is.
//!
//! **What it emits.** The 20-byte keys of the `files` dictionary whose
//! value is a dictionary, in batches of at most [`Limits::batch`]; each
//! [`Parser::feed`] call hands over every key it completed before it
//! returns, so nothing waits on the next chunk. The
//! counts inside each entry aren't kept: a full scrape is a discovery
//! source, and the scrape scheduler measures swarms itself.
//!
//! **Every reply is untrusted.**
//! - [`Limits`] caps the total bytes, the number of entries, the bytes in
//!   one entry and the nesting depth. Past the byte, entry or depth cap the
//!   parse fails. The keys of entries completed before the failure are
//!   still emitted: each came from a whole, well-formed entry.
//! - An entry whose key isn't exactly 20 bytes, whose value isn't a
//!   dictionary, or whose value is over [`Limits::max_entry_bytes`] is
//!   skipped and counted in [`Summary::malformed`].
//! - Anything that isn't bencode (a stray byte, a non-string dictionary
//!   key, an integer with no digits, trailing data) ends the parse: a
//!   streaming parser can't find its place again after one.
//! - Lengths are checked before bytes are skipped, and string bodies are
//!   skipped without being copied.
//!
//! Duplicate keys are emitted as often as they appear: removing them would
//! need memory that grows with the reply. The pipeline merges repeats.

use lodestar_metainfo::InfoHash;

use crate::{Error, Result, message};

/// What one parse may hold and produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Bytes in the whole reply.
    pub max_bytes: u64,
    /// Entries in `files`, malformed ones included.
    pub max_entries: u64,
    /// Bytes in one entry's value. opentracker's are about 50.
    pub max_entry_bytes: u64,
    /// Nesting depth: the top dictionary is 1, `files` 2, an entry 3.
    /// At most [`MAX_DEPTH`].
    pub max_depth: usize,
    /// Hashes per emitted batch, 1 to [`MAX_BATCH`].
    pub batch: usize,
}

/// The ceiling on [`Limits::max_depth`].
pub const MAX_DEPTH: usize = 64;
/// The ceiling on [`Limits::batch`].
pub const MAX_BATCH: usize = 65_536;

/// The defaults: room for a large public tracker (about 4 million
/// torrents, 256 MiB), with entries capped at 1 KiB.
pub const LIMITS: Limits = Limits {
    max_bytes: 256 << 20,
    max_entries: 4_000_000,
    max_entry_bytes: 1024,
    max_depth: 8,
    batch: 1024,
};

/// The longest dictionary key kept: enough for a hash and the top-level
/// names. Longer keys are skipped without being stored.
const KEY_CAP: usize = 32;
/// Bytes of a `failure reason` kept before [`message`] trims it further.
const REASON_CAP: usize = 1024;
/// Digits in a string length or an integer.
const MAX_DIGITS: u8 = 20;

/// What a finished parse saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Summary {
    /// Hashes emitted.
    pub keys: u64,
    /// Entries skipped for their shape.
    pub malformed: u64,
    /// Bytes read.
    pub bytes: u64,
}

/// What a value is, from where it sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// The reply itself.
    Top,
    /// The top-level `files`.
    Files,
    /// A value inside `files`.
    Entry,
    /// The top-level `failure reason`.
    Reason,
    /// Anything else: checked for syntax, skipped.
    Other,
}

#[derive(Debug, Clone, Copy)]
struct Level {
    dict: bool,
    role: Role,
    /// In a dictionary: the next item is a key.
    want_key: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sink {
    Skip,
    Key,
    Reason,
}

#[derive(Debug, Clone, Copy)]
enum Lex {
    /// Between items.
    Item,
    /// In a string's length; `sink` is where its bytes go.
    Len { len: u64, digits: u8, sink: Sink },
    /// In a string's bytes.
    Str { left: u64, sink: Sink },
    /// In an integer, after `i`.
    Int { digits: u8, sign: bool },
}

/// The entry being read: where it began and whether it's usable so far.
#[derive(Debug, Clone, Copy)]
struct Entry {
    start: u64,
    /// The stack depth of `files`: the entry is done when a value ends
    /// with the stack back at this depth.
    depth: usize,
    hash: Option<InfoHash>,
}

/// A streaming full-scrape parser. Feed it the body with [`feed`] as it
/// arrives, then call [`finish`].
///
/// [`feed`]: Parser::feed
/// [`finish`]: Parser::finish
#[derive(Debug)]
pub struct Parser {
    limits: Limits,
    bytes: u64,
    lex: Lex,
    stack: Vec<Level>,
    key: [u8; KEY_CAP],
    key_len: usize,
    key_over: bool,
    /// The role of the value after the key just read.
    next_role: Role,
    entry: Option<Entry>,
    reason: Option<Vec<u8>>,
    files: bool,
    done: bool,
    failed: bool,
    batch: Vec<InfoHash>,
    entries: u64,
    keys: u64,
    malformed: u64,
}

impl Parser {
    /// A parser under `limits`, clamped to sane ranges: a depth of 3 to
    /// [`MAX_DEPTH`] (an entry sits at 3) and a batch of 1 to
    /// [`MAX_BATCH`].
    pub fn new(limits: Limits) -> Parser {
        let limits = Limits {
            max_depth: limits.max_depth.clamp(3, MAX_DEPTH),
            batch: limits.batch.clamp(1, MAX_BATCH),
            ..limits
        };
        Parser {
            limits,
            bytes: 0,
            lex: Lex::Item,
            stack: Vec::with_capacity(limits.max_depth),
            key: [0; KEY_CAP],
            key_len: 0,
            key_over: false,
            next_role: Role::Other,
            entry: None,
            reason: None,
            files: false,
            done: false,
            failed: false,
            batch: Vec::with_capacity(limits.batch),
            entries: 0,
            keys: 0,
            malformed: 0,
        }
    }

    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    /// Heap bytes the parser holds: its stack, its batch and any
    /// `failure reason`. Fixed by its limits, whatever it has read, which
    /// is what the flat-memory test checks.
    pub fn heap_bytes(&self) -> usize {
        self.stack.capacity() * size_of::<Level>()
            + self.batch.capacity() * size_of::<InfoHash>()
            + self.reason.as_ref().map_or(0, Vec::capacity)
    }

    /// What has been seen so far.
    pub fn summary(&self) -> Summary {
        Summary {
            keys: self.keys,
            malformed: self.malformed,
            bytes: self.bytes,
        }
    }

    /// Parses the next `chunk` of the body, calling `emit` with each full
    /// batch of hashes and, before returning, with the rest of the keys it
    /// completed. After an error every later call fails too.
    pub fn feed(&mut self, chunk: &[u8], emit: &mut impl FnMut(&[InfoHash])) -> Result<()> {
        if self.failed {
            return Err(Error::Protocol("full scrape parse already failed"));
        }
        let r = self.feed_inner(chunk, emit);
        self.failed = r.is_err();
        // Each call hands over every key it completed, even when it then
        // failed: those entries were whole and well formed.
        self.flush(emit);
        r
    }

    /// Ends the parse: emits the last partial batch and checks the reply
    /// was complete. A `failure reason` is returned as
    /// [`Error::Refused`].
    pub fn finish(mut self, emit: &mut impl FnMut(&[InfoHash])) -> Result<Summary> {
        if self.failed {
            return Err(Error::Protocol("full scrape parse already failed"));
        }
        if !self.done {
            return Err(Error::Protocol("reply ended early"));
        }
        self.flush(emit);
        if let Some(reason) = &self.reason {
            return Err(Error::Refused(message(reason)));
        }
        if !self.files {
            return Err(Error::Protocol("reply has no files dictionary"));
        }
        Ok(self.summary())
    }

    fn flush(&mut self, emit: &mut impl FnMut(&[InfoHash])) {
        if !self.batch.is_empty() {
            emit(&self.batch);
            self.batch.clear();
        }
    }

    fn feed_inner(&mut self, chunk: &[u8], emit: &mut impl FnMut(&[InfoHash])) -> Result<()> {
        // The part of the chunk under the byte cap is parsed, so entries it
        // completes are emitted; the rest is refused.
        let room = self.limits.max_bytes.saturating_sub(self.bytes);
        if chunk.len() as u64 > room {
            self.feed_inner(&chunk[..room as usize], emit)?;
            return Err(Error::TooLarge);
        }
        let total = self.bytes + chunk.len() as u64;
        let mut i = 0;
        while i < chunk.len() {
            let at = self.bytes + i as u64;
            match self.lex {
                Lex::Str { left, sink } => {
                    let n = (left.min((chunk.len() - i) as u64)) as usize;
                    self.take(sink, &chunk[i..i + n]);
                    i += n;
                    let left = left - n as u64;
                    if left == 0 {
                        self.lex = Lex::Item;
                        self.string_done(sink, at + n as u64, emit)?;
                    } else {
                        self.lex = Lex::Str { left, sink };
                    }
                    continue;
                }
                Lex::Len { len, digits, sink } => {
                    let b = chunk[i];
                    i += 1;
                    match b {
                        b'0'..=b'9' if digits < MAX_DIGITS => {
                            let len = len
                                .checked_mul(10)
                                .and_then(|l| l.checked_add(u64::from(b - b'0')))
                                .ok_or(Error::Protocol("string length overflows"))?;
                            self.lex = Lex::Len {
                                len,
                                digits: digits + 1,
                                sink,
                            };
                        }
                        b':' => {
                            // Fails here rather than after skipping bytes
                            // that can't arrive under the cap.
                            if len > self.limits.max_bytes - (at + 1) {
                                return Err(Error::TooLarge);
                            }
                            if len == 0 {
                                self.lex = Lex::Item;
                                self.string_done(sink, at + 1, emit)?;
                            } else {
                                self.lex = Lex::Str { left: len, sink };
                            }
                        }
                        _ => return Err(Error::Protocol("bad string length")),
                    }
                }
                Lex::Int { digits, sign } => {
                    let b = chunk[i];
                    i += 1;
                    match b {
                        b'-' if digits == 0 && !sign => {
                            self.lex = Lex::Int { digits, sign: true };
                        }
                        b'0'..=b'9' if digits < MAX_DIGITS => {
                            self.lex = Lex::Int {
                                digits: digits + 1,
                                sign,
                            };
                        }
                        b'e' if digits > 0 => {
                            self.lex = Lex::Item;
                            self.value_done(at + 1, emit)?;
                        }
                        _ => return Err(Error::Protocol("bad integer")),
                    }
                }
                Lex::Item => {
                    let b = chunk[i];
                    i += 1;
                    self.item(b, at, emit)?;
                }
            }
        }
        self.bytes = total;
        Ok(())
    }

    /// The byte `b` at offset `at` starts an item or ends a container.
    fn item(&mut self, b: u8, at: u64, emit: &mut impl FnMut(&[InfoHash])) -> Result<()> {
        if self.done {
            // Some trackers end the body with a newline.
            return if b.is_ascii_whitespace() {
                Ok(())
            } else {
                Err(Error::Protocol("data after the reply"))
            };
        }
        let want_key = self.stack.last().is_some_and(|l| l.dict && l.want_key);
        if want_key {
            return match b {
                b'e' => self.close(at, emit),
                b'0'..=b'9' => {
                    self.key_len = 0;
                    self.key_over = false;
                    self.lex = Lex::Len {
                        len: u64::from(b - b'0'),
                        digits: 1,
                        sink: Sink::Key,
                    };
                    Ok(())
                }
                _ => Err(Error::Protocol("dictionary key is not a string")),
            };
        }
        let role = match self.stack.last() {
            None => Role::Top,
            Some(l) if l.dict => self.next_role,
            Some(_) => Role::Other,
        };
        if role == Role::Top && b != b'd' {
            return Err(Error::Protocol("reply is not a dictionary"));
        }
        if role == Role::Files && b != b'd' {
            return Err(Error::Protocol("files is not a dictionary"));
        }
        if role == Role::Entry {
            let hash = self.entry_hash();
            self.entry = Some(Entry {
                start: at,
                depth: self.stack.len(),
                // Only a dictionary value can be used.
                hash: hash.filter(|_| b == b'd'),
            });
        }
        match b {
            b'd' | b'l' => {
                if self.stack.len() >= self.limits.max_depth {
                    return Err(Error::Protocol("reply nests too deep"));
                }
                if role == Role::Files {
                    self.files = true;
                }
                // Inside an entry everything is skipped.
                let role = match role {
                    Role::Top | Role::Files => role,
                    _ => Role::Other,
                };
                self.stack.push(Level {
                    dict: b == b'd',
                    role,
                    want_key: b == b'd',
                });
                Ok(())
            }
            b'e' => self.close(at, emit),
            b'i' => {
                self.lex = Lex::Int {
                    digits: 0,
                    sign: false,
                };
                Ok(())
            }
            b'0'..=b'9' => {
                let sink = if role == Role::Reason {
                    // A repeated `failure reason` replaces the last one.
                    self.reason = Some(Vec::with_capacity(REASON_CAP));
                    Sink::Reason
                } else {
                    Sink::Skip
                };
                self.lex = Lex::Len {
                    len: u64::from(b - b'0'),
                    digits: 1,
                    sink,
                };
                Ok(())
            }
            _ => Err(Error::Protocol("not bencode")),
        }
    }

    /// The key just read, as a hash when it is exactly 20 bytes.
    fn entry_hash(&self) -> Option<InfoHash> {
        if self.key_over || self.key_len != 20 {
            return None;
        }
        let mut h = [0; 20];
        h.copy_from_slice(&self.key[..20]);
        Some(InfoHash(h))
    }

    fn take(&mut self, sink: Sink, bytes: &[u8]) {
        match sink {
            Sink::Skip => {}
            Sink::Key => {
                let room = KEY_CAP - self.key_len;
                if bytes.len() > room {
                    self.key_over = true;
                }
                let n = bytes.len().min(room);
                self.key[self.key_len..self.key_len + n].copy_from_slice(&bytes[..n]);
                self.key_len += n;
            }
            Sink::Reason => {
                if let Some(r) = &mut self.reason {
                    let n = bytes.len().min(REASON_CAP - r.len());
                    r.extend_from_slice(&bytes[..n]);
                }
            }
        }
    }

    fn string_done(
        &mut self,
        sink: Sink,
        end: u64,
        emit: &mut impl FnMut(&[InfoHash]),
    ) -> Result<()> {
        if sink != Sink::Key {
            return self.value_done(end, emit);
        }
        let Some(parent) = self.stack.last_mut() else {
            return Err(Error::Protocol("dictionary key outside a dictionary"));
        };
        parent.want_key = false;
        let key = &self.key[..self.key_len];
        self.next_role = match parent.role {
            Role::Top if self.key_over => Role::Other,
            Role::Top if key == b"files" => Role::Files,
            Role::Top if key == b"failure reason" => Role::Reason,
            Role::Files => Role::Entry,
            _ => Role::Other,
        };
        Ok(())
    }

    /// A container's `e` at offset `at`.
    fn close(&mut self, at: u64, emit: &mut impl FnMut(&[InfoHash])) -> Result<()> {
        let level = self
            .stack
            .pop()
            .ok_or(Error::Protocol("end with nothing open"))?;
        if level.dict && !level.want_key {
            // `d3:keye`: a key with no value.
            return Err(Error::Protocol("dictionary key has no value"));
        }
        if self.stack.is_empty() {
            self.done = true;
            return Ok(());
        }
        self.value_done(at + 1, emit)
    }

    /// A value ended just before offset `end`.
    fn value_done(&mut self, end: u64, emit: &mut impl FnMut(&[InfoHash])) -> Result<()> {
        if let Some(parent) = self.stack.last_mut()
            && parent.dict
        {
            parent.want_key = true;
        }
        let Some(entry) = self.entry else {
            return Ok(());
        };
        if self.stack.len() != entry.depth {
            return Ok(());
        }
        self.entry = None;
        if self.entries >= self.limits.max_entries {
            return Err(Error::Protocol("reply has more entries than the cap"));
        }
        self.entries += 1;
        let size = end - entry.start;
        match entry.hash {
            Some(h) if size <= self.limits.max_entry_bytes => {
                self.batch.push(h);
                self.keys += 1;
                if self.batch.len() >= self.limits.batch {
                    self.flush(emit);
                }
            }
            _ => self.malformed += 1,
        }
        Ok(())
    }
}

/// Parses a whole `body` in chunks of `chunk` bytes, as a caller with the
/// body already in memory would. For tests and the fuzz target.
pub fn parse(
    body: &[u8],
    limits: Limits,
    chunk: usize,
    emit: &mut impl FnMut(&[InfoHash]),
) -> Result<Summary> {
    let mut p = Parser::new(limits);
    for part in body.chunks(chunk.max(1)) {
        p.feed(part, emit)?;
    }
    p.finish(emit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(body: &[u8], limits: Limits) -> Result<(Vec<InfoHash>, Summary)> {
        let mut out = Vec::new();
        let s = parse(body, limits, 3, &mut |b| out.extend_from_slice(b))?;
        Ok((out, s))
    }

    fn entry(h: u8) -> Vec<u8> {
        let mut v = b"20:".to_vec();
        v.extend_from_slice(&[h; 20]);
        v.extend_from_slice(b"d8:completei1e10:downloadedi2e10:incompletei3ee");
        v
    }

    fn reply(entries: &[Vec<u8>]) -> Vec<u8> {
        let mut v = b"d5:filesd".to_vec();
        for e in entries {
            v.extend_from_slice(e);
        }
        v.extend_from_slice(b"ee");
        v
    }

    #[test]
    fn keys_come_out_in_order() {
        let (got, s) = keys(&reply(&[entry(1), entry(2)]), LIMITS).unwrap();
        assert_eq!(got, [InfoHash([1; 20]), InfoHash([2; 20])]);
        assert_eq!(s.keys, 2);
        assert_eq!(s.malformed, 0);
    }

    #[test]
    fn an_empty_files_dict_is_fine_and_a_missing_one_is_not() {
        assert_eq!(keys(b"d5:filesdee", LIMITS).unwrap().0, []);
        assert!(matches!(
            keys(b"de", LIMITS),
            Err(Error::Protocol("reply has no files dictionary"))
        ));
    }

    #[test]
    fn a_failure_reason_is_a_refusal() {
        let r = keys(b"d14:failure reason12:rate limitede", LIMITS);
        assert!(matches!(r, Err(Error::Refused(m)) if m == "rate limited"));
    }

    #[test]
    fn bad_shapes_are_skipped_and_counted() {
        let short = [&b"19:"[..], &[b'a'; 19], b"de"].concat();
        let long = [&b"21:"[..], &[b'a'; 21], b"de"].concat();
        let mut scalar = b"20:".to_vec();
        scalar.extend_from_slice(&[9; 20]);
        scalar.extend_from_slice(b"i5e");
        let (got, s) = keys(&reply(&[short, entry(1), long, scalar]), LIMITS).unwrap();
        assert_eq!(got, [InfoHash([1; 20])]);
        assert_eq!(s.malformed, 3);
    }

    #[test]
    fn an_oversized_entry_is_skipped() {
        let mut big = b"20:".to_vec();
        big.extend_from_slice(&[7; 20]);
        big.extend_from_slice(b"d4:name2000:");
        big.extend_from_slice(&[b'x'; 2000]);
        big.push(b'e');
        let (got, s) = keys(&reply(&[big, entry(1)]), LIMITS).unwrap();
        assert_eq!(got, [InfoHash([1; 20])]);
        assert_eq!(s.malformed, 1);
    }

    #[test]
    fn caps_end_the_parse() {
        let body = reply(&[entry(1), entry(2), entry(3)]);
        let few = Limits {
            max_entries: 2,
            ..LIMITS
        };
        assert!(keys(&body, few).is_err());
        let small = Limits {
            max_bytes: 100,
            ..LIMITS
        };
        assert!(matches!(keys(&body, small), Err(Error::TooLarge)));
        // The entry completed under the cap is still emitted.
        let mut out = Vec::new();
        let r = parse(&body, small, body.len(), &mut |b| out.extend_from_slice(b));
        assert!(matches!(r, Err(Error::TooLarge)));
        assert_eq!(out, [InfoHash([1; 20])]);
        assert!(matches!(
            keys(b"d5:filesd3:abc999999999999:", LIMITS),
            Err(Error::TooLarge)
        ));
        let deep = Limits {
            max_depth: 3,
            ..LIMITS
        };
        assert!(keys(&reply(&[entry(1)]), deep).is_ok());
        let mut nested = b"d5:filesd20:".to_vec();
        nested.extend_from_slice(&[1; 20]);
        nested.extend_from_slice(b"dlleeee");
        assert!(keys(&nested, deep).is_err());
    }

    #[test]
    fn broken_bencode_is_refused() {
        for bad in [
            &b""[..],
            b"l",
            b"le",
            b"d5:filesd",
            b"di1ei2ee",
            b"d5:filesdeex",
            b"d3:keye",
            b"d1:ai-e",
            b"d1:aie",
            b"d1:a2x",
            b"d1:ai1-e",
            b"d5:filesi1ee",
        ] {
            assert!(keys(bad, LIMITS).is_err(), "{bad:?}");
        }
        assert!(keys(b"d5:filesdee\r\n", LIMITS).is_ok());
    }

    #[test]
    fn batches_are_bounded() {
        let entries: Vec<_> = (0..10).map(entry).collect();
        let mut sizes = Vec::new();
        let l = Limits { batch: 4, ..LIMITS };
        // One chunk: full batches, then the rest when the call returns.
        let body = reply(&entries);
        parse(&body, l, body.len(), &mut |b| sizes.push(b.len())).unwrap();
        assert_eq!(sizes, [4, 4, 2]);
        sizes.clear();
        parse(&body, l, 7, &mut |b| sizes.push(b.len())).unwrap();
        assert!(sizes.iter().all(|n| (1..=4).contains(n)));
        assert_eq!(sizes.iter().sum::<usize>(), 10);
    }

    #[test]
    fn keys_completed_before_a_fault_are_emitted() {
        let mut body = b"d5:filesd".to_vec();
        body.extend_from_slice(&entry(1));
        body.extend_from_slice(&entry(2));
        body.push(b'x');
        let mut out = Vec::new();
        let r = parse(&body, LIMITS, body.len(), &mut |b| out.extend_from_slice(b));
        assert!(r.is_err());
        assert_eq!(out, [InfoHash([1; 20]), InfoHash([2; 20])]);
    }

    #[test]
    fn a_failed_parser_stays_failed() {
        let mut p = Parser::new(LIMITS);
        assert!(p.feed(b"x", &mut |_| {}).is_err());
        assert!(p.feed(b"d5:filesdee", &mut |_| {}).is_err());
        assert!(p.finish(&mut |_| {}).is_err());
    }
}
