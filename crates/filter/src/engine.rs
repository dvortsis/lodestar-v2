//! The engine the pipeline holds (§20.4): the rule set at a version, and
//! a handle that swaps in a new rule set without stopping anything.
//!
//! Layers run in precedence order (§20.1):
//! 1. the locked floor: a hit is final;
//! 2. CSX-5, the override list: an Override match passes everything
//!    below; a Neutralize entry removes its word from the evidence;
//! 3. the rest of the core, with your exceptions for single rules;
//! 4. *Always allow*, which skips your rules and the packs;
//! 5. your rules, in order;
//! 6. the pack layer: built-in packs and subscribed lists, the strongest
//!    action winning. Each pack rule applies the version this install
//!    accepted, and a version pending acceptance runs in shadow beside it
//!    ([`crate::load::upgrade`], RP-0). An override you set changes a
//!    rule's action or adds an `unless`, never what it matches
//!    ([`packs::overrides`], RP-2).
//!
//! Prepare stamps its verdict with the version it used; commit trusts a
//! verdict from the built-in worker at the current version and judges
//! again otherwise.

use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use lodestar_classify::ContentType;

use crate::core::{Action, Core, CoreRule, Evidence, Hit, Item};
use crate::hash::{IdSet, TermSet};
use crate::matcher::{Doc, Facts, Reading};
use crate::packs::manifest::Manifest;
use crate::packs::overrides::{self, PackOverrides};
use crate::packs::sp4::AtCommit;
use crate::packs::upgrade::{PackShadow, PackVersions};
use crate::packs::{self, PackRule, PackSwitches};
use crate::prefilter::RuleList;
use crate::rules::{self, Allow, ClassRules, Exception, Match, OverrideList, Strength};

/// Which rule decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuleRef {
    Core(CoreRule),
    Pack(PackRule),
    /// A subscribed content list, by subscription id.
    List(u32),
    /// One of your rules.
    User(u32),
    /// A CSX-5 entry (in shadow mode, what it would have released).
    Override(u32),
}

impl RuleRef {
    /// The stored id (`filter_hits.rule`, `tombstones.rule`).
    pub fn id(self) -> String {
        match self {
            RuleRef::Core(r) => r.id().to_owned(),
            RuleRef::Pack(r) => r.id().to_owned(),
            RuleRef::List(id) => format!("L-{id}"),
            RuleRef::User(id) => format!("U-{id}"),
            RuleRef::Override(id) => format!("O-{id}"),
        }
    }

    pub fn from_id(id: &str) -> Option<RuleRef> {
        if let Some(r) = CoreRule::from_id(id) {
            return Some(RuleRef::Core(r));
        }
        if let Some(r) = PackRule::from_id(id) {
            return Some(RuleRef::Pack(r));
        }
        let (kind, n) = id.split_once('-')?;
        let n: u32 = n.parse().ok()?;
        Some(match kind {
            "L" => RuleRef::List(n),
            "U" => RuleRef::User(n),
            "O" => RuleRef::Override(n),
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub rule: RuleRef,
    pub action: Action,
    pub evidence: Vec<Evidence>,
}

impl Decision {
    pub fn evidence_codes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for e in &self.evidence {
            e.encode(&mut out);
        }
        out
    }
}

impl From<Hit> for Decision {
    fn from(h: Hit) -> Decision {
        Decision {
            rule: RuleRef::Core(h.rule),
            action: h.action,
            evidence: h.evidence,
        }
    }
}

/// What the rules made of a torrent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Verdict {
    /// The decision that applies, if any.
    pub decision: Option<Decision>,
    /// What rules in shadow mode would have done: logged, never applied.
    /// A pack rule's pending version is among them where it holds; both
    /// sides of it are [`Engine::judge_shadowed`]'s.
    pub shadow: Vec<Decision>,
}

/// [`Engine::released`].
#[derive(Debug, Default)]
struct Released<'a> {
    overridden: bool,
    neutral: Vec<&'a rules::Prepared>,
    shadow: Vec<u32>,
}

/// `now` as a judgment reads it: each instant compared with it (an
/// expiry, the end of a shadow period) is noted, so a [`Trail`] can tell
/// whether another `now` would have decided the same.
#[derive(Debug)]
pub(crate) struct Clock {
    now: i64,
    edges: std::cell::RefCell<Vec<i64>>,
}

impl Clock {
    pub(crate) fn new(now: i64) -> Clock {
        Clock {
            now,
            edges: Default::default(),
        }
    }

    /// Whether `now` has reached `t` (`t <= now`).
    fn reached(&self, t: i64) -> bool {
        self.edges.borrow_mut().push(t);
        t <= self.now
    }
}

/// Where the layers above the pack layer left a reading.
#[derive(Debug, Clone)]
enum Above {
    /// Decided there (an Override entry, a core hit, *Always allow* or one
    /// of your rules): the pack layer isn't reached.
    Settled(Option<Decision>),
    /// On to the pack layer, with what your stand-in rules hold.
    Packs(Option<Decision>),
}

impl Default for Above {
    fn default() -> Above {
        Above::Settled(None)
    }
}

/// What one reading ([`Reading`]) of a torrent found above the pack layer,
/// and the pack layer's decisions.
#[derive(Debug, Clone, Default)]
struct ReadingRec {
    reading: Reading,
    /// The Neutralize entries that applied: their words, and the set the
    /// core reads.
    neutral: Vec<String>,
    neutral_set: TermSet,
    /// The shadow override entries that matched.
    released: Vec<u32>,
    above: Above,
    /// The shadow decisions logged above the pack layer (your rules').
    shadow: Vec<Decision>,
    /// The pack layer's decisions after exceptions, each with its place
    /// (a pack rule's in [`PackRule::ALL`], then the lists').
    packs: Vec<(usize, Decision)>,
    /// The pending versions' shadow decisions, each with its rule's place.
    pack_shadow: Vec<(usize, Decision)>,
    /// For the Adult reading of the same torrent, where one follows
    /// ([`ViewRec::paired`]).
    paired: Option<Paired>,
    /// Whether to keep what a [`Trail`] reads (`above` and after).
    trail: bool,
}

/// What the reading as found keeps for the Adult reading after it.
#[derive(Debug, Clone, Default)]
struct Paired {
    /// The Adult reading's tags, given.
    adult_tags: Arc<IdSet>,
    /// The core's signals (the joined reading's), which the content type
    /// doesn't change.
    signals: Option<crate::core::PatternSignals>,
    /// Which of your rules the Adult reading meets ([`Doc::candidates_pair`]).
    candidates: Option<Vec<u32>>,
    /// The last of your rules the reading as found checked, by position
    /// (`u32::MAX` once it checked them all), where it reached them; and
    /// those whose query matched and `unless` didn't, in order. A rule
    /// that reads no tags meets both readings or neither
    /// ([`Engine::below_overrides`]).
    checked_to: Option<u32>,
    matched: Vec<u32>,
    /// What the pack layer read from the name and files, taken by the
    /// Adult reading's.
    pack_text: std::cell::RefCell<Option<packs::CtxText>>,
}

/// One reading of the classification (as found, or as Adult): the joined
/// reading, and the earlier one where the torrent is read twice.
#[derive(Debug, Clone, Default)]
struct ViewRec {
    /// The Adult reading's tags, where one follows.
    adult_tags: Option<Arc<IdSet>>,
    /// Whether to keep what a [`Trail`] reads.
    trail: bool,
    joined: ReadingRec,
    before: Option<ReadingRec>,
}

impl ViewRec {
    /// For a [`Trail`].
    fn trail() -> ViewRec {
        ViewRec {
            trail: true,
            ..ViewRec::default()
        }
    }

    /// For the reading as found, keeping what `adult`'s reading reuses,
    /// and with `trail` what a [`Trail`] reads.
    fn paired(adult: &Adult, trail: bool) -> ViewRec {
        ViewRec {
            adult_tags: adult.doc.map(|d| Arc::new(d.tag_keys().clone())),
            trail,
            ..ViewRec::default()
        }
    }

    /// A new record of one of its readings.
    fn reading(&self, reading: Reading) -> ReadingRec {
        ReadingRec {
            reading,
            trail: self.trail,
            paired: self.adult_tags.as_ref().map(|t| Paired {
                adult_tags: t.clone(),
                ..Paired::default()
            }),
            ..ReadingRec::default()
        }
    }

    /// Without what only the Adult reading reads.
    fn for_trail(mut self) -> ViewRec {
        self.adult_tags = None;
        for r in std::iter::once(&mut self.joined).chain(self.before.as_mut()) {
            r.paired = None;
        }
        self
    }
}

