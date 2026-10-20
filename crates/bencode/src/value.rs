use std::collections::BTreeMap;
use std::fmt;

/// A decoded bencode value that borrows byte strings from the input buffer.
#[derive(Clone)]
pub enum Value<'a> {
    Int(i64),
    Bytes(&'a [u8]),
    List(List<'a>),
    Dict(Dict<'a>),
}

/// A decoded list together with the raw input span it was decoded from.
#[derive(Clone)]
pub struct List<'a> {
    pub(crate) items: Vec<Value<'a>>,
    pub(crate) raw: &'a [u8],
}

/// A decoded dictionary together with the raw input span it was decoded from.
///
/// Entries keep their input order. Keys are unique (the decoder rejects
/// duplicates); they are sorted unless the dictionary came from a lenient
/// decode of non-canonical input.
#[derive(Clone)]
pub struct Dict<'a> {
    pub(crate) entries: Vec<(&'a [u8], Value<'a>)>,
    pub(crate) raw: &'a [u8],
    pub(crate) sorted: bool,
}

impl<'a> Value<'a> {
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<&'a [u8]> {
        match self {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }

    /// The byte string as UTF-8, if it is a byte string and valid UTF-8.
    pub fn as_str(&self) -> Option<&'a str> {
        self.as_bytes().and_then(|b| std::str::from_utf8(b).ok())
    }

    pub fn as_list(&self) -> Option<&List<'a>> {
        match self {
            Value::List(l) => Some(l),
            _ => None,
        }
    }

    pub fn as_dict(&self) -> Option<&Dict<'a>> {
        match self {
            Value::Dict(d) => Some(d),
            _ => None,
        }
    }

    /// A short name for the value's type, for error messages.
    pub fn kind_name(&self) -> &'static str {
        match self {
            Value::Int(_) => "integer",
            Value::Bytes(_) => "byte string",
            Value::List(_) => "list",
            Value::Dict(_) => "dictionary",
        }
    }

    /// Copies the value into an owned, canonically ordered form.
    pub fn to_owned_value(&self) -> OwnedValue {
        match self {
            Value::Int(i) => OwnedValue::Int(*i),
            Value::Bytes(b) => OwnedValue::Bytes(b.to_vec()),
            Value::List(l) => OwnedValue::List(l.items.iter().map(Value::to_owned_value).collect()),
            Value::Dict(d) => OwnedValue::Dict(
                d.entries
                    .iter()
                    .map(|(k, v)| (k.to_vec(), v.to_owned_value()))
                    .collect(),
            ),
        }
    }
}

impl<'a> List<'a> {
    pub fn items(&self) -> &[Value<'a>] {
        &self.items
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Value<'a>> {
        self.items.iter()
    }

    pub fn get(&self, index: usize) -> Option<&Value<'a>> {
        self.items.get(index)
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The exact input bytes this list was decoded from, `l` through `e`.
    pub fn raw(&self) -> &'a [u8] {
        self.raw
    }
}

impl<'a> Dict<'a> {
    pub fn get(&self, key: &[u8]) -> Option<&Value<'a>> {
        if self.sorted {
            self.entries
                .binary_search_by(|(k, _)| (*k).cmp(key))
                .ok()
                .map(|i| &self.entries[i].1)
        } else {
            self.entries.iter().find(|(k, _)| *k == key).map(|(_, v)| v)
        }
    }

    pub fn contains_key(&self, key: &[u8]) -> bool {
        self.get(key).is_some()
    }

    /// Entries in input order.
    pub fn entries(&self) -> &[(&'a [u8], Value<'a>)] {
        &self.entries
    }

    /// Iterates entries in input order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&'a [u8], &Value<'a>)> + '_ {
        self.entries.iter().map(|(k, v)| (*k, v))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether the keys appeared in canonical (sorted) order in the input.
    pub fn is_sorted(&self) -> bool {
        self.sorted
    }

    /// The exact input bytes this dictionary was decoded from, `d` through `e`.
    pub fn raw(&self) -> &'a [u8] {
        self.raw
    }
}

/// An owned bencode value. Dictionaries are `BTreeMap`s, so keys are always
/// unique and sorted and encoding is canonical by construction.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OwnedValue {
    Int(i64),
    Bytes(Vec<u8>),
    List(Vec<OwnedValue>),
    Dict(BTreeMap<Vec<u8>, OwnedValue>),
}

impl From<i64> for OwnedValue {
    fn from(v: i64) -> Self {
        OwnedValue::Int(v)
    }
}

impl From<&[u8]> for OwnedValue {
    fn from(v: &[u8]) -> Self {
        OwnedValue::Bytes(v.to_vec())
    }
}

impl From<Vec<u8>> for OwnedValue {
    fn from(v: Vec<u8>) -> Self {
        OwnedValue::Bytes(v)
    }
}

impl From<&str> for OwnedValue {
    fn from(v: &str) -> Self {
        OwnedValue::Bytes(v.as_bytes().to_vec())
    }
}

struct ByteStr<'b>(&'b [u8]);

impl fmt::Debug for ByteStr<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "b\"{}\"", self.0.escape_ascii())
    }
}

impl fmt::Debug for Value<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Int(i) => write!(f, "{i}"),
            Value::Bytes(b) => ByteStr(b).fmt(f),
            Value::List(l) => l.fmt(f),
            Value::Dict(d) => d.fmt(f),
        }
    }
}

impl fmt::Debug for List<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(&self.items).finish()
    }
}

impl fmt::Debug for Dict<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(self.entries.iter().map(|(k, v)| (ByteStr(k), v)))
            .finish()
    }
}

impl fmt::Debug for OwnedValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OwnedValue::Int(i) => write!(f, "{i}"),
            OwnedValue::Bytes(b) => ByteStr(b).fmt(f),
            OwnedValue::List(l) => f.debug_list().entries(l).finish(),
            OwnedValue::Dict(d) => f
                .debug_map()
                .entries(d.iter().map(|(k, v)| (ByteStr(k), v)))
                .finish(),
        }
    }
}
