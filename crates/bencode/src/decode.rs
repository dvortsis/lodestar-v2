use std::fmt;

use crate::value::{Dict, List, Value};

/// Resource limits applied while decoding untrusted input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Maximum container nesting. A top-level list or dictionary is depth 1.
    /// Decoding itself is iterative, but dropping, cloning and encoding a
    /// [`Value`] recurse once per level, so keep this in the low hundreds.
    pub max_depth: usize,
    /// Maximum number of values (integers, byte strings and containers,
    /// excluding dictionary keys). Bounds memory use: each value costs a
    /// fixed-size slot in its parent container.
    pub max_items: usize,
}

impl Limits {
    pub const DEFAULT: Limits = Limits {
        max_depth: 64,
        max_items: 1_000_000,
    };
}

impl Default for Limits {
    fn default() -> Self {
        Limits::DEFAULT
    }
}

/// Decoder configuration: limits plus strict/lenient mode (see crate docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DecodeOptions {
    pub limits: Limits,
    pub strict: bool,
}

impl DecodeOptions {
    pub const fn lenient() -> Self {
        DecodeOptions {
            limits: Limits::DEFAULT,
            strict: false,
        }
    }

    pub const fn strict() -> Self {
        DecodeOptions {
            limits: Limits::DEFAULT,
            strict: true,
        }
    }

    pub const fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    UnexpectedEof,
    /// A byte that can't start a value (or a dictionary key) here.
    UnexpectedByte(u8),
    /// Malformed integer, e.g. `ie`, `i-e` or a non-digit before `e`.
    InvalidInteger,
    IntegerOverflow,
    /// Malformed or overflowing byte-string length prefix.
    InvalidLength,
    /// Leading zeros or `-0` (strict mode only).
    NonCanonicalNumber,
    /// Dictionary keys out of order (strict mode only).
    UnsortedKeys,
    DuplicateKey,
    DepthLimit,
    ItemLimit,
    TrailingData,
}

/// A decode failure and the input offset where it was detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeError {
    pub kind: ErrorKind,
    pub offset: usize,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match self.kind {
            ErrorKind::UnexpectedEof => "unexpected end of input".to_owned(),
            ErrorKind::UnexpectedByte(b) => format!("unexpected byte {:?}", b as char),
            ErrorKind::InvalidInteger => "invalid integer".to_owned(),
            ErrorKind::IntegerOverflow => "integer out of range".to_owned(),
            ErrorKind::InvalidLength => "invalid byte-string length".to_owned(),
            ErrorKind::NonCanonicalNumber => "non-canonical number".to_owned(),
            ErrorKind::UnsortedKeys => "dictionary keys are not sorted".to_owned(),
            ErrorKind::DuplicateKey => "duplicate dictionary key".to_owned(),
            ErrorKind::DepthLimit => "nesting depth limit exceeded".to_owned(),
            ErrorKind::ItemLimit => "item count limit exceeded".to_owned(),
            ErrorKind::TrailingData => "trailing data after value".to_owned(),
        };
        write!(f, "bencode: {what} at offset {}", self.offset)
    }
}

impl std::error::Error for DecodeError {}

/// Decodes exactly one value spanning the whole input, leniently, with default limits.
pub fn decode(input: &[u8]) -> Result<Value<'_>, DecodeError> {
    decode_with(input, &DecodeOptions::lenient())
}

/// Decodes exactly one canonical value spanning the whole input.
pub fn decode_strict(input: &[u8]) -> Result<Value<'_>, DecodeError> {
    decode_with(input, &DecodeOptions::strict())
}

/// Decodes exactly one value spanning the whole input.
pub fn decode_with<'a>(input: &'a [u8], opts: &DecodeOptions) -> Result<Value<'a>, DecodeError> {
    let (value, end) = decode_prefix(input, opts)?;
    if end != input.len() {
        return Err(DecodeError {
            kind: ErrorKind::TrailingData,
            offset: end,
        });
    }
    Ok(value)
}

/// Decodes one value from the start of `input` and returns it with the
/// number of bytes consumed. Used where a bencoded header is followed by a
/// raw payload (BEP 9 `ut_metadata` data messages).
pub fn decode_prefix<'a>(
    input: &'a [u8],
    opts: &DecodeOptions,
) -> Result<(Value<'a>, usize), DecodeError> {
    let mut parser = Parser {
        input,
        pos: 0,
        opts: *opts,
        items: 0,
    };
    let value = parser.parse()?;
    Ok((value, parser.pos))
}

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
    opts: DecodeOptions,
    items: usize,
}

enum Frame<'a> {
    List {
        start: usize,
        items: Vec<Value<'a>>,
    },
    Dict {
        start: usize,
        entries: Vec<(&'a [u8], Value<'a>)>,
        /// Key read but its value not yet complete.
        pending: Option<&'a [u8]>,
        sorted: bool,
    },
}

