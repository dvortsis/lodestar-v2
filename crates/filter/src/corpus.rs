//! The pack corpora (§20.6 "each ships with its test corpus", §20.8; RP-1):
//! must-match and must-not-match examples for every built stateless pack
//! rule, and the pack precision corpus of legitimate items each
//! false-positive guard exists for. Both are compiled in as plain text
//! (§20.7 allows that for shipped pack fixtures) and run through
//! [`Engine::judge`] here, in `cargo test` and, through RP-3, in
//! production for `GET /filters/fixtures`. Nothing here does I/O.
//!
//! # The format
//!
//! One keyword per line, then its value. Blank lines and lines starting
//! with `#` are skipped; leading spaces are only for reading.
//!
//! ```text
//! # crates/filter/data/packs/malware/corpus.txt
//! rule MW-2
//! match Example.Movie.2019.1080p.WEB
//!   file 4G Example.Movie.2019.1080p.WEB.mkv
//!   file 900K Codec Setup.exe
//! miss Example Tool v1.2 x64
//!   file 50M setup.exe
//!   type software
//! ```
//!
//! - **`rule <ID>`** (pack corpora only): the rule the examples below it
//!   are for, which must be in that file's pack. SP-3 and SP-4 are
//!   refused: they depend on state, so their cases are sequence fixtures
//!   (RP-4, RP-5), not items ([`STATEFUL`]).
//! - **`match <name>`** / **`miss <name>`** (pack corpora only): an
//!   example the rule must hold, or must not. The name is the rest of the
//!   line.
//! - **`item <name>`** (precision corpus only): a legitimate item no pack
//!   rule may hold.
//! - **`file <size> <path>`**: one of the example's files. The size is
//!   bytes, or a number with `K`, `M`, `G` or `T` (powers of 1,024). The
//!   path is the rest of the line, its folders split at `/` and kept as
//!   they are (spaces around a `/` are part of the names).
//! - **`pad <size> <path>`**: a BEP 47 padding file.
//! - **`type <type>`**: the content type to judge it as, instead of the
//!   classified one, as `POST /filters/test` takes it. Without it the
//!   example is classified as ingest would.
//! - **`guards <ID> …`** (precision corpus only, required): the rules
//!   whose false-positive guards the item exercises (§20.6's guard
//!   column).
//! - **`gap <ID> …`** (precision corpus only): rules known to hold the
//!   item today because the guard §20.6 names isn't built yet. The item
//!   then passes only while exactly those rules hold it, so the line has
//!   to go when the guard is built, and the report lists every gap
//!   ([`Report::gaps`]).
//!
//! The parser is bounded ([`MAX_BYTES`], [`MAX_LINE`], [`MAX_ITEMS`],
//! [`MAX_FILES`]), never panics, and names the file and line of the first
//! error.
//!
//! # What runs
//!
//! - **A pack corpus example** is judged with only its pack on and only
//!   its rule switched on in that pack, so each rule's result is its own
//!   (SC-2 by its own id). A must-match example passes when that rule
//!   decides, with its version's action; a must-not-match one when
//!   nothing does.
//! - **A precision item** is judged once per built stateless rule, the
//!   same way, and passes when none holds it but its `gap` rules.
//! - **Directions:** each version's recorded direction (RP-0) is checked
//!   against what the corpus shows between it and the version before,
//!   over the pack's examples and the precision items ([`Observed`]).
//!
//! The core runs with empty term lists, so only the pack rules decide.
//! The rules run as the manifest's current versions, as this build
//! ships them, whatever an install has applied.

use std::sync::{Arc, OnceLock};

use lodestar_classify::{Classification, ContentType, FileInfo, classify};

use crate::core::{Action, Core, CsList, Item, Lists, Switches};
use crate::engine::{Engine, RuleRef};
use crate::matcher::Facts;
use crate::packs::manifest::{Direction, Manifest, Version};
use crate::packs::upgrade::{Applied, PackVersions, Rows};
use crate::packs::{Ctx, Pack, PackRule, PackSwitches};

/// The largest corpus file read.
pub const MAX_BYTES: usize = 1 << 20;
/// The longest line, in bytes. SP-2's examples need names over 300
/// characters.
pub const MAX_LINE: usize = 4_096;
/// The most examples or items in one file.
pub const MAX_ITEMS: usize = 1_000;
/// The most files in one example.
pub const MAX_FILES: usize = 64;

/// The fewest must-match and must-not-match examples a stateless rule
/// has (§20.8 gate).
pub const MIN_EXAMPLES: usize = 3;

