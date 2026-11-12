//! Regex search's front half (architecture §8, plan 2026-11-05 RX-0): a
//! user's pattern, which is untrusted input, parsed under hard limits into
//! a trigram [`Plan`], the AND/OR of `name_trigram` terms that every name
//! the pattern matches must contain (the Google Code Search / zoekt
//! technique). It's pure: no index access and no matching engine. RX-1
//! runs the plan as a prefilter and verifies the candidates with `regex`.
//!
//! Every bound is checked before the work it guards grows: the pattern's
//! length before parsing, nesting during parsing (the parser's own nest
//! limit), node count, repetition, alternation, flags, the codepoints the
//! classes cover and an estimate of the compiled size on the syntax tree
//! before it's translated (case folding a wide class is the costly step). Each refusal is a [`RegexError`] with a stable
//! [`Reason::code`]. The plan's own size is held under [`MAX_PLAN_NODES`]
//! by weakening it, which never loses a match (a smaller AND, or no
//! constraint at all), so a valid pattern is never refused for its plan.
//!
//! **Soundness.** A gram is three consecutive characters of the name, each
//! lowercased on its own (`char::to_lowercase`), exactly as
//! [`crate::schema::trigram_analyzer`] makes them. The plan is built from
//! the same folding, so if the pattern matches anywhere in a name, the
//! name's grams satisfy the plan. Patterns are case-insensitive by default
//! ([`CASE_INSENSITIVE`]); `(?-i)` turns that off.

use std::collections::BTreeSet;
use std::fmt;

use regex_syntax::ast::{self, Ast};
use regex_syntax::hir::{Class, ClassUnicode, Hir, HirKind};

/// The longest pattern accepted, in bytes (checked before parsing).
pub const MAX_PATTERN_BYTES: usize = 512;
/// The most syntax-tree nodes a pattern may have, class items included.
pub const MAX_AST_NODES: usize = 256;
/// The deepest nesting of groups and classes (the parser's nest limit).
pub const MAX_DEPTH: u32 = 16;
/// The largest count a repetition (`{n}`, `{n,}`, `{n,m}`) may name.
pub const MAX_REPEAT: u32 = 100;
/// The largest product of nested repetition counts, e.g. `(a{10}){10}`
/// is 100: what the verifier's automaton would multiply out.
pub const MAX_REPEAT_PRODUCT: u64 = 1_000;
/// The most branches one alternation may have.
pub const MAX_ALTERNATION: usize = 32;
/// The most codepoints the pattern's Unicode classes (`\pL`, `\w`,
/// `[a-z]`, …) may cover between them, counted before translation. Case
/// folding works through every codepoint of a class, so this is what
/// bounds translation (and the verifier's own folding): Unicode `\w` is
/// about 140,000, `\p{Any}` 1,114,112.
pub const MAX_CLASS_CODEPOINTS: u64 = 2_500_000;
/// The largest estimated compiled size: each class item's ranges (one for
/// a literal), times the counts of the repetitions around it. Unicode
/// `\w` is about 770 ranges, so `\w{60}` is about 46,000. It's an
/// estimate made before translation; RX-1's verifier keeps `regex`'s own
/// size limit too.
pub const MAX_COMPILED_WEIGHT: u64 = 50_000;
/// Unicode scalar values: every codepoint but the 2,048 surrogates.
const UNICODE_SCALARS: u64 = 0x11_0000 - 0x800;
/// A backstop on one translated class's ranges. The codepoint budget
/// keeps real classes well under it (the largest is about 1,600).
pub const MAX_CLASS_RANGES: usize = 4_096;
/// The most nodes a plan may have; past it the plan is weakened.
pub const MAX_PLAN_NODES: usize = 256;
/// Patterns are case-insensitive unless they say `(?-i)`.
pub const CASE_INSENSITIVE: bool = true;

/// The most strings an exact, prefix or suffix set holds.
const MAX_SET: usize = 32;
/// The longest string, in characters, an exact set holds.
const MAX_SEQ_CHARS: usize = 64;
/// The largest class enumerated character by character; a bigger one
/// counts as "any character".
const MAX_CLASS_EXPAND: u64 = 16;
/// Recursion guard for the translated tree. Translation adds at most a
/// couple of levels per syntax level, so the nest limit keeps it well
/// under this; it's a backstop, not a user-facing limit.
const MAX_HIR_DEPTH: u32 = 4 * MAX_DEPTH + 8;

/// Why a pattern was refused. The codes are stable: the API and UI key
/// their messages on them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Reason {
    /// The pattern is empty.
    Empty,
    /// Longer than [`MAX_PATTERN_BYTES`].
    TooLong,
    /// Not a valid pattern (includes look-around and backreferences,
    /// which aren't supported).
    Syntax,
    /// Nested deeper than [`MAX_DEPTH`].
    TooDeep,
    /// More than [`MAX_AST_NODES`] nodes.
    TooManyNodes,
    /// A count over [`MAX_REPEAT`], or nested counts over
    /// [`MAX_REPEAT_PRODUCT`].
    RepeatTooLarge,
    /// An alternation wider than [`MAX_ALTERNATION`].
    AlternationTooWide,
    /// Classes covering more than [`MAX_CLASS_CODEPOINTS`], or one class
    /// over [`MAX_CLASS_RANGES`].
    ClassTooLarge,
    /// An estimated compiled size over [`MAX_COMPILED_WEIGHT`].
    TooLarge,
    /// A flag other than `i` (`m`, `s`, `x`, `u`, `U`, `R`).
    FlagNotAllowed,
}

