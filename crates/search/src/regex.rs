//! Regex search's front half (architecture §8, plan 2026-11-05 RX-0): a
//! user's pattern, which is untrusted input, parsed under hard limits into
//! a trigram [`Plan`], the AND/OR of `name_trigram` terms that every name
//! the pattern matches must contain (the Google Code Search / zoekt
//! technique). It's pure: no index access and no matching engine. RX-1
//! runs the plan as a prefilter and verifies the candidates with `regex`.
//!
//! Every bound is checked before the work it guards grows: the pattern's
//! length before parsing, nesting during parsing (the parser's own nest
//! limit), and node count, repetition, alternation, flags, the cost of
//! case-folding the classes and an estimate of the compiled size on the
//! syntax tree before it's translated (case folding is the costly step).
//! Each refusal is a [`RegexError`] with a stable [`Reason::code`]. The plan's own size is held under [`MAX_PLAN_NODES`]
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
/// The most case-folding work the pattern's classes may cost, in units
/// of at most about 2 ns (release build). Patterns are case-insensitive by
/// default, and `regex-syntax` folds a class by walking every codepoint
/// of each range that holds a cased character; inside the fold table's
/// span each step is a binary search ([`FOLD_IN_TABLE`] units), past it
/// one unit, plus [`FOLD_PER_CASED`] per cased character. Calibrated at
/// 0.1–1.9 ns a unit; `\p{Any}` costs about 2,290,000, so two of them
/// fit: about 7–9 ms to translate, and as long again for the verifier's
/// compile.
pub const MAX_FOLD_COST: u64 = 4_600_000;
/// A fold step inside the case-fold table's span costs about ten steps
/// outside it (measured: about 23 ns against 2–3 ns).
const FOLD_IN_TABLE: u64 = 10;
/// Each cased character folded adds its images to the set, which is then
/// sorted and merged: about 15 steps' worth (measured on letter classes).
const FOLD_PER_CASED: u64 = 15;
/// The largest estimated compiled size: each class item's ranges (one for
/// a literal), times the counts of the repetitions around it. Unicode
/// `\w` is about 770 ranges, so `\w{60}` is about 46,000. It's an
/// estimate made before translation; RX-1's verifier keeps `regex`'s own
/// size limit too.
pub const MAX_COMPILED_WEIGHT: u64 = 50_000;
/// A backstop on one translated class's ranges. Real classes stay well
/// under it (the largest is about 1,600).
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
    /// Classes that cost more than [`MAX_FOLD_COST`] to case-fold, or one
    /// class over [`MAX_CLASS_RANGES`].
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
            fold: 0,
            weight: 0,
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
    /// The case-folding cost so far, in [`MAX_FOLD_COST`]'s units.
    fold: u64,
    /// The compiled-size estimate so far.
    weight: u64,
}