/// The rules §20.6 gives a false-positive guard: each needs a precision
/// item that exercises it (§20.8 gate).
pub const GUARDED: &[PackRule] = &[
    PackRule::Mw2,
    PackRule::Mw3,
    PackRule::Mw4,
    PackRule::Mw5,
    PackRule::Lu1,
    PackRule::Lu2,
    PackRule::Fk1,
    PackRule::Fk2,
    PackRule::Fk5,
    PackRule::Sp2,
    PackRule::Sc1,
    PackRule::Sc2,
    PackRule::Mw6,
    PackRule::Uc1,
    PackRule::Uc2,
    PackRule::Uc3,
];

/// The fewest items the precision corpus has (§20.8 gate).
pub const MIN_PRECISION: usize = 20;

/// Pack rules that depend on state, exempt from the per-item corpus:
/// SP-3 (flooding) and SP-4 (clones of hidden spam) are checked by
/// sequence fixtures through their own engine entry point (RP-4, RP-5),
/// which also check their versions' directions.
pub const STATEFUL: &[&str] = &["SP-3", "SP-4"];

/// Whether a rule takes per-item corpus examples.
pub fn stateless(rule: PackRule) -> bool {
    !STATEFUL.contains(&rule.id())
}

/// A pack's corpus, compiled in.
pub fn pack_text(pack: Pack) -> (&'static str, &'static str) {
    match pack {
        Pack::Malware => (
            "crates/filter/data/packs/malware/corpus.txt",
            include_str!("../data/packs/malware/corpus.txt"),
        ),
        Pack::Lures => (
            "crates/filter/data/packs/lures/corpus.txt",
            include_str!("../data/packs/lures/corpus.txt"),
        ),
        Pack::Fakes => (
            "crates/filter/data/packs/fakes/corpus.txt",
            include_str!("../data/packs/fakes/corpus.txt"),
        ),
        Pack::Spam => (
            "crates/filter/data/packs/spam/corpus.txt",
            include_str!("../data/packs/spam/corpus.txt"),
        ),
        Pack::Scams => (
            "crates/filter/data/packs/scams/corpus.txt",
            include_str!("../data/packs/scams/corpus.txt"),
        ),
        Pack::UnauthorisedCopy => (
            "crates/filter/data/packs/unauthorised-copy/corpus.txt",
            include_str!("../data/packs/unauthorised-copy/corpus.txt"),
        ),
    }
}

/// The precision corpus, compiled in.
pub const PRECISION: (&str, &str) = (
    "crates/filter/data/packs/precision.txt",
    include_str!("../data/packs/precision.txt"),
);

/// A corpus file that won't read: where, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusError {
    pub file: String,
    /// 1-based; 0 for the file as a whole.
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for CorpusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}: {}", self.file, self.line, self.message)
    }
}

impl std::error::Error for CorpusError {}

/// One example torrent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Example {
    /// The line its name is on.
    pub line: usize,
    pub name: String,
    pub files: Vec<FileInfo>,
    /// Judge it as this type, not the classified one.
    pub content_type: Option<ContentType>,
}

/// What a pack corpus example expects of its rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    Match,
    Miss,
}

impl Expect {
    pub fn as_str(self) -> &'static str {
        match self {
            Expect::Match => "match",
            Expect::Miss => "miss",
        }
    }
}

/// A pack corpus example.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Case {
    pub rule: PackRule,
    pub expect: Expect,
    pub example: Example,
}

/// A precision corpus item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrecisionItem {
    pub example: Example,
    /// The rules whose guards it exercises.
    pub guards: Vec<PackRule>,
    /// The rules known to hold it, their guard not built yet.
    pub gaps: Vec<PackRule>,
}

/// Every corpus, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Corpus {
    /// Per pack, in [`Pack::ALL`] order: its file and its examples.
    pub packs: Vec<(Pack, &'static str, Vec<Case>)>,
    pub precision: Vec<PrecisionItem>,
}

impl Corpus {
    /// The compiled-in corpora, parsed once. An error names the file and
    /// line; it's a build defect the tests catch, never a panic.
    pub fn builtin() -> Result<&'static Corpus, &'static CorpusError> {
        static BUILTIN: OnceLock<Result<Corpus, CorpusError>> = OnceLock::new();
        BUILTIN
            .get_or_init(|| {
                let mut packs = Vec::with_capacity(Pack::ALL.len());
                for pack in Pack::ALL {
                    let (file, text) = pack_text(pack);
                    packs.push((pack, file, parse_pack(pack, file, text)?));
                }
                let precision = parse_precision(PRECISION.0, PRECISION.1)?;
                Ok(Corpus { packs, precision })
            })
            .as_ref()
    }

