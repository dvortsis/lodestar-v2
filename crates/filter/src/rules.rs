//! Your rules and exceptions, and the CSX-5 override list (§20.2, §20.5.5,
//! §20.12). Each stores only what it needs to work: the match, its
//! scope, the rule it applies to and its dates. Never an author, a reason
//! or a history (§20.11).

use lodestar_search::Node;

use crate::core::{Action, Item};
use crate::engine::RuleRef;
use crate::hash::TermSet;
use crate::matcher::Doc;
use crate::normalize::{self, Token};
use crate::payload::payload_sig;

/// A classification rule (§20.9): every torrent the query matches is
/// stored as adult (or not), like a per-torrent correction. It changes
/// what's stored and searched, never what filtering sees: the engine
/// judges with detection's own classification, so a Not adult rule can't
/// take a torrent out of the Adult-only core checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassRule {
    pub id: u32,
    pub query: Node,
    pub adult: bool,
}

/// A saved query plus an action (layer 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRule {
    pub id: u32,
    pub query: Node,
    pub action: Action,
    pub enabled: bool,
    /// Logs what it would do, changing nothing, until then (§20.3).
    pub shadow_until: Option<i64>,
    /// Context the rule never acts in (§20.4).
    pub unless: Option<Node>,
}

/// What an exception or override entry matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Match {
    /// Exact torrents: every hash form. Never their payload signature.
    Hashes(Vec<[u8; 20]>),
    /// Identical payloads (false-positive exceptions only).
    Payload(u64),
    /// A word or phrase, normalized like the core's matching.
    Keyword(String),
    Query(Node),
}

/// A false-positive exception or an `unless` added to one rule (§20.12).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exception {
    pub id: u32,
    pub rule: RuleRef,
    pub what: Match,
}

/// *Always allow*: skips your rules and the packs, never the core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allow {
    pub id: u32,
    pub query: Node,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strength {
    /// Bypasses every non-floor rule.
    Override,
    /// The keyword stops counting as evidence; everything else counts.
    Neutralize,
}

impl Strength {
    pub fn as_str(self) -> &'static str {
        match self {
            Strength::Override => "override",
            Strength::Neutralize => "neutralize",
        }
    }

    pub fn from_name(name: &str) -> Option<Strength> {
        [Strength::Override, Strength::Neutralize]
            .into_iter()
            .find(|s| s.as_str() == name)
    }
}

/// A CSX-5 entry: always allowed, past every filter but the floor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Override {
    pub id: u32,
    pub what: Match,
    pub strength: Strength,
    /// Narrows the entry (`jailbait type:adult`).
    pub scope: Option<Node>,
    pub expires_at: Option<i64>,
    pub shadow_until: Option<i64>,
}

/// Why an entry can't be saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invalid(pub &'static str);

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Invalid {}

impl Override {
    pub fn validate(&self) -> Result<(), Invalid> {
        match (&self.what, self.strength) {
            (Match::Payload(_), _) => Err(Invalid(
                "override entries match exact torrents, keywords or queries, never payloads",
            )),
            (Match::Keyword(k), _) if keyword_tokens(k).is_empty() => {
                Err(Invalid("the keyword is empty"))
            }
            (Match::Keyword(k), Strength::Neutralize) if keyword_tokens(k).len() > 1 => {
                Err(Invalid("Neutralize term takes one word"))
            }
            (Match::Hashes(_) | Match::Query(_), Strength::Neutralize) => {
                Err(Invalid("Neutralize term is for keywords"))
            }
            _ => Ok(()),
        }
    }

    /// The neutralized word: its hash for the core and its folded form
    /// for queries.
    pub(crate) fn neutral(&self) -> Option<(u64, String)> {
        match (&self.what, self.strength) {
            (Match::Keyword(k), Strength::Neutralize) => {
                let t = keyword_tokens(k);
                let folded: String = lodestar_search::tokenizer::tokenize(k)
                    .into_iter()
                    .map(|t| t.text)
                    .collect();
                t.first().map(|t| (t.hash, folded))
            }
            _ => None,
        }
    }
}

