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
          "replace": { "end": 5, "start": 0 }, "text": "The Dark Knight", "count": 12 }
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
    fn an_item_without_a_count_leaves_it_out() {
        let item = SuggestItem {
            count: None,
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