    /// A pack's examples.
    pub fn cases(&self, pack: Pack) -> &[Case] {
        self.packs
            .iter()
            .find(|(p, _, _)| *p == pack)
            .map_or(&[], |(_, _, c)| c.as_slice())
    }

    /// Runs every corpus against `manifest`'s current versions.
    pub fn run(&self, manifest: &Arc<Manifest>) -> Report {
        let judge = Judge::new(manifest);
        let mut packs = Vec::with_capacity(Pack::ALL.len());
        for pack in Pack::ALL {
            let cases = self.cases(pack);
            let rules = PackRule::ALL
                .into_iter()
                .filter(|r| r.pack() == pack && stateless(*r))
                .map(|rule| {
                    let current = manifest.entry(rule).current();
                    let items = cases
                        .iter()
                        .filter(|c| c.rule == rule)
                        .map(|c| {
                            let got = judge.rule(rule, &c.example, &classified(&c.example));
                            let pass = match c.expect {
                                Expect::Match => got == Some((RuleRef::Pack(rule), current.action)),
                                Expect::Miss => got.is_none(),
                            };
                            ItemResult {
                                line: c.example.line,
                                name: c.example.name.clone(),
                                expect: c.expect,
                                got,
                                pass,
                            }
                        })
                        .collect();
                    RuleReport {
                        rule,
                        version: current.version,
                        action: current.action,
                        items,
                    }
                })
                .collect();
            packs.push(PackReport { pack, rules });
        }
        let precision = self
            .precision
            .iter()
            .map(|p| {
                let class = classified(&p.example);
                PrecisionResult {
                    line: p.example.line,
                    name: p.example.name.clone(),
                    guards: p.guards.clone(),
                    gaps: p.gaps.clone(),
                    held: PackRule::ALL
                        .into_iter()
                        .filter(|r| stateless(*r))
                        .filter_map(|r| judge.rule(r, &p.example, &class).map(|(_, a)| (r, a)))
                        .collect(),
                }
            })
            .collect();
        let directions = self.directions(manifest);
        Report {
            packs,
            precision,
            directions,
        }
    }

    /// Each stateless rule's versions, checked against the corpus.
    fn directions(&self, manifest: &Manifest) -> Vec<DirectionCheck> {
        let mut out = Vec::new();
        for rule in PackRule::ALL.into_iter().filter(|r| stateless(*r)) {
            let examples: Vec<(&Example, Classification)> = self
                .cases(rule.pack())
                .iter()
                .map(|c| &c.example)
                .chain(self.precision.iter().map(|p| &p.example))
                .map(|e| (e, classified(e)))
                .collect();
            let mut prev: Option<&Version> = None;
            for v in manifest.entry(rule).versions() {
                let gone = v.code.is_none() || prev.is_some_and(|p| p.code.is_none());
                let observed = if gone {
                    Observed::NotChecked
                } else {
                    let (mut old_only, mut new_only) = (false, false);
                    for (e, class) in &examples {
                        let (new, old) = with_ctx(e, class, |ctx| {
                            (
                                v.matches(rule, ctx) == Some(true),
                                prev.is_some_and(|p| p.matches(rule, ctx) == Some(true)),
                            )
                        });
                        old_only |= old && !new;
                        new_only |= new && !old;
                    }
                    observe(old_only, new_only, prev.map(|p| v.action.cmp(&p.action)))
                };
                out.push(DirectionCheck {
                    rule,
                    version: v.version,
                    recorded: v.direction,
                    observed,
                });
                prev = Some(v);
            }
        }
        out
    }
}

/// The direction the corpus shows between two versions: which side
/// matches an example the other doesn't, and how the action changed
/// (`None`: the first version, against nothing).
fn observe(old_only: bool, new_only: bool, action: Option<std::cmp::Ordering>) -> Observed {
    use std::cmp::Ordering::*;
    let matching = match (old_only, new_only) {
        (true, true) => return Observed::Shown(Direction::Mixed),
        (true, false) => Some(Direction::Narrows),
        (false, true) => Some(Direction::Widens),
        (false, false) => None,
    };
    match (matching, action.unwrap_or(Equal)) {
        (None, Equal) => Observed::NoChange,
        (None, Greater) => Observed::Shown(Direction::ActionHeavier),
        (None, Less) => Observed::Shown(Direction::ActionLighter),
        (Some(d), Equal) => Observed::Shown(d),
        (Some(_), _) => Observed::Shown(Direction::Mixed),
    }
}

