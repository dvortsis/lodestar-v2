//! The child-safety core (§20.5): sub-rules, the locked floor, switches
//! and the built-in exceptions.
//!
//! - **Floor** (always on, never overridden): CS-1a/1b, exact hashes in
//!   your CS-1 list or an imported one; CS-8a, the payload signature of a
//!   CS-1 torrent when the payload is specific.
//! - **Pattern rules** (on by default, each can be switched off): CS-2a,
//!   a minor age near a sexual term; CS-3a/3b, a code term in the name or
//!   in file names; CS-4a, a child-role term near a sexual term; CS-5a/5b,
//!   the CS-4/CS-2 checks with looser windows inside Adult.
//! - **Exceptions** cancel evidence, never rules. CSX-3: a number after
//!   a rating system (`PEGI 16`) isn't an age. CSX-4: 18 and 19 are never
//!   minor ages. CSX-2: educational or medical context cancels the
//!   ambiguous evidence in the *name* of a non-adult torrent (never in
//!   file names, never CS-3, never the floor).
//!
//! CS-2b/2c, CS-4b, CS-6a, CS-7a and CS-9a, CSX-1 (it needs enrichment)
//! and CSX-5 arrive in later slices.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use lodestar_classify::{ContentType, FileInfo};

use crate::hash::{IdMap, TermSet, TermSetError, term_hash};
use crate::normalize::{self, Token, Tokenizer};
use crate::payload::payload_sig;

/// What a rule does to a torrent it matches (§20.3), weakest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Action {
    Flag,
    Demote,
    Hide,
    Quarantine,
    Drop,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Flag => "flag",
            Action::Demote => "demote",
            Action::Hide => "hide",
            Action::Quarantine => "quarantine",
            Action::Drop => "drop",
        }
    }

    /// Only these may act for a core rule (§20.5.6).
    pub fn protective(self) -> bool {
        matches!(self, Action::Quarantine | Action::Drop)
    }

    /// The stored code (`filter_hits.action`). Fixed forever.
    pub fn code(self) -> u8 {
        self as u8
    }

    pub fn from_code(code: u8) -> Option<Action> {
        Some(match code {
            0 => Action::Flag,
            1 => Action::Demote,
            2 => Action::Hide,
            3 => Action::Quarantine,
            4 => Action::Drop,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CoreRule {
    Cs1a,
    Cs1b,
    Cs8a,
    Cs2a,
    Cs3a,
    Cs3b,
    Cs4a,
    Cs5a,
    Cs5b,
}

impl CoreRule {
    /// In evaluation order: the floor first.
    pub const ALL: [CoreRule; 9] = [
        CoreRule::Cs1a,
        CoreRule::Cs1b,
        CoreRule::Cs8a,
        CoreRule::Cs2a,
        CoreRule::Cs3a,
        CoreRule::Cs3b,
        CoreRule::Cs4a,
        CoreRule::Cs5a,
        CoreRule::Cs5b,
    ];

    pub fn id(self) -> &'static str {
        match self {
            CoreRule::Cs1a => "CS-1a",
            CoreRule::Cs1b => "CS-1b",
            CoreRule::Cs8a => "CS-8a",
            CoreRule::Cs2a => "CS-2a",
            CoreRule::Cs3a => "CS-3a",
            CoreRule::Cs3b => "CS-3b",
            CoreRule::Cs4a => "CS-4a",
            CoreRule::Cs5a => "CS-5a",
            CoreRule::Cs5b => "CS-5b",
        }
    }

    pub fn from_id(id: &str) -> Option<CoreRule> {
        CoreRule::ALL.into_iter().find(|r| r.id() == id)
    }

    /// The locked floor: always on, never overridden (§20.5.3).
    pub fn floor(self) -> bool {
        matches!(self, CoreRule::Cs1a | CoreRule::Cs1b | CoreRule::Cs8a)
    }

    pub fn action(self) -> Action {
        if self.floor() {
            Action::Drop
        } else {
            Action::Quarantine
        }
    }
}

/// A floor rule can't be switched off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Locked(pub CoreRule);

impl std::fmt::Display for Locked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} is part of the locked floor", self.0.id())
    }
}

impl std::error::Error for Locked {}

/// Which core rules are switched off. Every rule is on by default, and
/// the floor can't be switched off.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Switches(BTreeSet<CoreRule>);