impl<'a> Parser<'a> {
    fn err(&self, kind: ErrorKind, offset: usize) -> DecodeError {
        DecodeError { kind, offset }
    }

    fn peek(&self) -> Result<u8, DecodeError> {
        self.input
            .get(self.pos)
            .copied()
            .ok_or(self.err(ErrorKind::UnexpectedEof, self.pos))
    }

    /// Iterative parse with an explicit frame stack.
    fn parse(&mut self) -> Result<Value<'a>, DecodeError> {
        let mut stack: Vec<Frame<'a>> = Vec::new();
        loop {
            let byte = self.peek()?;
            let completed = match stack.last_mut() {
                Some(Frame::Dict {
                    entries,
                    pending: pending @ None,
                    sorted,
                    ..
                }) => {
                    // Expecting a key or the end of the dictionary.
                    if byte == b'e' {
                        self.pos += 1;
                        self.finish(stack.pop())?
                    } else if byte.is_ascii_digit() {
                        let key_start = self.pos;
                        let key = self.byte_string()?;
                        let last = entries.last().map(|(k, _)| *k);
                        if let Some(last) = last {
                            if key == last {
                                return Err(self.err(ErrorKind::DuplicateKey, key_start));
                            }
                            if key < last {
                                if self.opts.strict {
                                    return Err(self.err(ErrorKind::UnsortedKeys, key_start));
                                }
                                *sorted = false;
                            }
                        }
                        *pending = Some(key);
                        continue;
                    } else {
                        return Err(self.err(ErrorKind::UnexpectedByte(byte), self.pos));
                    }
                }
                Some(Frame::List { .. }) if byte == b'e' => {
                    self.pos += 1;
                    self.finish(stack.pop())?
                }
                _ => {
                    // Expecting a value.
                    self.items += 1;
                    if self.items > self.opts.limits.max_items {
                        return Err(self.err(ErrorKind::ItemLimit, self.pos));
                    }
                    match byte {
                        b'i' => Value::Int(self.integer()?),
                        b'0'..=b'9' => Value::Bytes(self.byte_string()?),
                        b'l' | b'd' => {
                            if stack.len() >= self.opts.limits.max_depth {
                                return Err(self.err(ErrorKind::DepthLimit, self.pos));
                            }
                            let start = self.pos;
                            self.pos += 1;
                            stack.push(if byte == b'l' {
                                Frame::List {
                                    start,
                                    items: Vec::new(),
                                }
                            } else {
                                Frame::Dict {
                                    start,
                                    entries: Vec::new(),
                                    pending: None,
                                    sorted: true,
                                }
                            });
                            continue;
                        }
                        b => return Err(self.err(ErrorKind::UnexpectedByte(b), self.pos)),
                    }
                }
            };
            match stack.last_mut() {
                None => return Ok(completed),
                Some(Frame::List { items, .. }) => items.push(completed),
                Some(Frame::Dict {
                    entries, pending, ..
                }) => {
                    // A dict frame only expects a value once its key is pending.
                    let key = pending.take().expect("dict value without a pending key");
                    entries.push((key, completed));
                }
            }
        }
    }

    /// Turns a popped frame into a value; `self.pos` is just past its `e`.
    fn finish(&self, frame: Option<Frame<'a>>) -> Result<Value<'a>, DecodeError> {
        match frame.expect("finish called with an empty stack") {
            Frame::List { start, items } => Ok(Value::List(List {
                items,
                raw: &self.input[start..self.pos],
            })),
            Frame::Dict {
                start,
                entries,
                sorted,
                ..
            } => {
                if !sorted {
                    // Adjacent-key checks can't catch duplicates once order is lost.
                    let mut keys: Vec<&[u8]> = entries.iter().map(|(k, _)| *k).collect();
                    keys.sort_unstable();
                    if keys.windows(2).any(|w| w[0] == w[1]) {
                        return Err(self.err(ErrorKind::DuplicateKey, start));
                    }
                }
                Ok(Value::Dict(Dict {
                    entries,
                    raw: &self.input[start..self.pos],
                    sorted,
                }))
            }
        }
    }

    /// Parses `i<digits>e` at the current position.
    fn integer(&mut self) -> Result<i64, DecodeError> {
        let start = self.pos;
        self.pos += 1; // 'i'
        let negative = self.input.get(self.pos) == Some(&b'-');
        if negative {
            self.pos += 1;
        }
        let digits_start = self.pos;
        let mut magnitude: u64 = 0;
        loop {
            let b = self.peek()?;
            match b {
                b'0'..=b'9' => {
                    magnitude = magnitude
                        .checked_mul(10)
                        .and_then(|m| m.checked_add(u64::from(b - b'0')))
                        .ok_or(self.err(ErrorKind::IntegerOverflow, start))?;
                    self.pos += 1;
                }
                b'e' => break,
                _ => return Err(self.err(ErrorKind::InvalidInteger, self.pos)),
            }
        }
        let digits = &self.input[digits_start..self.pos];
        self.pos += 1; // 'e'
        if digits.is_empty() {
            return Err(self.err(ErrorKind::InvalidInteger, start));
        }
        if self.opts.strict
            && ((digits.len() > 1 && digits[0] == b'0') || (negative && magnitude == 0))
        {
            return Err(self.err(ErrorKind::NonCanonicalNumber, start));
        }
        if negative {
            if magnitude > i64::MAX as u64 + 1 {
                return Err(self.err(ErrorKind::IntegerOverflow, start));
            }
            // Covers i64::MIN, whose magnitude doesn't fit in i64.
            Ok((magnitude as i64).wrapping_neg())
        } else {
            i64::try_from(magnitude).map_err(|_| self.err(ErrorKind::IntegerOverflow, start))
        }
    }

    /// Parses `<len>:<bytes>` at the current position.
    fn byte_string(&mut self) -> Result<&'a [u8], DecodeError> {
        let start = self.pos;
        let mut len: usize = 0;
        loop {
            let b = self.peek()?;
            match b {
                b'0'..=b'9' => {
                    len = len
                        .checked_mul(10)
                        .and_then(|l| l.checked_add(usize::from(b - b'0')))
                        .ok_or(self.err(ErrorKind::InvalidLength, start))?;
                    self.pos += 1;
                }
                b':' => break,
                _ => return Err(self.err(ErrorKind::InvalidLength, self.pos)),
            }
        }
        if self.opts.strict && self.pos - start > 1 && self.input[start] == b'0' {
            return Err(self.err(ErrorKind::NonCanonicalNumber, start));
        }
        self.pos += 1; // ':'
        let end = self
            .pos
            .checked_add(len)
            .filter(|&end| end <= self.input.len())
            .ok_or(self.err(ErrorKind::UnexpectedEof, self.input.len()))?;
        let bytes = &self.input[self.pos..end];
        self.pos = end;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(input: &[u8]) -> ErrorKind {
        decode(input).unwrap_err().kind
    }

    fn strict_kind(input: &[u8]) -> ErrorKind {
        decode_strict(input).unwrap_err().kind
    }

    #[test]
    fn scalars() {
        assert_eq!(decode(b"i42e").unwrap().as_int(), Some(42));
        assert_eq!(decode(b"i-42e").unwrap().as_int(), Some(-42));
        assert_eq!(decode(b"i0e").unwrap().as_int(), Some(0));
        assert_eq!(decode(b"4:spam").unwrap().as_bytes(), Some(&b"spam"[..]));
        assert_eq!(decode(b"0:").unwrap().as_bytes(), Some(&b""[..]));
    }

    #[test]
    fn integer_extremes() {
        assert_eq!(
            decode(b"i9223372036854775807e").unwrap().as_int(),
            Some(i64::MAX)
        );
        assert_eq!(
            decode(b"i-9223372036854775808e").unwrap().as_int(),
            Some(i64::MIN)
        );
        assert_eq!(kind(b"i9223372036854775808e"), ErrorKind::IntegerOverflow);
        assert_eq!(kind(b"i-9223372036854775809e"), ErrorKind::IntegerOverflow);
        assert_eq!(
            kind(b"i99999999999999999999999e"),
            ErrorKind::IntegerOverflow
        );
    }

    #[test]
    fn malformed_integers() {
        assert_eq!(kind(b"ie"), ErrorKind::InvalidInteger);
        assert_eq!(kind(b"i-e"), ErrorKind::InvalidInteger);
        assert_eq!(kind(b"i1-2e"), ErrorKind::InvalidInteger);
        assert_eq!(kind(b"i+1e"), ErrorKind::InvalidInteger);
        assert_eq!(kind(b"i12"), ErrorKind::UnexpectedEof);
    }

    #[test]
    fn non_canonical_numbers() {
        assert_eq!(decode(b"i03e").unwrap().as_int(), Some(3));
        assert_eq!(decode(b"i-0e").unwrap().as_int(), Some(0));
        assert_eq!(decode(b"03:abc").unwrap().as_bytes(), Some(&b"abc"[..]));
        assert_eq!(strict_kind(b"i03e"), ErrorKind::NonCanonicalNumber);
        assert_eq!(strict_kind(b"i-0e"), ErrorKind::NonCanonicalNumber);
        assert_eq!(strict_kind(b"i-03e"), ErrorKind::NonCanonicalNumber);
        assert_eq!(strict_kind(b"03:abc"), ErrorKind::NonCanonicalNumber);
        assert_eq!(strict_kind(b"00:"), ErrorKind::NonCanonicalNumber);
    }

    #[test]
    fn byte_string_errors() {
        assert_eq!(kind(b"5:abc"), ErrorKind::UnexpectedEof);
        assert_eq!(kind(b"3abc"), ErrorKind::InvalidLength);
        assert_eq!(kind(b"99999999999999999999999:x"), ErrorKind::InvalidLength);
        assert_eq!(kind(b"18446744073709551615:x"), ErrorKind::UnexpectedEof);
    }

    #[test]
    fn containers_and_raw_spans() {
        let input = b"d3:cow3:moo4:spaml1:a1:bee";
        let v = decode_strict(input).unwrap();
        let d = v.as_dict().unwrap();
        assert_eq!(d.raw(), &input[..]);
        assert_eq!(d.get(b"cow").unwrap().as_bytes(), Some(&b"moo"[..]));
        let l = d.get(b"spam").unwrap().as_list().unwrap();
        assert_eq!(l.raw(), b"l1:a1:be");
        assert_eq!(l.len(), 2);
        assert!(d.get(b"missing").is_none());
    }

    #[test]
    fn unsorted_keys() {
        let input = b"d1:bi1e1:ai2ee";
        let v = decode(input).unwrap();
        let d = v.as_dict().unwrap();
        assert!(!d.is_sorted());
        assert_eq!(d.get(b"a").unwrap().as_int(), Some(2));
        assert_eq!(d.get(b"b").unwrap().as_int(), Some(1));
        assert_eq!(strict_kind(input), ErrorKind::UnsortedKeys);
    }

    #[test]
    fn duplicate_keys() {
        assert_eq!(kind(b"d1:ai1e1:ai2ee"), ErrorKind::DuplicateKey);
        // Non-adjacent duplicate, only detectable after sorting.
        assert_eq!(kind(b"d1:bi1e1:ai2e1:bi3ee"), ErrorKind::DuplicateKey);
    }

    #[test]
    fn structural_errors() {
        assert_eq!(kind(b""), ErrorKind::UnexpectedEof);
        assert_eq!(kind(b"l"), ErrorKind::UnexpectedEof);
        assert_eq!(kind(b"d1:a"), ErrorKind::UnexpectedEof);
        assert_eq!(kind(b"di1ei2ee"), ErrorKind::UnexpectedByte(b'i'));
        assert_eq!(kind(b"d1:ae"), ErrorKind::UnexpectedByte(b'e'));
        assert_eq!(kind(b"e"), ErrorKind::UnexpectedByte(b'e'));
        assert_eq!(kind(b"x"), ErrorKind::UnexpectedByte(b'x'));
        assert_eq!(kind(b"i1ei2e"), ErrorKind::TrailingData);
    }

    #[test]
    fn prefix_decoding() {
        let input = b"d8:msg_typei1e5:piecei0eeRAWPAYLOAD";
        let (v, used) = decode_prefix(input, &DecodeOptions::lenient()).unwrap();
        assert_eq!(&input[used..], b"RAWPAYLOAD");
        assert_eq!(
            v.as_dict().unwrap().get(b"piece").unwrap().as_int(),
            Some(0)
        );
    }

    #[test]
    fn depth_limit() {
        let limits = Limits {
            max_depth: 3,
            max_items: 100,
        };
        let opts = DecodeOptions::lenient().with_limits(limits);
        assert!(decode_with(b"llleee", &opts).is_ok());
        assert_eq!(
            decode_with(b"lllleeee", &opts).unwrap_err().kind,
            ErrorKind::DepthLimit
        );
    }

    #[test]
    fn hostile_nesting_is_rejected_cheaply() {
        let mut input = vec![b'l'; 1_000_000];
        input.extend(std::iter::repeat_n(b'e', 1_000_000));
        let err = decode(&input).unwrap_err();
        assert_eq!(err.kind, ErrorKind::DepthLimit);
        assert_eq!(err.offset, Limits::DEFAULT.max_depth);
    }

    #[test]
    fn item_limit() {
        let opts = DecodeOptions::lenient().with_limits(Limits {
            max_depth: 8,
            max_items: 3,
        });
        assert!(decode_with(b"li1ei2ee", &opts).is_ok());
        assert_eq!(
            decode_with(b"li1ei2ei3ee", &opts).unwrap_err().kind,
            ErrorKind::ItemLimit
        );
    }

    #[test]
    fn error_display() {
        let e = decode(b"i1x").unwrap_err();
        assert_eq!(e.to_string(), "bencode: invalid integer at offset 2");
    }
}