/// The example's classification, with its `type` if it has one.
fn classified(e: &Example) -> Classification {
    let mut class = classify(&e.name, &e.files);
    if let Some(t) = e.content_type {
        class.content_type = t;
    }
    class
}

/// Runs `f` on the example as the engine sees it: its item and facts,
/// nothing neutralized.
fn with_ctx<T>(e: &Example, class: &Classification, f: impl FnOnce(&Ctx<'_>) -> T) -> T {
    let (item, facts) = item_facts(e, class);
    f(&Ctx::new(&item, &facts, &[]))
}

fn item_facts<'a>(e: &'a Example, class: &'a Classification) -> (Item<'a>, Facts<'a>) {
    let facts = Facts {
        class,
        size: e.files.iter().map(|f| f.size).fold(0, u64::saturating_add),
        piece_len: 0,
        flags: 0,
        discovered_at: 0,
        now: 0,
    };
    let item = Item {
        hashes: &[],
        name: &e.name,
        files: &e.files,
        content_type: class.content_type,
    };
    (item, facts)
}

/// Engines with one rule on each, by [`PackRule::index`].
struct Judge {
    engines: Vec<Engine>,
}

impl Judge {
    fn new(manifest: &Arc<Manifest>) -> Judge {
        let lists = Lists {
            sexual: Default::default(),
            child_roles: Default::default(),
            codes: Default::default(),
        };
        let mut base = Engine::new(Core::new(lists, CsList::default(), Switches::default()));
        base.manifest = manifest.clone();
        base.pack_versions = current_versions(manifest);
        let mut engines = vec![base.clone(); PackRule::COUNT];
        for rule in PackRule::ALL {
            engines[rule.index()].packs = PackSwitches {
                on: [rule.pack()].into_iter().collect(),
                rules_off: PackRule::ALL
                    .into_iter()
                    .filter(|r| *r != rule && r.pack() == rule.pack())
                    .collect(),
            };
        }
        Judge { engines }
    }

    /// The decision with only `rule` on: what decided and its action,
    /// if anything did. The core's lists are empty and nothing else is
    /// set, so only that rule should.
    /// `class` is the example's [`classified`].
    fn rule(
        &self,
        rule: PackRule,
        e: &Example,
        class: &Classification,
    ) -> Option<(RuleRef, Action)> {
        let engine = self.engines.get(rule.index())?;
        let (item, facts) = item_facts(e, class);
        let d = engine.judge(&item, Some(&facts), 0).decision?;
        Some((d.rule, d.action))
    }
}

/// Every rule at its current version, accepted: a readable record of
/// them, so nothing is pending.
fn current_versions(manifest: &Manifest) -> PackVersions {
    let mut a = Applied::default();
    for r in PackRule::ALL {
        a.0.insert(r.id().to_owned(), manifest.entry(r).current().version);
    }
    let text = a.encode();
    let rows = Rows {
        applied: [Some(Ok(text.clone())), Some(Ok(text))],
        marker: Some(Ok("1".to_owned())),
    };
    PackVersions::resolve(manifest, &rows)
}

/// What every corpus showed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub packs: Vec<PackReport>,
    pub precision: Vec<PrecisionResult>,
    pub directions: Vec<DirectionCheck>,
}

impl Report {
    /// Whether everything passed.
    pub fn passes(&self) -> bool {
        self.failures().is_empty()
    }

