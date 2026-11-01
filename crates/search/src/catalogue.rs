//! The field catalogue (§8, §10.4 "Builder"): every field the query
//! language accepts, with its aliases, value type, operators and finite
//! values, read from the parser's own table ([`query::FIELDS`]), so a
//! field added there is listed with no second list to update.
//!
//! Built once per visibility and kept: it's served on every Builder open.

use std::sync::OnceLock;

use lodestar_classify::{AdultKind, Category, ContentType, ResolvedVia, Structure};

use crate::facets::Facet;
use crate::query::{self, FieldKind, TagKey};

/// What a field's value is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueType {
    /// Words, `"phrases"`, `prefix*` and `~fuzzy` words.
    Text,
    /// A plain number (`files:>10`, `year:2019..2021`).
    Number,
    /// Bytes with an optional unit (`size:>4GB`).
    Size,
    /// A UTC day, a day range or an age (`discovered:<7d`).
    Date,
    /// One of a fixed list of values (`values`).
    Enum,
    /// A stored keyword, open-ended (`group:`, `ext:`).
    Keyword,
    /// `yes` or `no` (`true`/`false` and `1`/`0` also parse).
    Flag,
    /// 40 hex digits.
    Hash,
    /// A magnet link's `?xt=urn:btih:…`.
    Magnet,
    /// A file category's share, `cat:video>=80%` (`values` are the
    /// categories).
    Share,
    /// A clause over single files: `file:(ext:flac size:>50MB)`.
    File,
    /// A time of day or a range of them, `22:00..02:00`.
    Time,
}

impl ValueType {
    pub fn as_str(self) -> &'static str {
        match self {
            ValueType::Text => "text",
            ValueType::Number => "number",
            ValueType::Size => "size",
            ValueType::Date => "date",
            ValueType::Enum => "enum",
            ValueType::Keyword => "keyword",
            ValueType::Flag => "flag",
            ValueType::Hash => "hash",
            ValueType::Magnet => "magnet",
            ValueType::Share => "share",
            ValueType::File => "file",
            ValueType::Time => "time",
        }
    }
}

/// How a value can be written after `field:`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// The value itself (`res:1080p`, `size:4GB`, `name:dark`).
    Eq,
    /// `>x`
    Gt,
    /// `>=x`
    Gte,
    /// `<x`
    Lt,
    /// `<=x`
    Lte,
    /// `a..b`, `a..`, `..b`, inclusive.
    Range,
    /// `a,b,c`: any of them.
    AnyOf,
    /// `field:"two words"`.
    Phrase,
    /// `field:dar*`.
    Prefix,
    /// `field:~knigt`.
    Fuzzy,
    /// `field:( … )`.
    Group,
}

impl Op {
    pub fn as_str(self) -> &'static str {
        match self {
            Op::Eq => "eq",
            Op::Gt => "gt",
            Op::Gte => "gte",
            Op::Lt => "lt",
            Op::Lte => "lte",
            Op::Range => "range",
            Op::AnyOf => "any_of",
            Op::Phrase => "phrase",
            Op::Prefix => "prefix",
            Op::Fuzzy => "fuzzy",
            Op::Group => "group",
        }
    }
}

/// One field of the catalogue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldInfo {
    pub name: &'static str,
    /// Other names for the same field (`age` for `discovered`).
    pub aliases: Vec<&'static str>,
    pub value: ValueType,
    pub ops: &'static [Op],
    /// The values it takes, when they're a fixed list; empty otherwise.
    pub values: Vec<&'static str>,
    /// `/facets` counts its values (a term facet). The histograms
    /// (`size`, `discovered`, `year`) aren't term facets and say false.
    pub facetable: bool,
}

const TEXT: &[Op] = &[Op::Eq, Op::Phrase, Op::Prefix, Op::Fuzzy];
const NUMBER: &[Op] = &[Op::Eq, Op::Gt, Op::Gte, Op::Lt, Op::Lte, Op::Range];
const DATE: &[Op] = &[Op::Eq, Op::Gt, Op::Lt, Op::Range];
const LIST: &[Op] = &[Op::Eq, Op::AnyOf];
const ONE: &[Op] = &[Op::Eq];
const FILE: &[Op] = &[Op::Eq, Op::Phrase, Op::Prefix, Op::Fuzzy, Op::Group];
const TIME: &[Op] = &[Op::Eq, Op::Range];