impl Reason {
    /// The stable reason code.
    pub fn code(self) -> &'static str {
        match self {
            Reason::Empty => "regex_empty",
            Reason::TooLong => "regex_too_long",
            Reason::Syntax => "regex_syntax",
            Reason::TooDeep => "regex_too_deep",
            Reason::TooManyNodes => "regex_too_many_nodes",
            Reason::RepeatTooLarge => "regex_repeat_too_large",
            Reason::AlternationTooWide => "regex_alternation_too_wide",
            Reason::ClassTooLarge => "regex_class_too_large",
            Reason::TooLarge => "regex_too_large",
            Reason::FlagNotAllowed => "regex_flag_not_allowed",
        }
    }
}

/// A refused pattern: the reason and a short message for the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegexError {
    pub reason: Reason,
    pub detail: String,
}

impl RegexError {
    fn new(reason: Reason, detail: impl Into<String>) -> RegexError {
        RegexError {
            reason,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for RegexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.detail, self.reason.code())
    }
}

impl std::error::Error for RegexError {}

/// A parsed pattern and its prefilter plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pattern {
    source: String,
    plan: Plan,
}

impl Pattern {
    /// The pattern as given.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The trigram plan. [`Plan::All`] means no prefilter: the pattern
    /// can only be verified name by name.
    pub fn plan(&self) -> &Plan {
        &self.plan
    }

    /// The pattern with the default flags spelled out, for the verifier
    /// to compile: it matches exactly what this module planned for.
    pub fn verify_pattern(&self) -> String {
        if CASE_INSENSITIVE {
            format!("(?i){}", self.source)
        } else {
            self.source.clone()
        }
    }
}

/// What a name's grams must satisfy for the pattern to match it.
///
/// Children of `And` and `Or` are sorted and distinct, and neither holds
/// `All`, `Nothing` or a node of its own kind, so equal plans compare
/// equal.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Plan {
    /// Every name passes: no prefilter.
    All,
    /// No name can match (e.g. an empty class).
    Nothing,
    /// The name has this gram (three lowercased characters).
    Gram(String),
    /// The name satisfies every child.
    And(Vec<Plan>),
    /// The name satisfies at least one child.
    Or(Vec<Plan>),
}

impl Plan {
    /// Nodes in the plan, at most [`MAX_PLAN_NODES`].
    pub fn size(&self) -> usize {
        match self {
            Plan::All | Plan::Nothing | Plan::Gram(_) => 1,
            Plan::And(kids) | Plan::Or(kids) => 1 + kids.iter().map(Plan::size).sum::<usize>(),
        }
    }

    /// Whether a name whose grams are those `has` answers yes for passes.
    pub fn admits(&self, has: &mut impl FnMut(&str) -> bool) -> bool {
        match self {
            Plan::All => true,
            Plan::Nothing => false,
            Plan::Gram(g) => has(g),
            Plan::And(kids) => kids.iter().all(|k| k.admits(has)),
            Plan::Or(kids) => kids.iter().any(|k| k.admits(has)),
        }
    }

    /// Every gram the plan names, once each.
    pub fn grams(&self) -> BTreeSet<&str> {
        let mut out = BTreeSet::new();
        let mut stack = vec![self];
        while let Some(p) = stack.pop() {
            match p {
                Plan::Gram(g) => {
                    out.insert(g.as_str());
                }
                Plan::And(kids) | Plan::Or(kids) => stack.extend(kids),
                Plan::All | Plan::Nothing => {}
            }
        }
        out
    }

    fn write(&self, f: &mut fmt::Formatter<'_>, nested: bool) -> fmt::Result {
        let (kids, op) = match self {
            Plan::All => return f.write_str("ALL"),
            Plan::Nothing => return f.write_str("NONE"),
            Plan::Gram(g) => return write!(f, "{g:?}"),
            Plan::And(kids) => (kids, " & "),
            Plan::Or(kids) => (kids, " | "),
        };
        if nested {
            f.write_str("(")?;
        }
        for (i, k) in kids.iter().enumerate() {
            if i > 0 {
                f.write_str(op)?;
            }
            k.write(f, true)?;
        }
        if nested {
            f.write_str(")")?;
        }
        Ok(())
    }
}

impl fmt::Display for Plan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.write(f, false)
    }
}