    /// One line per failure, naming the rule, the file line and why. A
    /// rule with fewer than [`MIN_EXAMPLES`] examples of either kind, a
    /// precision corpus under [`MIN_PRECISION`] items, and a [`GUARDED`]
    /// rule no precision item exercises fail too.
    pub fn failures(&self) -> Vec<String> {
        let mut out = Vec::new();
        for p in &self.packs {
            for r in &p.rules {
                for expect in [Expect::Match, Expect::Miss] {
                    let n = r.count(expect);
                    if n < MIN_EXAMPLES {
                        out.push(format!(
                            "{} has {n} `{}` examples; it needs at least {MIN_EXAMPLES}",
                            r.rule.id(),
                            expect.as_str()
                        ));
                    }
                }
                for i in r.items.iter().filter(|i| !i.pass) {
                    out.push(format!(
                        "{} line {}: {} {:?}: {}",
                        r.rule.id(),
                        i.line,
                        i.expect.as_str(),
                        i.name,
                        match i.got {
                            None => "nothing held it".to_owned(),
                            Some((by, a)) => format!("{} took {}", by.id(), a.as_str()),
                        }
                    ));
                }
            }
        }
        if self.precision.len() < MIN_PRECISION {
            out.push(format!(
                "the precision corpus has {} items; it needs at least {MIN_PRECISION}",
                self.precision.len()
            ));
        }
        for g in GUARDED {
            if !self.precision.iter().any(|p| p.guards.contains(g)) {
                out.push(format!("no precision item exercises {}'s guard", g.id()));
            }
        }
        for p in &self.precision {
            let by: Vec<String> = p
                .held
                .iter()
                .filter(|(r, _)| !p.gaps.contains(r))
                .map(|(r, a)| format!("{} {}", r.id(), a.as_str()))
                .collect();
            if !by.is_empty() {
                out.push(format!(
                    "precision line {}: {:?} held by {}",
                    p.line,
                    p.name,
                    by.join(", ")
                ));
            }
            for g in p
                .gaps
                .iter()
                .filter(|g| !p.held.iter().any(|(r, _)| r == *g))
            {
                out.push(format!(
                    "precision line {}: {:?} is no longer held by {}: remove its `gap` line",
                    p.line,
                    p.name,
                    g.id()
                ));
            }
        }
        for d in self.directions.iter().filter(|d| !d.agrees()) {
            out.push(format!(
                "{}@{} records {} but the corpus shows {}",
                d.rule.id(),
                d.version,
                d.recorded.as_str(),
                match d.observed {
                    Observed::Shown(o) => o.as_str(),
                    Observed::NoChange => "no change (add an example that shows it)",
                    Observed::NotChecked => "nothing",
                }
            ));
        }
        out
    }

    /// The known gaps: each precision item a rule holds because its
    /// guard isn't built yet, by line.
    pub fn gaps(&self) -> Vec<(PackRule, usize)> {
        self.precision
            .iter()
            .flat_map(|p| p.gaps.iter().map(|g| (*g, p.line)))
            .collect()
    }

    pub fn rule(&self, rule: PackRule) -> Option<&RuleReport> {
        self.packs
            .iter()
            .flat_map(|p| &p.rules)
            .find(|r| r.rule == rule)
    }
}

/// One pack's rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackReport {
    pub pack: Pack,
    pub rules: Vec<RuleReport>,
}

/// One rule's examples.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleReport {
    pub rule: PackRule,
    /// The version judged: the build's current one.
    pub version: u32,
    pub action: Action,
    pub items: Vec<ItemResult>,
}

impl RuleReport {
    pub fn count(&self, expect: Expect) -> usize {
        self.items.iter().filter(|i| i.expect == expect).count()
    }

    /// How many examples the rule got wrong.
    pub fn disagreements(&self) -> usize {
        self.items.iter().filter(|i| !i.pass).count()
    }
}

/// One example's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemResult {
    pub line: usize,
    pub name: String,
    pub expect: Expect,
    /// What decided, with only the rule on.
    pub got: Option<(RuleRef, Action)>,
    pub pass: bool,
}

/// One precision item's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrecisionResult {
    pub line: usize,
    pub name: String,
    pub guards: Vec<PackRule>,
    /// The rules known to hold it ([`PrecisionItem::gaps`]).
    pub gaps: Vec<PackRule>,
    /// Every rule that holds it, each judged alone: empty, or exactly its
    /// gaps, when it passes.
    pub held: Vec<(PackRule, Action)>,
}

/// What the corpus shows a version changed against the one before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observed {
    /// An example only one side matches, or a different action.
    Shown(Direction),
    /// Both sides match the same examples with the same action: the
    /// corpus has nothing that shows the change.
    NoChange,
    /// The code of this version or the one before is gone from the build.
    NotChecked,
}

/// One version's recorded direction against the corpus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectionCheck {
    pub rule: PackRule,
    pub version: u32,
    pub recorded: Direction,
    pub observed: Observed,
}

impl DirectionCheck {
    /// The record agrees with what the corpus shows. `mixed` agrees with
    /// any change, since it already counts as weakening; a version whose
    /// change the corpus doesn't show never agrees, so a bump comes with
    /// an example of it.
    pub fn agrees(&self) -> bool {
        match self.observed {
            Observed::Shown(o) => o == self.recorded || self.recorded == Direction::Mixed,
            Observed::NoChange => false,
            Observed::NotChecked => true,
        }
    }
}

