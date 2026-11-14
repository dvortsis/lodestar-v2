//! The child-safety core (§20.5): sub-rules, the locked floor, switches
//! and the built-in exceptions.
//!
//! - **Floor** (always on, never overridden): CS-1a/1b, exact hashes in
//!   your CS-1 list or an imported one; CS-8a, the payload signature of a
//!   CS-1 torrent when the payload is specific.
//! - **Too large to judge** (CS-0a, locked like the floor): a torrent
//!   whose name and paths are past the core's work budget ([`BUDGET`]) is
//!   held, not judged in part. Every torrent within it is judged whole:
//!   its full name and every path.
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
    Cs0a,
    Cs2a,
    Cs3a,
    Cs3b,
    Cs4a,
    Cs5a,
    Cs5b,
}

impl CoreRule {
    /// In evaluation order: the floor first, then the work budget.
    pub const ALL: [CoreRule; 10] = [
        CoreRule::Cs1a,
        CoreRule::Cs1b,
        CoreRule::Cs8a,
        CoreRule::Cs0a,
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
            CoreRule::Cs0a => "CS-0a",
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

    /// Checks only torrents classified Adult (CS-5a/5b).
    pub fn adult_only(self) -> bool {
        matches!(self, CoreRule::Cs5a | CoreRule::Cs5b)
    }

    /// The locked floor: always on, never overridden (§20.5.3).
    pub fn floor(self) -> bool {
        matches!(self, CoreRule::Cs1a | CoreRule::Cs1b | CoreRule::Cs8a)
    }

    /// Always on, never overridden and without exceptions: the floor, and
    /// CS-0a, which holds a torrent too large to judge. CS-0a quarantines
    /// rather than drops, and matches no hash, so it isn't [`floor`]: the
    /// hash-list paths that read that leave it alone.
    ///
    /// [`floor`]: CoreRule::floor
    pub fn locked(self) -> bool {
        self.floor() || self == CoreRule::Cs0a
    }

    pub fn action(self) -> Action {
        if self.floor() {
            Action::Drop
        } else {
            Action::Quarantine
        }
    }
}

/// A locked rule (the floor, CS-0a) can't be switched off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Locked(pub CoreRule);

impl std::fmt::Display for Locked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0.floor() {
            write!(f, "{} is part of the locked floor", self.0.id())
        } else {
            write!(f, "{} is locked", self.0.id())
        }
    }
}

impl std::error::Error for Locked {}

/// Which core rules are switched off. Every rule is on by default, and
/// the locked rules can't be switched off.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Switches(BTreeSet<CoreRule>);