/// A torrent read as Adult beside its reading as found ([`Engine::with_adult`]).
struct Adult<'x> {
    item: &'x Item<'x>,
    facts: Option<&'x Facts<'x>>,
    /// The floor's hit on it, which is final.
    floor: Option<Hit>,
    /// Where the floor didn't decide: the torrent prepared, sharing the
    /// reading as found's text.
    doc: Option<&'x Doc<'x>>,
}

impl Adult<'_> {
    /// The Adult reading's verdict, after the reading as found (`found`,
    /// on `doc`, kept with [`ViewRec::paired`]): the floor's hit, or the
    /// layers, where the core judges the reading as found's signals as
    /// Adult and your rules' matches that don't read tags are the reading
    /// as found's. Returns whether `rec` kept a reading (the floor didn't
    /// decide).
    fn judge(
        &self,
        engine: &Engine,
        doc: &Doc,
        found: &ViewRec,
        clock: &Clock,
        at: AtCommit,
        rec: Option<&mut ViewRec>,
    ) -> (Verdict, bool) {
        match (&self.floor, self.doc) {
            (Some(hit), _) => {
                let v = Verdict {
                    decision: Some(hit.clone().into()),
                    shadow: Vec::new(),
                };
                (v, false)
            }
            (None, Some(d)) => {
                let v = engine
                    .judge_doc(self.item, self.facts, d, clock, at, Some(found), rec)
                    .0;
                if d.read_time() {
                    doc.note_read_time();
                }
                (v, true)
            }
            (None, None) => unreachable!("prepared where the floor didn't decide"),
        }
    }
}

/// What a judgment of a torrent found above the pack layer, kept for the
/// writer's second judgment of it ([`Engine::judge_at_commit_reusing`]),
/// with what it depended on. Empty until a judgment fills it
/// ([`Engine::judge_classified_with`]); an empty one judges in full.
#[derive(Debug, Clone, Default)]
pub struct Trail(Option<Box<TrailOf>>);

#[derive(Debug, Clone)]
struct TrailOf {
    generation: u64,
    version: u64,
    /// The torrent and its facts, but for the times ([`fingerprint`]).
    fingerprint: u64,
    now: i64,
    facts_now: i64,
    discovered_at: i64,
    /// The instants `now` was compared with.
    edges: Vec<i64>,
    /// A rule read a date or a time of day.
    read_time: bool,
    /// What your classification rules said.
    correction: Option<bool>,
    found: ViewRec,
    /// The Adult reading, where it was judged and the floor didn't decide.
    adult: Option<ViewRec>,
}

impl Trail {
    #[allow(clippy::too_many_arguments)]
    fn of(
        engine: &Engine,
        item: &Item,
        facts: &Facts,
        clock: &Clock,
        doc: &Doc,
        correction: Option<bool>,
        found: ViewRec,
        adult: Option<ViewRec>,
    ) -> Trail {
        Trail(Some(Box::new(TrailOf {
            generation: engine.generation,
            version: engine.version,
            fingerprint: fingerprint(item, facts),
            now: clock.now,
            facts_now: facts.now,
            discovered_at: facts.discovered_at,
            edges: clock.edges.take(),
            read_time: doc.read_time(),
            correction,
            found: found.for_trail(),
            adult: adult.map(ViewRec::for_trail),
        })))
    }

    /// Whether a judgment filled it.
    pub fn is_kept(&self) -> bool {
        self.0.is_some()
    }

    /// Whether [`Engine::judge_at_commit_reusing`] reads it for this
    /// engine, torrent and `now` rather than judging in full (where it
    /// also has the readings that judgment needs). For tests.
    #[doc(hidden)]
    pub fn holds_for(&self, engine: &Engine, item: &Item, facts: &Facts, now: i64) -> bool {
        self.fits(engine, item, facts, now).is_some()
    }

    /// The trail, if it holds for this engine, torrent and `now`.
    fn fits(&self, engine: &Engine, item: &Item, facts: &Facts, now: i64) -> Option<&TrailOf> {
        let t = self.0.as_deref()?;
        let same_side = |e: &i64| (*e <= t.now) == (*e <= now);
        (t.generation == engine.generation
            && t.version == engine.version
            && t.fingerprint == fingerprint(item, facts)
            && t.edges.iter().all(same_side)
            && (!t.read_time
                || (t.facts_now == facts.now && t.discovered_at == facts.discovered_at)))
            .then_some(t)
    }
}

/// What a trail is checked against, so it isn't read for another
/// torrent: the hashes, name and file count, and some of the facts (size,
/// piece length, flags, and the classification's type, confidence, adult
/// kind, junk score, structure, depth and extensions). It's a guard, not a
/// proof: the trail travels inside its own torrent's record, whose name,
/// files and classification the writer never changes, and the
/// classification is derived from the name and files. The times are
/// checked on their own ([`Trail`]).
fn fingerprint(item: &Item, facts: &Facts) -> u64 {
    // A quick mix, not a defence: the trail travels inside the torrent's
    // own record, so this only catches a record handed the wrong trail.
    let mut h: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut mix = |x: u64| {
        h = (h ^ x).wrapping_mul(0xbf58_476d_1ce4_e5b9).rotate_left(29);
    };
    let mut bytes = |b: &[u8]| {
        mix(b.len() as u64);
        for c in b.chunks(8) {
            let mut w = [0u8; 8];
            w[..c.len()].copy_from_slice(c);
            mix(u64::from_le_bytes(w));
        }
    };
    for hash in item.hashes {
        bytes(hash);
    }
    bytes(item.name.as_bytes());
    let c = facts.class;
    for x in [
        item.files.len() as u64,
        u64::from(item.content_type.code()),
        facts.size,
        facts.piece_len,
        facts.flags as u64,
        u64::from(c.content_type.code()),
        u64::from(c.confidence),
        c.adult_kind.map_or(0, |k| k as u64 + 1),
        u64::from(c.junk.score),
        u64::from(c.structure.bits()),
        u64::from(c.depth),
        u64::from(c.distinct_extensions),
    ] {
        mix(x);
    }
    h
}

/// A process-wide number for each engine made or changed, so a [`Trail`]
/// is never read by another engine that happens to share its version.
fn next_generation() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Both readings of [`Engine::judge_doc`] as one verdict: the stricter
/// (the earlier reading's on a tie), and the shadow override entries
/// either matched, logged once, at the decision that stands.
fn both_readings(joined: (Verdict, Vec<u32>), before: Option<(Verdict, Vec<u32>)>) -> Verdict {
    let (mut v, mut released) = joined;
    if let Some((before, also)) = before {
        v = stricter(before, v);
        for id in also {
            if !released.contains(&id) {
                released.push(id);
            }
        }
    }
    if let Some(d) = &v.decision {
        for id in released {
            v.shadow.push(Decision {
                rule: RuleRef::Override(id),
                action: d.action,
                evidence: vec![Evidence::Released],
            });
        }
    }
    v
}

/// The strongest of the pack layer's decisions, the last of equals.
fn strongest<D: std::borrow::Borrow<Decision>>(packs: impl Iterator<Item = D>) -> Option<D> {
    packs.max_by_key(|d| d.borrow().action)
}

/// A subscribed content list in the pack layer.
#[derive(Debug, Clone)]
pub struct ListLayer {
    pub id: u32,
    pub action: Action,
    pub hashes: Arc<HashSet<[u8; 20]>>,
}

#[derive(Debug)]
pub struct Engine {
    pub core: Core,
    pub lists: Vec<ListLayer>,
    pub packs: PackSwitches,
    /// The pack rules' versions, compiled in.
    pub manifest: Arc<Manifest>,
    /// Which version of each pack rule applies, and which is pending
    /// ([`crate::load`] resolves it from the stored record).
    pub pack_versions: PackVersions,
    /// Your overrides of pack rules, resolved against `pack_versions`
    /// ([`crate::load`]).
    pub pack_overrides: PackOverrides,
    /// Your rules, indexed so a torrent only meets those that could match.
    pub rules: RuleList,
    pub overrides: OverrideList,
    pub exceptions: Vec<Exception>,
    pub allow: Vec<Allow>,
    /// Classification rules (§20.9): they decide what's stored, and an
    /// Is adult one adds the Adult-only core checks.
    pub class_rules: ClassRules,
    /// Stored rows the last load couldn't read ([`crate::load`]).
    pub unreadable: Vec<crate::load::Unreadable>,
    /// Your rules that are stand-ins for unreadable rows, or lost their
    /// `unless`, by id: usually none.
    pub stand_ins: Vec<u32>,
    /// SP-4's signature set (RP-5): shared by every version of the engine,
    /// the writer that adds to it and the API that releases from it, so a
    /// reload never copies it. [`crate::load`] fills it at start.
    pub spam_sigs: Arc<crate::spam_sigs::SpamSigs>,
    pub version: u64,
    /// This engine's own number: a new one for every engine made, copied
    /// or swapped in ([`Handle::update_engine`]), so a [`Trail`] is read
    /// only by the engine that made it.
    generation: u64,
}