// ---------------------------------------------------------------------
// The parser.

/// Which file is being read.
#[derive(Clone, Copy)]
enum Kind {
    Pack(Pack),
    Precision,
}

/// A pack corpus.
pub fn parse_pack(pack: Pack, file: &str, text: &str) -> Result<Vec<Case>, CorpusError> {
    let mut rule = None;
    let mut out: Vec<Case> = Vec::new();
    parse(file, text, Kind::Pack(pack), |ev| match ev {
        Event::Rule(r) => {
            rule = Some(r);
            Ok(())
        }
        Event::Example(expect, example) => {
            let rule = rule.ok_or("an example before any `rule` line")?;
            // A copy can't pad a rule's minimum.
            if let Some(c) = out.iter().find(|c| {
                c.rule == rule
                    && c.expect == expect
                    && c.example.name == example.name
                    && c.example.files == example.files
                    && c.example.content_type == example.content_type
            }) {
                return Err(format!(
                    "a copy of the {} example on line {}",
                    rule.id(),
                    c.example.line
                ));
            }
            out.push(Case {
                rule,
                expect,
                example,
            });
            Ok(())
        }
        Event::Precision(..) => Err("`item` is for the precision corpus".to_owned()),
    })?;
    Ok(out)
}

/// The precision corpus.
pub fn parse_precision(file: &str, text: &str) -> Result<Vec<PrecisionItem>, CorpusError> {
    let mut out = Vec::new();
    parse(file, text, Kind::Precision, |ev| match ev {
        Event::Precision(example, guards, gaps) => {
            out.push(PrecisionItem {
                example,
                guards,
                gaps,
            });
            Ok(())
        }
        _ => Err("`rule`, `match` and `miss` are for pack corpora".to_owned()),
    })?;
    Ok(out)
}

enum Event {
    Rule(PackRule),
    Example(Expect, Example),
    Precision(Example, Vec<PackRule>, Vec<PackRule>),
}

/// The example being read: its kind and its guards so far.
struct Open {
    expect: Option<Expect>,
    example: Example,
    guards: Option<Vec<PackRule>>,
    gaps: Option<Vec<PackRule>>,
    /// The `gap` line, for its errors.
    gap_line: usize,
}

