//! Splitting a name into words while keeping what separated them.
//!
//! The parser needs more than the words: whether `WEB` and `DL` were
//! joined by `-`, whether a number follows ` - ` (an anime episode), and
//! which words sit in brackets. So every word carries its byte range, the
//! separator text before it and the bracket it's in.

/// Words past this are ignored. A real name has 5–30.
pub(crate) const MAX_TOKENS: usize = 96;

#[derive(Debug)]
pub(crate) struct Tok<'a> {
    pub text: &'a str,
    /// Lowercase form for lexicon lookups.
    pub low: String,
    pub from: usize,
    pub to: usize,
    /// Everything between the previous word (or the start) and this one.
    pub sep: &'a str,
    /// The outermost bracket group this word is in.
    pub bracket: Option<usize>,
}

impl Tok<'_> {
    /// Joined to the previous word by a bare hyphen: `WEB-DL`, `x264-GROUP`.
    pub fn tight_dash(&self) -> bool {
        self.sep == "-"
    }

    /// Preceded by a hyphen with spaces or underscores around it:
    /// `Title - 01`, `x264 - YIFY`, `Title_-_01`.
    pub fn spaced_dash(&self) -> bool {
        self.sep.contains('-')
            && self.sep.chars().any(|c| c.is_whitespace() || c == '_')
            && !self.sep.chars().any(is_bracket)
    }

    /// Joined to the previous word by a single dot (`5.1`, `H.264`) or, in
    /// space-separated names, a single space (`DDP5 1`).
    pub fn dot_or_space(&self) -> bool {
        self.sep == "." || self.sep == " "
    }

    pub fn is_digits(&self) -> bool {
        !self.low.is_empty() && self.low.bytes().all(|b| b.is_ascii_digit())
    }

    pub fn number(&self) -> Option<u32> {
        if self.is_digits() && self.low.len() <= 6 {
            self.low.parse().ok()
        } else {
            None
        }
    }

    /// One letter or digit: a piece of an acronym like `S.H.I.E.L.D.`
    pub fn is_single_char(&self) -> bool {
        let mut chars = self.text.chars();
        matches!((chars.next(), chars.next()), (Some(c), None) if c.is_alphanumeric())
    }
}

/// A bracket group: `[…]`, `(…)`, `{…}` and the CJK forms. Words inside
/// nested brackets belong to the outermost group.
#[derive(Debug)]
pub(crate) struct Bracket {
    /// The first word inside, and one past the last.
    pub first: usize,
    pub end: usize,
    /// Byte range of the text between the brackets.
    pub inner_from: usize,
    pub inner_to: usize,
    /// Nothing but whitespace comes before the opening bracket.
    pub at_start: bool,
}

#[derive(Debug)]
pub(crate) struct Tokens<'a> {
    pub text: &'a str,
    pub toks: Vec<Tok<'a>>,
    pub brackets: Vec<Bracket>,
}

fn is_open(c: char) -> bool {
    matches!(c, '(' | '[' | '{' | '【' | '「' | '『' | '（' | '〔')
}

fn is_close(c: char) -> bool {
    matches!(c, ')' | ']' | '}' | '】' | '」' | '』' | '）' | '〕')
}

pub(crate) fn is_bracket(c: char) -> bool {
    is_open(c) || is_close(c)
}

/// Characters that belong to words. Everything else separates them.
/// `+` is a separator except in `DD+` and `HDR10+`, where it's part of
/// the marker.
fn is_word_char(c: char, word_so_far: &str) -> bool {
    if c.is_alphanumeric() || matches!(c, '\'' | '’' | '!' | '?' | ';') {
        return true;
    }
    c == '+'
        && ["dd", "hdr10", "hdr", "ddp"]
            .iter()
            .any(|w| word_so_far.eq_ignore_ascii_case(w))
}