impl Clone for Engine {
    /// A copy is another engine: it gets its own generation, so a trail
    /// one made is never read by the other, however either changes.
    fn clone(&self) -> Engine {
        Engine {
            core: self.core.clone(),
            lists: self.lists.clone(),
            packs: self.packs.clone(),
            manifest: self.manifest.clone(),
            pack_versions: self.pack_versions.clone(),
            pack_overrides: self.pack_overrides.clone(),
            rules: self.rules.clone(),
            overrides: self.overrides.clone(),
            exceptions: self.exceptions.clone(),
            allow: self.allow.clone(),
            class_rules: self.class_rules.clone(),
            unreadable: self.unreadable.clone(),
            stand_ins: self.stand_ins.clone(),
            spam_sigs: self.spam_sigs.clone(),
            version: self.version,
            generation: next_generation(),
        }
    }
}

impl Engine {
    /// The core alone: no packs, rules, lists or exceptions.
    pub fn new(core: Core) -> Engine {
        Engine {
            core,
            lists: Vec::new(),
            packs: PackSwitches::default(),
            manifest: Manifest::builtin(),
            pack_versions: PackVersions::default(),
            pack_overrides: PackOverrides::default(),
            rules: RuleList::default(),
            overrides: OverrideList::default(),
            exceptions: Vec::new(),
            allow: Vec::new(),
            class_rules: ClassRules::default(),
            unreadable: Vec::new(),
            stand_ins: Vec::new(),
            spam_sigs: Arc::default(),
            version: 1,
            generation: next_generation(),
        }
    }

    /// What your classification rules say about this torrent: the newest
    /// matching rule's Is adult (`true`) or Not adult (`false`).
    pub fn class_correction(&self, item: &Item, facts: Option<&Facts>) -> Option<bool> {
        if self.class_rules.is_empty() {
            return None;
        }
        self.class_rules.decide(&Doc::new(item, facts))
    }

    /// [`Engine::judge`] plus your classification rules (§20.9), sharing
    /// one prepared torrent. Returns the verdict and what the rules say.
    ///
    /// The verdict is on detection's classification, so a Not adult rule
    /// never takes a torrent out of the Adult-only core checks. When an Is
    /// adult rule makes a torrent adult that detection didn't, it's judged
    /// as adult too and the stricter verdict is kept: a torrent stored as
    /// adult has always met the Adult-only checks.
    pub fn judge_classified(
        &self,
        item: &Item,
        facts: Option<&Facts>,
        now: i64,
    ) -> (Verdict, Option<bool>) {
        self.judge_classified_with(item, facts, now, false, None)
    }

    /// [`Engine::judge_classified`], and with `also_adult` (an always-adult
    /// source found it, [`crate::reclassify::judge_source_adult`]) read as
    /// Adult too where it didn't already, unless the floor decided: the
    /// stricter verdict stands. The Adult reading shares what the reading
    /// as found read from the name and paths, and your rules' matches that
    /// don't read tags ([`Engine::adult_reading`]).
    ///
    /// `trail`: keeps what the judgment found above the pack layer, for
    /// the writer's second judgment of the torrent
    /// ([`Engine::judge_at_commit_reusing`]). Kept only with `facts`.
    pub fn judge_classified_with(
        &self,
        item: &Item,
        facts: Option<&Facts>,
        now: i64,
        also_adult: bool,
        trail: Option<&mut Trail>,
    ) -> (Verdict, Option<bool>) {
        if let Some(hit) = self.core.floor_hit(item) {
            let v = Verdict {
                decision: Some(hit.into()),
                shadow: Vec::new(),
            };
            return (v, None);
        }
        let clock = Clock::new(now);
        let doc = Doc::new(item, facts).with_near(self.rules.near_terms());
        if let Some(v) = self.unreadable(&doc) {
            return (v, None);
        }
        let correction = self.class_rules.decide(&doc);
        let as_adult = correction == Some(true) && item.content_type != ContentType::Adult;
        // An always-adult source's find is read as Adult unless detection
        // or an Is adult rule made it adult already
        // (`reclassify::judge_source_adult`).
        let source_adult = also_adult
            && !facts.is_some_and(|f| f.class.content_type == ContentType::Adult)
            && correction != Some(true);
        let adult_next = as_adult || source_adult;
        let floor_decided = |v: &Verdict| {
            v.decision
                .as_ref()
                .is_some_and(|d| matches!(d.rule, RuleRef::Core(r) if r.locked()))
        };
        let at = AtCommit::default();
        let (v, found, adult) = if adult_next {
            self.with_adult(&doc, item, facts, |adult| {
                let mut found = ViewRec::paired(&adult, trail.is_some());
                let v = self
                    .judge_doc(item, facts, &doc, &clock, at, None, Some(&mut found))
                    .0;
                if as_adult || (source_adult && !floor_decided(&v)) {
                    let mut rec = ViewRec::trail();
                    let rec_adult = trail.is_some().then_some(&mut rec);
                    let (a, kept) = adult.judge(self, &doc, &found, &clock, at, rec_adult);
                    (stricter(v, a), found, kept.then_some(rec))
                } else {
                    (v, found, None)
                }
            })
        } else {
            let mut found = ViewRec::trail();
            let rec = (trail.is_some() && facts.is_some()).then_some(&mut found);
            let v = self.judge_doc(item, facts, &doc, &clock, at, None, rec).0;
            (v, found, None)
        };
        if let (Some(t), Some(f)) = (trail, facts) {
            *t = Trail::of(self, item, f, &clock, &doc, correction, found, adult);
        }
        (v, correction)
    }

    /// The decision for one torrent. `facts` (classification and the
    /// stored fields) let packs and your rules read more than the name
    /// and files; `now` resolves shadow periods, expiries and relative
    /// dates.
    pub fn judge(&self, item: &Item, facts: Option<&Facts>, now: i64) -> Verdict {
        self.judge_shadowed(item, facts, now).0
    }

    /// [`Engine::judge`], with each pending pack version's result beside
    /// the applied one's where either holds the torrent ([`PackShadow`]):
    /// which side holds and whether the shadow action is heavier or
    /// lighter, for the acceptance preview (RP-3). Nothing in it applies.
    pub fn judge_shadowed(
        &self,
        item: &Item,
        facts: Option<&Facts>,
        now: i64,
    ) -> (Verdict, Vec<PackShadow>) {
        if let Some(hit) = self.core.floor_hit(item) {
            let v = Verdict {
                decision: Some(hit.into()),
                shadow: Vec::new(),
            };
            return (v, Vec::new());
        }
        let doc = Doc::new(item, facts).with_near(self.rules.near_terms());
        if let Some(v) = self.unreadable(&doc) {
            return (v, Vec::new());
        }
        self.judge_doc(
            item,
            facts,
            &doc,
            &Clock::new(now),
            AtCommit::default(),
            None,
            None,
        )
    }

    /// [`Engine::judge_at_commit`] with SP-3 at `level` alone (RP-4).
    pub fn judge_flooded(
        &self,
        item: &Item,
        facts: &Facts,
        now: i64,
        level: crate::flood::Level,
        adult: bool,
    ) -> Verdict {
        let at = AtCommit {
            flood: Some(level),
            clone_of: None,
        };
        self.judge_at_commit(item, facts, now, at, adult)
    }

    /// The writer's verdict for a torrent the rules that depend on state
    /// match: one whose publisher, or source, is flooding (SP-3 at
    /// `at.flood`, RP-4), or a clone of hidden spam (SP-4 inheriting
    /// `at.clone_of`, RP-5). The torrent is judged again with them
    /// matching, inside the engine's precedence. The floor is final; a
    /// CSX-5 Override, *Always allow*, a core hit or one of your rules
    /// decides before the pack layer is reached, and an exception for
    /// SP-3 or SP-4, their switches and your overrides of them apply as
    /// for any pack rule; in the pack layer the strongest action wins.
    /// Pending, they run in shadow ([`crate::packs::upgrade`]).
    ///
    /// SP-4 follows its original: it matches only while the original's
    /// rule (SP-2 or SP-3) is on, and not where an exception to that rule
    /// covers this torrent too.
    ///
    /// `adult`: an always-adult source found it, so it's judged as adult
    /// too, as the writer's other verdicts are
    /// (`lodestar_pipeline::source_adult`); so is a torrent your Is adult
    /// rule makes adult ([`Engine::judge_classified`]). The stricter
    /// verdict stands. Nothing here changes what the classification rules
    /// say: the writer keeps that from its first verdict.
    ///
    /// Adding SP-3 or SP-4 only adds to the pack layer, so this is never
    /// weaker than the verdict without them.
    pub fn judge_at_commit(
        &self,
        item: &Item,
        facts: &Facts,
        now: i64,
        at: AtCommit,
        adult: bool,
    ) -> Verdict {
        self.at_commit(item, facts, now, at, adult, true)
    }