fn parse(
    file: &str,
    text: &str,
    kind: Kind,
    mut emit: impl FnMut(Event) -> Result<(), String>,
) -> Result<(), CorpusError> {
    let err = |line: usize, message: String| CorpusError {
        file: file.to_owned(),
        line,
        message,
    };
    if text.len() > MAX_BYTES {
        return Err(err(0, format!("larger than {MAX_BYTES} bytes")));
    }
    let mut open: Option<Open> = None;
    let mut items = 0usize;
    // Hands the open example over, checked.
    let close = |open: &mut Option<Open>,
                 emit: &mut dyn FnMut(Event) -> Result<(), String>|
     -> Result<(), CorpusError> {
        let Some(o) = open.take() else {
            return Ok(());
        };
        let line = o.example.line;
        let ev = match (kind, o.expect) {
            (Kind::Pack(_), Some(expect)) => Event::Example(expect, o.example),
            (Kind::Precision, _) => {
                let guards = o
                    .guards
                    .ok_or_else(|| err(line, "an item needs a `guards` line".to_owned()))?;
                let gaps = o.gaps.unwrap_or_default();
                if let Some(g) = gaps.iter().find(|g| !guards.contains(g)) {
                    return Err(err(
                        o.gap_line,
                        format!("the gap {} isn't one of the item's `guards`", g.id()),
                    ));
                }
                Event::Precision(o.example, guards, gaps)
            }
            (Kind::Pack(_), None) => return Err(err(line, "an example needs a kind".to_owned())),
        };
        emit(ev).map_err(|m| err(line, m))
    };
    for (i, raw) in text.split('\n').enumerate() {
        let n = i + 1;
        if raw.len() > MAX_LINE {
            return Err(err(n, format!("longer than {MAX_LINE} bytes")));
        }
        let line = raw.strip_suffix('\r').unwrap_or(raw).trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = match line.split_once(char::is_whitespace) {
            Some((k, v)) => (k, v.trim()),
            None => (line, ""),
        };
        match key {
            "rule" | "match" | "miss" | "item" => {
                close(&mut open, &mut emit)?;
                let pack_only = key != "item";
                match (kind, pack_only) {
                    (Kind::Precision, true) => {
                        return Err(err(n, format!("`{key}` is for pack corpora")));
                    }
                    (Kind::Pack(_), false) => {
                        return Err(err(n, "`item` is for the precision corpus".to_owned()));
                    }
                    _ => {}
                }
                if key == "rule" {
                    let rule = pack_rule(value, kind).map_err(|m| err(n, m))?;
                    emit(Event::Rule(rule)).map_err(|m| err(n, m))?;
                    continue;
                }
                if value.is_empty() {
                    return Err(err(n, format!("`{key}` needs a name")));
                }
                items += 1;
                if items > MAX_ITEMS {
                    return Err(err(n, format!("more than {MAX_ITEMS} examples")));
                }
                open = Some(Open {
                    expect: match key {
                        "match" => Some(Expect::Match),
                        "miss" => Some(Expect::Miss),
                        _ => None,
                    },
                    example: Example {
                        line: n,
                        name: value.to_owned(),
                        files: Vec::new(),
                        content_type: None,
                    },
                    guards: None,
                    gaps: None,
                    gap_line: 0,
                });
            }
            "file" | "pad" | "type" | "guards" | "gap" => {
                let Some(o) = open.as_mut() else {
                    return Err(err(n, format!("`{key}` before any example")));
                };
                match key {
                    "file" | "pad" => {
                        if o.example.files.len() >= MAX_FILES {
                            return Err(err(n, format!("more than {MAX_FILES} files")));
                        }
                        let mut f = file_line(value).map_err(|m| err(n, m))?;
                        f.padding = key == "pad";
                        o.example.files.push(f);
                    }
                    "type" => {
                        if o.example.content_type.is_some() {
                            return Err(err(n, "a second `type`".to_owned()));
                        }
                        let t = ContentType::from_name(value)
                            .ok_or_else(|| err(n, format!("no content type {:?}", short(value))))?;
                        o.example.content_type = Some(t);
                    }
                    _ => {
                        if !matches!(kind, Kind::Precision) {
                            return Err(err(n, format!("`{key}` is for the precision corpus")));
                        }
                        let slot = if key == "guards" {
                            &mut o.guards
                        } else {
                            &mut o.gaps
                        };
                        if slot.is_some() {
                            return Err(err(n, format!("a second `{key}`")));
                        }
                        let mut rules = Vec::new();
                        for id in value.split_whitespace() {
                            let r = pack_rule(id, kind).map_err(|m| err(n, m))?;
                            if !rules.contains(&r) {
                                rules.push(r);
                            }
                        }
                        if rules.is_empty() {
                            return Err(err(n, format!("`{key}` needs a rule")));
                        }
                        *slot = Some(rules);
                        if key == "gap" {
                            o.gap_line = n;
                        }
                    }
                }
            }
            _ => {
                return Err(err(n, format!("unknown keyword {:?}", short(key))));
            }
        }
    }
    close(&mut open, &mut emit)
}

/// A value cut to 40 characters for an error message.
fn short(s: &str) -> String {
    s.chars().take(40).collect()
}

/// A built stateless rule, in the file's pack for a pack corpus.
fn pack_rule(id: &str, kind: Kind) -> Result<PackRule, String> {
    let shown = short(id);
    if STATEFUL.contains(&id) {
        return Err(format!(
            "{shown} depends on state: its cases are sequence fixtures, not items"
        ));
    }
    let rule = PackRule::from_id(id).ok_or_else(|| format!("no pack rule {shown:?}"))?;
    if let Kind::Pack(pack) = kind
        && rule.pack() != pack
    {
        return Err(format!(
            "{shown} is in the {} pack, not {}",
            rule.pack().as_str(),
            pack.as_str()
        ));
    }
    Ok(rule)
}

/// `<size> <path>`.
fn file_line(value: &str) -> Result<FileInfo, String> {
    let (size, path) = value
        .split_once(char::is_whitespace)
        .ok_or("a file needs a size and a path")?;
    let size = parse_size(size)?;
    let path = path.trim();
    let parts: Vec<String> = path.split('/').map(str::to_owned).collect();
    if parts.iter().any(String::is_empty) {
        return Err(format!("an empty folder or file name in {:?}", short(path)));
    }
    Ok(FileInfo::new(parts, size))
}