pub(crate) fn tokenize(text: &str) -> Tokens<'_> {
    let mut toks: Vec<Tok<'_>> = Vec::new();
    let mut brackets: Vec<Bracket> = Vec::new();
    let mut depth = 0usize;
    let mut current: Option<usize> = None;
    let mut start: Option<usize> = None;
    let mut prev_end = 0usize;

    for (i, c) in text.char_indices() {
        let so_far = start.map_or("", |s| &text[s..i]);
        if is_word_char(c, so_far) {
            start.get_or_insert(i);
            continue;
        }
        if let Some(s) = start.take() {
            push_word(text, &mut toks, s, i, &mut prev_end, current);
        }
        if is_open(c) {
            if depth == 0 {
                current = Some(brackets.len());
                brackets.push(Bracket {
                    first: toks.len(),
                    end: toks.len(),
                    inner_from: i + c.len_utf8(),
                    inner_to: text.len(),
                    at_start: text[..i].trim().is_empty(),
                });
            }
            depth += 1;
        } else if is_close(c) && depth > 0 {
            depth -= 1;
            if depth == 0 {
                if let Some(b) = current.take() {
                    brackets[b].end = toks.len();
                    brackets[b].inner_to = i;
                }
            }
        }
    }
    if let Some(s) = start {
        push_word(text, &mut toks, s, text.len(), &mut prev_end, current);
    }
    if let Some(b) = current {
        brackets[b].end = toks.len();
    }
    Tokens {
        text,
        toks,
        brackets,
    }
}

fn push_word<'a>(
    text: &'a str,
    toks: &mut Vec<Tok<'a>>,
    from: usize,
    to: usize,
    prev_end: &mut usize,
    bracket: Option<usize>,
) {
    let word = &text[from..to];
    // Runs of symbols (`!!`, `'`) aren't words; they stay part of the
    // separator before the next word.
    if toks.len() >= MAX_TOKENS || !word.chars().any(char::is_alphanumeric) {
        return;
    }
    let low = if word.is_ascii() {
        word.to_ascii_lowercase()
    } else {
        word.to_lowercase()
    };
    toks.push(Tok {
        text: word,
        low,
        from,
        to,
        sep: &text[*prev_end..from],
        bracket,
    });
    *prev_end = to;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<(&str, &str)> {
        tokenize(text)
            .toks
            .iter()
            .map(|t| (t.sep, t.text))
            .collect()
    }

    #[test]
    fn keeps_separators() {
        assert_eq!(
            words("Show.Name.S01E02.WEB-DL.x264-GRP"),
            [
                ("", "Show"),
                (".", "Name"),
                (".", "S01E02"),
                (".", "WEB"),
                ("-", "DL"),
                (".", "x264"),
                ("-", "GRP")
            ]
        );
    }

    #[test]
    fn plus_only_inside_markers() {
        assert_eq!(
            words("DD+5.1 HDR10+ Notepad++ A+B"),
            [
                ("", "DD+5"),
                (".", "1"),
                (" ", "HDR10+"),
                (" ", "Notepad"),
                ("++ ", "A"),
                ("+", "B")
            ]
        );
    }

    #[test]
    fn brackets_group_words() {
        let t = tokenize("[Group] Title (2019) [1080p [x]] (open");
        assert_eq!(t.brackets.len(), 4);
        let b = &t.brackets[0];
        assert!(b.at_start);
        assert_eq!((b.first, b.end), (0, 1));
        assert_eq!(&t.text[b.inner_from..b.inner_to], "Group");
        // Nested words belong to the outer group.
        assert_eq!(t.toks[4].text, "x");
        assert_eq!(t.toks[4].bracket, Some(2));
        // An unclosed bracket runs to the end.
        assert_eq!(t.brackets[3].end, t.toks.len());
        assert_eq!(t.toks[1].bracket, None);
    }

    #[test]
    fn symbol_runs_join_the_separator() {
        assert_eq!(words("Haikyuu!!_-_01"), [("", "Haikyuu!!"), ("_-_", "01")]);
        assert_eq!(words("A & B"), [("", "A"), (" & ", "B")]);
        assert!(tokenize("Title - 01").toks[1].spaced_dash());
        assert!(!tokenize("Title (- 01").toks[1].spaced_dash());
    }

    #[test]
    fn bounded() {
        let long = "a ".repeat(500);
        assert_eq!(tokenize(&long).toks.len(), MAX_TOKENS);
    }
}
