//! Torznab's query parameters, all untrusted, and the search they ask
//! for (plan §4.3 TZ-0).
//!
//! **Parsing:**
//! - The query string is read once, at most [`MAX_QUERY_STRING`] bytes
//!   and [`MAX_PARAMS`] parameters.
//! - Percent-decoding that isn't valid UTF-8 is replaced, never trusted.
//! - A parameter this server reads may appear once; a second copy is
//!   refused rather than one of them picked.
//! - Parameters it doesn't read are ignored, as Newznab clients send many
//!   (`extended`, `o`, `attrs`, `year`, …).
//! - An ID parameter (`imdbid`, `tvdbid`, …) with a value is refused
//!   (`203`). Ignoring it would answer an ID search with whatever else
//!   matched. ENR-11 adds them.
//!
//! **Bounds:**
//! - `q`: at most [`MAX_Q_BYTES`] bytes and [`MAX_WORDS`] words, with
//!   control characters removed.
//! - `cat`: at most [`MAX_CATS`] ids.
//! - `season`, `ep`: four digits, or a daily show's `MM/DD`.
//! - `limit`: 1 or more, capped at [`MAX_LIMIT`].
//! - `offset`: nine digits at most; past the search's window it's an
//!   empty page (see the handler).
//!
//! Any other malformed value is a `201`. Error descriptions name the
//! parameter and never echo its value.
//!
//! **The search** is built as an AST, never parsed as query language, so
//! a `q` such as `verified:no` or `-flag:hidden` is just words. The
//! clauses:
//! - each word of `q`, on name and paths;
//! - `season` and `ep` as ranges (a daily `MM/DD` becomes the phrase
//!   `YYYY MM DD`, the year from `season`);
//! - the categories, or the mode's own when there's no `cat`;
//! - never a private or private-source torrent.
//!
//! Search's own [`base_query`](crate::search::base_query) adds the
//! hidden, unverified and Adult defaults on top.

use lodestar_search::Node;
use lodestar_search::query::{NumField, TextField};

use super::categories;

/// Query string bytes read, at most.
pub const MAX_QUERY_STRING: usize = 4096;
/// Parameters read, at most, known or not.
pub const MAX_PARAMS: usize = 64;
/// `q` bytes, after decoding.
pub const MAX_Q_BYTES: usize = 256;
/// Words of `q`.
pub const MAX_WORDS: usize = 32;
/// Category ids in `cat`.
pub const MAX_CATS: usize = 32;
/// Items per page, at most (plan: `limit` ≤ 100), and by default.
pub const MAX_LIMIT: usize = 100;

/// What `t=` asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Function {
    Caps,
    Search(Mode),
}

/// A search function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `t=search`: everything.
    Search,
    /// `t=tvsearch`.
    Tv,
    /// `t=movie`.
    Movie,
    /// `t=music`.
    Music,
    /// `t=book`.
    Book,
}

impl Mode {
    /// The categories a search of this mode is limited to without `cat`.
    fn default_cats(self) -> &'static [u32] {
        match self {
            Mode::Search => &[],
            Mode::Tv => &[categories::TV],
            Mode::Movie => &[categories::MOVIES],
            Mode::Music => &[categories::AUDIO],
            Mode::Book => &[categories::BOOKS],
        }
    }
}

/// `ep=`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Episode {
    Number(u32),
    /// A daily show's `MM/DD`; `season` is its year.
    Daily {
        month: u32,
        day: u32,
    },
}

/// A parsed request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub function: Function,
    /// `q`'s words, controls removed.
    pub words: Vec<String>,
    /// `cat`'s ids, in order, without repeats; `None` without `cat`.
    pub cats: Option<Vec<u32>>,
    pub season: Option<u32>,
    pub ep: Option<Episode>,
    /// 1 to [`MAX_LIMIT`].
    pub limit: usize,
    pub offset: usize,
}

/// A refused request: the Newznab error code and a fixed description.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refused {
    pub code: u16,
    pub description: &'static str,
}

const fn refused(code: u16, description: &'static str) -> Refused {
    Refused { code, description }
}

/// Newznab's "Missing parameter".
pub const MISSING: u16 = 200;
/// Newznab's "Incorrect parameter".
pub const INCORRECT: u16 = 201;
/// Newznab's "No such function".
pub const NO_FUNCTION: u16 = 202;
/// Newznab's "Function not available".
pub const NOT_AVAILABLE: u16 = 203;