/// Parses `pattern` under every limit and builds its plan.
pub fn parse(pattern: &str) -> Result<Pattern, RegexError> {
    if pattern.is_empty() {
        return Err(RegexError::new(Reason::Empty, "the pattern is empty"));
    }
    if pattern.len() > MAX_PATTERN_BYTES {
        return Err(RegexError::new(
            Reason::TooLong,
            format!("pattern longer than {MAX_PATTERN_BYTES} bytes"),
        ));
    }
    let ast = ast::parse::ParserBuilder::new()
        .nest_limit(MAX_DEPTH)
        .build()
        .parse(pattern)
        .map_err(|e| match e.kind() {
            ast::ErrorKind::NestLimitExceeded(_) => RegexError::new(
                Reason::TooDeep,
                format!("pattern nested deeper than {MAX_DEPTH}"),
            ),
            kind => RegexError::new(Reason::Syntax, kind.to_string()),
        })?;
    ast::visit(
        &ast,
        Bounds {
            pattern,
            nodes: 0,
            repeats: Vec::new(),
            codepoints: 0,
            weight: 0,
            brackets: 0,
        },
    )?;
    let hir = regex_syntax::hir::translate::TranslatorBuilder::new()
        .utf8(true)
        .case_insensitive(CASE_INSENSITIVE)
        .build()
        .translate(pattern, &ast)
        .map_err(|e| RegexError::new(Reason::Syntax, e.kind().to_string()))?;
    drop(ast);
    let plan = finish(info(&hir, 0)?);
    Ok(Pattern {
        source: pattern.to_owned(),
        plan,
    })
}

/// The syntax-tree limits, checked in one heap-stack walk (the crate's
/// visitor never recurses).
struct Bounds<'a> {
    pattern: &'a str,
    nodes: usize,
    /// The running product of the enclosing repetitions' counts.
    repeats: Vec<u64>,
    /// Codepoints the classes cover so far.
    codepoints: u64,
    /// How many brackets enclose the class item being visited.
    brackets: u64,
    /// The compiled-size estimate so far.
    weight: u64,
}

impl Bounds<'_> {
    /// Counts a leaf: `codepoints` against the class budget once, and
    /// `ranges` times the enclosing repetitions against the size estimate.
    fn leaf(&mut self, codepoints: u64, ranges: u64) -> Result<(), RegexError> {
        self.codepoints = self.codepoints.saturating_add(codepoints);
        if self.codepoints > MAX_CLASS_CODEPOINTS {
            return Err(RegexError::new(
                Reason::ClassTooLarge,
                "character classes too large",
            ));
        }
        let times = self.repeats.last().copied().unwrap_or(1);
        self.weight = self.weight.saturating_add(ranges.saturating_mul(times));
        if self.weight > MAX_COMPILED_WEIGHT {
            return Err(RegexError::new(
                Reason::TooLarge,
                "pattern too large to run",
            ));
        }
        Ok(())
    }

    /// A Unicode or Perl class (`\pL`, `\W`, …), measured by translating
    /// its own text case-sensitively, which costs little even for
    /// `\p{Any}`: it's case folding that walks every codepoint.
    ///
    /// The translator folds a negated class *before* negating it, so
    /// `\P{Any}` (empty) folds all of `\p{Any}`: a negated class is charged
    /// its complement, which is what gets folded (`\W` costs `\w`).
    fn named_class(&mut self, span: &ast::Span, negated: bool) -> Result<(), RegexError> {
        let (width, ranges) = self.measure(span)?;
        // What gets folded is the class before negation: the complement.
        let width = if negated {
            UNICODE_SCALARS.saturating_sub(width)
        } else {
            width
        };
        self.leaf(width, ranges)
    }

    /// A class item inside brackets. The bracket folds its whole set, so
    /// whatever is negated inside it (`[\S]`, `[\P{Any}]`, `[[:^alpha:]]`,
    /// a nested `[^…]`, a set operation) can be folded at its full width
    /// on either side of the negation: those are charged every codepoint.
    /// A budget may over-charge, never under-charge (runner review B2).
    fn bracket_class(&mut self, span: &ast::Span, negated: bool) -> Result<(), RegexError> {
        let (width, ranges) = self.measure(span)?;
        self.in_bracket(if negated { UNICODE_SCALARS } else { width }, ranges)
    }

    /// Charges an item inside brackets. Translation folds the item, then
    /// each enclosing bracket folds its whole set again (measured: a
    /// `\p{Any}` one bracket deep costs about three bare ones), so the
    /// codepoints count once per bracket level and once more.
    fn in_bracket(&mut self, codepoints: u64, ranges: u64) -> Result<(), RegexError> {
        self.leaf(codepoints.saturating_mul(self.brackets + 1), ranges)
    }

    /// The codepoints and ranges of a Unicode or Perl class item's own text,
    /// translated case-sensitively.
    fn measure(&self, span: &ast::Span) -> Result<(u64, u64), RegexError> {
        let text = self
            .pattern
            .get(span.start.offset..span.end.offset)
            .unwrap_or_default();
        let hir = regex_syntax::ParserBuilder::new()
            .build()
            .parse(text)
            .map_err(|e| match e {
                regex_syntax::Error::Parse(e) => {
                    RegexError::new(Reason::Syntax, e.kind().to_string())
                }
                regex_syntax::Error::Translate(e) => {
                    RegexError::new(Reason::Syntax, e.kind().to_string())
                }
                _ => RegexError::new(Reason::Syntax, "invalid class"),
            })?;
        // An empty class (`\P{Any}`) translates to the byte class "fail".
        Ok(match hir.kind() {
            HirKind::Class(Class::Unicode(c)) => (
                c.ranges()
                    .iter()
                    .map(|r| u64::from(r.end()) - u64::from(r.start()) + 1)
                    .sum(),
                (c.ranges().len() as u64).max(1),
            ),
            _ => (0, 1),
        })
    }

    fn node(&mut self) -> Result<(), RegexError> {
        self.nodes += 1;
        if self.nodes > MAX_AST_NODES {
            return Err(RegexError::new(
                Reason::TooManyNodes,
                format!("pattern has more than {MAX_AST_NODES} parts"),
            ));
        }
        Ok(())
    }
}