    /// [`Engine::judge_at_commit`] as it judged before
    /// X-commit-rejudge-reuse: the Adult reading on a torrent prepared
    /// afresh, sharing nothing with the reading as found. The oracle the
    /// differential tests compare the faster paths with.
    #[doc(hidden)]
    pub fn judge_at_commit_unshared(
        &self,
        item: &Item,
        facts: &Facts,
        now: i64,
        at: AtCommit,
        adult: bool,
    ) -> Verdict {
        self.at_commit(item, facts, now, at, adult, false)
    }

    /// [`Engine::judge_classified`] as it judged before
    /// X-commit-rejudge-reuse, as [`Engine::judge_at_commit_unshared`].
    #[doc(hidden)]
    pub fn judge_classified_unshared(
        &self,
        item: &Item,
        facts: Option<&Facts>,
        now: i64,
    ) -> (Verdict, Option<bool>) {
        if let Some(hit) = self.core.floor_hit(item) {
            let v = Verdict {
                decision: Some(hit.into()),
                shadow: Vec::new(),
            };
            return (v, None);
        }
        let doc = Doc::new(item, facts).with_near(self.rules.near_terms());
        if let Some(v) = self.unreadable(&doc) {
            return (v, None);
        }
        let correction = self.class_rules.decide(&doc);
        let clock = Clock::new(now);
        let v = self
            .judge_doc(item, facts, &doc, &clock, AtCommit::default(), None, None)
            .0;
        if correction != Some(true) || item.content_type == ContentType::Adult {
            return (v, correction);
        }
        let adult_item = Item {
            content_type: ContentType::Adult,
            ..*item
        };
        let adult_class = facts.map(|f| f.class.as_adult());
        let adult_facts = facts
            .zip(adult_class.as_ref())
            .map(|(f, class)| Facts { class, ..*f });
        let as_adult = self.judge(&adult_item, adult_facts.as_ref(), now);
        (stricter(v, as_adult), correction)
    }

    fn at_commit(
        &self,
        item: &Item,
        facts: &Facts,
        now: i64,
        at: AtCommit,
        adult: bool,
        share: bool,
    ) -> Verdict {
        if let Some(hit) = self.core.floor_hit(item) {
            return Verdict {
                decision: Some(hit.into()),
                shadow: Vec::new(),
            };
        }
        let clock = Clock::new(now);
        let doc = Doc::new(item, Some(facts)).with_near(self.rules.near_terms());
        if let Some(v) = self.unreadable(&doc) {
            return v;
        }
        let correction = self.class_rules.decide(&doc);
        let adult_next =
            (adult || correction == Some(true)) && item.content_type != ContentType::Adult;
        if !adult_next {
            return self
                .judge_doc(item, Some(facts), &doc, &clock, at, None, None)
                .0;
        }
        if !share {
            let v = self
                .judge_doc(item, Some(facts), &doc, &clock, at, None, None)
                .0;
            return stricter(v, self.adult_unshared(item, Some(facts), &clock, at));
        }
        self.with_adult(&doc, item, Some(facts), |adult| {
            let mut found = ViewRec::paired(&adult, false);
            let v = self
                .judge_doc(item, Some(facts), &doc, &clock, at, None, Some(&mut found))
                .0;
            let (a, _) = adult.judge(self, &doc, &found, &clock, at, None);
            stricter(v, a)
        })
    }

    /// The writer's second judgment of a torrent ([`Engine::judge_at_commit`]),
    /// from what its first judgment found above the pack layer (`trail`,
    /// [`Engine::judge_classified_with`]): the floor is judged again, and
    /// SP-3 and SP-4, the only rules `at` changes, are judged with it and
    /// set among the pack layer's other decisions, which stand as they
    /// were. Everything above the pack layer (CSX-5, *Always allow*, the
    /// core and your rules) decided as it did; where it settled the
    /// reading, the pack layer isn't reached, as before.
    ///
    /// The trail is used only for the engine that made it (the same
    /// generation and version: no rule, pack, switch, override or version
    /// changed), for the same torrent, and where the instants the first
    /// judgment compared `now` against (expiries, shadow periods) fall on
    /// the same side of this `now`, and no date or time of day was read
    /// unless the facts' times are the same. Anywhere else, or where the
    /// trail lacks a reading this judgment needs, it's
    /// [`Engine::judge_at_commit`] in full. The verdict is that one's
    /// either way (docs/decisions/2026-11-06-X-commit-rejudge-reuse.md).
    pub fn judge_at_commit_reusing(
        &self,
        trail: &Trail,
        item: &Item,
        facts: &Facts,
        now: i64,
        at: AtCommit,
        adult: bool,
    ) -> Verdict {
        let Some(t) = trail.fits(self, item, facts, now) else {
            return self.judge_at_commit(item, facts, now, at, adult);
        };
        if let Some(hit) = self.core.floor_hit(item) {
            return Verdict {
                decision: Some(hit.into()),
                shadow: Vec::new(),
            };
        }
        let adult_next =
            (adult || t.correction == Some(true)) && item.content_type != ContentType::Adult;
        let adult_item = Item {
            content_type: ContentType::Adult,
            ..*item
        };
        // The Adult reading's floor, judged again as the full path does.
        let adult_floor = adult_next
            .then(|| self.core.floor_hit(&adult_item))
            .flatten();
        if adult_next && adult_floor.is_none() && t.adult.is_none() {
            return self.judge_at_commit(item, facts, now, at, adult);
        }
        let v = self.replay_view(&t.found, item, facts, at);
        if !adult_next {
            return v;
        }
        let as_adult = match (adult_floor, &t.adult) {
            (Some(hit), _) => Verdict {
                decision: Some(hit.into()),
                shadow: Vec::new(),
            },
            (None, Some(rec)) => {
                let adult_class = facts.class.as_adult();
                let adult_facts = Facts {
                    class: &adult_class,
                    ..*facts
                };
                self.replay_view(rec, &adult_item, &adult_facts, at)
            }
            (None, None) => unreachable!("checked above"),
        };
        stricter(v, as_adult)
    }

    /// CS-0a's verdict, final as the floor's is, where the core can't read
    /// the torrent within its budget: decided before any entry, rule or
    /// pack reads it. Reads the name and paths once, metered; the core
    /// and keyword entries reuse that read.
    fn unreadable(&self, doc: &Doc) -> Option<Verdict> {
        doc.too_large().then(|| Verdict {
            decision: Some(self.core.too_large_hit().into()),
            shadow: Vec::new(),
        })
    }

    /// The torrent read as Adult, for a judgment that reads it so after
    /// the reading as found, on `doc`: its floor, judged as
    /// [`Engine::judge`] judges it first, and where that doesn't decide, a
    /// [`Doc::as_adult`] of `doc`, sharing what was read from the name and
    /// paths ([`Adult`]).
    fn with_adult<R>(
        &self,
        doc: &Doc,
        item: &Item,
        facts: Option<&Facts>,
        f: impl FnOnce(Adult<'_>) -> R,
    ) -> R {
        let adult_item = Item {
            content_type: ContentType::Adult,
            ..*item
        };
        let adult_class = facts.map(|f| f.class.as_adult());
        let adult_facts = facts
            .zip(adult_class.as_ref())
            .map(|(f, class)| Facts { class, ..*f });
        let floor = self.core.floor_hit(&adult_item);
        let adult_doc = floor
            .is_none()
            .then(|| doc.as_adult(&adult_item, adult_facts.as_ref()));
        f(Adult {
            item: &adult_item,
            facts: adult_facts.as_ref(),
            floor,
            doc: adult_doc.as_ref(),
        })
    }

    /// The Adult reading as it was before X-commit-rejudge-reuse: on a
    /// torrent prepared afresh.
    fn adult_unshared(
        &self,
        item: &Item,
        facts: Option<&Facts>,
        clock: &Clock,
        at: AtCommit,
    ) -> Verdict {
        let adult_item = Item {
            content_type: ContentType::Adult,
            ..*item
        };
        if let Some(hit) = self.core.floor_hit(&adult_item) {
            return Verdict {
                decision: Some(hit.into()),
                shadow: Vec::new(),
            };
        }
        let adult_class = facts.map(|f| f.class.as_adult());
        let adult_facts = facts
            .zip(adult_class.as_ref())
            .map(|(f, class)| Facts { class, ..*f });
        let doc = Doc::new(&adult_item, adult_facts.as_ref()).with_near(self.rules.near_terms());
        if let Some(v) = self.unreadable(&doc) {
            return v;
        }
        self.judge_doc(
            &adult_item,
            adult_facts.as_ref(),
            &doc,
            clock,
            at,
            None,
            None,
        )
        .0
    }

    /// The override and neutralize entries that apply to a torrent:
    /// whether an Override entry releases it, the Neutralize entries, and
    /// the entries in shadow mode.
    fn released(&self, item: &Item, doc: &Doc, clock: &Clock, reading: Reading) -> Released<'_> {
        let mut r = Released::default();
        for (o, p) in self.overrides.prepared() {
            if o.expires_at.is_some_and(|t| clock.reached(t))
                || !rules::override_matches(o, p, item, doc, reading)
                || o.scope.as_ref().is_some_and(|q| !doc.matches(q, &[]))
            {
                continue;
            }
            if o.shadow_until.is_some_and(|t| !clock.reached(t)) {
                r.shadow.push(o.id);
                continue;
            }
            match o.strength {
                Strength::Override => r.overridden = true,
                Strength::Neutralize => r.neutral.push(p),
            }
        }
        r
    }

