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

use crate::core::{Action, Core, CoreRule, Evidence, Hit, Item};
use crate::hash::TermSet;
use crate::matcher::{Doc, Facts};
use crate::packs::{self, PackRule, PackSwitches};
use crate::prefilter::RuleList;
use crate::rules::{self, Allow, ClassRule, Exception, Match, Override, Strength};

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
    pub overrides: Vec<Override>,
    pub exceptions: Vec<Exception>,
    pub allow: Vec<Allow>,
    /// Classification rules, oldest first; not part of filtering.
    pub class_rules: Vec<ClassRule>,
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
            overrides: Vec::new(),
            exceptions: Vec::new(),
            allow: Vec::new(),
            class_rules: Vec::new(),
            version: 1,
        }
    }

    /// What your classification rules say about this torrent: the newest
    /// matching rule's Is adult (`true`) or Not adult (`false`).
    pub fn class_correction(&self, item: &Item, facts: Option<&Facts>) -> Option<bool> {
        if self.class_rules.is_empty() {
            return None;
        }
        let doc = Doc::new(item, facts);
        self.class_rules
            .iter()
            .rev()
            .find(|r| doc.matches(&r.query, &[]))
            .map(|r| r.adult)
    }

    /// The decision for one torrent. `facts` (classification and the
    /// stored fields) let packs and your rules read more than the name
    /// and files; `now` resolves shadow periods, expiries and relative
    /// dates.
    pub fn judge(&self, item: &Item, facts: Option<&Facts>, now: i64) -> Verdict {
        let mut v = Verdict::default();
        if let Some(hit) = self.core.floor_hit(item) {
            v.decision = Some(hit.into());
            return v;
        }
        let doc = Doc::new(item, facts);
        let mut neutral = Vec::new();
        let mut shadow_overrides = Vec::new();
        let mut overridden = false;
        for o in &self.overrides {
            if o.expires_at.is_some_and(|t| t <= now)
                || !rules::matches(&o.what, item, &doc)
                || o.scope.as_ref().is_some_and(|q| !doc.matches(q, &[]))
            {
                continue;
            }
            if o.shadow_until.is_some_and(|t| t > now) {
                shadow_overrides.push(o.id);
                continue;
            }
            match o.strength {
                Strength::Override => overridden = true,
                Strength::Neutralize => neutral.push(o),
            }
        }
        let (neutral_set, neutral_words) = rules::neutral_sets(&neutral);
        if !overridden {
            v.decision = self.below_overrides(
                item,
                facts,
                &doc,
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
        for hit in self.core.pattern_hits(item, neutral) {
            if hit.on && !self.excepted(RuleRef::Core(hit.rule), item, doc) {
                return Some(hit.into());
            }
        }
        if self
            .allow
            .iter()
            .any(|a| doc.matches(&a.query, neutral_words))
        {
            return None;
        }
        for i in doc.candidates(&self.rules) {
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
        let ctx = facts.map(|f| packs::Ctx::new(item, f));
        let packs = PackRule::ALL
            .into_iter()
            .filter(|r| self.packs.is_on(*r) && ctx.as_ref().is_some_and(|c| c.matches(*r)))
            .map(|r| Decision {
                rule: RuleRef::Pack(r),
                action: r.action(),
                evidence: vec![Evidence::Pack],
            });
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
        let doc = Doc::new(item, facts);
        let candidates = doc.candidates(&self.rules);
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
    /// then subscribed lists that drop, unless something that needs the
    /// torrent's text could still let it through.
    pub fn blocked_hash(&self, hashes: &[[u8; 20]]) -> Option<Decision> {
        if let Some(hit) = self.core.blocked_hash(hashes) {
            return Some(hit.into());
        }
        let d = self.listed(hashes).filter(|d| d.action == Action::Drop)?;
        let by_hash = |m: &Match| match m {
            Match::Hashes(hs) => hashes.iter().any(|h| hs.contains(h)),
            _ => true,
        };
        let releasable = !self.allow.is_empty()
            || self
                .overrides
                .iter()
                .any(|o| o.strength == Strength::Override && by_hash(&o.what))
            || self
                .exceptions
                .iter()
                .any(|e| e.rule == d.rule && by_hash(&e.what));
        (!releasable).then_some(d)
    }

    fn listed(&self, hashes: &[[u8; 20]]) -> Option<Decision> {
        let l = self
            .lists
            .iter()
            .filter(|l| hashes.iter().any(|h| l.hashes.contains(h)))
            .max_by_key(|l| l.action)?;
        Some(Decision {
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
    use crate::{CsList, Lists, Switches, TermSet};
    use lodestar_classify::ContentType;

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
}