/// The catalogue, in the parser's table order. `adult` is "Show adult
/// content": while it's off, the `adult:` field and `type:adult` are left
/// out, as the Adult facet is (§10.4, §20.9).
pub fn catalogue(adult: bool) -> &'static [FieldInfo] {
    static SHOWN: OnceLock<Vec<FieldInfo>> = OnceLock::new();
    static HIDDEN: OnceLock<Vec<FieldInfo>> = OnceLock::new();
    let cell = if adult { &SHOWN } else { &HIDDEN };
    cell.get_or_init(|| build(adult))
}

fn build(adult: bool) -> Vec<FieldInfo> {
    let mut out: Vec<FieldInfo> = Vec::new();
    for (i, &(name, kind)) in query::FIELDS.iter().enumerate() {
        if matches!(kind, FieldKind::Later(_)) {
            continue;
        }
        if !adult && kind == FieldKind::Tag(TagKey::Adult) {
            continue;
        }
        // A later name for a kind already listed is an alias of it.
        if let Some(&(first, _)) = query::FIELDS[..i].iter().find(|(_, k)| *k == kind) {
            if let Some(f) = out.iter_mut().find(|f| f.name == first) {
                f.aliases.push(name);
            }
            continue;
        }
        let (value, ops, mut values) = shape(kind);
        if !adult && kind == FieldKind::Tag(TagKey::Type) {
            values.retain(|v| *v != ContentType::Adult.as_str());
        }
        out.push(FieldInfo {
            name,
            aliases: Vec::new(),
            value,
            ops,
            values,
            facetable: facetable(name, adult),
        });
    }
    out
}

fn shape(kind: FieldKind) -> (ValueType, &'static [Op], Vec<&'static str>) {
    match kind {
        FieldKind::Text(_) => (ValueType::Text, TEXT, Vec::new()),
        FieldKind::Num(f) if f.is_count() => (ValueType::Number, NUMBER, Vec::new()),
        FieldKind::Num(_) => (ValueType::Size, NUMBER, Vec::new()),
        FieldKind::Date(_) => (ValueType::Date, DATE, Vec::new()),
        FieldKind::Ext => (ValueType::Keyword, LIST, Vec::new()),
        FieldKind::Flag(_) => (ValueType::Flag, ONE, vec!["yes", "no"]),
        FieldKind::Hash => (ValueType::Hash, ONE, Vec::new()),
        FieldKind::Magnet => (ValueType::Magnet, ONE, Vec::new()),
        FieldKind::Tag(key) => match key {
            TagKey::Type => (
                ValueType::Enum,
                LIST,
                ContentType::ALL.iter().map(|t| t.as_str()).collect(),
            ),
            TagKey::Has => (
                ValueType::Enum,
                LIST,
                Structure::NAMED.iter().map(|(_, n)| *n).collect(),
            ),
            TagKey::Resolved => (
                ValueType::Enum,
                LIST,
                ResolvedVia::ALL.iter().map(|r| r.as_str()).collect(),
            ),
            TagKey::Adult => (
                ValueType::Enum,
                LIST,
                AdultKind::ALL.iter().map(|k| k.as_str()).collect(),
            ),
            TagKey::Res
            | TagKey::Hdr
            | TagKey::Codec
            | TagKey::Audio
            | TagKey::Lang
            | TagKey::Group => (ValueType::Keyword, LIST, Vec::new()),
        },
        FieldKind::Share(_) => (
            ValueType::Share,
            NUMBER,
            Category::ALL.iter().map(|c| c.as_str()).collect(),
        ),
        FieldKind::File => (ValueType::File, FILE, Vec::new()),
        FieldKind::TimeOfDay => (ValueType::Time, TIME, Vec::new()),
        FieldKind::Later(_) => unreachable!("left out of the catalogue"),
    }
}