    /// [`Engine::judge`] past the floor, on a prepared torrent.
    ///
    /// Where a name or a path holds a character the matching once read as
    /// a word break, or a keyword entry that holds one matches differently
    /// in the two readings, the torrent is judged in both readings
    /// ([`Reading`]), each from the top, and the stricter verdict stands
    /// (the earlier reading's on a tie): no torrent is decided more weakly
    /// than it was (docs/decisions/2026-11-02-F-normalize-invisible.md,
    /// docs/decisions/2026-11-02-F-visible-format-two-readings.md).
    /// Elsewhere the two judgments are the same, so one is made.
    ///
    /// `at`: SP-3's level and SP-4's original, from the writer
    /// ([`Engine::judge_at_commit`]); `default()` everywhere else, where
    /// neither matches.
    ///
    /// `seen`: the reading as found's, for the Adult reading ([`Adult`]).
    /// `rec` keeps what each reading found above the pack layer, and the
    /// pack layer's decisions ([`Trail`], [`ViewRec::paired`]).
    #[allow(clippy::too_many_arguments)]
    fn judge_doc(
        &self,
        item: &Item,
        facts: Option<&Facts>,
        doc: &Doc,
        clock: &Clock,
        at: AtCommit,
        seen: Option<&ViewRec>,
        rec: Option<&mut ViewRec>,
    ) -> (Verdict, Vec<PackShadow>) {
        let mut joined_rec = rec.as_ref().map(|r| r.reading(Reading::Joined));
        let mut pack_shadow = Vec::new();
        let joined = self.judge_reading(
            item,
            facts,
            doc,
            clock,
            Reading::Joined,
            at,
            &mut pack_shadow,
            seen.map(|s| &s.joined),
            joined_rec.as_mut(),
        );
        let mut before_rec = None;
        let before = if doc.reads_twice() || self.keywords_differ(item, doc) {
            before_rec = rec.as_ref().map(|r| r.reading(Reading::Before));
            let mut before_shadow = Vec::new();
            let before = self.judge_reading(
                item,
                facts,
                doc,
                clock,
                Reading::Before,
                at,
                &mut before_shadow,
                seen.and_then(|s| s.before.as_ref()),
                before_rec.as_mut(),
            );
            // Each pending rule's stronger side over the two readings.
            for s in before_shadow {
                match pack_shadow.iter_mut().find(|p| p.rule == s.rule) {
                    Some(p) => {
                        p.applied = p.applied.max(s.applied);
                        p.shadow = p.shadow.max(s.shadow);
                    }
                    None => pack_shadow.push(s),
                }
            }
            Some(before)
        } else {
            None
        };
        if let Some(r) = rec {
            r.joined = joined_rec.unwrap_or_default();
            r.before = before_rec;
        }
        (both_readings(joined, before), pack_shadow)
    }

    /// Whether a keyword entry matches this torrent differently in the two
    /// readings: only entries whose keyword reads differently are checked
    /// (`OverrideList::marked`, and keyword exceptions, which are refused
    /// when saved). Where none does, and the text reads the same both
    /// ways, the two judgments are the same.
    fn keywords_differ(&self, item: &Item, doc: &Doc) -> bool {
        let both = |f: &dyn Fn(Reading) -> bool| f(Reading::Joined) != f(Reading::Before);
        self.overrides
            .marked()
            .any(|(o, p)| both(&|r| rules::override_matches(o, p, item, doc, r)))
            || self.exceptions.iter().any(|e| {
                matches!(&e.what, Match::Keyword(k) if crate::normalize::splits([k.as_str()]))
                    && both(&|r| rules::matches(&e.what, item, doc, r))
            })
    }

    /// [`Engine::judge_doc`] in one reading: the verdict, and the shadow
    /// override entries that matched.
    #[allow(clippy::too_many_arguments)]
    fn judge_reading(
        &self,
        item: &Item,
        facts: Option<&Facts>,
        doc: &Doc,
        clock: &Clock,
        reading: Reading,
        at: AtCommit,
        pack_shadow: &mut Vec<PackShadow>,
        seen: Option<&ReadingRec>,
        mut rec: Option<&mut ReadingRec>,
    ) -> (Verdict, Vec<u32>) {
        let mut v = Verdict::default();
        let Released {
            overridden,
            neutral,
            shadow: shadow_overrides,
        } = self.released(item, doc, clock, reading);
        let (neutral_set, neutral_words) = rules::neutral_sets(&neutral);
        if let Some(r) = rec.as_deref_mut() {
            r.neutral.clone_from(&neutral_words);
            r.neutral_set.clone_from(&neutral_set);
            r.released.clone_from(&shadow_overrides);
        }
        // What the reading as found kept holds where the same entries
        // neutralize the same words.
        let seen = seen
            .filter(|s| s.neutral == neutral_words && s.neutral_set == neutral_set)
            .and_then(|s| s.paired.as_ref());
        if !overridden {
            v.decision = self.below_overrides(
                item,
                facts,
                doc,
                &neutral_set,
                &neutral_words,
                clock,
                reading,
                at,
                &mut v.shadow,
                pack_shadow,
                seen,
                rec,
            );
        }
        (v, shadow_overrides)
    }