impl Switches {
    /// Switches as stored: `(rule id, enabled)`. Unknown ids (rules a
    /// newer version added and this one doesn't have) and the floor are
    /// ignored.
    pub fn from_stored<'a>(entries: impl IntoIterator<Item = (&'a str, bool)>) -> Switches {
        let mut s = Switches::default();
        for (id, on) in entries {
            if let Some(rule) = CoreRule::from_id(id) {
                let _ = s.set(rule, on);
            }
        }
        s
    }

    pub fn is_on(&self, rule: CoreRule) -> bool {
        rule.floor() || !self.0.contains(&rule)
    }

    pub fn set(&mut self, rule: CoreRule, on: bool) -> Result<(), Locked> {
        if rule.floor() {
            return if on { Ok(()) } else { Err(Locked(rule)) };
        }
        if on {
            self.0.remove(&rule);
        } else {
            self.0.insert(rule);
        }
        Ok(())
    }

    pub fn off(&self) -> impl Iterator<Item = CoreRule> + '_ {
        self.0.iter().copied()
    }
}

/// Where a CS-1 entry came from (§20.5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Origin {
    /// Confirmed in the review queue.
    Confirmed,
    /// Reported directly.
    Reported,
    /// From a hash list you imported.
    Imported,
}

impl Origin {
    /// The stored code (`cs_hashes.origin`). Fixed forever.
    pub fn code(self) -> u8 {
        match self {
            Origin::Confirmed => 0,
            Origin::Reported => 1,
            Origin::Imported => 2,
        }
    }

    pub fn from_code(code: u8) -> Option<Origin> {
        Some(match code {
            0 => Origin::Confirmed,
            1 => Origin::Reported,
            2 => Origin::Imported,
            _ => return None,
        })
    }

    fn rule(self) -> CoreRule {
        match self {
            Origin::Confirmed | Origin::Reported => CoreRule::Cs1a,
            Origin::Imported => CoreRule::Cs1b,
        }
    }
}

/// The CS-1 hashes and the CS-8 payload signatures.
#[derive(Debug, Clone, Default)]
pub struct CsList {
    hashes: HashMap<[u8; 20], Origin>,
    sigs: HashSet<u64>,
    /// Subscribed lists you marked as child-abuse lists (§20.13): CS-1b,
    /// replaced whole on each refresh so upstream removals take effect.
    lists: Vec<(u32, Arc<HashSet<[u8; 20]>>)>,
}

impl CsList {
    /// Adds a hash. Your own confirmations outrank an import of the same
    /// hash.
    pub fn add_hash(&mut self, hash: [u8; 20], origin: Origin) {
        let e = self.hashes.entry(hash).or_insert(origin);
        if *e == Origin::Imported {
            *e = origin;
        }
    }

    pub fn add_sig(&mut self, sig: u64) {
        self.sigs.insert(sig);
    }

    pub fn remove_hash(&mut self, hash: &[u8; 20]) -> bool {
        self.hashes.remove(hash).is_some()
    }

    pub fn remove_sig(&mut self, sig: u64) -> bool {
        self.sigs.remove(&sig)
    }

    pub fn has_sig(&self, sig: u64) -> bool {
        self.sigs.contains(&sig)
    }

    pub fn hash(&self, hash: &[u8; 20]) -> Option<Origin> {
        self.hashes.get(hash).copied().or_else(|| {
            self.lists
                .iter()
                .any(|(_, l)| l.contains(hash))
                .then_some(Origin::Imported)
        })
    }

    /// Sets (or with `None` removes) a floor-marked subscribed list.
    pub fn set_list(&mut self, id: u32, hashes: Option<Arc<HashSet<[u8; 20]>>>) {
        self.lists.retain(|(i, _)| *i != id);
        if let Some(h) = hashes {
            self.lists.push((id, h));
        }
    }

    pub fn len(&self) -> usize {
        self.hashes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hashes.is_empty() && self.sigs.is_empty()
    }
}

/// The core's term lists (§20.5.1), hashed.
#[derive(Debug, Clone)]
pub struct Lists {
    pub sexual: TermSet,
    pub child_roles: TermSet,
    /// Ships empty: filled only from lists you import.
    pub codes: TermSet,
}

impl Lists {
    pub fn shipped() -> Result<Lists, TermSetError> {
        Ok(Lists {
            sexual: TermSet::parse(include_str!("../data/cs-sexual.txt"))?,
            child_roles: TermSet::parse(include_str!("../data/cs-child-roles.txt"))?,
            codes: TermSet::parse(include_str!("../data/cs-codes.txt"))?,
        })
    }
}

