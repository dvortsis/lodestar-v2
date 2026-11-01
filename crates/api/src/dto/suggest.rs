//! `/suggest` (§8, §9, §10.4): typeahead for the query bar.

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

/// `GET /suggest?q=&pos=&limit=&hidden=`.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[serde(default)]
#[into_params(parameter_in = Query)]
pub struct SuggestQuery {
    /// The query text as typed so far.
    pub q: String,
    /// The caret, in characters from the start; the end by default.
    pub pos: Option<usize>,
    /// Items at most: 8 by default, 1–20.
    pub limit: Option<usize>,
    /// Include torrents a filter rule hid (§20.3), as `/search`.
    pub hidden: Option<bool>,
}

/// `GET /suggest`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({
    "items": [
        { "insert": "res:2160p", "kind": "chip", "replace": { "end": 5, "start": 0 },
          "text": "res:2160p", "count": 1284 },
        { "insert": "title:\"The Dark Knight\"", "kind": "title",
          "replace": { "end": 5, "start": 0 }, "text": "The Dark Knight", "count": 12 },
        { "insert": "title:\"The Office\"", "kind": "title",
          "replace": { "end": 5, "start": 0 }, "text": "The Office", "count": 4210,
          "count_approx": true }
    ]
}))]
pub struct SuggestResponse {
    /// "Did you mean" first, then chips, field names, values and titles.
    pub items: Vec<SuggestItem>,
}

/// One suggestion: replace the query's characters `replace.start` to
/// `replace.end` (end exclusive) with `insert`. A leading `-` or `!`
/// stays outside the range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SuggestItem {
    /// Torrents it would find, of those this search may show; absent for
    /// field names, categories and "did you mean".
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub count: Option<u64>,
    /// `count` is an estimate, shown with "≈" as `total_approx` is on a
    /// search (§8): a title's past 1,000 matches, from the title set
    /// rather than counted live.
    /// Absent when the count is exact or there's no count.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub count_approx: bool,
    /// The query text to put in.
    pub insert: String,
    pub kind: SuggestKind,
    pub replace: CharRange,
    /// What to show: the value, title or field, or for `did_you_mean`
    /// the whole corrected query.
    pub text: String,
}

/// What an item offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SuggestKind {
    /// A field name (`res:`).
    Field,
    /// A value of the field being typed, from the index.
    Value,
    /// A title, as a `title:"…"` phrase.
    Title,
    /// A recognised word in its field form (`2160p` as `res:2160p`).
    Chip,
    /// A near field name for an unknown one, or a likelier word when the
    /// query finds few torrents.
    DidYouMean,
}

impl From<lodestar_search::suggest::Kind> for SuggestKind {
    fn from(k: lodestar_search::suggest::Kind) -> SuggestKind {
        use lodestar_search::suggest::Kind;
        match k {
            Kind::Field => SuggestKind::Field,
            Kind::Value => SuggestKind::Value,
            Kind::Title => SuggestKind::Title,
            Kind::Chip => SuggestKind::Chip,
            Kind::DidYouMean => SuggestKind::DidYouMean,
        }
    }
}

/// `GET /search/fields`: the fields the query language accepts, for the
/// query Builder (§8, §10.4), in the parser's order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({
    "fields": [
        { "aliases": [], "facetable": true, "name": "res",
          "operators": ["eq", "any_of"], "type": "keyword" },
        { "aliases": ["age"], "facetable": false, "name": "discovered",
          "operators": ["eq", "gt", "lt", "range"], "type": "date" },
        { "aliases": [], "facetable": true, "name": "v2",
          "operators": ["eq"], "type": "flag", "values": ["yes", "no"] }
    ]
}))]
pub struct SearchFieldsResponse {
    pub fields: Vec<SearchField>,
}

/// One field of the query language.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SearchField {
    /// Other names for it, which parse the same (`age` for `discovered`).
    pub aliases: Vec<String>,
    /// `/facets` counts its values as a term facet. The histograms
    /// (`size`, `discovered`, `year`) aren't term facets: false.
    pub facetable: bool,
    /// What goes before the `:`.
    pub name: String,
    /// How a value can be written after `name:`.
    pub operators: Vec<FieldOperator>,
    #[serde(rename = "type")]
    pub value_type: FieldType,
    /// The values it takes, when they're a fixed list (for `share`, the
    /// file categories); absent when it's open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub values: Option<Vec<String>>,
}

/// What a field's value is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    /// Words, `"phrases"`, `prefix*`, `~fuzzy`.
    Text,
    /// A plain number (`year:2019..2021`).
    Number,
    /// Bytes, with an optional unit (`size:>4GB`).
    Size,
    /// A UTC day, a day range or an age (`discovered:<7d`).
    Date,
    /// One of `values`.
    Enum,
    /// An open-ended stored keyword (`group:`, `ext:`).
    Keyword,
    /// `yes` or `no`.
    Flag,
    /// 40 hex digits.
    Hash,
    /// A magnet link's `?xt=urn:btih:…`.
    Magnet,
    /// A file category's share (`cat:video>=80%`); `values` are the
    /// categories.
    Share,
    /// A clause over single files, `file:(ext:flac size:>50MB)`.
    File,
    /// A time of day or a range, `22:00..02:00`.
    Time,
}