    /// The layers below CSX-5 in one reading. `seen`: what the reading as
    /// found kept, for the Adult reading of the same torrent: the core's
    /// signals, judged here as Adult; which of your rules this reading
    /// meets; and the matches of your rules that don't read tags, the only
    /// part of the torrent that reads differently as Adult.
    #[allow(clippy::too_many_arguments)]
    fn below_overrides(
        &self,
        item: &Item,
        facts: Option<&Facts>,
        doc: &Doc,
        neutral: &TermSet,
        neutral_words: &[String],
        clock: &Clock,
        reading: Reading,
        at: AtCommit,
        shadow: &mut Vec<Decision>,
        pack_shadow: &mut Vec<PackShadow>,
        seen: Option<&Paired>,
        mut rec: Option<&mut ReadingRec>,
    ) -> Option<Decision> {
        // Where the layers above the pack layer settle the reading.
        let settle = |rec: Option<&mut ReadingRec>, d: Option<Decision>, shadow: &[Decision]| {
            if let Some(r) = rec.filter(|r| r.trail) {
                r.above = Above::Settled(d.clone());
                r.shadow = shadow.to_vec();
            }
            d
        };
        let mut paired = rec.as_deref_mut().and_then(|r| r.paired.as_mut());
        let hits = match reading {
            Reading::Before => self
                .core
                .pattern_hits_split(item, neutral, doc.reads_twice()),
            Reading::Joined => None,
        };
        let hits = hits.unwrap_or_else(|| match seen.and_then(|s| s.signals.as_ref()) {
            Some(s) => self.core.judge_signals(s, item.content_type),
            None => {
                let s = self.core.pattern_signals(item, neutral, doc.read());
                let hits = self.core.judge_signals(&s, item.content_type);
                if let Some(p) = paired.as_deref_mut() {
                    p.signals = Some(s);
                }
                hits
            }
        });
        for hit in hits {
            if hit.on && !self.excepted(RuleRef::Core(hit.rule), item, doc, reading) {
                return settle(rec, Some(hit.into()), shadow);
            }
        }
        // Neutralizing only takes evidence away: an allow query must
        // match without the neutralized words too, so one that needs a
        // neutralized word absent (`-"amber beacon"`) releases nothing
        // more than it did.
        if self.allow.iter().any(|a| {
            doc.matches(&a.query, neutral_words)
                && (neutral_words.is_empty() || doc.matches(&a.query, &[]))
        }) {
            return settle(rec, None, shadow);
        }
        let mut held: Option<Decision> = None;
        let candidates: std::borrow::Cow<[u32]> = match (
            seen.and_then(|s| s.candidates.as_ref()),
            paired.as_deref_mut(),
        ) {
            (Some(c), _) => c.into(),
            (None, Some(p)) => {
                let (here, adult) = doc.candidates_pair(&self.rules, neutral_words, &p.adult_tags);
                p.candidates = Some(adult);
                here.into()
            }
            (None, None) => doc.candidates(&self.rules, neutral_words).into(),
        };
        // What the reading as found checked: a rule that reads no tags is
        // narrowed by neither its query's tags nor a tag gate, so it meets
        // this reading where it met that one, and matches the same.
        let checked_to = seen.and_then(|s| s.checked_to);
        let mut known = seen.map_or(&[][..], |s| &s.matched[..]);
        if let Some(p) = paired.as_deref_mut() {
            p.checked_to = Some(u32::MAX);
        }
        for &i in candidates.iter() {
            let matched = if checked_to.is_some_and(|c| i <= c) && !self.rules.reads_tags(i) {
                while known.first().is_some_and(|k| *k < i) {
                    known = &known[1..];
                }
                known.first() == Some(&i)
            } else {
                let (query, unless) = self.rules.compiled(i);
                doc.matches_compiled(query, neutral_words)
                    && !unless.is_some_and(|u| doc.matches_compiled(u, &[]))
            };
            if !matched {
                continue;
            }
            if let Some(p) = paired.as_deref_mut() {
                p.matched.push(i);
            }
            let r = &self.rules[i as usize];
            if self.excepted(RuleRef::User(r.id), item, doc, reading) {
                continue;
            }
            let d = Decision {
                rule: RuleRef::User(r.id),
                action: r.action,
                evidence: vec![Evidence::Query],
            };
            if r.shadow_until.is_some_and(|t| !clock.reached(t)) {
                shadow.push(d);
                continue;
            }
            // A stand-in for an unreadable rule, or a rule whose `unless`
            // was dropped ([`crate::load`]), never pre-empts what the
            // layers after it would decide: the strongest of them applies,
            // so a drop stays a drop.
            if self.stand_ins.contains(&r.id) {
                held = stronger_of(held, Some(d));
                continue;
            }
            if let Some(p) = paired.as_deref_mut() {
                p.checked_to = Some(i);
            }
            return settle(rec, stronger_of(held, Some(d)), shadow);
        }
        let trail = rec.as_ref().is_some_and(|r| r.trail);
        if trail && let Some(r) = rec.as_deref_mut() {
            r.above = Above::Packs(held.clone());
            r.shadow = shadow.clone();
        }
        let at = self.follows_original(at, item, doc, reading);
        // The Adult reading's context, from what the reading as found read
        // from the name and files.
        let text = seen.and_then(|s| s.pack_text.borrow_mut().take());
        let ctx = facts.map(|f| {
            match text {
                Some(t) => packs::Ctx::with_text(item, f, neutral_words, t),
                None => packs::Ctx::new(item, f, neutral_words),
            }
            .with_flood(at.flood)
            .with_clone(at.clone_of)
        });
        let mut packs: Vec<(usize, Decision)> = Vec::new();
        if let Some(ctx) = &ctx {
            for (o, r) in PackRule::ALL.into_iter().enumerate() {
                if !self.packs.is_on(r) {
                    continue;
                }
                let logged = shadow.len();
                if let Some(d) = self.pack_rule(r, ctx, at, item, doc, reading, shadow, pack_shadow)
                {
                    packs.push((o, d));
                }
                if trail && let Some(rec) = rec.as_deref_mut() {
                    rec.pack_shadow
                        .extend(shadow[logged..].iter().map(|d| (o, d.clone())));
                }
            }
        }
        if let (Some(ctx), Some(p)) = (ctx, rec.as_deref_mut().and_then(|r| r.paired.as_mut())) {
            *p.pack_text.get_mut() = Some(ctx.into_text());
        }
        // Each pack and list is excepted on its own (§20.12), before the
        // strongest action left wins (§20.6): an exception to one list
        // never lifts another that also has the torrent.
        let n = PackRule::ALL.len();
        packs.extend(
            self.listed(item.hashes)
                .enumerate()
                .map(|(i, d)| (n + i, d)),
        );
        packs.retain(|(_, d)| !self.excepted(d.rule, item, doc, reading));
        match rec.filter(|_| trail) {
            Some(r) => {
                let best = strongest(packs.iter().map(|p| &p.1)).cloned();
                r.packs = packs;
                stronger_of(held, best)
            }
            None => stronger_of(held, strongest(packs.into_iter().map(|p| p.1))),
        }
    }

    /// SP-4 follows its original: while the rule that hid it is on, and
    /// not where your exception to that rule covers this torrent.
    fn follows_original(&self, at: AtCommit, item: &Item, doc: &Doc, reading: Reading) -> AtCommit {
        AtCommit {
            clone_of: at.clone_of.filter(|o| {
                self.packs.is_on(o.rule)
                    && !self.excepted(RuleRef::Pack(o.rule), item, doc, reading)
            }),
            ..at
        }
    }

    /// One pack rule's decision, before exceptions: the applied versions'
    /// action with your override over it. A pending version's result is
    /// logged in `shadow` and `pack_shadow` where the rule isn't excepted.
    #[allow(clippy::too_many_arguments)]
    fn pack_rule(
        &self,
        r: PackRule,
        ctx: &packs::Ctx,
        at: AtCommit,
        item: &Item,
        doc: &Doc,
        reading: Reading,
        shadow: &mut Vec<Decision>,
        pack_shadow: &mut Vec<PackShadow>,
    ) -> Option<Decision> {
        let pv = &self.pack_versions;
        // SP-3 flags below its demote level and SP-4 takes its original's
        // action ([`packs::sp4::cap`]), in both versions' results.
        let cap = |a: Option<Action>| a.map(|a| packs::sp4::cap(r, at, a));
        let applied = cap(pv.applied_action(&self.manifest, r, ctx));
        // A pending version runs in shadow where the rule isn't excepted:
        // nothing it holds is applied.
        if pv.state(r).pending()
            && let Some(mut s) = pv.shadow(&self.manifest, r, ctx, applied)
            && !self.excepted(RuleRef::Pack(r), item, doc, reading)
        {
            s.shadow = cap(s.shadow);
            if let Some(action) = s.shadow {
                shadow.push(Decision {
                    rule: RuleRef::Pack(r),
                    action,
                    evidence: vec![Evidence::Pack],
                });
            }
            pack_shadow.push(s);
        }
        // Your override, over what the applied versions decided (the
        // shadow above compares the base rule's sides).
        let applied = match self.pack_overrides.effect(r) {
            None => applied,
            Some(e) => overrides::apply(e, applied, !pv.state(r).unaccepted.is_empty(), |u| {
                doc.matches(u, &[])
            }),
        };
        applied.map(|action| Decision {
            rule: RuleRef::Pack(r),
            action,
            evidence: vec![Evidence::Pack],
        })
    }

    /// A reading the trail kept, judged again with `at` for this torrent.
    fn replay_view(&self, view: &ViewRec, item: &Item, facts: &Facts, at: AtCommit) -> Verdict {
        // Read only where an exception or an override's `unless` is.
        let doc = Doc::new(item, Some(facts)).with_near(self.rules.near_terms());
        let one = |r: &ReadingRec| {
            let v = match &r.above {
                Above::Settled(d) => Verdict {
                    decision: d.clone(),
                    shadow: r.shadow.clone(),
                },
                Above::Packs(held) => self.replay_packs(r, held.clone(), item, facts, &doc, at),
            };
            (v, r.released.clone())
        };
        both_readings(one(&view.joined), view.before.as_ref().map(one))
    }