/// Where in the torrent evidence was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    Name,
    /// The path of the file at this index.
    File(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Evidence {
    Hash(Origin),
    Payload,
    MinorAge {
        field: Field,
        pos: u32,
    },
    Sexual {
        field: Field,
        pos: u32,
    },
    ChildRole {
        field: Field,
        pos: u32,
    },
    Code {
        field: Field,
        pos: u32,
    },
    /// The hash is in a subscribed list.
    Listed,
    /// One of your rules' queries matched.
    Query,
    /// A built-in pack rule matched.
    Pack,
    /// A shadow-mode override would have released this.
    Released,
}

impl Evidence {
    /// Appends a compact code for `filter_hits.evidence`: what fired and
    /// where, never any text.
    pub fn encode(&self, out: &mut Vec<u8>) {
        let (tag, place) = match *self {
            Evidence::Hash(o) => {
                out.extend([0, o.code()]);
                return;
            }
            Evidence::Payload => {
                out.push(1);
                return;
            }
            Evidence::Listed => {
                out.push(6);
                return;
            }
            Evidence::Query => {
                out.push(7);
                return;
            }
            Evidence::Pack => {
                out.push(8);
                return;
            }
            Evidence::Released => {
                out.push(9);
                return;
            }
            Evidence::MinorAge { field, pos } => (2, (field, pos)),
            Evidence::Sexual { field, pos } => (3, (field, pos)),
            Evidence::ChildRole { field, pos } => (4, (field, pos)),
            Evidence::Code { field, pos } => (5, (field, pos)),
        };
        out.push(tag);
        let file = match place.0 {
            Field::Name => u32::MAX,
            Field::File(i) => i,
        };
        out.extend(file.to_le_bytes());
        out.extend(place.1.to_le_bytes());
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub rule: CoreRule,
    pub action: Action,
    /// The rule is switched on. Off rules still report hits for previews.
    pub on: bool,
    pub evidence: Vec<Evidence>,
}

impl Hit {
    pub fn evidence_codes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for e in &self.evidence {
            e.encode(&mut out);
        }
        out
    }
}

/// The parts of a torrent the core reads.
#[derive(Debug, Clone, Copy)]
pub struct Item<'a> {
    /// Every swarm hash (v1 and truncated v2).
    pub hashes: &'a [[u8; 20]],
    pub name: &'a str,
    pub files: &'a [FileInfo],
    pub content_type: ContentType,
}

/// Pairs closer than this many words are evidence.
const WINDOW: u32 = 8;
/// Inside Adult (CS-5a/5b).
const ADULT_WINDOW: u32 = 12;
/// File paths read per torrent; the rest add nothing a pack this large
/// doesn't already show.
pub const MAX_FILES: usize = 1_000;

const MINOR_AGES: std::ops::RangeInclusive<u16> = 1..=17;

// What a token is, as bits in the lexicon.
const SEXUAL: u16 = 1;
const ROLE: u16 = 1 << 1;
const CODE: u16 = 1 << 2;
/// Educational and medical context (CSX-2).
const CONTEXT: u16 = 1 << 3;
/// Before a number: a rating, not an age (CSX-3).
const RATING: u16 = 1 << 4;
/// `NN yo`.
const AFTER: u16 = 1 << 5;
/// `NN year old`, `NN y o`.
const AFTER_FIRST: u16 = 1 << 6;
const AFTER_SECOND: u16 = 1 << 7;
/// `age NN`.
const BEFORE: u16 = 1 << 8;

/// Words that aren't indicative, kept as plain lists.
const PLAIN: &[(u16, &[&str])] = &[
    (
        RATING,
        &[
            "pegi", "usk", "fsk", "cero", "esrb", "acb", "oflc", "bbfc", "kmrb", "grac",
            "classind", "rated", "rating", "pg", "nc",
        ],
    ),
    (AFTER, &["yo", "yearold", "yearsold", "yrold", "yrsold"]),
    (AFTER_FIRST, &["y", "year", "years", "yr", "yrs"]),
    (AFTER_SECOND, &["o", "old"]),
    (BEFORE, &["age", "aged"]),
    (
        CONTEXT,
        &[
            "education",
            "educational",
            "puberty",
            "health",
            "paediatrics",
            "pediatrics",
            "paediatric",
            "pediatric",
            "safeguarding",
            "documentary",
            "docu",
            "lecture",
            "medical",
            "biology",
            "anatomy",
            "parenting",
            "curriculum",
        ],
    ),
];