fn count(n: u32) -> Result<u64, RegexError> {
    if n > MAX_REPEAT {
        return Err(RegexError::new(
            Reason::RepeatTooLarge,
            format!("repetition count over {MAX_REPEAT}"),
        ));
    }
    Ok(u64::from(n))
}

fn flags(f: &ast::Flags) -> Result<(), RegexError> {
    for item in &f.items {
        match item.kind {
            ast::FlagsItemKind::Negation | ast::FlagsItemKind::Flag(ast::Flag::CaseInsensitive) => {
            }
            ast::FlagsItemKind::Flag(_) => {
                return Err(RegexError::new(
                    Reason::FlagNotAllowed,
                    "only the i flag is allowed",
                ));
            }
        }
    }
    Ok(())
}

impl ast::Visitor for Bounds<'_> {
    type Output = ();
    type Err = RegexError;

    fn finish(self) -> Result<(), RegexError> {
        Ok(())
    }

    fn visit_pre(&mut self, ast: &Ast) -> Result<(), RegexError> {
        self.node()?;
        match ast {
            Ast::Flags(f) => flags(&f.flags)?,
            Ast::Group(g) => {
                if let ast::GroupKind::NonCapturing(f) = &g.kind {
                    flags(f)?;
                }
            }
            Ast::Alternation(a) if a.asts.len() > MAX_ALTERNATION => {
                return Err(RegexError::new(
                    Reason::AlternationTooWide,
                    format!("more than {MAX_ALTERNATION} alternatives"),
                ));
            }
            Ast::Literal(_) => self.leaf(0, 1)?,
            // Any character: a handful of UTF-8 ranges, never case-folded.
            Ast::Dot(_) => self.leaf(0, 4)?,
            Ast::ClassUnicode(c) => self.named_class(&c.span, c.is_negated())?,
            Ast::ClassPerl(c) => self.named_class(&c.span, c.negated)?,
            Ast::ClassBracketed(_) => self.brackets = 1,
            Ast::Repetition(r) => {
                use ast::{RepetitionKind as K, RepetitionRange as R};
                let factor = match r.op.kind {
                    K::ZeroOrOne | K::ZeroOrMore | K::OneOrMore => 1,
                    K::Range(R::Exactly(n)) | K::Range(R::AtLeast(n)) => count(n)?,
                    K::Range(R::Bounded(lo, hi)) => count(lo)?.max(count(hi)?),
                };
                let outer = self.repeats.last().copied().unwrap_or(1);
                let product = outer.saturating_mul(factor.max(1));
                if product > MAX_REPEAT_PRODUCT {
                    return Err(RegexError::new(
                        Reason::RepeatTooLarge,
                        format!("nested repetition counts multiply past {MAX_REPEAT_PRODUCT}"),
                    ));
                }
                self.repeats.push(product);
            }
            _ => {}
        }
        Ok(())
    }

    fn visit_post(&mut self, ast: &Ast) -> Result<(), RegexError> {
        match ast {
            Ast::Repetition(_) => {
                self.repeats.pop();
            }
            Ast::ClassBracketed(_) => self.brackets = 0,
            _ => {}
        }
        Ok(())
    }

    fn visit_class_set_item_post(&mut self, item: &ast::ClassSetItem) -> Result<(), RegexError> {
        if let ast::ClassSetItem::Bracketed(_) = item {
            self.brackets = self.brackets.saturating_sub(1);
        }
        Ok(())
    }

    fn visit_class_set_item_pre(&mut self, item: &ast::ClassSetItem) -> Result<(), RegexError> {
        self.node()?;
        match item {
            ast::ClassSetItem::Literal(_) => self.in_bracket(1, 1),
            ast::ClassSetItem::Range(r) => {
                let width = u64::from(r.end.c).saturating_sub(u64::from(r.start.c)) + 1;
                self.in_bracket(width, 1)
            }
            // `[[:alpha:]]`: ASCII only, a few ranges; `[[:^alpha:]]` is
            // everything else.
            ast::ClassSetItem::Ascii(c) if c.negated => self.in_bracket(UNICODE_SCALARS, 4),
            ast::ClassSetItem::Ascii(_) => self.in_bracket(128, 4),
            ast::ClassSetItem::Unicode(c) => self.bracket_class(&c.span, c.is_negated()),
            ast::ClassSetItem::Perl(c) => self.bracket_class(&c.span, c.negated),
            // A nested bracket is one more level of folding for its items.
            // A nested `[^…]` can be folded at full width, whatever its
            // items are (`[[^a]b]`); its items are charged on their own too.
            ast::ClassSetItem::Bracketed(b) => {
                // Charged at its parent's depth: the parent folds it.
                let charged = if b.negated {
                    self.in_bracket(UNICODE_SCALARS, 1)
                } else {
                    Ok(())
                };
                self.brackets += 1;
                charged
            }
            // Its items are visited on their own.
            ast::ClassSetItem::Empty(_) | ast::ClassSetItem::Union(_) => Ok(()),
        }
    }

    /// `[a--b]`, `[a&&b]`, `[a~~b]`: the operands are folded as sets and
    /// combined, which can produce anything, so the operation is charged
    /// every codepoint on top of its operands' own items.
    fn visit_class_set_binary_op_pre(
        &mut self,
        _: &ast::ClassSetBinaryOp,
    ) -> Result<(), RegexError> {
        self.node()?;
        self.in_bracket(UNICODE_SCALARS, 1)
    }
}