impl Switches {
    /// Switches as stored: `(rule id, enabled)`. Unknown ids (rules a
    /// newer version added and this one doesn't have) and the locked rules are
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
        rule.locked() || !self.0.contains(&rule)
    }

    pub fn set(&mut self, rule: CoreRule, on: bool) -> Result<(), Locked> {
        if rule.locked() {
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

    /// Keeps only the floor-marked lists `keep` names.
    pub fn retain_lists(&mut self, keep: impl Fn(u32) -> bool) {
        self.lists.retain(|(i, _)| keep(*i));
    }

    /// Whether the floor holds `hash` other than through list `list`.
    pub fn held_without_list(&self, hash: &[u8; 20], list: u32) -> bool {
        self.hashes.contains_key(hash)
            || self
                .lists
                .iter()
                .any(|(i, l)| *i != list && l.contains(hash))
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
/// Folders [`Core`] keeps the tokens of while reading one torrent.
const FOLDERS: usize = 4;

/// A folder's tokens, their flags, and the position after them.
#[derive(Default)]
struct Folder {
    tokens: Vec<Token>,
    flags: Vec<u16>,
    end: u32,
}

/// A torrent read whole ([`normalize::read_whole`]), kept between
/// torrents.
#[derive(Default)]
struct ReadBufs {
    tokens: Vec<Token>,
    ends: Vec<usize>,
    apart: Vec<bool>,
    read: normalize::Metered,
}

impl Default for normalize::Metered {
    fn default() -> normalize::Metered {
        normalize::Metered {
            work: Some(0),
            marks: false,
        }
    }
}

impl ReadBufs {
    fn read(&mut self, item: &Item) -> normalize::Read<'_> {
        self.tokens.clear();
        self.ends.clear();
        self.apart.clear();
        self.read = normalize::read_whole(
            item.name,
            item.files.iter().map(|f| &f.path[..]),
            &mut self.tokens,
            &mut self.ends,
            &mut self.apart,
        );
        normalize::Read {
            tokens: &self.tokens,
            ends: &self.ends,
            apart: &self.apart,
            over: self.read.work.is_none(),
            marks: self.read.marks,
        }
    }

    /// Lets go of a large torrent's buffers, so a thread doesn't keep
    /// them for good.
    fn trim(&mut self) {
        const KEEP: usize = 1 << 16;
        if self.tokens.capacity() > KEEP {
            self.tokens = Vec::new();
        }
        if self.ends.capacity() > KEEP {
            self.ends = Vec::new();
            self.apart = Vec::new();
        }
    }
}

/// Buffers for reading a torrent, kept between torrents.
#[derive(Default)]
struct Scratch {
    tokens: Vec<Token>,
    flags: Vec<u16>,
    /// [`Core::signals`]' table.
    at: Vec<u16>,
    folders: Vec<Folder>,
}

/// Texts whose positions fit this many, or four per token, get a flag
/// table in `signals`. The tokenizer's positions never pass its tokens,
/// so only a stored fixture is ever scanned sparse.
const DENSE_POSITIONS: usize = 4096;
/// Inside Adult (CS-5a/5b).
const ADULT_WINDOW: u32 = 12;
/// The core's work budget per torrent (§20.8), in units of about a
/// nanosecond on the reference runner: what reading its name and every
/// path costs, as the tokenizer meters it while it reads (bytes, words,
/// characters, and how much each costs the first time). A torrent past it
/// is held as too large to judge (CS-0a); every torrent within it is read
/// whole. The core's time to judge one at the budget, whatever its
/// text, is held under 15 ms on the reference runner, with 2-12 ms
/// measured across every input shape (`budget_worst_case` in
/// `tests/props.rs`).
pub const BUDGET: u64 = 9_000_000;

/// The most files a torrent within the budget can have: each costs some
/// of it, however short its path. Every file of a torrent the core judges
/// is read.
pub const MAX_FILES: usize = (BUDGET / normalize::FILE_UNITS) as usize;

/// Whether `item` is past the budget before any of it is read: more
/// files than [`MAX_FILES`], or more than the least its read can cost (the
/// name's bytes, and each path's least, [`normalize::least_path_cost`]).
/// Never more than the metered read would count, so nothing within the
/// budget is held unread. Stops counting once past, so a torrent far
/// past costs no more to check than one just past.
pub(crate) fn past_budget_unread(item: &Item) -> bool {
    if item.files.len() > MAX_FILES {
        return true;
    }
    let mut least = item.name.len() as u64;
    for f in item.files {
        least += normalize::least_path_cost(&f.path);
        if least > BUDGET {
            return true;
        }
    }
    false
}

/// Whether the core holds `item` as too large to judge (CS-0a): past
/// the budget before it's read, or while it's read. Reads it to find out.
pub fn too_large(item: &Item) -> bool {
    read_cost(item).is_none() || crate::matcher::Doc::new(item, None).too_large()
}

/// What reading `item` whole costs, in units of [`BUDGET`], or `None`
/// where it's past the budget. For tests and the budget's bench.
#[doc(hidden)]
pub fn read_cost(item: &Item) -> Option<u64> {
    if past_budget_unread(item) {
        return None;
    }
    let (mut tokens, mut ends, mut apart) = (Vec::new(), Vec::new(), Vec::new());
    normalize::read_whole(
        item.name,
        item.files.iter().map(|f| &f.path[..]),
        &mut tokens,
        &mut ends,
        &mut apart,
    )
    .work
}

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
/// The hit with the stronger action, the first on a tie.
pub(crate) fn stronger(first: Option<Hit>, second: Option<Hit>) -> Option<Hit> {
    match (first, second) {
        (Some(f), Some(s)) => Some(if s.action > f.action { s } else { f }),
        (f, s) => f.or(s),
    }
}

/// Whether `hits` hold CS-0a: the torrent wasn't read whole, so nothing
/// else is read of it.
fn too_large_read(hits: &[Hit]) -> bool {
    hits.iter().any(|h| h.rule == CoreRule::Cs0a)
}

/// The hits of both readings in one list ([`Core::hits`]), its first
/// switched-on hit the one [`stronger`] picks of the two readings' first:
/// the earlier reading's hits, then the joined reading's for rules the
/// earlier reading didn't hit, and the joined reading's first switched-on
/// hit ahead of them all where it is strictly stronger. A rule hit in both
/// readings is listed once, with the earlier reading's evidence unless the
/// joined hit leads.
fn both_readings(split: Vec<Hit>, joined: Vec<Hit>) -> Vec<Hit> {
    let first_on = |hits: &[Hit]| hits.iter().find(|h| h.on).cloned();
    let lead = match (first_on(&split), first_on(&joined)) {
        (Some(s), Some(j)) if j.action > s.action => Some(j),
        _ => None,
    };
    let mut out: Vec<Hit> = Vec::with_capacity(split.len() + joined.len() + 1);
    out.extend(lead.clone());
    let led = |h: &Hit| lead.as_ref().is_some_and(|l| l.rule == h.rule);
    out.extend(split.into_iter().filter(|h| !led(h)));
    let earlier = out.len();
    for h in joined {
        if out[..earlier].iter().all(|o| o.rule != h.rule) {
            out.push(h);
        }
    }
    out
}

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

/// Pairs worth a sorted index ([`near_index`]) rather than a scan: one
/// threshold for where the index is made and where it's read.
const PAIR_SCAN: usize = 64;

fn indexed(first: usize, second: usize) -> bool {
    first.saturating_mul(second) > PAIR_SCAN
}

/// The first `x` of `first`, in order, with a `y` of `second` within `w`
/// positions, and the first such `y`, in order. `near` is `second` as
/// [`near_index`] sorts it, made once per text for every rule that reads
/// it. No hashing: a binary search per `x`, and the window's positions.
fn nearest_pair(first: &[u32], second: &[u32], near: &[(u32, u32)], w: u32) -> Option<(u32, u32)> {
    if first.is_empty() || second.is_empty() {
        return None;
    }
    // Few enough to scan, or no index made: scan, so a missing index can
    // only cost time, never a pair.
    if !indexed(first.len(), second.len()) || near.is_empty() {
        return first.iter().find_map(|&x| {
            let y = second.iter().find(|&&y| x.abs_diff(y) <= w)?;
            Some((x, *y))
        });
    }
    first.iter().find_map(|&x| {
        let (lo, hi) = (x.saturating_sub(w), x.saturating_add(w));
        let from = near.partition_point(|&(y, _)| y < lo);
        let i = near[from..]
            .iter()
            .take_while(|&&(y, _)| y <= hi)
            .map(|&(_, i)| i)
            .min()?;
        Some((x, second[i as usize]))
    })
}

/// `second`'s positions with their index in it, sorted, for
/// [`nearest_pair`].
fn near_index(second: &[u32]) -> Vec<(u32, u32)> {
    let mut near: Vec<(u32, u32)> = second
        .iter()
        .enumerate()
        .map(|(i, &y)| (y, i as u32))
        .collect();
    near.sort_unstable();
    near
}

/// The signals of a torrent's name (first) and files, as the pattern
/// rules read them ([`Core::pattern_signals`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PatternSignals {
    texts: Vec<(Field, Signals)>,
    /// The read passed the budget: the torrent is too large to judge
    /// (CS-0a), and `texts` are empty.
    over: bool,
    /// A text reads differently in the earlier reading
    /// ([`Core::pattern_hits_split`]).
    marks: bool,
}

/// What one text (the name or a file path) contains.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
        let files = &self.files[..self.files.len().min(MAX_FIXTURE_FILES)];
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
        if files > MAX_FIXTURE_FILES {
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
/// Files kept in a stored fixture (its file count is a `u16`).
pub const MAX_FIXTURE_FILES: usize = 1_000;

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
        let none = TermSet::default();
        let first = |hits: Vec<Hit>| hits.into_iter().find(|h| h.on);
        let (joined, marks) = self.patterns(item, &none, None);
        if too_large_read(&joined) {
            return first(joined);
        }
        let hit = first(joined);
        // The earlier reading's hit wins a tie, so its rule and evidence
        // stay as they were.
        match self.pattern_hits_split(item, &none, marks) {
            Some(split) => stronger(first(split), hit),
            None => hit,
        }
    }

    /// The locked rules that read no words (CS-1a/1b, CS-8a, and CS-0a for
    /// a torrent too large to judge): final when one hits.
    pub fn floor_hit(&self, item: &Item) -> Option<Hit> {
        self.floor(item)
    }

    /// Every pattern rule's hit, switched on or not. `neutral` words are
    /// ignored as evidence (CSX-5 *Neutralize term*, §20.5.5).
    ///
    /// A text that holds a character matching once read as a word break
    /// is read both ways (`Core::pattern_hits_split`), and this has the
    /// hits of both, the earlier reading's first: a rule hits if it hits
    /// in either reading.
    pub fn pattern_hits(&self, item: &Item, neutral: &TermSet) -> Vec<Hit> {
        let (joined, marks) = self.patterns(item, neutral, None);
        if too_large_read(&joined) {
            return joined;
        }
        match self.pattern_hits_split(item, neutral, marks) {
            Some(mut hits) => {
                hits.extend(joined);
                hits
            }
            None => joined,
        }
    }

    /// The hits of the earlier reading, with the characters
    /// [`Tokenizer::splitting`] names as word breaks, where `marks` (the
    /// joined reading found one in the name or a path); `None` otherwise,
    /// when both readings are the same. Rare, so it pays for a tokenizer
    /// of its own.
    pub(crate) fn pattern_hits_split(
        &self,
        item: &Item,
        neutral: &TermSet,
        marks: bool,
    ) -> Option<Vec<Hit>> {
        if !marks {
            return None;
        }
        let mut tok = Tokenizer::splitting();
        tok.meter(normalize::CUT_ONLY);
        let mut scratch = Scratch::default();
        let s = self.patterns_with(&mut tok, &mut scratch, item, neutral, None);
        if tok.over() {
            // A text of this reading reached the token cap: it isn't read
            // whole, so the torrent is too large to judge.
            return Some(vec![self.too_large_hit()]);
        }
        Some(self.judge_signals(&s, item.content_type))
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
    /// queue). Its first switched-on hit is [`Core::verdict`].
    ///
    /// A text that holds a character the matching once read as a word
    /// break is read both ways, as the verdict reads it
    /// (docs/decisions/2026-11-02-F-hits-two-readings.md): the earlier
    /// reading's hits first, then the joined reading's for rules it
    /// didn't hit. Where the joined reading's first switched-on hit is
    /// stronger, and so is the verdict, it leads.
    pub fn hits(&self, item: &Item) -> Vec<Hit> {
        let mut hits: Vec<Hit> = self.floor(item).into_iter().collect();
        if too_large_read(&hits) {
            return hits;
        }
        let none = TermSet::default();
        let (joined, marks) = self.patterns(item, &none, None);
        if too_large_read(&joined) {
            hits.extend(joined);
            return hits;
        }
        match self.pattern_hits_split(item, &none, marks) {
            Some(split) => hits.extend(both_readings(split, joined)),
            None => hits.extend(joined),
        }
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
        let payload = (!self.cs.sigs.is_empty())
            .then(|| payload_sig(item.files))
            .flatten()
            .filter(|p| p.specific && self.cs.sigs.contains(&p.sig))
            .map(|_| self.hit(CoreRule::Cs8a, vec![Evidence::Payload]));
        // A torrent past the budget is held whole, before any of its text
        // is read.
        payload.or_else(|| past_budget_unread(item).then(|| self.too_large_hit()))
    }

    /// Each token's lexicon flags, appended to `out`.
    fn flags_of(&self, tokens: &[Token], out: &mut Vec<u16>) {
        out.extend(tokens.iter().map(|t| self.flags(t)));
    }

    /// The signals in one text, from its tokens and their flags. `table`
    /// is scratch space: per position, the flags of every token there. A
    /// stored fixture can carry any positions, so sparse ones are scanned.
    fn signals(
        &self,
        tokens: &[Token],
        flags: &[u16],
        neutral: &TermSet,
        table: &mut Vec<u16>,
    ) -> Signals {
        let mut s = Signals::default();
        if flags.iter().all(|f| *f == 0) {
            return s;
        }
        let end = tokens.iter().map(|t| t.pos as usize + 3).max().unwrap_or(0);
        let dense = end <= DENSE_POSITIONS.max(tokens.len().saturating_mul(4));
        table.clear();
        if dense {
            table.resize(end, 0);
            for (t, f) in tokens.iter().zip(flags) {
                table[t.pos as usize] |= f;
            }
        }
        let at = |pos: u32, bit: u16| {
            if dense {
                table.get(pos as usize).is_some_and(|f| f & bit != 0)
            } else {
                tokens
                    .iter()
                    .zip(flags)
                    .any(|(t, f)| t.pos == pos && f & bit != 0)
            }
        };
        for (t, &f) in tokens.iter().zip(flags) {
            if let Some(n) = t.num {
                // CSX-4: only 1 to 17 can be a minor age.
                // A position is listed once. Its numbers come one after
                // another, so only the last needs checking.
                if !MINOR_AGES.contains(&n) || s.ages.last() == Some(&t.pos) {
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

    /// The joined reading's hits, and whether a text reads differently
    /// in the earlier one.
    fn patterns(
        &self,
        item: &Item,
        neutral: &TermSet,
        read: Option<normalize::Read>,
    ) -> (Vec<Hit>, bool) {
        let s = self.pattern_signals(item, neutral, read);
        (self.judge_signals(&s, item.content_type), s.marks)
    }

    /// The pattern rules' hits on signals [`Core::pattern_signals`] read,
    /// for a torrent of `content_type`: what the core's own reading of it
    /// finds, the content type being the only part of the item the rules
    /// read after the signals. A torrent read again as Adult reuses its
    /// signals so (`engine::Adult`).
    pub(crate) fn judge_signals(&self, s: &PatternSignals, content_type: ContentType) -> Vec<Hit> {
        if s.over {
            return vec![self.too_large_hit()];
        }
        self.judge(&s.texts, content_type)
    }

    /// CS-0a's hit: the torrent is too large to judge.
    pub(crate) fn too_large_hit(&self) -> Hit {
        self.hit(CoreRule::Cs0a, Vec::new())
    }

    /// What the name and each file path hold, as the pattern rules read
    /// it: the same whatever the torrent's content type. `read`: the
    /// item's name and paths already in tokens, reused where it reads the
    /// same as the core would.
    pub(crate) fn pattern_signals(
        &self,
        item: &Item,
        neutral: &TermSet,
        read: Option<normalize::Read>,
    ) -> PatternSignals {
        thread_local! {
            // Kept per thread: its buffers carry over between torrents.
            static SCRATCH: std::cell::RefCell<Scratch> = Default::default();
            static READ: std::cell::RefCell<ReadBufs> = Default::default();
        }
        // The thread's tokenizer, whose word plans keyword entries' pass
        // over the same torrent just made (`normalize::with_tokenizer`).
        let judge = |read: normalize::Read| {
            if read.over {
                return PatternSignals {
                    texts: Vec::new(),
                    over: true,
                    marks: read.marks,
                };
            }
            let mut s = normalize::with_tokenizer(|tok| {
                SCRATCH.with(|s| match s.try_borrow_mut() {
                    Ok(mut scratch) => {
                        self.patterns_with(tok, &mut scratch, item, neutral, Some(read))
                    }
                    Err(_) => {
                        self.patterns_with(tok, &mut Scratch::default(), item, neutral, Some(read))
                    }
                })
            });
            s.marks = read.marks;
            s
        };
        if let Some(read) = read {
            return judge(read);
        }
        // Read whole, metered, as an entry's read of the torrent would be.
        READ.with(|r| match r.try_borrow_mut() {
            Ok(mut bufs) => {
                let s = judge(bufs.read(item));
                bufs.trim();
                s
            }
            Err(_) => judge(ReadBufs::default().read(item)),
        })
    }

    fn patterns_with(
        &self,
        tok: &mut Tokenizer,
        scratch: &mut Scratch,
        item: &Item,
        neutral: &TermSet,
        read: Option<normalize::Read>,
    ) -> PatternSignals {
        let Scratch {
            tokens,
            flags,
            at,
            folders,
        } = scratch;
        // The read has a path for each file, in order: a filter on either
        // side would misalign them.
        debug_assert!(
            read.is_none_or(|r| r.apart.len() == item.files.len()),
            "the read's paths don't match the files"
        );
        tokens.clear();
        flags.clear();
        match read {
            Some(r) => tokens.extend_from_slice(r.name()),
            None => tok.text(item.name, tokens),
        }
        self.flags_of(tokens, flags);
        let mut texts = vec![(Field::Name, self.signals(tokens, flags, neutral, at))];
        // Files in one folder share its tokens, and a few folders are
        // remembered: a season's episodes and subtitles alternate
        // between two.
        let mut dirs_of: [&[String]; FOLDERS] = [&[]; FOLDERS];
        let mut known = 0;
        let mut next = 0;
        for (i, f) in item.files.iter().enumerate() {
            // Every path, a padding file's too: the torrent marks a file
            // padding, and its path is shown like any other.
            let Some((name, dirs)) = f.path.split_last() else {
                continue;
            };
            // Read already, and the same as read here: the tokens of the
            // path's segments read apart.
            if let Some(path) = read.and_then(|r| r.path(i)) {
                // `tokens` and `flags` still hold the text before, flag for
                // flag: the tokens this path starts with alike (its folder,
                // as a rule) keep theirs, so only the rest are looked up.
                let same = tokens.iter().zip(path).take_while(|(a, b)| a == b).count();
                tokens.truncate(same);
                flags.truncate(same);
                tokens.extend_from_slice(&path[same..]);
                self.flags_of(&path[same..], flags);
                let s = self.signals(tokens, flags, neutral, at);
                if s.paired() {
                    texts.push((Field::File(i as u32), s));
                }
                continue;
            }
            let slot = match dirs_of[..known].iter().position(|d| *d == dirs) {
                Some(slot) => slot,
                None => {
                    let slot = if known < FOLDERS {
                        known += 1;
                        known - 1
                    } else {
                        let slot = next;
                        next = (next + 1) % FOLDERS;
                        slot
                    };
                    if folders.len() <= slot {
                        folders.resize_with(slot + 1, Folder::default);
                    }
                    let d = &mut folders[slot];
                    dirs_of[slot] = dirs;
                    d.tokens.clear();
                    d.flags.clear();
                    tok.path(dirs, &mut d.tokens);
                    d.end = tok.pos();
                    self.flags_of(&d.tokens, &mut d.flags);
                    slot
                }
            };
            let d = &folders[slot];
            tokens.clear();
            tokens.extend_from_slice(&d.tokens);
            tok.continue_path(&[name], tokens, 0, d.end);
            flags.clear();
            flags.extend_from_slice(&d.flags);
            self.flags_of(&tokens[d.tokens.len()..], flags);
            let s = self.signals(tokens, flags, neutral, at);
            if s.paired() {
                texts.push((Field::File(i as u32), s));
            }
        }
        PatternSignals {
            texts,
            over: false,
            marks: false,
        }
    }

    /// Every pattern rule's hit on a hashed fixture (§20.4): the fixture
    /// stands in for the torrent's text.
    pub fn fixture_hits(&self, f: &Fixture) -> Vec<Hit> {
        let none = TermSet::default();
        let (mut at, mut flags) = (Vec::new(), Vec::new());
        let mut signals = |tokens: &[Token]| {
            flags.clear();
            self.flags_of(tokens, &mut flags);
            self.signals(tokens, &flags, &none, &mut at)
        };
        let mut texts = vec![(Field::Name, signals(&f.name))];
        for (i, tokens) in f.files.iter().enumerate().take(MAX_FIXTURE_FILES) {
            texts.push((Field::File(i as u32), signals(tokens)));
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
        // Each text's sexual terms sorted once, for every rule that pairs
        // them; only where a pair is possible and too many to scan, so most
        // torrents build nothing.
        let wanted = |s: &Signals| indexed(s.ages.len().max(s.roles.len()), s.sexual.len());
        let near: Vec<Vec<(u32, u32)>> = if texts.iter().any(|(_, s)| wanted(s)) {
            texts
                .iter()
                .map(|(_, s)| {
                    if wanted(s) {
                        near_index(&s.sexual)
                    } else {
                        Vec::new()
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        let p = |rule, w, first, evidence| {
            self.pair(texts, &near, educational, rule, w, first, evidence)
        };
        let mut hits = Vec::new();
        hits.extend(p(CoreRule::Cs2a, WINDOW, ages, age));
        hits.extend(self.code(texts, CoreRule::Cs3a));
        hits.extend(self.code(texts, CoreRule::Cs3b));
        hits.extend(p(CoreRule::Cs4a, WINDOW, roles, role));
        if adult {
            hits.extend(p(CoreRule::Cs5a, ADULT_WINDOW, roles, role));
            hits.extend(p(CoreRule::Cs5b, ADULT_WINDOW, ages, age));
        }
        hits
    }

    /// The first text with a `first` signal within `w` words of a sexual
    /// term. CSX-2 skips the name of an educational torrent.
    #[allow(clippy::too_many_arguments)]
    fn pair(
        &self,
        texts: &[(Field, Signals)],
        near: &[Vec<(u32, u32)>],
        educational: bool,
        rule: CoreRule,
        w: u32,
        first: fn(&Signals) -> &[u32],
        evidence: fn(Field, u32) -> Evidence,
    ) -> Option<Hit> {
        texts
            .iter()
            .enumerate()
            .filter(|(_, (field, _))| !(educational && *field == Field::Name))
            .find_map(|(i, (field, s))| {
                let near = near.get(i).map_or(&[][..], Vec::as_slice);
                let (x, y) = nearest_pair(first(s), &s.sexual, near, w)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// The store never expires the hold of a torrent too large to judge.
    #[test]
    fn the_stores_kept_rules_are_cs0a() {
        assert_eq!(lodestar_store::KEPT_RULES, [CoreRule::Cs0a.id()]);
    }

    /// The store keeps the floor's rule ids for the purge (§20.9).
    #[test]
    fn the_stores_floor_rules_are_the_floor() {
        let floor: Vec<&str> = CoreRule::ALL
            .iter()
            .filter(|r| r.floor())
            .map(|r| r.id())
            .collect();
        assert_eq!(floor, lodestar_store::FLOOR_RULES);
    }

    fn core() -> Core {
        let lists = Lists {
            sexual: TermSet::of(&["lexa"]),
            child_roles: TermSet::of(&["rolea"]),
            codes: TermSet::of(&["codex"]),
        };
        Core::new(lists, CsList::default(), Switches::default())
    }

    /// [`both_readings`] leads with the hit [`stronger`] picks of the
    /// two readings' first switched-on hits, the earlier reading's on a
    /// tie, and lists each rule once. Every pattern rule quarantines
    /// today, so a strictly stronger joined hit is built here.
    #[test]
    fn both_readings_lead_with_the_stronger() {
        let hit = |rule, action, on, at: u32| Hit {
            rule,
            action,
            on,
            evidence: vec![Evidence::Sexual {
                field: Field::Name,
                pos: at,
            }],
        };
        let q = Action::Quarantine;
        let first = |hs: &[Hit]| hs.iter().find(|h| h.on).cloned();
        let cases = [
            // A tie: the earlier reading's hit and evidence stand.
            (
                vec![hit(CoreRule::Cs4a, q, true, 1)],
                vec![
                    hit(CoreRule::Cs4a, q, true, 2),
                    hit(CoreRule::Cs5b, q, true, 2),
                ],
            ),
            // Nothing on in the earlier reading.
            (
                vec![hit(CoreRule::Cs4a, q, false, 1)],
                vec![
                    hit(CoreRule::Cs4a, q, false, 2),
                    hit(CoreRule::Cs5b, q, true, 2),
                ],
            ),
            // The joined reading strictly stronger.
            (
                vec![hit(CoreRule::Cs5a, Action::Hide, true, 1)],
                vec![
                    hit(CoreRule::Cs4a, q, true, 2),
                    hit(CoreRule::Cs5a, Action::Hide, true, 2),
                ],
            ),
            // Strictly stronger on a rule the earlier reading also hit.
            (
                vec![
                    hit(CoreRule::Cs4a, Action::Hide, true, 1),
                    hit(CoreRule::Cs5b, q, true, 1),
                ],
                vec![hit(CoreRule::Cs5b, q, true, 2)],
            ),
            (Vec::new(), vec![hit(CoreRule::Cs4a, q, true, 2)]),
            (vec![hit(CoreRule::Cs4a, q, true, 1)], Vec::new()),
        ];
        for (split, joined) in cases {
            let verdict = stronger(first(&split), first(&joined));
            let both = both_readings(split.clone(), joined.clone());
            assert_eq!(first(&both), verdict, "{split:?} / {joined:?}");
            let mut rules: Vec<CoreRule> = both.iter().map(|h| h.rule).collect();
            rules.sort();
            rules.dedup();
            assert_eq!(rules.len(), both.len(), "{both:?}");
            for h in split.iter().chain(&joined) {
                assert!(both.iter().any(|b| b.rule == h.rule), "{h:?} missing");
            }
        }
    }

    /// A path whose spelled-out letters run on over a `/` (`l.e.x/a`)
    /// reads differently whole: the core reads it itself, and its hits
    /// are its own read's, whichever path the hit comes from.
    #[test]
    fn a_path_read_differently_whole_is_read_again() {
        let c = core();
        let path = |p: &str| p.split('/').map(String::from).collect::<Vec<_>>();
        let files = [
            FileInfo::new(path("l.e.x/a.16yo.mkv"), 1),
            FileInfo::new(path("lexa/16yo.mkv"), 1),
            FileInfo::new(path("x.mkv"), 1),
        ];
        let name = "x";
        let (mut tokens, mut apart) = (Vec::new(), Vec::new());
        normalize::tokens(name, &mut tokens);
        let mut ends = vec![tokens.len()];
        normalize::paths_apart(
            files.iter().map(|f| &f.path[..]),
            &mut tokens,
            &mut ends,
            &mut apart,
        );
        assert_eq!(apart, [false, true, true]);
        let read = normalize::Read {
            tokens: &tokens,
            ends: &ends,
            apart: &apart,
            over: false,
            marks: false,
        };
        let none = TermSet::default();
        for (files, hits) in [
            (&files[..1], false),
            (&files[1..], true),
            (&files[..], true),
        ] {
            let item = Item {
                hashes: &[],
                name,
                files,
                content_type: ContentType::Video,
            };
            let own = c.patterns(&item, &none, None).0;
            assert_eq!(!own.is_empty(), hits, "{files:?}");
            // Only the files given: the read's paths past them aren't read.
            let (mut t, mut a) = (Vec::new(), Vec::new());
            normalize::tokens(name, &mut t);
            let mut e = vec![t.len()];
            normalize::paths_apart(files.iter().map(|f| &f.path[..]), &mut t, &mut e, &mut a);
            let read_of = normalize::Read {
                tokens: &t,
                ends: &e,
                apart: &a,
                over: false,
                marks: false,
            };
            assert_eq!(c.patterns(&item, &none, Some(read_of)).0, own, "{files:?}");
        }
        // Reusing the whole read's tokens for the first path would differ.
        let item = Item {
            hashes: &[],
            name,
            files: &files[..1],
            content_type: ContentType::Video,
        };
        let forced = normalize::Read {
            apart: &[true],
            ends: &ends[..2],
            ..read
        };
        assert_ne!(
            c.patterns(&item, &none, Some(forced)).0,
            c.patterns(&item, &none, None).0
        );
    }

    proptest! {
        /// The sorted pairing finds what scanning every pair finds: the
        /// first of `first`, in order, with one of `second` close enough,
        /// and the first of those, in order.
        #[test]
        fn pairing_finds_what_the_scan_finds(
            first in prop::collection::vec(0u32..400, 0..120),
            second in prop::collection::vec(0u32..400, 0..120),
            w in 0u32..16,
        ) {
            let scan = first.iter().find_map(|&x| {
                let y = second.iter().find(|&&y| x.abs_diff(y) <= w)?;
                Some((x, *y))
            });
            let near = near_index(&second);
            prop_assert_eq!(nearest_pair(&first, &second, &near, w), scan);
            // Without an index it scans: never fewer pairs.
            prop_assert_eq!(nearest_pair(&first, &second, &[], w), scan);
        }
    }

    proptest! {
        /// The flag table finds what scanning every token finds: positions
        /// pushed past the table's reach take the scan, and give the same
        /// signals once shifted back.
        #[test]
        fn the_flag_table_agrees_with_the_scan(
            words in prop::collection::vec(prop::sample::select(vec![
                "lexa", "rolea", "codex", "yo", "y", "o", "years", "old", "age", "pegi",
                "rated", "15", "12", "3", "17", "18", "docu", "x", "16yo", "h3ll0",
            ]), 0..40),
        ) {
            let c = core();
            let mut tokens = Vec::new();
            normalize::tokens(&words.join(" "), &mut tokens);
            let mut flags = Vec::new();
            c.flags_of(&tokens, &mut flags);
            let mut table = Vec::new();
            let dense = c.signals(&tokens, &flags, &TermSet::default(), &mut table);
            let shift = DENSE_POSITIONS as u32;
            let far: Vec<Token> = tokens.iter().map(|t| Token { pos: t.pos + shift, ..*t }).collect();
            let mut sparse = c.signals(&far, &flags, &TermSet::default(), &mut table);
            for v in [&mut sparse.ages, &mut sparse.sexual, &mut sparse.roles, &mut sparse.codes] {
                for p in v.iter_mut() {
                    *p -= shift;
                }
            }
            prop_assert_eq!(dense, sparse);
        }

        /// Sharing a folder's tokens between files, however the files
        /// move between folders, finds what reading each path whole finds.
        #[test]
        fn shared_folders_read_as_whole_paths(
            segs in prop::collection::vec(
                prop::collection::vec(prop::sample::select(vec![
                    "lexa", "rolea", "codex", "yo", "y", "o", "years", "old", "15", "12",
                    "docu", "x", "16yo", "a", "b",
                ]), 1..5).prop_map(|w| w.join(".")),
                1..7,
            ),
            layout in prop::collection::vec((0usize..7, 0usize..7, 0usize..4), 1..14),
        ) {
            let c = core();
            let files: Vec<FileInfo> = layout
                .iter()
                .map(|&(a, b, depth)| {
                    let mut path: Vec<String> =
                        (0..depth).map(|d| segs[(a + d) % segs.len()].clone()).collect();
                    path.push(segs[b % segs.len()].clone());
                    FileInfo::new(path, 1)
                })
                .collect();
            let item = Item { hashes: &[], name: "", files: &files, content_type: ContentType::Video };
            let none = TermSet::default();
            let (mut flags, mut table) = (Vec::new(), Vec::new());
            let mut texts = vec![(Field::Name, Signals::default())];
            for (i, f) in files.iter().enumerate() {
                let mut tokens = Vec::new();
                Tokenizer::default().path(&f.path, &mut tokens);
                flags.clear();
                c.flags_of(&tokens, &mut flags);
                let s = c.signals(&tokens, &flags, &none, &mut table);
                if s.paired() {
                    texts.push((Field::File(i as u32), s));
                }
            }
            for adult in [false, true] {
                let ct = if adult { ContentType::Adult } else { ContentType::Video };
                let item = Item { content_type: ct, ..item };
                prop_assert_eq!(c.patterns(&item, &none, None).0, c.judge(&texts, ct));
            }
        }

        /// Reusing keyword entries' read of the name and paths finds what
        /// the core's own read finds: paths whose spelled-out letters
        /// run on over a `/` (`y/o`), which read differently whole, are
        /// read again, and padding files are read like any other.
        #[test]
        fn reusing_the_whole_read_finds_the_same(
            segs in prop::collection::vec(
                prop::collection::vec(prop::sample::select(vec![
                    "lexa", "rolea", "codex", "yo", "y", "o", "l", "e", "x", "a", "years",
                    "old", "15", "12", "docu", "16yo", "ⓛⓔⓧⓐ", "Ｒｏｌｅａ", "c\u{301}",
                ]), 1..5).prop_map(|w| w.join(".")),
                1..7,
            ),
            layout in prop::collection::vec((0usize..7, 0usize..7, 0usize..4, any::<bool>()), 0..14),
            name in 0usize..7,
            neutral in prop::sample::subsequence(vec!["lexa", "rolea", "codex"], 0..2),
        ) {
            let c = core();
            let files: Vec<FileInfo> = layout
                .iter()
                .map(|&(a, b, depth, padding)| {
                    let mut path: Vec<String> =
                        (0..depth).map(|d| segs[(a + d) % segs.len()].clone()).collect();
                    path.push(segs[b % segs.len()].clone());
                    let mut f = FileInfo::new(path, 1);
                    f.padding = padding && b % 3 == 0;
                    f
                })
                .collect();
            let name = &segs[name % segs.len()];
            let (mut tokens, mut apart) = (Vec::new(), Vec::new());
            normalize::tokens(name, &mut tokens);
            let mut ends = vec![tokens.len()];
            normalize::paths_apart(files.iter().map(|f| &f.path[..]), &mut tokens, &mut ends, &mut apart);
            let read = normalize::Read { tokens: &tokens, ends: &ends, apart: &apart, over: false, marks: false };
            let neutral = TermSet::of(&neutral);
            for adult in [false, true] {
                let ct = if adult { ContentType::Adult } else { ContentType::Video };
                let item = Item { hashes: &[], name, files: &files, content_type: ct };
                prop_assert_eq!(
                    c.patterns(&item, &neutral, Some(read)).0,
                    c.patterns(&item, &neutral, None).0
                );
            }
        }
    }
}