/// The ID parameters Newznab clients may send, none of them supported
/// until ENR-11.
const ID_PARAMS: &[&str] = &[
    "imdbid", "tmdbid", "tvdbid", "rid", "tvmazeid", "traktid", "doubanid", "tvrageid",
];

/// Parses a raw query string (the part after `?`, or `None`).
pub fn parse(query: Option<&str>) -> Result<Request, Refused> {
    let query = query.unwrap_or("");
    if query.len() > MAX_QUERY_STRING {
        return Err(refused(
            INCORRECT,
            "Incorrect parameter: the query is too long",
        ));
    }
    let mut t = None;
    let mut q = None;
    let mut cat = None;
    let mut season = None;
    let mut ep = None;
    let mut limit = None;
    let mut offset = None;
    let mut n = 0usize;
    for (k, v) in form_urlencoded::parse(query.as_bytes()) {
        n += 1;
        if n > MAX_PARAMS {
            return Err(refused(
                INCORRECT,
                "Incorrect parameter: too many parameters",
            ));
        }
        let slot = match &*k {
            "t" => &mut t,
            "q" => &mut q,
            "cat" => &mut cat,
            "season" => &mut season,
            "ep" => &mut ep,
            "limit" => &mut limit,
            "offset" => &mut offset,
            id if ID_PARAMS.contains(&id) => {
                if v.trim().is_empty() {
                    continue;
                }
                return Err(refused(
                    NOT_AVAILABLE,
                    "Function not available: searching by ID isn't supported yet",
                ));
            }
            // `apikey` (read by the guard), `extended`, `o`, `attrs` and
            // whatever else a client sends.
            _ => continue,
        };
        if slot.is_some() {
            return Err(refused(
                INCORRECT,
                "Incorrect parameter: a parameter is repeated",
            ));
        }
        *slot = Some(v.into_owned());
    }
    let function = match t.as_deref() {
        None | Some("") => return Err(refused(MISSING, "Missing parameter: t")),
        Some("caps") => Function::Caps,
        Some("search") => Function::Search(Mode::Search),
        Some("tvsearch") => Function::Search(Mode::Tv),
        Some("movie") => Function::Search(Mode::Movie),
        Some("music") => Function::Search(Mode::Music),
        Some("book") => Function::Search(Mode::Book),
        Some(_) => return Err(refused(NO_FUNCTION, "No such function")),
    };
    let words = match q {
        Some(q) => words(&q)?,
        None => Vec::new(),
    };
    let cats = cat.as_deref().map(cats).transpose()?.flatten();
    let season = season
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|s| number(s, 4).ok_or(refused(INCORRECT, "Incorrect parameter: season")))
        .transpose()?;
    let ep = ep
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|e| episode(e, season))
        .transpose()?;
    let limit = match limit.as_deref().filter(|s| !s.is_empty()) {
        None => MAX_LIMIT,
        Some(l) => match number(l, 9) {
            Some(0) | None => return Err(refused(INCORRECT, "Incorrect parameter: limit")),
            Some(l) => (l as usize).min(MAX_LIMIT),
        },
    };
    let offset = match offset.as_deref().filter(|s| !s.is_empty()) {
        None => 0,
        Some(o) => number(o, 9).ok_or(refused(INCORRECT, "Incorrect parameter: offset"))? as usize,
    };
    Ok(Request {
        function,
        words,
        cats,
        season,
        ep,
        limit,
        offset,
    })
}

/// Up to `digits` ASCII digits, nothing else.
fn number(s: &str, digits: usize) -> Option<u32> {
    if s.is_empty() || s.len() > digits || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// `q`'s words: controls removed, split on whitespace.
fn words(q: &str) -> Result<Vec<String>, Refused> {
    if q.len() > MAX_Q_BYTES {
        return Err(refused(INCORRECT, "Incorrect parameter: q is too long"));
    }
    let clean: String = q
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let words: Vec<String> = clean.split_whitespace().map(str::to_owned).collect();
    if words.len() > MAX_WORDS {
        return Err(refused(
            INCORRECT,
            "Incorrect parameter: q has too many words",
        ));
    }
    Ok(words)
}

/// `cat`'s ids; `None` when it names none (`cat=` or `cat=,`).
fn cats(cat: &str) -> Result<Option<Vec<u32>>, Refused> {
    let mut out: Vec<u32> = Vec::new();
    for part in cat.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if out.len() == MAX_CATS {
            return Err(refused(
                INCORRECT,
                "Incorrect parameter: cat has too many categories",
            ));
        }
        let id = number(part, 6).ok_or(refused(INCORRECT, "Incorrect parameter: cat"))?;
        if !out.contains(&id) {
            out.push(id);
        }
    }
    Ok((!out.is_empty()).then_some(out))
}

