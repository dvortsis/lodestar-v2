//! The light English stemmer behind `title_stem` (architecture §8): the
//! "plurals and suffixes" tier of matching, so `knights` finds *The Dark
//! Knight* and `knight` finds *Knights of the Zodiac*.
//!
//! It's deliberately light. It only undoes inflection that doesn't
//! change a word's meaning: plurals (`-s`, `-es`, `-ies`) and the
//! possessive, which the tokenizer already splits off. Snowball (Porter2)
//! goes further and conflates unrelated titles (`organization` and
//! `organ`, `university` and `universe`, `general` and `generous`), and
//! `-ing`/`-ed` rules without a dictionary mangle short words (`king`,
//! `red`, `bed`). A stem match ranks below an exact one, but it still
//! counts, so a wrong conflation would show up in every total.
//!
//! The same function runs on indexed titles and on query words, so both
//! sides agree; only the folded, lowercase tokens of the name tokenizer
//! come in.

/// Shorter tokens aren't candidates.
const MIN_CHARS: usize = 3;
/// Shorter words keep their `s`: too many of them only look plural
/// (`bus`, `gas`, `yes`, `his`).
const MIN_STRIP_CHARS: usize = 4;

/// Words ending in `s` that aren't plurals, or whose plural is the same
/// word. Stemming one only matters when it lands on another word (`news`
/// on `new`, `does` on `doe`); the rest are listed so they stay readable.
const INVARIANT: &[&str] = &[
    "always",
    "chaos",
    "cosmos",
    "does",
    "goes",
    "lens",
    "mathematics",
    "means",
    "news",
    "perhaps",
    "physics",
    "politics",
    "series",
    "species",
    "sometimes",
    "whereas",
];

/// The stem of a folded token, or `None` when it isn't a stemming
/// candidate (too short, or not all ASCII letters). A candidate's stem
/// may equal the token: `knight` stems to itself, so that it meets
/// `knights` in the middle.
pub fn light_stem(token: &str) -> Option<String> {
    if token.len() < MIN_CHARS || !token.bytes().all(|b| b.is_ascii_lowercase()) {
        return None;
    }
    if token.len() < MIN_STRIP_CHARS || INVARIANT.contains(&token) {
        return Some(token.to_owned());
    }
    let stem = if let Some(base) = token.strip_suffix("ies") {
        // stories → story; but not movies, cookies, zombies (ie + s).
        if base.len() >= 2 && !base.ends_with(['a', 'e', 'o']) && !ie_word(base) {
            format!("{base}y")
        } else {
            token[..token.len() - 1].to_owned()
        }
    } else if let Some(base) = token.strip_suffix("es") {
        if base.ends_with("ss")
            || base.ends_with("sh")
            || base.ends_with("ch")
            || base.ends_with('x')
            || base.ends_with("zz")
        {
            // classes → class, dishes → dish, matches → match, boxes → box
            base.to_owned()
        } else {
            // games → game, shoes → shoe. (heroes → heroe misses hero:
            // telling those apart needs a dictionary.)
            token[..token.len() - 1].to_owned()
        }
    } else if token.ends_with("ss") || token.ends_with("us") || token.ends_with("is") {
        // class, virus, tennis: not plurals.
        token.to_owned()
    } else if let Some(base) = token.strip_suffix('s') {
        base.to_owned()
    } else {
        token.to_owned()
    };
    Some(stem)
}

/// Words whose singular ends in `ie`, so their `-ies` isn't `y` + `s`:
/// movie, cookie, zombie, rookie, pixie, hippie, goalie, selfie…
fn ie_word(base: &str) -> bool {
    matches!(
        base,
        "mov"
            | "cook"
            | "zomb"
            | "rook"
            | "pix"
            | "hipp"
            | "goal"
            | "self"
            | "brown"
            | "cut"
            | "hood"
            | "lass"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(w: &str) -> Option<String> {
        light_stem(w)
    }

    #[test]
    fn plurals_meet_their_singulars() {
        for (plural, singular) in [
            ("knights", "knight"),
            ("stories", "story"),
            ("movies", "movie"),
            ("zombies", "zombie"),
            ("classes", "class"),
            ("dishes", "dish"),
            ("matches", "match"),
            ("boxes", "box"),
            ("games", "game"),
            ("avengers", "avenger"),
            ("babies", "baby"),
            ("ladies", "lady"),
        ] {
            assert_eq!(s(plural), s(singular), "{plural} / {singular}");
            assert!(s(plural).is_some());
        }
    }

    #[test]
    fn non_plurals_stay() {
        for w in [
            "news", "series", "species", "class", "virus", "tennis", "physics", "chaos",
            "analysis", "status", "bonus",
        ] {
            assert_eq!(s(w).as_deref(), Some(w), "{w}");
        }
        assert_eq!(s("news"), Some("news".into()));
        assert_ne!(s("news"), s("new"));
    }

    #[test]
    fn only_long_ascii_words() {
        assert_eq!(s("tv"), None);
        assert_eq!(s("bus"), Some("bus".into()));
        assert_eq!(s("cats"), Some("cat".into()));
        assert_eq!(s("1080p"), None);
        assert_eq!(s("x265"), None);
        assert_eq!(s("café"), None);
        assert_eq!(s("Knights"), None, "tokens arrive folded");
    }

    #[test]
    fn unrelated_words_stay_apart() {
        for (a, b) in [
            ("organization", "organ"),
            ("university", "universe"),
            ("general", "generous"),
            ("kings", "king"),
        ] {
            assert_eq!(s(a) == s(b), a == "kings", "{a} / {b}");
        }
    }
}