/// Bytes, or a number with `K`, `M`, `G` or `T`.
fn parse_size(s: &str) -> Result<u64, String> {
    let shown = short(s);
    let bad = || format!("{shown:?} isn't a size");
    let (digits, shift) = match s.as_bytes().last() {
        Some(b'K') => (&s[..s.len() - 1], 10),
        Some(b'M') => (&s[..s.len() - 1], 20),
        Some(b'G') => (&s[..s.len() - 1], 30),
        Some(b'T') => (&s[..s.len() - 1], 40),
        _ => (s, 0),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let n: u64 = digits.parse().map_err(|_| bad())?;
    n.checked_mul(1u64 << shift).ok_or_else(bad)
}

#[cfg(test)]
mod tests {
    use super::*;
    use Direction::*;
    use std::cmp::Ordering::*;

    #[test]
    fn sizes() {
        assert_eq!(parse_size("0"), Ok(0));
        assert_eq!(parse_size("1500"), Ok(1500));
        assert_eq!(parse_size("2K"), Ok(2048));
        assert_eq!(parse_size("3M"), Ok(3 << 20));
        assert_eq!(parse_size("4G"), Ok(4 << 30));
        assert_eq!(parse_size("5T"), Ok(5 << 40));
        assert_eq!(parse_size("16777215T"), Ok(16_777_215 << 40));
        for bad in ["", "K", "1k", "1.5G", "+1", "-1", "1 G", "16777216T", "١"] {
            assert!(parse_size(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn what_the_corpus_shows() {
        assert_eq!(observe(false, true, None), Observed::Shown(Widens));
        assert_eq!(observe(false, false, None), Observed::NoChange);
        assert_eq!(observe(false, true, Some(Equal)), Observed::Shown(Widens));
        assert_eq!(observe(true, false, Some(Equal)), Observed::Shown(Narrows));
        assert_eq!(observe(true, true, Some(Equal)), Observed::Shown(Mixed));
        assert_eq!(
            observe(false, false, Some(Greater)),
            Observed::Shown(ActionHeavier)
        );
        assert_eq!(
            observe(false, false, Some(Less)),
            Observed::Shown(ActionLighter)
        );
        assert_eq!(observe(false, true, Some(Greater)), Observed::Shown(Mixed));
        assert_eq!(observe(true, false, Some(Less)), Observed::Shown(Mixed));
        assert_eq!(observe(false, false, Some(Equal)), Observed::NoChange);
        let check = |recorded, observed| DirectionCheck {
            rule: PackRule::Mw1,
            version: 2,
            recorded,
            observed,
        };
        for d in Direction::ALL {
            assert!(check(d, Observed::Shown(d)).agrees());
            assert!(check(Mixed, Observed::Shown(d)).agrees());
            assert!(!check(d, Observed::NoChange).agrees());
            assert!(check(d, Observed::NotChecked).agrees());
        }
        assert!(!check(Widens, Observed::Shown(Narrows)).agrees());
        assert!(!check(ActionHeavier, Observed::Shown(Mixed)).agrees());
    }

    #[test]
    fn the_builtin_corpus_parses() {
        let c = Corpus::builtin().expect("the shipped corpus parses");
        assert_eq!(c.packs.len(), Pack::ALL.len());
        for pack in Pack::ALL {
            assert!(!c.cases(pack).is_empty(), "{}", pack.as_str());
            assert!(c.cases(pack).iter().all(|x| x.rule.pack() == pack));
        }
        assert!(!c.precision.is_empty());
    }

    #[test]
    fn only_the_rule_judged_decides() {
        let c = Corpus::builtin().expect("parses");
        let judge = Judge::new(&Manifest::builtin());
        // An example that MW-1, MW-2 and MW-5 would all hold.
        let e = Example {
            line: 1,
            name: "Example.Movie.2019.1080p.WEB".into(),
            files: vec![
                FileInfo::new(vec!["Example.Movie.2019.1080p.WEB.mkv".into()], 4 << 30),
                FileInfo::new(vec!["trailer.mp4.exe".into()], 1 << 20),
                FileInfo::new(vec!["x.scr".into()], 1 << 20),
            ],
            content_type: None,
        };
        for r in [PackRule::Mw1, PackRule::Mw2, PackRule::Mw5] {
            assert_eq!(
                judge.rule(r, &e, &classified(&e)),
                Some((RuleRef::Pack(r), Action::Hide))
            );
        }
        assert_eq!(judge.rule(PackRule::Sc1, &e, &classified(&e)), None);
        assert!(c.run(&Manifest::builtin()).passes());
    }
}