impl Bounds<'_> {
    /// Adds the cost of case-folding a set to the fold budget.
    fn fold(&mut self, cost: u64) -> Result<(), RegexError> {
        self.fold = self.fold.saturating_add(cost);
        if self.fold > MAX_FOLD_COST {
            return Err(RegexError::new(
                Reason::ClassTooLarge,
                "character classes too large to match case-insensitively",
            ));
        }
        Ok(())
    }

    /// Adds `ranges`, times the enclosing repetitions, to the size estimate.
    fn weigh(&mut self, ranges: u64) -> Result<(), RegexError> {
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

    /// A Unicode or Perl class item (`\pL`, `\W`, `[:alpha:]`), translated
    /// from its own text case-sensitively, which folds nothing and costs
    /// little even for `\p{Any}`.
    fn class_text(&self, text: &str) -> Result<ClassUnicode, RegexError> {
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
            HirKind::Class(Class::Unicode(c)) => c.clone(),
            _ => ClassUnicode::empty(),
        })
    }

    fn span_text(&self, span: &ast::Span) -> &str {
        self.pattern
            .get(span.start.offset..span.end.offset)
            .unwrap_or_default()
    }

    /// A Unicode property item: the translator folds the property's set
    /// *before* negating it (`\P{Any}` folds all of `\p{Any}`), so that's
    /// what is charged. Returns the item's set, negated as written.
    fn unicode_item(&mut self, c: &ast::ClassUnicode) -> Result<ClassUnicode, RegexError> {
        let set = self.class_text(self.span_text(&c.span))?;
        let mut folded = set.clone();
        if c.is_negated() {
            folded.negate();
        }
        self.fold(fold_cost(&folded))?;
        Ok(set)
    }

    /// The set a bracket's contents make, case-sensitively, charging every
    /// fold the translator does on the way (`regex-syntax` 0.8.11
    /// `hir/translate.rs`): each Unicode property item's own set, each
    /// set operation's two operands, and each bracket's whole set before
    /// its negation. Perl classes (`\w`, `\D`) are closed under folding
    /// and never folded. Recursion is bounded by the parser's nest limit.
    fn class_set(&mut self, set: &ast::ClassSet) -> Result<ClassUnicode, RegexError> {
        match set {
            ast::ClassSet::Item(item) => self.class_item(item),
            ast::ClassSet::BinaryOp(op) => {
                use ast::ClassSetBinaryOpKind as K;
                let mut lhs = self.class_set(&op.lhs)?;
                let rhs = self.class_set(&op.rhs)?;
                self.fold(fold_cost_folded(&lhs))?;
                self.fold(fold_cost_folded(&rhs))?;
                match op.kind {
                    K::Intersection => lhs.intersect(&rhs),
                    K::Difference => lhs.difference(&rhs),
                    K::SymmetricDifference => lhs.symmetric_difference(&rhs),
                }
                Ok(lhs)
            }
        }
    }

    fn class_item(&mut self, item: &ast::ClassSetItem) -> Result<ClassUnicode, RegexError> {
        use regex_syntax::hir::ClassUnicodeRange as R;
        Ok(match item {
            ast::ClassSetItem::Empty(_) => ClassUnicode::empty(),
            ast::ClassSetItem::Literal(l) => ClassUnicode::new([R::new(l.c, l.c)]),
            ast::ClassSetItem::Range(r) => ClassUnicode::new([R::new(r.start.c, r.end.c)]),
            // `[:alpha:]` / `[:^alpha:]`: translated as a one-item bracket.
            ast::ClassSetItem::Ascii(a) => {
                self.class_text(&format!("[{}]", self.span_text(&a.span)))?
            }
            ast::ClassSetItem::Unicode(c) => self.unicode_item(c)?,
            ast::ClassSetItem::Perl(c) => self.class_text(self.span_text(&c.span))?,
            ast::ClassSetItem::Bracketed(b) => self.bracketed(b)?,
            ast::ClassSetItem::Union(u) => {
                let mut out = ClassUnicode::empty();
                for item in &u.items {
                    out.union(&self.class_item(item)?);
                }
                out
            }
        })
    }

    /// A bracket, top-level or nested: its set is folded, then negated.
    fn bracketed(&mut self, b: &ast::ClassBracketed) -> Result<ClassUnicode, RegexError> {
        let mut set = self.class_set(&b.kind)?;
        self.fold(fold_cost_folded(&set))?;
        if b.negated {
            set.negate();
        }
        Ok(set)
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

/// Characters with a case mapping, per std: a superset of `regex-syntax`'s
/// simple case-folding table (a test checks it against the pinned crate),
/// and the span `[lo, hi]` that table covers.
struct Cased {
    set: ClassUnicode,
    lo: u32,
    hi: u32,
}

/// The last character with a case mapping is in Adlam (U+1E943); nothing
/// past U+1FFFF has one.
const CASED_SCAN_END: u32 = 0x1_FFFF;

fn cased() -> &'static Cased {
    static CASED: std::sync::OnceLock<Cased> = std::sync::OnceLock::new();
    CASED.get_or_init(|| {
        use regex_syntax::hir::ClassUnicodeRange as R;
        let chars = (0..=CASED_SCAN_END)
            .filter_map(char::from_u32)
            .filter(|&c| {
                !c.to_lowercase().eq(std::iter::once(c)) || !c.to_uppercase().eq(std::iter::once(c))
            });
        let set = ClassUnicode::new(chars.map(|c| R::new(c, c)));
        let lo = set.ranges().first().map_or(0, |r| u32::from(r.start()));
        let hi = set.ranges().last().map_or(0, |r| u32::from(r.end()));
        Cased { set, lo, hi }
    })
}