fn episode(e: &str, season: Option<u32>) -> Result<Episode, Refused> {
    let bad = refused(INCORRECT, "Incorrect parameter: ep");
    if let Some(n) = number(e, 4) {
        return Ok(Episode::Number(n));
    }
    // A daily show: `season` is the year, `ep` the month and day.
    let (m, d) = e.split_once('/').ok_or(bad)?;
    let (month, day) = (number(m, 2).ok_or(bad)?, number(d, 2).ok_or(bad)?);
    let year_ok = season.is_some_and(|y| (1900..=2999).contains(&y));
    if !year_ok || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return Err(bad);
    }
    Ok(Episode::Daily { month, day })
}

impl Request {
    /// The categories this search is limited to: `cat`'s, or the mode's.
    fn categories(&self, mode: Mode) -> Vec<u32> {
        match &self.cats {
            Some(c) => c.clone(),
            None => mode.default_cats().to_vec(),
        }
    }

    /// The search's AST, before search's own defaults; `None` when it
    /// can match nothing (only categories this server doesn't list).
    pub fn ast(&self, mode: Mode) -> Option<Node> {
        let mut of: Vec<Node> = self
            .words
            .iter()
            .map(|w| Node::Word {
                field: TextField::Any,
                word: w.clone(),
            })
            .collect();
        match (self.season, self.ep) {
            (Some(year), Some(Episode::Daily { month, day })) => of.push(Node::Phrase {
                field: TextField::Any,
                text: format!("{year:04} {month:02} {day:02}"),
            }),
            (season, ep) => {
                let range = |field, n: u32| Node::Range {
                    field,
                    lo: Some(u64::from(n)),
                    hi: Some(u64::from(n)),
                };
                of.extend(season.map(|s| range(NumField::Season, s)));
                if let Some(Episode::Number(e)) = ep {
                    of.push(range(NumField::Episode, e));
                }
            }
        }
        let cats = self.categories(mode);
        if !cats.is_empty() {
            let known: Vec<Node> = cats.iter().filter_map(|c| categories::clause(*c)).collect();
            match known.len() {
                0 => return None,
                1 => of.extend(known),
                _ => of.push(Node::Or { of: known }),
            }
        }
        // Never served (§18.7): the stored flag is checked again per row.
        for flag in ["private", "private_source"] {
            of.push(Node::Flag {
                name: flag.into(),
                value: false,
            });
        }
        Some(Node::And { of })
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn ok(q: &str) -> Request {
        parse(Some(q)).unwrap()
    }

    fn code(q: &str) -> u16 {
        parse(Some(q)).unwrap_err().code
    }

    #[test]
    fn functions() {
        assert_eq!(ok("t=caps").function, Function::Caps);
        for (t, m) in [
            ("search", Mode::Search),
            ("tvsearch", Mode::Tv),
            ("movie", Mode::Movie),
            ("music", Mode::Music),
            ("book", Mode::Book),
        ] {
            assert_eq!(ok(&format!("t={t}")).function, Function::Search(m));
        }
        assert_eq!(parse(None).unwrap_err().code, MISSING);
        assert_eq!(code("t="), MISSING);
        assert_eq!(code("q=x"), MISSING);
        assert_eq!(code("t=details"), NO_FUNCTION);
        assert_eq!(code("t=CAPS"), NO_FUNCTION);
    }

    #[test]
    fn defaults() {
        let r = ok("t=search");
        assert_eq!(
            (r.words.len(), r.cats, r.season, r.ep, r.limit, r.offset),
            (0, None, None, None, MAX_LIMIT, 0)
        );
    }

    #[test]
    fn limit_is_capped_and_offset_bounded() {
        assert_eq!(ok("t=search&limit=5").limit, 5);
        assert_eq!(ok("t=search&limit=100000").limit, MAX_LIMIT);
        assert_eq!(ok("t=search&limit=").limit, MAX_LIMIT);
        for bad in ["0", "-1", "1e3", " 5", "0x10", "9999999999"] {
            assert_eq!(code(&format!("t=search&limit={bad}")), INCORRECT, "{bad}");
        }
        assert_eq!(ok("t=search&offset=999999999").offset, 999_999_999);
        for bad in ["-1", "1000000000", "1.5"] {
            assert_eq!(code(&format!("t=search&offset={bad}")), INCORRECT, "{bad}");
        }
    }

    #[test]
    fn q_is_words_with_controls_removed() {
        assert_eq!(
            ok("t=search&q=The%20Show%09S01E02%00%1b").words,
            ["The", "Show", "S01E02"]
        );
        assert_eq!(
            ok("t=search&q=verified:no+-flag:hidden").words,
            ["verified:no", "-flag:hidden"]
        );
        // Invalid UTF-8 is replaced, not refused.
        assert_eq!(ok("t=search&q=%ff%fe").words, ["\u{FFFD}\u{FFFD}"]);
        let long = "a".repeat(MAX_Q_BYTES + 1);
        assert_eq!(code(&format!("t=search&q={long}")), INCORRECT);
        let many = vec!["w"; MAX_WORDS + 1].join("+");
        assert_eq!(code(&format!("t=search&q={many}")), INCORRECT);
        assert_eq!(
            ok(&format!("t=search&q={}", vec!["w"; MAX_WORDS].join("+")))
                .words
                .len(),
            MAX_WORDS
        );
    }

    #[test]
    fn categories_parse() {
        assert_eq!(
            ok("t=search&cat=5030,5040,,5030").cats,
            Some(vec![5030, 5040])
        );
        assert_eq!(ok("t=search&cat=").cats, None);
        assert_eq!(ok("t=search&cat=,").cats, None);
        assert_eq!(ok("t=search&cat=999999").cats, Some(vec![999_999]));
        for bad in ["abc", "5030;5040", "1000000", "-5000", "5000.5"] {
            assert_eq!(code(&format!("t=search&cat={bad}")), INCORRECT, "{bad}");
        }
        let many: Vec<String> = (0..=MAX_CATS).map(|i| (1000 + i).to_string()).collect();
        assert_eq!(code(&format!("t=search&cat={}", many.join(","))), INCORRECT);
    }

    #[test]
    fn seasons_and_episodes() {
        let r = ok("t=tvsearch&season=1&ep=2");
        assert_eq!((r.season, r.ep), (Some(1), Some(Episode::Number(2))));
        let r = ok("t=tvsearch&season=2024&ep=01/15");
        assert_eq!(r.ep, Some(Episode::Daily { month: 1, day: 15 }));
        assert_eq!(ok("t=tvsearch&season=&ep=").season, None);
        for bad in [
            "season=x",
            "season=10000",
            "season=-1",
            "season=1&ep=x",
            "season=1&ep=01/15",
            "ep=01/15",
            "season=2024&ep=13/01",
            "season=2024&ep=01/32",
            "season=2024&ep=1/2/3",
            "season=2024&ep=00/10",
        ] {
            assert_eq!(code(&format!("t=tvsearch&{bad}")), INCORRECT, "{bad}");
        }
    }

    #[test]
    fn repeats_ids_and_size_limits() {
        assert_eq!(code("t=search&q=a&q=b"), INCORRECT);
        assert_eq!(code("t=search&t=caps"), INCORRECT);
        assert_eq!(code("t=movie&imdbid=tt0111161"), NOT_AVAILABLE);
        assert_eq!(code("t=tvsearch&tvdbid=1"), NOT_AVAILABLE);
        // An empty ID, as some clients send, is no ID.
        assert_eq!(ok("t=movie&imdbid=&q=x").words, ["x"]);
        // Unknown parameters are ignored.
        assert_eq!(
            ok("t=search&extended=1&o=xml&attrs=a&year=2020&apikey=k").limit,
            MAX_LIMIT
        );
        let params = vec!["x=1"; MAX_PARAMS].join("&");
        assert_eq!(code(&format!("t=search&{params}")), INCORRECT);
        let long = format!("t=search&pad={}", "a".repeat(MAX_QUERY_STRING));
        assert_eq!(code(&long), INCORRECT);
    }

    #[test]
    fn descriptions_never_echo_values() {
        for q in [
            "t=<script>",
            "t=search&limit=<b>",
            "t=search&cat=<b>",
            "t=tvsearch&season=<b>",
            "t=tvsearch&season=1&ep=<b>",
            "t=movie&imdbid=<b>",
        ] {
            let e = parse(Some(q)).unwrap_err();
            assert!(!e.description.contains('<'), "{q}: {}", e.description);
        }
    }

    #[test]
    fn the_ast_is_words_ranges_categories_and_privacy() {
        let r = ok("t=tvsearch&q=show&season=1&ep=2");
        let ast = r.ast(Mode::Tv).unwrap();
        let Node::And { of } = &ast else {
            panic!("{ast:?}")
        };
        assert_eq!(
            of[0],
            Node::Word {
                field: TextField::Any,
                word: "show".into()
            }
        );
        assert!(of.contains(&Node::Range {
            field: NumField::Season,
            lo: Some(1),
            hi: Some(1)
        }));
        assert!(of.contains(&Node::Range {
            field: NumField::Episode,
            lo: Some(2),
            hi: Some(2)
        }));
        assert!(of.contains(&categories::clause(categories::TV).unwrap()));
        for flag in ["private", "private_source"] {
            assert!(of.contains(&Node::Flag {
                name: flag.into(),
                value: false
            }));
        }
        // An explicit cat replaces the mode's.
        let r = ok("t=tvsearch&cat=2000");
        let Some(Node::And { of }) = r.ast(Mode::Tv) else {
            panic!()
        };
        assert!(of.contains(&categories::clause(2000).unwrap()));
        assert!(!of.contains(&categories::clause(5000).unwrap()));
        // Unknown categories: nothing; mixed: the known ones.
        assert_eq!(ok("t=search&cat=1234").ast(Mode::Search), None);
        let Some(Node::And { of }) = ok("t=search&cat=1234,2000").ast(Mode::Search) else {
            panic!()
        };
        assert!(of.contains(&categories::clause(2000).unwrap()));
        let Some(Node::And { of }) = ok("t=search&cat=2000,5000").ast(Mode::Search) else {
            panic!()
        };
        assert!(
            of.iter()
                .any(|n| matches!(n, Node::Or { of } if of.len() == 2))
        );
        // A daily episode is a phrase of the date.
        let Some(Node::And { of }) = ok("t=tvsearch&season=2024&ep=1/5").ast(Mode::Tv) else {
            panic!()
        };
        assert!(of.contains(&Node::Phrase {
            field: TextField::Any,
            text: "2024 01 05".into()
        }));
        assert!(!of.iter().any(|n| matches!(n, Node::Range { .. })));
    }

    proptest! {
        /// No query string panics, and every refusal is one of the four
        /// codes with a fixed description.
        #[test]
        fn any_query_string_parses_or_is_refused(s in ".{0,600}") {
            match parse(Some(&s)) {
                Ok(r) => {
                    prop_assert!((1..=MAX_LIMIT).contains(&r.limit));
                    prop_assert!(r.words.len() <= MAX_WORDS);
                    prop_assert!(r.words.iter().all(|w| !w.chars().any(char::is_control)));
                    prop_assert!(r.cats.as_ref().is_none_or(|c| c.len() <= MAX_CATS));
                }
                Err(e) => prop_assert!([MISSING, INCORRECT, NO_FUNCTION, NOT_AVAILABLE].contains(&e.code)),
            }
        }

        #[test]
        fn structured_strings_parse_or_are_refused(
            t in prop::sample::select(vec!["caps", "search", "tvsearch", "movie", "music", "book", "x"]),
            q in ".{0,80}",
            cat in "[0-9,]{0,40}",
            season in "[0-9]{0,5}",
            ep in "[0-9/]{0,6}",
            limit in "[0-9]{0,4}",
        ) {
            let enc = |s: &str| form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>();
            let s = format!("t={t}&q={}&cat={cat}&season={season}&ep={}&limit={limit}", enc(&q), enc(&ep));
            if let Ok(r) = parse(Some(&s)) && let Function::Search(m) = r.function {
                let _ = r.ast(m);
            }
        }
    }
}