    /// The pack layer of a kept reading with `at`: SP-3 and SP-4 judged
    /// (in the first judgment, without `at`, neither matched, so neither
    /// is among what it kept), and set in their places among the other
    /// rules' and lists' decisions, which `at` doesn't change.
    fn replay_packs(
        &self,
        r: &ReadingRec,
        held: Option<Decision>,
        item: &Item,
        facts: &Facts,
        doc: &Doc,
        at: AtCommit,
    ) -> Verdict {
        let mut fresh: Vec<(usize, Decision)> = Vec::new();
        let mut logged: Vec<(usize, Decision)> = Vec::new();
        if at.any() {
            let at = self.follows_original(at, item, doc, r.reading);
            let ctx = packs::Ctx::for_state(item, facts, &r.neutral)
                .with_flood(at.flood)
                .with_clone(at.clone_of);
            for (o, rule) in PackRule::ALL.into_iter().enumerate() {
                if !matches!(rule, PackRule::Sp3 | PackRule::Sp4) || !self.packs.is_on(rule) {
                    continue;
                }
                let (mut shadow, mut pack_shadow) = (Vec::new(), Vec::new());
                let d = self.pack_rule(
                    rule,
                    &ctx,
                    at,
                    item,
                    doc,
                    r.reading,
                    &mut shadow,
                    &mut pack_shadow,
                );
                #[cfg(debug_assertions)]
                {
                    // Nothing SP-3 and SP-4 read is in the files.
                    let full = packs::Ctx::new(item, facts, &r.neutral)
                        .with_flood(at.flood)
                        .with_clone(at.clone_of);
                    let (mut s2, mut p2) = (Vec::new(), Vec::new());
                    let d2 =
                        self.pack_rule(rule, &full, at, item, doc, r.reading, &mut s2, &mut p2);
                    assert_eq!(
                        (&d, &shadow, &pack_shadow),
                        (&d2, &s2, &p2),
                        "{rule:?} read the files"
                    );
                }
                if let Some(d) = d
                    && !self.excepted(d.rule, item, doc, r.reading)
                {
                    fresh.push((o, d));
                }
                logged.extend(shadow.into_iter().map(|d| (o, d)));
            }
        }
        // The kept decisions and the new ones in order, as the full pack
        // layer has them: the strongest action wins, the last of equals.
        let mut best: Option<&Decision> = None;
        let mut kept = r.packs.iter().peekable();
        let mut new = fresh.iter().peekable();
        loop {
            let next = match (kept.peek(), new.peek()) {
                (Some(k), Some(n)) if n.0 < k.0 => new.next(),
                (Some(_), _) => kept.next(),
                (None, _) => new.next(),
            };
            let Some((_, d)) = next else { break };
            if best.is_none_or(|b| d.action >= b.action) {
                best = Some(d);
            }
        }
        let mut shadow = r.shadow.clone();
        if logged.is_empty() {
            shadow.extend(r.pack_shadow.iter().map(|p| p.1.clone()));
        } else {
            let mut all: Vec<&(usize, Decision)> = r.pack_shadow.iter().chain(&logged).collect();
            all.sort_by_key(|p| p.0);
            shadow.extend(all.into_iter().map(|p| p.1.clone()));
        }
        Verdict {
            decision: stronger_of(held, best.cloned()),
            shadow,
        }
    }

    /// How many of your rules a torrent meets: those the narrowing
    /// returns, and those that pass the cheap check too and get the full
    /// match. For the §20.8 budget test's report.
    #[doc(hidden)]
    pub fn rule_counts(&self, item: &Item, facts: Option<&Facts>) -> (usize, usize) {
        let doc = Doc::new(item, facts).with_near(self.rules.near_terms());
        let candidates = doc.candidates(&self.rules, &[]);
        let full = candidates
            .iter()
            .filter(|&&i| doc.could_match(self.rules.compiled(i).0))
            .count();
        (candidates.len(), full)
    }

    fn excepted(&self, rule: RuleRef, item: &Item, doc: &Doc, reading: Reading) -> bool {
        // A locked rule takes no exception, whatever a stored row says.
        if matches!(rule, RuleRef::Core(r) if r.locked()) {
            return false;
        }
        self.exceptions
            .iter()
            .any(|e| e.rule == rule && rules::matches(&e.what, item, doc, reading))
    }

    /// The decision on bare hashes, before anything is fetched: the floor,
    /// then a subscribed list that drops, unless something that needs the
    /// torrent's text could still let it through. Lists are released one
    /// by one: an exception to one dropping list doesn't fetch a torrent
    /// another still drops.
    pub fn blocked_hash(&self, hashes: &[[u8; 20]]) -> Option<Decision> {
        if let Some(hit) = self.core.blocked_hash(hashes) {
            return Some(hit.into());
        }
        let by_hash = |m: &Match| match m {
            Match::Hashes(hs) => hashes.iter().any(|h| hs.contains(h)),
            _ => true,
        };
        if !self.allow.is_empty()
            || self
                .overrides
                .iter()
                .any(|o| o.strength == Strength::Override && by_hash(&o.what))
        {
            return None;
        }
        self.listed(hashes)
            .filter(|d| d.action == Action::Drop)
            .filter(|d| {
                !self
                    .exceptions
                    .iter()
                    .any(|e| e.rule == d.rule && by_hash(&e.what))
            })
            .last()
    }

    /// Whether this engine would hold back (quarantine or drop) a held
    /// torrent, judged from what's kept of it: its hash, and the name its
    /// quarantine sealed, classified from that name as an import candidate
    /// is (`reclassify::judge_candidate`). The floor, lists, *Always
    /// allow*, CSX-5, packs, your rules with their exceptions and your
    /// classification rules judge it; what reads files or sizes can't,
    /// without the torrent. Re-judging a stand-in's quarantine keeps
    /// what it says yes to; one it releases is judged in full, files
    /// included, when it's fetched again, before anything is stored
    /// (docs/decisions/2026-11-02-F-filter-standin-undo.md).
    pub fn holds(&self, hash: &[u8; 20], name: &str, now: i64) -> bool {
        let c = crate::reclassify::Candidate {
            hash: *hash,
            hash_v2: None,
            name,
            size: None,
            source_at: None,
        };
        crate::reclassify::judge_candidate(self, c, now)
            .verdict
            .decision
            .is_some_and(|d| d.action >= Action::Quarantine)
    }

    /// Every subscribed list that has one of these hashes, in order.
    fn listed<'a>(&'a self, hashes: &'a [[u8; 20]]) -> impl Iterator<Item = Decision> + 'a {
        self.lists
            .iter()
            .filter(|l| hashes.iter().any(|h| l.hashes.contains(h)))
            .map(|l| Decision {
                rule: RuleRef::List(l.id),
                action: l.action,
                evidence: vec![Evidence::Listed],
            })
    }

    /// Sets (or with `None` removes) a subscribed content list.
    pub fn set_list(&mut self, layer: Option<ListLayer>, id: u32) {
        self.generation = next_generation();
        self.lists.retain(|l| l.id != id);
        if let Some(l) = layer {
            self.lists.push(l);
        }
    }

    /// Keeps only the list layers `keep` names.
    pub fn retain_lists(&mut self, keep: impl Fn(u32) -> bool) {
        self.generation = next_generation();
        self.lists.retain(|l| keep(l.id));
    }
}

/// The stronger of two decisions; the second on a tie (a real rule over
/// a stand-in, so it's the one named).
fn stronger_of(first: Option<Decision>, second: Option<Decision>) -> Option<Decision> {
    match (first, second) {
        (Some(f), Some(s)) => Some(if f.action > s.action { f } else { s }),
        (f, s) => s.or(f),
    }
}

/// The stricter of two verdicts: the stronger decision (the first on a
/// tie), and every shadow decision either logged.
pub(crate) fn stricter(a: Verdict, b: Verdict) -> Verdict {
    let decision = match (a.decision, b.decision) {
        (Some(x), Some(y)) => Some(if y.action > x.action { y } else { x }),
        (x, y) => x.or(y),
    };
    let mut shadow = a.shadow;
    for d in b.shadow {
        if !shadow.contains(&d) {
            shadow.push(d);
        }
    }
    Verdict { decision, shadow }
}

/// The current engine, shared by intake, prepare, commit and the API.
#[derive(Debug)]
pub struct Handle(RwLock<Arc<Engine>>);

impl Handle {
    pub fn new(mut engine: Engine) -> Handle {
        engine.generation = next_generation();
        Handle(RwLock::new(Arc::new(engine)))
    }

    pub fn current(&self) -> Arc<Engine> {
        self.0.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Changes the core: a copy is edited and swapped in under the next
    /// version, so judgments in flight finish on the old one.
    pub fn update<T>(&self, f: impl FnOnce(&mut Core) -> T) -> T {
        self.update_engine(|e| f(&mut e.core))
    }

    /// Changes any layer, like [`Handle::update`].
    pub fn update_engine<T>(&self, f: impl FnOnce(&mut Engine) -> T) -> T {
        let mut slot = self.0.write().unwrap_or_else(|e| e.into_inner());
        let mut next = Engine::clone(&slot);
        let out = f(&mut next);
        next.version += 1;
        next.generation = next_generation();
        *slot = Arc::new(next);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CsList, Lists, Override, Switches, TermSet};
    use lodestar_classify::{ContentType, FileInfo};
    use proptest::prelude::*;

    fn engine() -> Engine {
        let lists = Lists {
            sexual: TermSet::default(),
            child_roles: TermSet::default(),
            codes: TermSet::default(),
        };
        Engine::new(Core::new(lists, CsList::default(), Switches::default()))
    }

    fn item(hashes: &[[u8; 20]]) -> Item<'_> {
        Item {
            hashes,
            name: "x",
            files: &[],
            content_type: ContentType::Other,
        }
    }