/// What `regex-syntax` spends case-folding `set`: a range holding no cased
/// character is skipped after one binary search; any other range is walked
/// codepoint by codepoint, each lookup inside the fold table's span a
/// binary search ([`FOLD_IN_TABLE`] units) and each one past it cheap
/// (1 unit).
fn fold_cost(set: &ClassUnicode) -> u64 {
    let cased = cased();
    let table = cased.set.ranges();
    set.ranges()
        .iter()
        .map(|r| {
            let (start, end) = (u32::from(r.start()), u32::from(r.end()));
            // The first cased range ending at or after `start`.
            let i = table.partition_point(|t| u32::from(t.end()) < start);
            let overlaps = table.get(i).is_some_and(|t| u32::from(t.start()) <= end);
            if !overlaps {
                return 1;
            }
            let width = u64::from(end - start) + 1;
            let inside = u64::from(end.min(cased.hi).saturating_sub(start.max(cased.lo)) + 1)
                .min(width)
                * u64::from(start <= cased.hi && end >= cased.lo);
            // Each cased character found adds its fold images, which are
            // then sorted and merged.
            let hits: u64 = table[i..]
                .iter()
                .take_while(|t| u32::from(t.start()) <= end)
                .map(|t| {
                    let (a, b) = (u32::from(t.start()).max(start), u32::from(t.end()).min(end));
                    u64::from(b - a) + 1
                })
                .sum();
            inside * FOLD_IN_TABLE + (width - inside) + hits * FOLD_PER_CASED
        })
        .sum()
}