/// One character of a gram: a name character, lowercased as the analyzer
/// does (one to three characters).
type Unit = Box<str>;
/// A string of units.
type Seq = Vec<Unit>;
type Set = BTreeSet<Seq>;

fn fold(c: char) -> Unit {
    c.to_lowercase().collect::<String>().into_boxed_str()
}

fn single(seq: Seq) -> Set {
    BTreeSet::from([seq])
}

/// What's known about the strings a sub-pattern matches (Cox, "Regular
/// Expression Matching with a Trigram Index"): either exactly which
/// strings (`exact`), or a set each starts with, a set each ends with,
/// and a plan each satisfies. Every set is at most [`MAX_SET`] strings.
#[derive(Clone)]
struct Info {
    exact: Option<Set>,
    prefix: Set,
    suffix: Set,
    plan: Plan,
}

impl Info {
    fn exact(set: Set) -> Info {
        Info {
            exact: Some(set),
            prefix: Set::new(),
            suffix: Set::new(),
            plan: Plan::All,
        }
    }

    fn empty() -> Info {
        Info::exact(single(Seq::new()))
    }

    /// Any string at all: nothing known.
    fn any() -> Info {
        Info {
            exact: None,
            prefix: single(Seq::new()),
            suffix: single(Seq::new()),
            plan: Plan::All,
        }
    }

    /// The same knowledge without an exact set: the strings' grams go into
    /// the plan, and they're their own prefixes and suffixes.
    fn inexact(mut self) -> Info {
        if let Some(e) = self.exact.take() {
            self.plan = and(self.plan, grams(&e));
            self.prefix = e.clone();
            self.suffix = e;
        }
        self.trim()
    }

    /// Moves the grams of long prefixes and suffixes into the plan and
    /// keeps only the two units a gram across a boundary can use. Shorter
    /// prefixes are still prefixes, so cutting them is sound.
    fn trim(mut self) -> Info {
        debug_assert!(self.exact.is_none());
        self.plan = and(self.plan, grams(&self.prefix));
        self.plan = and(self.plan, grams(&self.suffix));
        self.prefix = cut(&self.prefix, 2, true);
        self.suffix = cut(&self.suffix, 2, false);
        self
    }
}

/// `set` with each string cut to its first (or last) `keep` units, keeping
/// fewer until there are at most [`MAX_SET`].
fn cut(set: &Set, keep: usize, front: bool) -> Set {
    for k in (0..=keep).rev() {
        let out: Set = set
            .iter()
            .map(|s| {
                let n = s.len().min(k);
                if front {
                    s[..n].to_vec()
                } else {
                    s[s.len() - n..].to_vec()
                }
            })
            .collect();
        if out.len() <= MAX_SET {
            return out;
        }
    }
    unreachable!("cutting to nothing leaves at most one string")
}

fn longest(set: &Set) -> usize {
    set.iter().map(Vec::len).max().unwrap_or(0)
}

/// Whether `a × b` stays within the set limits.
fn fits(a: &Set, b: &Set) -> bool {
    a.len().saturating_mul(b.len()) <= MAX_SET && longest(a) + longest(b) <= MAX_SEQ_CHARS
}

fn cross(a: &Set, b: &Set) -> Set {
    let mut out = Set::new();
    for x in a {
        for y in b {
            let mut s = x.clone();
            s.extend(y.iter().cloned());
            out.insert(s);
        }
    }
    out
}

fn seq_grams(s: &Seq) -> Plan {
    let mut p = Plan::All;
    for w in s.windows(3) {
        p = and(p, Plan::Gram(w.concat()));
    }
    p
}

/// What a string from `set` must contain: OR over the strings of the AND
/// of their grams. When that's over the cap, the weaker AND over
/// positions of the OR of the grams there.
fn grams(set: &Set) -> Plan {
    if set.is_empty() {
        return Plan::Nothing;
    }
    let Some(shortest) = set.iter().map(Vec::len).min() else {
        return Plan::Nothing;
    };
    if shortest < 3 {
        return Plan::All;
    }
    let ors: Vec<Plan> = set.iter().map(seq_grams).collect();
    if ors.iter().map(Plan::size).sum::<usize>() < MAX_PLAN_NODES {
        return ors.into_iter().fold(Plan::Nothing, or);
    }
    let mut p = Plan::All;
    for i in 0..shortest - 2 {
        let at = set
            .iter()
            .map(|s| Plan::Gram(s[i..i + 3].concat()))
            .fold(Plan::Nothing, or);
        p = and(p, at);
    }
    p
}

fn flatten(kids: &mut Vec<Plan>, p: Plan, conj: bool) {
    match p {
        Plan::And(k) if conj => kids.extend(k),
        Plan::Or(k) if !conj => kids.extend(k),
        p => kids.push(p),
    }
}