/// Every term the core reads, compiled into one map: one lookup per
/// token.
fn lexicon(lists: &Lists) -> IdMap<u16> {
    let mut map = IdMap::default();
    let mut add = |h: u64, bit: u16| *map.entry(h).or_insert(0) |= bit;
    for (bit, words) in PLAIN {
        for w in *words {
            add(term_hash(w), *bit);
        }
    }
    for (bit, set) in [
        (SEXUAL, &lists.sexual),
        (ROLE, &lists.child_roles),
        (CODE, &lists.codes),
    ] {
        for h in set.iter() {
            add(h, bit);
        }
    }
    map
}

/// What one text (the name or a file path) contains.
#[derive(Debug, Default)]
struct Signals {
    ages: Vec<u32>,
    sexual: Vec<u32>,
    roles: Vec<u32>,
    codes: Vec<u32>,
    context: bool,
}

impl Signals {
    /// Anything a file-level rule could fire on.
    fn paired(&self) -> bool {
        !(self.ages.is_empty() && self.roles.is_empty() && self.codes.is_empty())
    }
}

/// A torrent's text as hashed tokens (§20.4): how the core's fixtures and
/// the false-positive examples you create are stored, with no names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fixture {
    pub name: Vec<Token>,
    pub files: Vec<Vec<Token>>,
    pub content_type: ContentType,
}

impl Fixture {
    pub fn of(name: &str, paths: &[&str], content_type: ContentType) -> Fixture {
        let mut n = Vec::new();
        normalize::tokens(name, &mut n);
        let files = paths
            .iter()
            .map(|p| {
                let mut t = Vec::new();
                normalize::tokens(p, &mut t);
                t
            })
            .collect();
        Fixture {
            name: n,
            files,
            content_type,
        }
    }

    /// The stored form (`filter_fixtures.tokens`): a version byte, the
    /// content type, the file count (u16), then each text (name first) as
    /// a token count (u32) and its tokens (pos u32, hash u64, num u16,
    /// `u16::MAX` for none), little-endian. Hashes only: no names.
    pub fn encode(&self) -> Vec<u8> {
        let files = &self.files[..self.files.len().min(MAX_FILES)];
        let mut out = vec![FIXTURE_V1, self.content_type.code()];
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        for text in std::iter::once(&self.name).chain(files) {
            let text = &text[..text.len().min(MAX_FIXTURE_TOKENS)];
            out.extend_from_slice(&(text.len() as u32).to_le_bytes());
            for t in text {
                out.extend_from_slice(&t.pos.to_le_bytes());
                out.extend_from_slice(&t.hash.to_le_bytes());
                out.extend_from_slice(&t.num.unwrap_or(u16::MAX).to_le_bytes());
            }
        }
        out
    }

    /// Reads [`Fixture::encode`]'s form; `None` for anything else.
    pub fn decode(bytes: &[u8]) -> Option<Fixture> {
        let mut r = bytes;
        let mut take = |n: usize| -> Option<&[u8]> {
            let (head, rest) = r.split_at_checked(n)?;
            r = rest;
            Some(head)
        };
        if take(1)? != [FIXTURE_V1] {
            return None;
        }
        let content_type = ContentType::from_code(take(1)?[0])?;
        let files = u16::from_le_bytes(take(2)?.try_into().ok()?) as usize;
        if files > MAX_FILES {
            return None;
        }
        let mut texts = Vec::with_capacity(files + 1);
        for _ in 0..=files {
            let n = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
            if n > MAX_FIXTURE_TOKENS {
                return None;
            }
            let mut text = Vec::with_capacity(n);
            for _ in 0..n {
                let t = take(14)?;
                let num = u16::from_le_bytes([t[12], t[13]]);
                text.push(Token {
                    pos: u32::from_le_bytes(t[0..4].try_into().ok()?),
                    hash: u64::from_le_bytes(t[4..12].try_into().ok()?),
                    num: (num != u16::MAX).then_some(num),
                });
            }
            texts.push(text);
        }
        if !r.is_empty() {
            return None;
        }
        let name = texts.remove(0);
        Some(Fixture {
            name,
            files: texts,
            content_type,
        })
    }
}

const FIXTURE_V1: u8 = 1;
/// Tokens kept per text of a stored fixture.
pub const MAX_FIXTURE_TOKENS: usize = 4096;

#[derive(Debug, Clone)]
pub struct Core {
    lists: Lists,
    lexicon: IdMap<u16>,
    pub cs: CsList,
    pub switches: Switches,
}