impl Exception {
    pub fn validate(&self) -> Result<(), Invalid> {
        if let RuleRef::Core(r) = self.rule {
            if r.floor() {
                return Err(Invalid(
                    "the locked floor has no exceptions: remove the CS-1 entry instead",
                ));
            }
        }
        if matches!(self.what, Match::Keyword(_)) {
            return Err(Invalid("exceptions match torrents, payloads or queries"));
        }
        Ok(())
    }
}

/// A keyword's normalized tokens: the first token at each word position.
fn keyword_tokens(k: &str) -> Vec<Token> {
    let mut all = Vec::new();
    normalize::tokens(k, &mut all);
    let mut out: Vec<Token> = Vec::new();
    for t in all {
        if out.iter().all(|o| o.pos != t.pos) {
            out.push(t);
        }
    }
    out
}

/// Whether the keyword's words appear in a row in the text.
#[cfg(test)]
fn has_keyword(words: &[Token], text: &str) -> bool {
    let mut tokens = Vec::new();
    normalize::tokens(text, &mut tokens);
    in_a_row(words, &tokens)
}

/// Whether the keyword's words appear in a row in these tokens.
fn in_a_row(words: &[Token], tokens: &[Token]) -> bool {
    let Some(first) = words.first() else {
        return false;
    };
    tokens.iter().filter(|t| t.hash == first.hash).any(|start| {
        words.iter().enumerate().skip(1).all(|(i, w)| {
            tokens
                .iter()
                .any(|t| t.pos == start.pos + i as u32 && t.hash == w.hash)
        })
    })
}

/// Whether a [`Match`] covers the torrent.
pub(crate) fn matches(what: &Match, item: &Item, doc: &Doc) -> bool {
    match what {
        Match::Hashes(hs) => item.hashes.iter().any(|h| hs.contains(h)),
        Match::Payload(sig) => payload_sig(item.files).is_some_and(|p| p.sig == *sig),
        Match::Keyword(k) => {
            let words = keyword_tokens(k);
            doc.normalized().iter().any(|t| in_a_row(&words, t))
        }
        Match::Query(q) => doc.matches(q, &[]),
    }
}

/// The neutral sets of the active Neutralize entries.
pub(crate) fn neutral_sets(entries: &[&Override]) -> (TermSet, Vec<String>) {
    let mut set = TermSet::default();
    let mut words = Vec::new();
    for e in entries {
        if let Some((h, w)) = e.neutral() {
            set.insert(h);
            words.push(w);
        }
    }
    (set, words)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_search::parse;

    fn entry(what: Match, strength: Strength) -> Override {
        Override {
            id: 1,
            what,
            strength,
            scope: None,
            expires_at: None,
            shadow_until: None,
        }
    }

    #[test]
    fn validation() {
        let kw = |k: &str| Match::Keyword(k.into());
        assert!(entry(kw("alpha"), Strength::Neutralize).validate().is_ok());
        assert!(
            entry(kw("alpha beta"), Strength::Neutralize)
                .validate()
                .is_err()
        );
        assert!(
            entry(kw("alpha beta"), Strength::Override)
                .validate()
                .is_ok()
        );
        assert!(entry(kw(" "), Strength::Override).validate().is_err());
        assert!(
            entry(Match::Payload(1), Strength::Override)
                .validate()
                .is_err()
        );
        let q = Match::Query(parse("alpha").unwrap());
        assert!(entry(q, Strength::Neutralize).validate().is_err());
        let floor = Exception {
            id: 1,
            rule: RuleRef::Core(crate::CoreRule::Cs1a),
            what: Match::Hashes(vec![[0; 20]]),
        };
        assert!(floor.validate().is_err());
    }

    #[test]
    fn keywords_resist_evasion() {
        let w = keyword_tokens("alpha beta");
        assert_eq!(w.len(), 2);
        assert!(has_keyword(&w, "The.Alpha.Beta.2019"));
        assert!(has_keyword(&w, "the al\u{200B}pha b3ta"));
        assert!(!has_keyword(&w, "beta alpha"));
        assert!(!has_keyword(&w, "alphabet"));
    }
}