impl From<lodestar_search::catalogue::ValueType> for FieldType {
    fn from(t: lodestar_search::catalogue::ValueType) -> FieldType {
        use lodestar_search::catalogue::ValueType as V;
        match t {
            V::Text => FieldType::Text,
            V::Number => FieldType::Number,
            V::Size => FieldType::Size,
            V::Date => FieldType::Date,
            V::Enum => FieldType::Enum,
            V::Keyword => FieldType::Keyword,
            V::Flag => FieldType::Flag,
            V::Hash => FieldType::Hash,
            V::Magnet => FieldType::Magnet,
            V::Share => FieldType::Share,
            V::File => FieldType::File,
            V::Time => FieldType::Time,
        }
    }
}

/// How a value is written after `name:`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FieldOperator {
    /// The value itself (`res:1080p`).
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
    /// `a,b`: any of them.
    AnyOf,
    /// `name:"two words"`
    Phrase,
    /// `name:dar*`
    Prefix,
    /// `name:~knigt`
    Fuzzy,
    /// `name:( … )`
    Group,
}

impl From<lodestar_search::catalogue::Op> for FieldOperator {
    fn from(o: lodestar_search::catalogue::Op) -> FieldOperator {
        use lodestar_search::catalogue::Op;
        match o {
            Op::Eq => FieldOperator::Eq,
            Op::Gt => FieldOperator::Gt,
            Op::Gte => FieldOperator::Gte,
            Op::Lt => FieldOperator::Lt,
            Op::Lte => FieldOperator::Lte,
            Op::Range => FieldOperator::Range,
            Op::AnyOf => FieldOperator::AnyOf,
            Op::Phrase => FieldOperator::Phrase,
            Op::Prefix => FieldOperator::Prefix,
            Op::Fuzzy => FieldOperator::Fuzzy,
            Op::Group => FieldOperator::Group,
        }
    }
}

impl From<&lodestar_search::catalogue::FieldInfo> for SearchField {
    fn from(f: &lodestar_search::catalogue::FieldInfo) -> SearchField {
        SearchField {
            aliases: f.aliases.iter().map(|a| (*a).to_owned()).collect(),
            facetable: f.facetable,
            name: f.name.to_owned(),
            operators: f.ops.iter().map(|&o| o.into()).collect(),
            value_type: f.value.into(),
            values: (!f.values.is_empty())
                .then(|| f.values.iter().map(|v| (*v).to_owned()).collect()),
        }
    }
}

/// A range of characters (Unicode scalar values) in the query, end
/// exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CharRange {
    pub end: usize,
    pub start: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_serialize_as_the_search_crate_names_them() {
        use lodestar_search::suggest::Kind;
        for k in [
            Kind::Field,
            Kind::Value,
            Kind::Title,
            Kind::Chip,
            Kind::DidYouMean,
        ] {
            let json = serde_json::to_value(SuggestKind::from(k)).unwrap();
            assert_eq!(json, k.as_str());
        }
    }

    #[test]
    fn field_types_and_operators_serialize_as_the_search_crate_names_them() {
        use lodestar_search::catalogue::catalogue;
        for f in catalogue(true) {
            let json = serde_json::to_value(SearchField::from(f)).unwrap();
            assert_eq!(json["type"], f.value.as_str());
            let ops: Vec<&str> = f.ops.iter().map(|o| o.as_str()).collect();
            assert_eq!(json["operators"], serde_json::json!(ops));
        }
    }

    #[test]
    fn an_approximate_count_says_so_and_an_exact_one_says_nothing() {
        let item = |count_approx| SuggestItem {
            count: Some(1200),
            count_approx,
            insert: "title:\"The Office\"".into(),
            kind: SuggestKind::Title,
            replace: CharRange { end: 3, start: 0 },
            text: "The Office".into(),
        };
        let exact = serde_json::to_string(&item(false)).unwrap();
        assert!(!exact.contains("count_approx"), "{exact}");
        let approx = serde_json::to_value(item(true)).unwrap();
        assert_eq!(approx["count_approx"], true);
        assert_eq!(approx["count"], 1200);
        // Absent reads back as exact.
        let back: SuggestItem = serde_json::from_str(&exact).unwrap();
        assert!(!back.count_approx);
    }

    #[test]
    fn an_item_without_a_count_leaves_it_out() {
        let item = SuggestItem {
            count: None,
            count_approx: false,
            insert: "res:".into(),
            kind: SuggestKind::Field,
            replace: CharRange { end: 2, start: 0 },
            text: "res:".into(),
        };
        assert_eq!(
            serde_json::to_string(&item).unwrap(),
            r#"{"insert":"res:","kind":"field","replace":{"end":2,"start":0},"text":"res:"}"#
        );
    }
}