/// `a` AND `b`. Over the cap, conjuncts are dropped (smallest kept
/// first), which only lets more names through.
fn and(a: Plan, b: Plan) -> Plan {
    match (a, b) {
        (Plan::Nothing, _) | (_, Plan::Nothing) => Plan::Nothing,
        (Plan::All, p) | (p, Plan::All) => p,
        (a, b) => {
            let mut kids = Vec::new();
            flatten(&mut kids, a, true);
            flatten(&mut kids, b, true);
            kids.sort();
            kids.dedup();
            let mut total = 1 + kids.iter().map(Plan::size).sum::<usize>();
            if total > MAX_PLAN_NODES {
                let mut by_size: Vec<(usize, Plan)> =
                    kids.into_iter().map(|k| (k.size(), k)).collect();
                by_size.sort();
                total = 1;
                kids = Vec::new();
                for (n, k) in by_size {
                    if total + n <= MAX_PLAN_NODES {
                        total += n;
                        kids.push(k);
                    }
                }
                kids.sort();
            }
            match kids.len() {
                0 => Plan::All,
                1 => kids.pop().unwrap_or(Plan::All),
                _ => Plan::And(kids),
            }
        }
    }
}

/// `a` OR `b`. Over the cap the plan gives up (`All`), which only lets
/// more names through.
fn or(a: Plan, b: Plan) -> Plan {
    match (a, b) {
        (Plan::All, _) | (_, Plan::All) => Plan::All,
        (Plan::Nothing, p) | (p, Plan::Nothing) => p,
        (a, b) => {
            let mut kids = Vec::new();
            flatten(&mut kids, a, false);
            flatten(&mut kids, b, false);
            kids.sort();
            kids.dedup();
            if 1 + kids.iter().map(Plan::size).sum::<usize>() > MAX_PLAN_NODES {
                return Plan::All;
            }
            match kids.len() {
                1 => kids.pop().unwrap_or(Plan::All),
                _ => Plan::Or(kids),
            }
        }
    }
}

fn concat(x: Info, y: Info) -> Info {
    if let (Some(a), Some(b)) = (&x.exact, &y.exact)
        && fits(a, b)
    {
        return Info::exact(cross(a, b));
    }
    let prefix = match (&x.exact, &y.exact) {
        (Some(a), None) if fits(a, &y.prefix) => Some(cross(a, &y.prefix)),
        _ => None,
    };
    let suffix = match (&x.exact, &y.exact) {
        (None, Some(b)) if fits(&x.suffix, b) => Some(cross(&x.suffix, b)),
        _ => None,
    };
    let x = x.inexact();
    let y = y.inexact();
    let mut plan = and(x.plan, y.plan);
    // A match is x's part then y's: it holds the grams across the join.
    if fits(&x.suffix, &y.prefix) {
        plan = and(plan, grams(&cross(&x.suffix, &y.prefix)));
    }
    Info {
        exact: None,
        prefix: prefix.unwrap_or(x.prefix),
        suffix: suffix.unwrap_or(y.suffix),
        plan,
    }
    .trim()
}

fn alt(x: Info, y: Info) -> Info {
    if let (Some(a), Some(b)) = (&x.exact, &y.exact) {
        let union: Set = a.union(b).cloned().collect();
        if union.len() <= MAX_SET {
            return Info::exact(union);
        }
    }
    let x = x.inexact();
    let y = y.inexact();
    Info {
        exact: None,
        prefix: x.prefix.union(&y.prefix).cloned().collect(),
        suffix: x.suffix.union(&y.suffix).cloned().collect(),
        plan: or(x.plan, y.plan),
    }
    .trim()
}

/// `x{min,max}`, without multiplying anything out past four copies.
fn repeat(x: Info, min: u32, max: Option<u32>) -> Info {
    match (min, max) {
        (0, Some(0)) => Info::empty(),
        (0, Some(1)) => alt(x, Info::empty()),
        (0, _) => Info::any(),
        (n, Some(m)) if n == m && n <= 4 => {
            let mut out = x.clone();
            for _ in 1..n {
                out = concat(out, x.clone());
            }
            out
        }
        // At least one copy: its prefixes, suffixes and plan hold.
        (1, _) => x.inexact(),
        // At least two: `x{n-1}` then `x+`, with `x{n-1}` multiplied out
        // to three copies at most and anything after them.
        (n, _) => {
            let mut out = x.clone();
            for _ in 1..(n - 1).min(3) {
                out = concat(out, x.clone());
            }
            if n - 1 > 3 {
                out = concat(out, Info::any());
            }
            concat(out, x.inexact())
        }
    }
}

fn class(c: &ClassUnicode) -> Result<Info, RegexError> {
    if c.ranges().len() > MAX_CLASS_RANGES {
        return Err(RegexError::new(
            Reason::ClassTooLarge,
            "character class too large",
        ));
    }
    let mut chars = 0u64;
    for r in c.ranges() {
        chars += u64::from(r.end()) - u64::from(r.start()) + 1;
        if chars > MAX_CLASS_EXPAND {
            return Ok(Info::any());
        }
    }
    let set = c
        .ranges()
        .iter()
        .flat_map(|r| r.start()..=r.end())
        .map(|ch| vec![fold(ch)])
        .collect();
    Ok(Info::exact(set))
}