impl Core {
    pub fn new(lists: Lists, cs: CsList, switches: Switches) -> Core {
        Core {
            lexicon: lexicon(&lists),
            lists,
            cs,
            switches,
        }
    }

    pub fn lists(&self) -> &Lists {
        &self.lists
    }

    /// Replaces the term lists (an import, or your own additions).
    pub fn set_lists(&mut self, lists: Lists) {
        self.lexicon = lexicon(&lists);
        self.lists = lists;
    }

    fn flags(&self, t: &Token) -> u16 {
        if t.num.is_some() {
            return 0;
        }
        self.lexicon.get(&t.hash).copied().unwrap_or(0)
    }

    /// The first hit of a switched-on rule, in evaluation order. A floor
    /// hit is final, so the text isn't read at all.
    pub fn verdict(&self, item: &Item) -> Option<Hit> {
        if let Some(hit) = self.floor(item) {
            return Some(hit);
        }
        self.patterns(item, &TermSet::default())
            .into_iter()
            .find(|h| h.on)
    }

    /// The floor alone (CS-1a/1b, CS-8a): final when it hits.
    pub fn floor_hit(&self, item: &Item) -> Option<Hit> {
        self.floor(item)
    }

    /// Every pattern rule's hit, switched on or not. `neutral` words are
    /// ignored as evidence (CSX-5 *Neutralize term*, §20.5.5).
    pub fn pattern_hits(&self, item: &Item, neutral: &TermSet) -> Vec<Hit> {
        self.patterns(item, neutral)
    }

    /// The floor's verdict on bare hashes, before anything is fetched.
    pub fn blocked_hash(&self, hashes: &[[u8; 20]]) -> Option<Hit> {
        self.floor(&Item {
            hashes,
            name: "",
            files: &[],
            content_type: ContentType::Other,
        })
    }

    /// Every rule's hit, switched on or not (for previews and the review
    /// queue).
    pub fn hits(&self, item: &Item) -> Vec<Hit> {
        let mut hits: Vec<Hit> = self.floor(item).into_iter().collect();
        hits.extend(self.patterns(item, &TermSet::default()));
        hits
    }

    fn hit(&self, rule: CoreRule, evidence: Vec<Evidence>) -> Hit {
        Hit {
            rule,
            action: rule.action(),
            on: self.switches.is_on(rule),
            evidence,
        }
    }

    fn floor(&self, item: &Item) -> Option<Hit> {
        let mut best: Option<Origin> = None;
        for h in item.hashes {
            match (self.cs.hash(h), best) {
                (Some(o), None) | (Some(o), Some(Origin::Imported)) => best = Some(o),
                _ => {}
            }
        }
        if let Some(o) = best {
            return Some(self.hit(o.rule(), vec![Evidence::Hash(o)]));
        }
        if self.cs.sigs.is_empty() {
            return None;
        }
        payload_sig(item.files)
            .filter(|p| p.specific && self.cs.sigs.contains(&p.sig))
            .map(|_| self.hit(CoreRule::Cs8a, vec![Evidence::Payload]))
    }

    fn signals(&self, tokens: &[Token], neutral: &TermSet) -> Signals {
        let mut s = Signals::default();
        let at = |pos: u32, bit: u16| {
            tokens
                .iter()
                .any(|t| t.pos == pos && self.flags(t) & bit != 0)
        };
        for t in tokens {
            if let Some(n) = t.num {
                // CSX-4: only 1 to 17 can be a minor age.
                if !MINOR_AGES.contains(&n) || s.ages.contains(&t.pos) {
                    continue;
                }
                let before = t.pos.checked_sub(1);
                let rated = before.is_some_and(|b| at(b, RATING));
                let marked = at(t.pos + 1, AFTER)
                    || (at(t.pos + 1, AFTER_FIRST) && at(t.pos + 2, AFTER_SECOND))
                    || before.is_some_and(|b| at(b, BEFORE));
                if marked && !rated {
                    s.ages.push(t.pos);
                }
                continue;
            }
            let f = self.flags(t);
            if f == 0 || (!neutral.is_empty() && neutral.contains(t.hash)) {
                continue;
            }
            if f & SEXUAL != 0 {
                s.sexual.push(t.pos);
            }
            if f & ROLE != 0 {
                s.roles.push(t.pos);
            }
            if f & CODE != 0 {
                s.codes.push(t.pos);
            }
            s.context |= f & CONTEXT != 0;
        }
        s
    }