    /// An exception set on the engine directly, past validation and
    /// loading, is never taken for a locked rule, and is for a rule that
    /// isn't (every engine entry holds an over-budget torrent first, so
    /// only this reaches the guard).
    #[test]
    fn a_locked_rule_is_never_excepted() {
        let hashes = [[4; 20]];
        let it = item(&hashes);
        let doc = Doc::new(&it, None);
        for rule in CoreRule::ALL {
            let mut e = engine();
            e.exceptions.push(Exception {
                id: 1,
                rule: RuleRef::Core(rule),
                what: Match::Hashes(hashes.to_vec()),
            });
            let excepted = e.excepted(RuleRef::Core(rule), &it, &doc, Reading::Joined);
            assert_eq!(excepted, !rule.locked(), "{rule:?}");
        }
        assert!(CoreRule::ALL.iter().any(|r| r.locked()));
    }

    #[test]
    fn lists_sit_below_the_core() {
        let h = Handle::new(engine());
        let v = h.current().version;
        h.update_engine(|e| {
            e.set_list(
                Some(ListLayer {
                    id: 3,
                    action: Action::Hide,
                    hashes: Arc::new(HashSet::from([[1; 20]])),
                }),
                3,
            )
        });
        let e = h.current();
        assert_eq!(e.version, v + 1);
        let d = e.judge(&item(&[[1; 20]]), None, 0).decision.unwrap();
        assert_eq!((d.rule, d.action), (RuleRef::List(3), Action::Hide));
        assert_eq!(d.rule.id(), "L-3");
        // A hiding list doesn't stop a fetch.
        assert!(e.blocked_hash(&[[1; 20]]).is_none());
        h.update(|c| c.cs.add_hash([1; 20], crate::Origin::Confirmed));
        let d = h
            .current()
            .judge(&item(&[[1; 20]]), None, 0)
            .decision
            .unwrap();
        assert_eq!(d.rule, RuleRef::Core(CoreRule::Cs1a));
        h.update_engine(|e| e.set_list(None, 3));
        assert!(h.current().lists.is_empty());
    }

    #[test]
    fn floor_marked_lists_are_cs1b() {
        let mut e = engine();
        e.core
            .cs
            .set_list(7, Some(Arc::new(HashSet::from([[9; 20]]))));
        let d = e.blocked_hash(&[[9; 20]]).unwrap();
        assert_eq!(d.rule, RuleRef::Core(CoreRule::Cs1b));
        e.core.cs.set_list(7, None);
        assert!(e.blocked_hash(&[[9; 20]]).is_none());
    }

    /// Every way of writing a word the neutralize tests use: spellings,
    /// look-alikes, letter-for-number swaps, spelled-out letters, single
    /// letters that run on over a `/`, and words that aren't neutralized.
    const SPELLINGS: &[&str] = &[
        "web",
        "Web-DL",
        "WEB.DL",
        "W.E.B",
        "w3b",
        "WEBRip",
        "webrip",
        "HEVC",
        "h.e.v.c",
        "h3vc",
        "mkv",
        "Sample",
        "sample",
        "x265",
        "x",
        "265",
        "amber",
        "a",
        "b",
        "c",
        "\u{FF57}eb",
        "w\u{0435}b",
        "we\u{200B}b",
        "S01E02",
        "$ample",
        "@mber",
    ];
    const KEYWORDS: &[&str] = &[
        "web",
        "webrip",
        "hevc",
        "mkv",
        "sample",
        "amber",
        "x265",
        "a",
        "abc",
        "web-dl",
        "\u{FF57}eb",
        "h3vc",
        "s01e02",
    ];

    fn text() -> impl Strategy<Value = String> {
        prop::collection::vec(
            (
                prop::sample::select(SPELLINGS.to_vec()),
                prop::sample::select(vec![" ", ".", "-", "_", ""]),
            ),
            0..6,
        )
        .prop_map(|v| v.into_iter().map(|(w, s)| format!("{w}{s}")).collect())
    }

    /// A torrent: a name, and files in a few folders, in any order.
    fn torrent() -> impl Strategy<Value = (String, Vec<FileInfo>)> {
        let folder = prop::collection::vec(text(), 0..3);
        (
            text(),
            prop::collection::vec(folder, 1..7),
            prop::collection::vec((any::<prop::sample::Index>(), text(), any::<bool>()), 0..14),
        )
            .prop_map(|(name, folders, files)| {
                let files = files
                    .into_iter()
                    .map(|(at, own, padding)| {
                        let mut path = folders[at.index(folders.len())].clone();
                        path.push(format!("{own}.mkv"));
                        FileInfo {
                            padding,
                            ..FileInfo::new(path, 1 << 20)
                        }
                    })
                    .collect();
                (name, files)
            })
    }

    fn entries() -> impl Strategy<Value = Vec<Override>> {
        let entry = (
            prop::sample::select(KEYWORDS.to_vec()),
            prop::option::of(prop::sample::select(KEYWORDS.to_vec())),
            any::<bool>(),
            prop::option::of(0..20i64),
            prop::option::of(0..20i64),
        )
            .prop_map(|(k, more, neutralize, expires_at, shadow_until)| {
                // A Neutralize term is one word; an Override may be more.
                let (what, strength) = match more {
                    Some(m) if !neutralize => (format!("{k} {m}"), Strength::Override),
                    _ if neutralize => (k.to_owned(), Strength::Neutralize),
                    _ => (k.to_owned(), Strength::Override),
                };
                Override {
                    id: 0,
                    what: Match::Keyword(what),
                    strength,
                    scope: None,
                    expires_at,
                    shadow_until,
                }
            });
        prop::collection::vec(entry, 0..7).prop_map(|mut v| {
            for (o, id) in v.iter_mut().zip(1..) {
                o.id = id;
            }
            v
        })
    }

    /// [`Engine::released`] as it was decided before the entries were
    /// prepared once and the texts read in one pass: each keyword's
    /// tokens worked out for each torrent, and each text read by a new
    /// tokenizer as one string.
    fn reference_released(
        overrides: &[Override],
        item: &Item,
        now: i64,
    ) -> (bool, Vec<(u64, String)>, Vec<u32>) {
        let texts: Vec<Vec<crate::normalize::Token>> = std::iter::once(item.name.to_owned())
            .chain(item.files.iter().map(|f| f.path.join("/")))
            .map(|t| {
                let mut out = Vec::new();
                crate::normalize::Tokenizer::default().text(&t, &mut out);
                out
            })
            .collect();
        let (mut overridden, mut neutral, mut shadow) = (false, Vec::new(), Vec::new());
        for o in overrides {
            let Match::Keyword(k) = &o.what else {
                unreachable!("keyword entries only")
            };
            let words = rules::keyword_tokens(&k[..]);
            if o.expires_at.is_some_and(|t| t <= now)
                || !texts.iter().any(|t| rules::in_a_row(&words, t))
            {
                continue;
            }
            if o.shadow_until.is_some_and(|t| t > now) {
                shadow.push(o.id);
                continue;
            }
            match o.strength {
                Strength::Override => overridden = true,
                Strength::Neutralize => {
                    if let Some(t) = words.first() {
                        neutral.push((t.hash, crate::matcher::neutral_key(&k[..])));
                    }
                }
            }
        }
        (overridden, neutral, shadow)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(4000))]

        /// The entries that apply to a torrent are those the reference
        /// finds, over random Neutralize and Override lists, and stay so
        /// as the list changes.
        #[test]
        fn released_entries_match_the_reference(
            (name, files) in torrent(),
            list in entries(),
            more in entries(),
            now in 0..20i64,
        ) {
            let it = Item {
                hashes: &[[1; 20]],
                name: &name,
                files: &files,
                content_type: ContentType::Video,
            };
            let mut e = engine();
            e.overrides = list.into();
            for round in 0..2 {
                let doc = Doc::new(&it, None);
                let r = e.released(&it, &doc, &Clock::new(now), Reading::Joined);
                let neutral: Vec<(u64, String)> =
                    r.neutral.iter().map(|p| p.neutral.clone().unwrap()).collect();
                prop_assert_eq!(
                    (r.overridden, neutral, r.shadow),
                    reference_released(&e.overrides, &it, now),
                    "round {}: {:?} {:?} {:?}", round, name, files, e.overrides
                );
                // A change after the first judgment is honoured.
                e.overrides.extend(more.iter().cloned());
                e.overrides.retain(|o| o.id % 3 != 0);
            }
        }
    }
}