fn info(h: &Hir, depth: u32) -> Result<Info, RegexError> {
    if depth > MAX_HIR_DEPTH {
        return Err(RegexError::new(
            Reason::TooDeep,
            format!("pattern nested deeper than {MAX_DEPTH}"),
        ));
    }
    let d = depth + 1;
    Ok(match h.kind() {
        HirKind::Empty | HirKind::Look(_) => Info::empty(),
        HirKind::Literal(l) => match std::str::from_utf8(&l.0) {
            Ok(s) => Info::exact(single(s.chars().map(fold).collect())),
            Err(_) => Info::any(),
        },
        HirKind::Class(Class::Unicode(c)) => class(c)?,
        // The translator's "fail" (`[^\s\S]`) is an empty byte class.
        // Others are unreachable with Unicode on and `u` refused;
        // "anything" is sound for them.
        HirKind::Class(Class::Bytes(c)) if c.ranges().is_empty() => Info::exact(Set::new()),
        HirKind::Class(Class::Bytes(_)) => Info::any(),
        HirKind::Repetition(r) => repeat(info(&r.sub, d)?, r.min, r.max),
        HirKind::Capture(c) => info(&c.sub, d)?,
        HirKind::Concat(hs) => {
            let mut out = Info::empty();
            for h in hs {
                out = concat(out, info(h, d)?);
            }
            out
        }
        HirKind::Alternation(hs) => {
            let mut out: Option<Info> = None;
            for h in hs {
                let i = info(h, d)?;
                out = Some(match out {
                    None => i,
                    Some(o) => alt(o, i),
                });
            }
            out.unwrap_or_else(|| Info::exact(Set::new()))
        }
    })
}

