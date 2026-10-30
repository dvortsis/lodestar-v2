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
//!    action winning.
//!
//! Prepare stamps its verdict with the version it used; commit trusts a
//! verdict from the built-in worker at the current version and judges
//! again otherwise.

use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use lodestar_classify::ContentType;

use crate::core::{Action, Core, CoreRule, Evidence, Hit, Item};
use crate::hash::TermSet;
use crate::matcher::{Doc, Facts};
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
    pub shadow: Vec<Decision>,
}

/// [`Engine::released`].
#[derive(Debug, Default)]
struct Released<'a> {
    overridden: bool,
    neutral: Vec<&'a rules::Prepared>,
    shadow: Vec<u32>,
}

/// A subscribed content list in the pack layer.
#[derive(Debug, Clone)]
pub struct ListLayer {
    pub id: u32,
    pub action: Action,
    pub hashes: Arc<HashSet<[u8; 20]>>,
}

#[derive(Debug, Clone)]
pub struct Engine {
    pub core: Core,
    pub lists: Vec<ListLayer>,
    pub packs: PackSwitches,
    /// Your rules, indexed so a torrent only meets those that could match.
    pub rules: RuleList,
    pub overrides: OverrideList,
    pub exceptions: Vec<Exception>,
    pub allow: Vec<Allow>,
    /// Classification rules (§20.9): they decide what's stored, and an
    /// Is adult one adds the Adult-only core checks.
    pub class_rules: ClassRules,
    pub version: u64,
}

impl Engine {
    /// The core alone: no packs, rules, lists or exceptions.
    pub fn new(core: Core) -> Engine {
        Engine {
            core,
            lists: Vec::new(),
            packs: PackSwitches::default(),
            rules: RuleList::default(),
            overrides: OverrideList::default(),
            exceptions: Vec::new(),
            allow: Vec::new(),
            class_rules: ClassRules::default(),
            version: 1,
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
        if let Some(hit) = self.core.floor_hit(item) {
            let v = Verdict {
                decision: Some(hit.into()),
                shadow: Vec::new(),
            };
            return (v, None);
        }
        let doc = Doc::new(item, facts).with_near(self.rules.near_terms());
        let correction = self.class_rules.decide(&doc);
        let v = self.judge_doc(item, facts, &doc, now);
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

    /// The decision for one torrent. `facts` (classification and the
    /// stored fields) let packs and your rules read more than the name
    /// and files; `now` resolves shadow periods, expiries and relative
    /// dates.
    pub fn judge(&self, item: &Item, facts: Option<&Facts>, now: i64) -> Verdict {
        if let Some(hit) = self.core.floor_hit(item) {
            return Verdict {
                decision: Some(hit.into()),
                shadow: Vec::new(),
            };
        }
        let doc = Doc::new(item, facts).with_near(self.rules.near_terms());
        self.judge_doc(item, facts, &doc, now)
    }

    /// The override and neutralize entries that apply to a torrent:
    /// whether an Override entry releases it, the Neutralize entries, and
    /// the entries in shadow mode.
    fn released(&self, item: &Item, doc: &Doc, now: i64) -> Released<'_> {
        let mut r = Released::default();
        for (o, p) in self.overrides.prepared() {
            if o.expires_at.is_some_and(|t| t <= now)
                || !rules::override_matches(o, p, item, doc)
                || o.scope.as_ref().is_some_and(|q| !doc.matches(q, &[]))
            {
                continue;
            }
            if o.shadow_until.is_some_and(|t| t > now) {
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
    fn judge_doc(&self, item: &Item, facts: Option<&Facts>, doc: &Doc, now: i64) -> Verdict {
        let mut v = Verdict::default();
        let Released {
            overridden,
            neutral,
            shadow: shadow_overrides,
        } = self.released(item, doc, now);
        let (neutral_set, neutral_words) = rules::neutral_sets(&neutral);
        if !overridden {
            v.decision = self.below_overrides(
                item,
                facts,
                doc,
                &neutral_set,
                &neutral_words,
                now,
                &mut v.shadow,
            );
        }
        if let Some(d) = &v.decision {
            for id in shadow_overrides {
                v.shadow.push(Decision {
                    rule: RuleRef::Override(id),
                    action: d.action,
                    evidence: vec![Evidence::Released],
                });
            }
        }
        v
    }

    #[allow(clippy::too_many_arguments)]
    fn below_overrides(
        &self,
        item: &Item,
        facts: Option<&Facts>,
        doc: &Doc,
        neutral: &TermSet,
        neutral_words: &[String],
        now: i64,
        shadow: &mut Vec<Decision>,
    ) -> Option<Decision> {
        for hit in self.core.pattern_hits_read(item, neutral, doc.read()) {
            if hit.on && !self.excepted(RuleRef::Core(hit.rule), item, doc) {
                return Some(hit.into());
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
            return None;
        }
        for i in doc.candidates(&self.rules, neutral_words) {
            let (query, unless) = self.rules.compiled(i);
            if !doc.matches_compiled(query, neutral_words)
                || unless.is_some_and(|u| doc.matches_compiled(u, &[]))
            {
                continue;
            }
            let r = &self.rules[i as usize];
            if self.excepted(RuleRef::User(r.id), item, doc) {
                continue;
            }
            let d = Decision {
                rule: RuleRef::User(r.id),
                action: r.action,
                evidence: vec![Evidence::Query],
            };
            if r.shadow_until.is_some_and(|t| t > now) {
                shadow.push(d);
                continue;
            }
            return Some(d);
        }
        let ctx = facts.map(|f| packs::Ctx::new(item, f, neutral_words));
        let packs = PackRule::ALL
            .into_iter()
            .filter(|r| self.packs.is_on(*r) && ctx.as_ref().is_some_and(|c| c.matches(*r)))
            .map(|r| Decision {
                rule: RuleRef::Pack(r),
                action: r.action(),
                evidence: vec![Evidence::Pack],
            });
        // Each pack and list is excepted on its own (§20.12), before the
        // strongest action left wins (§20.6): an exception to one list
        // never lifts another that also has the torrent.
        packs
            .chain(self.listed(item.hashes))
            .filter(|d| !self.excepted(d.rule, item, doc))
            .max_by_key(|d| d.action)
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

    fn excepted(&self, rule: RuleRef, item: &Item, doc: &Doc) -> bool {
        self.exceptions
            .iter()
            .any(|e| e.rule == rule && rules::matches(&e.what, item, doc))
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
        self.lists.retain(|l| l.id != id);
        if let Some(l) = layer {
            self.lists.push(l);
        }
    }
}

/// The stricter of two verdicts: the stronger decision (the first on a
/// tie), and every shadow decision either logged.
fn stricter(a: Verdict, b: Verdict) -> Verdict {
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
    pub fn new(engine: Engine) -> Handle {
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
            .chain(
                item.files
                    .iter()
                    .take(crate::MAX_FILES)
                    .map(|f| f.path.join("/")),
            )
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
                let r = e.released(&it, &doc, now);
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
