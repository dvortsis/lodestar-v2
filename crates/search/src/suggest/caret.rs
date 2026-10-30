//! The token being typed (B7): query text is split the way the query
//! lexer splits it (`query.rs`), but leniently, since a query being
//! typed is usually unfinished: an unclosed quote runs to the end, and
//! brackets need not balance.
//!
//! Spans are byte ranges here; [`chars`] turns them into the character
//! ranges the API returns.

use crate::query::{self, FieldKind};

/// A piece of query text, as the lexer splits it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Raw {
    pub start: usize,
    pub end: usize,
    pub kind: RawKind,
    /// Brackets open before it, `file:(` ones included.
    pub depth: usize,
    /// Inside a `file:(…)` group.
    pub in_file: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RawKind {
    Open,
    Close,
    /// `|`.
    Bar,
    /// `"…"`; `closed` is false for one that runs to the end.
    Quoted {
        closed: bool,
    },
    /// Anything else up to whitespace, a bracket, a quote or `|`:
    /// words, `field:value`, `-word`, `OR`, `NEAR/3`.
    Word,
}

/// Splits `q` as the lexer would.
pub(crate) fn split(q: &str) -> Vec<Raw> {
    let b = q.as_bytes();
    let mut out: Vec<Raw> = Vec::new();
    // For each open bracket: whether it's a `file:(`.
    let mut stack: Vec<bool> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let depth = stack.len();
        let in_file = stack.iter().any(|f| *f);
        let start = i;
        let kind = match c {
            b'(' => {
                // `file:(`: the word `file:` runs right up to the bracket.
                let file = out.last().is_some_and(|t| {
                    t.kind == RawKind::Word
                        && t.end == i
                        && q[t.start..t.end].trim_start_matches(['-', '!']).eq("file:")
                });
                stack.push(file);
                i += 1;
                RawKind::Open
            }
            b')' => {
                stack.pop();
                i += 1;
                RawKind::Close
            }
            b'|' => {
                i += 1;
                RawKind::Bar
            }
            b'"' => match q[i + 1..].find('"') {
                Some(p) => {
                    i += p + 2;
                    RawKind::Quoted { closed: true }
                }
                None => {
                    i = b.len();
                    RawKind::Quoted { closed: false }
                }
            },
            _ => {
                while i < b.len()
                    && !b[i].is_ascii_whitespace()
                    && !matches!(b[i], b'(' | b')' | b'"' | b'|')
                {
                    i += 1;
                }
                RawKind::Word
            }
        };
        out.push(Raw {
            start,
            end: i,
            kind,
            depth: if kind == RawKind::Close {
                stack.len()
            } else {
                depth
            },
            in_file,
        });
    }
    out
}

/// The character offset of byte `at` in `q` (a char boundary).
pub(crate) fn chars(q: &str, at: usize) -> usize {
    q[..at].chars().count()
}

/// The byte offset of character `pos` in `q`, or `None` past the end.
pub(crate) fn byte_at(q: &str, pos: usize) -> Option<usize> {
    if pos == 0 {
        return Some(0);
    }
    match q.char_indices().nth(pos) {
        Some((b, _)) => Some(b),
        None => (q.chars().count() == pos).then_some(q.len()),
    }
}

/// What the caret is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Context {
    /// Between tokens, or in an operator or bracket: nothing to complete.
    None,
    /// A bare word (`replace` leaves a leading `-`/`!` out).
    Word {
        replace: (usize, usize),
        /// What's typed from the word's start to the caret.
        partial: String,
        /// The whole word.
        word: String,
    },
    /// `field:value`, with `partial` the value up to the caret (or the
    /// name, while the caret is still in it: `name_only`).
    Field {
        replace: (usize, usize),
        name: String,
        kind: FieldKind,
        partial: String,
        /// The caret is in the name, before the `:`.
        name_only: bool,
    },
    /// A quoted phrase, on its own or after `title:`.
    /// `partial` is the text from the opening quote to the caret.
    Phrase {
        replace: (usize, usize),
        partial: String,
    },
}

/// The token at byte `caret` and what it is, plus where it sits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct At {
    pub context: Context,
    /// The index in [`split`]'s output of the token, if any.
    pub token: Option<usize>,
    pub in_file: bool,
}