/// [`fold_cost`] for a set whose parts may already have been folded: the
/// translator's set is then `set` plus some cased characters, so `set`
/// with every cased character added bounds it.
fn fold_cost_folded(set: &ClassUnicode) -> u64 {
    let mut widened = set.clone();
    widened.union(&cased().set);
    fold_cost(&widened)
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
    /// The fold cost the pattern was charged.
    type Output = u64;
    type Err = RegexError;

    fn finish(self) -> Result<u64, RegexError> {
        Ok(self.fold)
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
            Ast::Literal(_) => self.weigh(1)?,
            // Any character: a handful of UTF-8 ranges, never case-folded.
            Ast::Dot(_) => self.weigh(4)?,
            Ast::ClassUnicode(c) => {
                let set = self.unicode_item(c)?;
                self.weigh((set.ranges().len() as u64).max(1))?;
            }
            // Closed under folding, never folded.
            Ast::ClassPerl(c) => {
                let set = self.class_text(self.span_text(&c.span))?;
                self.weigh((set.ranges().len() as u64).max(1))?;
            }
            // The fold cost of the whole bracket, nested ones included.
            // The items' compiled weight is counted as they're visited.
            Ast::ClassBracketed(b) => {
                self.bracketed(b)?;
            }
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
        if let Ast::Repetition(_) = ast {
            self.repeats.pop();
        }
        Ok(())
    }

    /// Counts class items as nodes and their compiled weight; the fold
    /// cost is the bracket's, charged when the bracket is first seen.
    fn visit_class_set_item_pre(&mut self, item: &ast::ClassSetItem) -> Result<(), RegexError> {
        self.node()?;
        match item {
            ast::ClassSetItem::Literal(_) | ast::ClassSetItem::Range(_) => self.weigh(1),
            ast::ClassSetItem::Ascii(_) => self.weigh(4),
            ast::ClassSetItem::Unicode(c) => {
                let set = self.class_text(self.span_text(&c.span))?;
                self.weigh((set.ranges().len() as u64).max(1))
            }
            ast::ClassSetItem::Perl(c) => {
                let set = self.class_text(self.span_text(&c.span))?;
                self.weigh((set.ranges().len() as u64).max(1))
            }
            ast::ClassSetItem::Bracketed(_) => self.weigh(1),
            ast::ClassSetItem::Empty(_) | ast::ClassSetItem::Union(_) => Ok(()),
        }
    }

    fn visit_class_set_binary_op_pre(
        &mut self,
        _: &ast::ClassSetBinaryOp,
    ) -> Result<(), RegexError> {
        self.node()?;
        self.weigh(1)
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
        // A negated property folds the property itself before negating (B1).
        assert_eq!(refused(&r"\P{Any}".repeat(3)), Reason::ClassTooLarge);
        assert_eq!(refused(&r"[\P{Any}]".repeat(3)), Reason::ClassTooLarge);
        // Inside brackets the whole set is folded, negated items included,
        // and again at each bracket level (B2).
        // Each one past its boundary (see tests/regex_plan.rs).
        for (p, n) in [
            (r"[\S]", 3),
            (r"[\D]", 5),
            (r"[[^a]b]", 2),
            (r"[\D--a]", 3),
            (r"[[:^alpha:]]", 3),
            (r"[^\D]", 5),
        ] {
            assert_eq!(refused(&p.repeat(n)), Reason::ClassTooLarge, "{p}");
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

    /// The fold cost model skips ranges with no character from `cased()`;
    /// that's sound only if every character `regex-syntax` folds is in it.
    #[test]
    fn the_cased_set_covers_everything_regex_syntax_folds() {
        use regex_syntax::hir::ClassUnicodeRange as R;
        let cased = &cased().set;
        let mut missing = Vec::new();
        for c in (0..=CASED_SCAN_END).filter_map(char::from_u32) {
            let mut one = ClassUnicode::new([R::new(c, c)]);
            one.case_fold_simple();
            if one.ranges() != [R::new(c, c)] {
                let mut probe = ClassUnicode::new([R::new(c, c)]);
                probe.intersect(cased);
                if probe.ranges().is_empty() {
                    missing.push(c);
                }
            }
        }
        assert!(missing.is_empty(), "folded but not in cased(): {missing:?}");
        // Nothing past the scan folds at all.
        let tail = R::new(char::from_u32(CASED_SCAN_END + 1).unwrap(), char::MAX);
        let mut rest = ClassUnicode::new([tail]);
        rest.case_fold_simple();
        assert_eq!(rest.ranges(), [tail]);
    }

    #[test]
    fn fold_costs_follow_what_gets_folded() {
        let cost = |p: &str| {
            let ast = ast::parse::ParserBuilder::new().build().parse(p).unwrap();
            let b = Bounds {
                pattern: p,
                nodes: 0,
                repeats: Vec::new(),
                fold: 0,
                weight: 0,
            };
            ast::visit(&ast, b).unwrap()
        };
        // Perl classes stand alone unfolded.
        assert_eq!(cost(r"\W\w\D"), 0);
        // A negated property folds the property itself.
        assert_eq!(cost(r"\P{Any}"), cost(r"\p{Any}"));
        // `[\W_]` has no cased character to walk; `[\S]` walks nearly all.
        assert!(cost(r"[\W_]") * 10 < cost(r"[\S]"));
        // A nested bracket is folded once more.
        assert!(cost(r"[[\x{80}-\x{FFFF}]a]") > cost(r"[\x{80}-\x{FFFF}]") * 3 / 2);
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