/// Whether `/facets` counts the field. Its `category` facet is the
/// query's `cat:`; the Adult facet isn't served while Adult is hidden.
fn facetable(name: &str, adult: bool) -> bool {
    Facet::ALL.iter().any(|f| {
        let field = match f.name() {
            "category" => "cat",
            n => n,
        };
        field == name && (adult || *f != Facet::Adult)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(adult: bool, name: &str) -> Option<&'static FieldInfo> {
        catalogue(adult).iter().find(|f| f.name == name)
    }

    /// A value written with `op`, for a field of type `t`.
    fn sample(t: ValueType, op: Op, values: &[&str]) -> String {
        let v = |n: &str| match t {
            ValueType::Size => format!("{n}GB"),
            ValueType::Date => "2026-01-01".to_owned(),
            ValueType::Time => "09:00".to_owned(),
            _ => n.to_owned(),
        };
        let first = values.first().copied().unwrap_or("abc");
        match (t, op) {
            (ValueType::Hash, _) => "0123456789abcdef0123456789abcdef01234567".to_owned(),
            (ValueType::Magnet, _) => {
                "?xt=urn:btih:0123456789abcdef0123456789abcdef01234567".to_owned()
            }
            (ValueType::Date, Op::Lt | Op::Gt) => format!("{}7d", op_prefix(op)),
            (ValueType::Date, Op::Range) => "2026-01-01..2026-02-01".to_owned(),
            (ValueType::Time, Op::Range) => "22:00..02:00".to_owned(),
            (ValueType::Share, Op::Eq) => first.to_owned(),
            (ValueType::Share, Op::Range) => format!("{first}=10..50"),
            (ValueType::Share, _) => format!("{first}{}50%", op_prefix(op)),
            (_, Op::Eq) => v(first).replace("abc", "1"),
            (_, Op::Range) => format!("{}..{}", v("1"), v("2")),
            (_, Op::Gt | Op::Gte | Op::Lt | Op::Lte) => format!("{}{}", op_prefix(op), v("2")),
            (_, Op::AnyOf) => match values {
                [a, b, ..] => format!("{a},{b}"),
                _ => "abc,def".to_owned(),
            },
            (_, Op::Phrase) => "\"dark knight\"".to_owned(),
            (_, Op::Prefix) => "dar*".to_owned(),
            (_, Op::Fuzzy) => "~knigt".to_owned(),
            (_, Op::Group) => "(ext:flac size:>50MB)".to_owned(),
        }
    }

    fn op_prefix(op: Op) -> &'static str {
        match op {
            Op::Gt => ">",
            Op::Gte => ">=",
            Op::Lt => "<",
            Op::Lte => "<=",
            _ => "",
        }
    }

    #[test]
    fn every_listed_operator_and_value_parses() {
        for f in catalogue(true) {
            for name in std::iter::once(f.name).chain(f.aliases.iter().copied()) {
                for &op in f.ops {
                    let q = format!("{name}:{}", sample(f.value, op, &f.values));
                    assert!(query::parse(&q).is_ok(), "{q}: {:?}", query::parse(&q));
                }
                for v in &f.values {
                    let q = match f.value {
                        ValueType::Share => format!("{name}:{v}>=50%"),
                        _ => format!("{name}:{v}"),
                    };
                    assert!(query::parse(&q).is_ok(), "{q}: {:?}", query::parse(&q));
                }
            }
        }
    }

    #[test]
    fn every_field_the_parser_accepts_is_listed_once() {
        let listed: Vec<&str> = catalogue(true)
            .iter()
            .flat_map(|f| std::iter::once(f.name).chain(f.aliases.iter().copied()))
            .collect();
        let mut sorted = listed.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), listed.len(), "a name listed twice");
        for (name, kind) in query::FIELDS {
            assert_eq!(
                listed.contains(name),
                !matches!(kind, FieldKind::Later(_)),
                "{name}"
            );
        }
        assert_eq!(find(true, "discovered").unwrap().aliases, ["age"]);
        assert!(find(true, "age").is_none());
        assert!(find(true, "sig").is_none(), "not accepted yet");
    }

    #[test]
    fn types_values_and_facets() {
        let size = find(true, "size").unwrap();
        assert_eq!((size.value, size.facetable), (ValueType::Size, false));
        assert_eq!(find(true, "files").unwrap().value, ValueType::Number);
        let res = find(true, "res").unwrap();
        assert_eq!(res.value, ValueType::Keyword);
        assert!(res.values.is_empty() && res.facetable);
        assert_eq!(find(true, "v2").unwrap().values, ["yes", "no"]);
        assert!(find(true, "v2").unwrap().facetable);
        assert!(!find(true, "partial").unwrap().facetable);
        let cat = find(true, "cat").unwrap();
        assert!(cat.facetable && cat.values.contains(&"video"));
        assert!(!find(true, "bytes").unwrap().facetable);
        assert_eq!(find(true, "type").unwrap().value, ValueType::Enum);
    }

    #[test]
    fn adult_fields_and_values_only_while_adult_is_shown() {
        let ty = |adult| find(adult, "type").unwrap().values.contains(&"adult");
        assert!(ty(true));
        assert!(!ty(false));
        let a = find(true, "adult").unwrap();
        assert!(a.facetable && a.values.contains(&"scene"));
        assert!(find(false, "adult").is_none());
        // Nothing else in the hidden catalogue names Adult.
        for f in catalogue(false) {
            assert!(f.name != "adult" && !f.values.contains(&"adult"), "{f:?}");
        }
        assert_eq!(catalogue(true).len(), catalogue(false).len() + 1);
    }
}