/// What's being typed at byte offset `caret` (a char boundary).
pub(crate) fn at(q: &str, toks: &[Raw], caret: usize) -> At {
    // The token the caret is in or just after.
    let Some(i) = toks.iter().position(|t| t.start < caret && caret <= t.end) else {
        return At {
            context: Context::None,
            token: None,
            in_file: false,
        };
    };
    let t = toks[i];
    let none = |in_file| At {
        context: Context::None,
        token: Some(i),
        in_file,
    };
    let context = match t.kind {
        RawKind::Open | RawKind::Close | RawKind::Bar => return none(t.in_file),
        RawKind::Quoted { closed } => {
            if closed && caret == t.end {
                // After the closing quote: the phrase is finished.
                return none(t.in_file);
            }
            let mut start = t.start;
            // A word touching the quote is part of it: `title:"…` (or
            // `-title:"…`, the negation left out) is a title phrase; any
            // other (`name:"…`, `-"…`, `ab"…`) isn't one to replace.
            if let Some(prev) = i.checked_sub(1).map(|p| toks[p])
                && prev.end == t.start
            {
                let word = &q[prev.start..prev.end];
                let neg = negation(word);
                if prev.kind != RawKind::Word || &word[neg..] != "title:" {
                    return none(t.in_file);
                }
                start = prev.start + neg;
            }
            Context::Phrase {
                replace: (start, t.end),
                partial: q[t.start + 1..caret].to_string(),
            }
        }
        RawKind::Word => word(q, t, caret),
    };
    At {
        context,
        token: Some(i),
        in_file: t.in_file,
    }
}

/// How many bytes of `w` are a negation: a leading `-` or `!` with more
/// after it, as the lexer reads one.
pub(crate) fn negation(w: &str) -> usize {
    usize::from(w.len() > 1 && (w.starts_with('-') || w.starts_with('!')))
}

fn word(q: &str, t: Raw, caret: usize) -> Context {
    let text = &q[t.start..t.end];
    let neg = negation(text);
    let start = t.start + neg;
    if caret <= start {
        // On the `-` itself, or just after it: nothing typed yet.
        return Context::None;
    }
    let body = &q[start..t.end];
    if let Some((name, _)) = body.split_once(':')
        && query::is_field_name(name)
        && let Some(kind) = query::field(name)
    {
        let colon = start + name.len();
        return Context::Field {
            replace: (start, t.end),
            name: name.to_string(),
            kind,
            partial: if caret > colon {
                q[colon + 1..caret].to_string()
            } else {
                q[start..caret].to_string()
            },
            name_only: caret <= colon,
        };
    }
    Context::Word {
        replace: (start, t.end),
        partial: q[start..caret].to_string(),
        word: body.to_string(),
    }
}

/// Whether a word token is plain text that could be part of a title: no
/// field, operator, negation, prefix or fuzzy syntax.
pub(crate) fn is_title_word(w: &str) -> bool {
    !w.is_empty()
        && !matches!(w, "OR" | "AND" | "NEAR")
        && !w.starts_with("NEAR/")
        && negation(w) == 0
        && !w.starts_with(['~', '/'])
        && !w.contains('*')
        && !w
            .split_once(':')
            .is_some_and(|(n, _)| query::is_field_name(n) && query::field(n).is_some())
}