fn finish(i: Info) -> Plan {
    match i.exact {
        Some(e) => grams(&e),
        None => i.trim().plan,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(p: &str) -> String {
        parse(p).expect(p).plan().to_string()
    }

    fn refused(p: &str) -> Reason {
        parse(p).expect_err(p).reason
    }

    #[test]
    fn a_literal_needs_its_grams() {
        assert_eq!(plan("(?-i)abcd"), r#""abc" & "bcd""#);
        // Case-insensitive by default; the grams are lowercase either way.
        assert_eq!(plan("ABCD"), r#""abc" & "bcd""#);
    }

    #[test]
    fn short_or_open_patterns_have_no_prefilter() {
        assert_eq!(plan("ab"), "ALL");
        assert_eq!(plan("a.*b"), "ALL");
        assert_eq!(plan(".*"), "ALL");
        assert_eq!(plan("x*"), "ALL");
        assert_eq!(plan("^$"), "ALL");
    }

    #[test]
    fn an_empty_class_matches_nothing() {
        assert_eq!(plan(r"[^\s\S]"), "NONE");
        assert_eq!(plan(r"abc|[^\s\S]"), r#""abc""#);
    }

    #[test]
    fn alternation_is_an_or() {
        assert_eq!(plan("(?-i)abc|xyz"), r#""abc" | "xyz""#);
    }

    #[test]
    fn small_classes_are_enumerated_and_large_ones_are_any() {
        assert_eq!(plan("(?-i)a[bc]d"), r#""abd" | "acd""#);
        assert_eq!(plan(r"(?-i)abc\wdef"), r#""abc" & "def""#);
    }

    #[test]
    fn the_long_s_folds_to_itself_so_it_stays_an_alternative() {
        // (?i)s also matches ſ, whose lowercase is ſ: a name "ſab" has the
        // gram "ſab", not "sab".
        assert_eq!(plan("sab"), r#""sab" | "ſab""#);
    }

    #[test]
    fn the_join_of_two_parts_gives_grams() {
        // `(ab|cd)ef`: either "abe" & "bef" or "cde" & "def".
        assert_eq!(
            plan("(?-i)(ab|cd)ef"),
            r#"("abe" & "bef") | ("cde" & "def")"#
        );
        assert_eq!(plan(r"(?-i)ab\d+cd"), "ALL");
        assert_eq!(plan(r"(?-i)abc\d+cde"), r#""abc" & "cde""#);
    }

    #[test]
    fn repetitions() {
        assert_eq!(plan("(?-i)(ab){2}"), r#""aba" & "bab""#);
        assert_eq!(plan("(?-i)(abc)+"), r#""abc""#);
        assert_eq!(plan("(?-i)(abc)?"), "ALL");
        assert_eq!(plan("(?-i)(abc){0}x"), "ALL");
        assert_eq!(plan("(?-i)a{3,}"), r#""aaa""#);
        assert_eq!(plan("(?-i)(abc){5,9}"), r#""abc" & "bca" & "cab""#);
    }

    #[test]
    fn the_verify_pattern_carries_the_default_flags() {
        let p = parse("S0[1-9]").unwrap();
        assert_eq!(p.source(), "S0[1-9]");
        assert_eq!(p.verify_pattern(), "(?i)S0[1-9]");
        // Spelling the flag out changes nothing.
        assert_eq!(parse(&p.verify_pattern()).unwrap().plan(), p.plan());
    }

    #[test]
    fn refusals_have_stable_codes() {
        assert_eq!(refused(""), Reason::Empty);
        assert_eq!(refused(&"a".repeat(MAX_PATTERN_BYTES + 1)), Reason::TooLong);
        assert_eq!(refused("(abc"), Reason::Syntax);
        assert_eq!(refused(r"(?=a)"), Reason::Syntax);
        assert_eq!(refused(r"(a)\1"), Reason::Syntax);
        assert_eq!(refused(r"\p{NotAProperty}"), Reason::Syntax);
        let deep = format!("{}a{}", "(".repeat(17), ")".repeat(17));
        assert_eq!(refused(&deep), Reason::TooDeep);
        let deep_class = format!("{}a{}", "[".repeat(17), "]".repeat(17));
        assert_eq!(refused(&deep_class), Reason::TooDeep);
        assert_eq!(refused(&"a".repeat(MAX_AST_NODES)), Reason::TooManyNodes);
        assert_eq!(refused("a{101}"), Reason::RepeatTooLarge);
        assert_eq!(refused("a{2,101}"), Reason::RepeatTooLarge);
        assert_eq!(refused("a{101,}"), Reason::RepeatTooLarge);
        assert_eq!(refused("((a{10}){10}){11}"), Reason::RepeatTooLarge);
        let wide = vec!["a"; MAX_ALTERNATION + 1].join("|");
        assert_eq!(refused(&wide), Reason::AlternationTooWide);
        for f in ["m", "s", "x", "u", "U", "R", "-u", "i-x"] {
            assert_eq!(refused(&format!("(?{f})a")), Reason::FlagNotAllowed, "{f}");
            assert_eq!(refused(&format!("(?{f}:a)")), Reason::FlagNotAllowed, "{f}");
        }
        // Wide classes are refused before they're case-folded, wherever
        // they are: bare, negated, in brackets, as a range.
        assert_eq!(refused(r"\p{Any}\p{Any}\p{Any}"), Reason::ClassTooLarge);
        // Negated classes fold their complement before negating (B1).
        assert_eq!(refused(&r"\P{Any}".repeat(3)), Reason::ClassTooLarge);
        assert_eq!(refused(&r"[\P{Any}]".repeat(3)), Reason::ClassTooLarge);
        // Inside brackets the whole set is folded, negated items included,
        // and again at each bracket level (B2).
        for p in [
            r"[\S]",
            r"[\D]",
            r"[[^a]b]",
            r"[\D--a]",
            r"[[:^alpha:]]",
            r"[^\D]",
        ] {
            assert_eq!(refused(&p.repeat(3)), Reason::ClassTooLarge, "{p}");
        }
        assert_eq!(
            refused(&format!("abc|{}", r"\P{Any}".repeat(72))),
            Reason::ClassTooLarge
        );
        assert_eq!(
            refused(r"[\p{Any}][\W\pL][^a\p{Any}]"),
            Reason::ClassTooLarge
        );
        assert_eq!(
            refused(r"[\x{0}-\x{10FFFF}][\x{0}-\x{10FFFF}][\x{0}-\x{10FFFF}]"),
            Reason::ClassTooLarge
        );
        // The compiled-size estimate: repetitions multiply class ranges.
        assert_eq!(refused(r"(\w{10}){100}"), Reason::TooLarge);
        assert_eq!(refused(r"\pL{100}"), Reason::TooLarge);
        assert_eq!(refused(&r"\w{20}".repeat(4)), Reason::TooLarge);
        for r in [
            Reason::Empty,
            Reason::TooLong,
            Reason::Syntax,
            Reason::TooDeep,
            Reason::TooManyNodes,
            Reason::RepeatTooLarge,
            Reason::AlternationTooWide,
            Reason::ClassTooLarge,
            Reason::TooLarge,
            Reason::FlagNotAllowed,
        ] {
            assert!(r.code().starts_with("regex_"));
        }
    }

    #[test]
    fn limits_just_inside_are_accepted() {
        parse(&"a".repeat(MAX_AST_NODES - 2)).unwrap();
        parse("a{100}").unwrap();
        parse("(a{10}){100}").unwrap();
        parse(&vec!["a"; MAX_ALTERNATION].join("|")).unwrap();
        parse(&format!("{}a{}", "(".repeat(16), ")".repeat(16))).unwrap();
        parse(r"\w\pL").unwrap();
        // Realistic patterns with many word classes, and one wide class.
        parse(&r"\w+\.".repeat(16)).unwrap();
        parse(r"\p{Any}\p{Any}").unwrap();
        parse(r"\w{60}").unwrap();
        parse(".{0,100}").unwrap();
        parse("(?i)a(?-i:b)").unwrap();
    }

    #[test]
    fn plans_stay_under_the_cap() {
        // Alternatives of long literals.
        let alts: Vec<String> = (0..16).map(|i| format!("{i:02}{}", "qwertyuiop")).collect();
        let p = parse(&alts.join("|")).unwrap();
        assert!(p.plan().size() <= MAX_PLAN_NODES);
        // A long case-insensitive literal of letters with many foldings.
        let p = parse(&"sk".repeat(60)).unwrap();
        assert!(p.plan().size() <= MAX_PLAN_NODES);
        assert_ne!(p.plan(), &Plan::All);
    }

    #[test]
    fn grams_are_listed_once() {
        let p = parse("(?-i)abcabc").unwrap();
        assert_eq!(
            p.plan().grams().into_iter().collect::<Vec<_>>(),
            ["abc", "bca", "cab"]
        );
    }
}