    fn patterns(&self, item: &Item, neutral: &TermSet) -> Vec<Hit> {
        let mut tok = Tokenizer::default();
        let mut tokens = Vec::new();
        tok.text(item.name, &mut tokens);
        let mut texts = vec![(Field::Name, self.signals(&tokens, neutral))];
        // Files in one folder share its tokens.
        let mut folder: &[String] = &[];
        let mut folder_tokens: Vec<Token> = Vec::new();
        let mut folder_end = 0;
        for (i, f) in item.files.iter().take(MAX_FILES).enumerate() {
            let Some((name, dirs)) = f.path.split_last() else {
                continue;
            };
            if f.padding {
                continue;
            }
            if dirs != folder || i == 0 {
                folder = dirs;
                folder_tokens.clear();
                tok.path(dirs, &mut folder_tokens);
                folder_end = tok.pos();
            }
            tokens.clear();
            tokens.extend_from_slice(&folder_tokens);
            tok.continue_path(&[name], &mut tokens, 0, folder_end);
            let s = self.signals(&tokens, neutral);
            if s.paired() {
                texts.push((Field::File(i as u32), s));
            }
        }
        self.judge(&texts, item.content_type)
    }

    /// Every pattern rule's hit on a hashed fixture (§20.4): the fixture
    /// stands in for the torrent's text.
    pub fn fixture_hits(&self, f: &Fixture) -> Vec<Hit> {
        let none = TermSet::default();
        let mut texts = vec![(Field::Name, self.signals(&f.name, &none))];
        for (i, tokens) in f.files.iter().enumerate().take(MAX_FILES) {
            texts.push((Field::File(i as u32), self.signals(tokens, &none)));
        }
        self.judge(&texts, f.content_type)
    }

    /// The pattern rules over the signals of the name (first) and files.
    fn judge(&self, texts: &[(Field, Signals)], content_type: ContentType) -> Vec<Hit> {
        let adult = content_type == ContentType::Adult;
        // CSX-2: educational or medical context, outside Adult.
        let educational = texts[0].1.context && !adult;
        let ages: fn(&Signals) -> &[u32] = |s| &s.ages;
        let roles: fn(&Signals) -> &[u32] = |s| &s.roles;
        let age = |field, pos| Evidence::MinorAge { field, pos };
        let role = |field, pos| Evidence::ChildRole { field, pos };
        let mut hits = Vec::new();
        hits.extend(self.pair(texts, educational, CoreRule::Cs2a, WINDOW, ages, age));
        hits.extend(self.code(texts, CoreRule::Cs3a));
        hits.extend(self.code(texts, CoreRule::Cs3b));
        hits.extend(self.pair(texts, educational, CoreRule::Cs4a, WINDOW, roles, role));
        if adult {
            let w = ADULT_WINDOW;
            hits.extend(self.pair(texts, educational, CoreRule::Cs5a, w, roles, role));
            hits.extend(self.pair(texts, educational, CoreRule::Cs5b, w, ages, age));
        }
        hits
    }

    /// The first text with a `first` signal within `w` words of a sexual
    /// term. CSX-2 skips the name of an educational torrent.
    fn pair(
        &self,
        texts: &[(Field, Signals)],
        educational: bool,
        rule: CoreRule,
        w: u32,
        first: fn(&Signals) -> &[u32],
        evidence: fn(Field, u32) -> Evidence,
    ) -> Option<Hit> {
        texts
            .iter()
            .filter(|(field, _)| !(educational && *field == Field::Name))
            .find_map(|(field, s)| {
                let (x, y) = first(s).iter().find_map(|&x| {
                    let y = s.sexual.iter().find(|&&y| x.abs_diff(y) <= w)?;
                    Some((x, *y))
                })?;
                let sexual = Evidence::Sexual {
                    field: *field,
                    pos: y,
                };
                Some(self.hit(rule, vec![evidence(*field, x), sexual]))
            })
    }

    /// CS-3a reads the name, CS-3b the file paths.
    fn code(&self, texts: &[(Field, Signals)], rule: CoreRule) -> Option<Hit> {
        let files = rule == CoreRule::Cs3b;
        texts
            .iter()
            .filter(|(field, _)| matches!(field, Field::File(_)) == files)
            .find_map(|(field, s)| {
                let pos = *s.codes.first()?;
                Some(self.hit(rule, vec![Evidence::Code { field: *field, pos }]))
            })
    }
}