/// The runs of plain words that end with the caret's word, longest
/// first (at most `max` words): the title being typed may be any of
/// them, as the byte where each starts.
pub(crate) fn title_runs(q: &str, toks: &[Raw], i: usize, max: usize) -> Vec<usize> {
    let t = toks[i];
    if t.kind != RawKind::Word || !is_title_word(&q[t.start..t.end]) {
        return Vec::new();
    }
    let mut first = i;
    while first > 0 && i - first + 1 < max {
        let p = toks[first - 1];
        let gap = &q[p.end..toks[first].start];
        if p.kind != RawKind::Word
            || p.depth != t.depth
            || !gap.bytes().all(|b| b.is_ascii_whitespace())
            || !is_title_word(&q[p.start..p.end])
        {
            break;
        }
        first -= 1;
    }
    (first..=i).map(|j| toks[j].start).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The context with `|` marking the caret.
    fn ctx(marked: &str) -> (String, At) {
        let caret = marked.find('|').expect("a caret");
        let q = marked.replacen('|', "", 1);
        let toks = split(&q);
        let a = at(&q, &toks, caret);
        (q, a)
    }

    fn replaced(q: &str, r: (usize, usize)) -> &str {
        &q[r.0..r.1]
    }

    #[test]
    fn words_fields_and_negation() {
        let (q, a) = ctx("dark kni|");
        let Context::Word {
            replace,
            partial,
            word,
        } = a.context
        else {
            panic!("{a:?}")
        };
        assert_eq!((replaced(&q, replace), partial.as_str()), ("kni", "kni"));
        assert_eq!(word, "kni");
        // In the middle of a word: the whole word is replaced.
        let (q, a) = ctx("dar|kness");
        let Context::Word {
            replace, partial, ..
        } = a.context
        else {
            panic!()
        };
        assert_eq!(
            (replaced(&q, replace), partial.as_str()),
            ("darkness", "dar")
        );
        // A negation stays out of the replaced range.
        let (q, a) = ctx("-res:21|");
        let Context::Field {
            replace,
            name,
            partial,
            name_only,
            ..
        } = a.context
        else {
            panic!()
        };
        assert_eq!(replaced(&q, replace), "res:21");
        assert_eq!(
            (name.as_str(), partial.as_str(), name_only),
            ("res", "21", false)
        );
        let (_, a) = ctx("!x2|65");
        assert!(matches!(a.context, Context::Word { ref partial, .. } if partial == "x2"));
        // On the `-` itself, or between words: nothing.
        assert_eq!(ctx("-|cam").1.context, Context::None);
        assert_eq!(ctx("dark |").1.context, Context::None);
        assert_eq!(ctx("|").1.context, Context::None);
        // In the field name.
        let (_, a) = ctx("re|s:1080p");
        assert!(
            matches!(a.context, Context::Field { name_only: true, ref partial, .. } if partial == "re")
        );
        // `Re:Zero` isn't a field.
        assert!(matches!(ctx("Re:Ze|").1.context, Context::Word { .. }));
        // Operators and brackets aren't completed.
        assert_eq!(ctx("a |||").1.context, Context::None);
        assert_eq!(ctx("(a)|").1.context, Context::None);
    }

    #[test]
    fn quotes_and_file_groups() {
        let (q, a) = ctx("x \"dark kn|");
        let Context::Phrase { replace, partial } = a.context else {
            panic!()
        };
        assert_eq!(
            (replaced(&q, replace), partial.as_str()),
            ("\"dark kn", "dark kn")
        );
        let (q, a) = ctx("title:\"dark| knight\" 4k");
        let Context::Phrase { replace, partial } = a.context else {
            panic!()
        };
        assert_eq!(replaced(&q, replace), "title:\"dark knight\"");
        assert_eq!(partial, "dark");
        // A negated title phrase keeps its negation; another field or
        // word touching the quote isn't a title.
        let (q, a) = ctx("-title:\"dark kn|");
        let Context::Phrase { replace, .. } = a.context else {
            panic!()
        };
        assert_eq!(replaced(&q, replace), "title:\"dark kn");
        for marked in [
            "name:\"dark kn|",
            "file:\"dark kn|",
            "ab\"dark kn|",
            "-\"dark kn|",
        ] {
            assert_eq!(ctx(marked).1.context, Context::None, "{marked}");
        }
        // After a closed quote: done.
        assert_eq!(ctx("\"dark knight\"|").1.context, Context::None);
        // A quote inside a word ends it, as the lexer reads it.
        let toks = split("ab\"cd\"ef");
        assert_eq!(toks.len(), 3);
        let (_, a) = ctx("file:(ext:fl|");
        assert!(a.in_file);
        assert!(matches!(a.context, Context::Field { ref name, .. } if name == "ext"));
        let (_, a) = ctx("file:(ext:flac) fl|");
        assert!(!a.in_file);
        let (_, a) = ctx("(a (b) c|");
        assert!(!a.in_file);
        // A stray `)` doesn't underflow.
        assert!(!split(")) a (").is_empty());
    }

    #[test]
    fn unicode_ranges_are_char_correct() {
        let q = "日本 Amé|li";
        let caret = q.find('|').unwrap();
        let q = q.replacen('|', "", 1);
        let toks = split(&q);
        let a = at(&q, &toks, caret);
        let Context::Word {
            replace, partial, ..
        } = a.context
        else {
            panic!()
        };
        assert_eq!(partial, "Amé");
        assert_eq!((chars(&q, replace.0), chars(&q, replace.1)), (3, 8));
        assert_eq!(byte_at(&q, 3), Some(replace.0));
        assert_eq!(byte_at(&q, 8), Some(q.len()));
        assert_eq!(byte_at(&q, 9), None);
        assert_eq!(byte_at("", 0), Some(0));
    }

    #[test]
    fn escapes_are_literal_as_in_the_lexer() {
        // The query language has no escape character: a backslash is
        // part of the word, and a quote still opens a phrase.
        let (q, a) = ctx("a\\\"b|");
        let toks = split(&q);
        assert_eq!(toks.len(), 2);
        // `a\` touches the quote, so it isn't a title phrase to replace.
        assert_eq!(a.context, Context::None);
        let (_, a) = ctx("a\\ \"b|");
        assert!(matches!(a.context, Context::Phrase { ref partial, .. } if partial == "b"));
        let (_, a) = ctx("c:\\x|");
        assert!(matches!(a.context, Context::Word { ref word, .. } if word == "c:\\x"));
    }

    #[test]
    fn title_runs_stop_at_syntax() {
        let q = "res:1080p the dark kn";
        let toks = split(q);
        let runs = title_runs(q, &toks, toks.len() - 1, 6);
        let starts: Vec<&str> = runs.iter().map(|s| &q[*s..]).collect();
        assert_eq!(starts, ["the dark kn", "dark kn", "kn"]);
        let q = "(a b) c";
        let toks = split(q);
        assert_eq!(title_runs(q, &toks, toks.len() - 1, 6).len(), 1);
        let q = "a -b c";
        let toks = split(q);
        assert_eq!(title_runs(q, &toks, toks.len() - 1, 6).len(), 1);
        let q = "a b c d";
        let toks = split(q);
        assert_eq!(title_runs(q, &toks, 3, 2).len(), 2);
        let q = "a NEAR b";
        let toks = split(q);
        assert_eq!(title_runs(q, &toks, 2, 6).len(), 1);
    }
}
