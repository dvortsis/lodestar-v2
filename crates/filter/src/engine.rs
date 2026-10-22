//! The engine the pipeline holds (§20.4): the rule set at a version, and
//! a handle that swaps in a new rule set without stopping anything.
//!
//! Layers run in precedence order (§20.1): the child-safety core (floor
//! first), then the pack layer, where subscribed lists sit (§20.13).
//!
//! Prepare stamps its verdict with the version it used; commit trusts a
//! verdict from the built-in worker at the current version and judges
//! again otherwise.

use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use crate::core::{Action, Core, CoreRule, Evidence, Hit, Item};

/// Which rule decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuleRef {
    Core(CoreRule),
    /// A subscribed content list, by subscription id.
    List(u32),
}

impl RuleRef {
    /// The stored id (`filter_hits.rule`, `tombstones.rule`).
    pub fn id(self) -> String {
        match self {
            RuleRef::Core(r) => r.id().to_owned(),
            RuleRef::List(id) => format!("L-{id}"),
        }
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
    pub version: u64,
}

impl Engine {
    pub fn new(core: Core) -> Engine {
        Engine {
            core,
            lists: Vec::new(),
            version: 1,
        }
    }

    /// The decision for one torrent: the first rule that acts, in
    /// precedence order.
    pub fn judge(&self, item: &Item) -> Option<Decision> {
        if let Some(hit) = self.core.verdict(item) {
            return Some(hit.into());
        }
        self.listed(item.hashes)
    }

    /// The decision on bare hashes, before anything is fetched: the floor,
    /// then subscribed lists that drop.
    pub fn blocked_hash(&self, hashes: &[[u8; 20]]) -> Option<Decision> {
        if let Some(hit) = self.core.blocked_hash(hashes) {
            return Some(hit.into());
        }
        self.listed(hashes).filter(|d| d.action == Action::Drop)
    }

    fn listed(&self, hashes: &[[u8; 20]]) -> Option<Decision> {
        let l = self
            .lists
            .iter()
            .find(|l| hashes.iter().any(|h| l.hashes.contains(h)))?;
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
        let d = e.judge(&item(&[[1; 20]])).unwrap();
        assert_eq!((d.rule, d.action), (RuleRef::List(3), Action::Hide));
        assert_eq!(d.rule.id(), "L-3");
        // A hiding list doesn't stop a fetch; a dropping one does.
        assert!(e.blocked_hash(&[[1; 20]]).is_none());
        h.update(|c| c.cs.add_hash([1; 20], crate::Origin::Confirmed));
        let d = h.current().judge(&item(&[[1; 20]])).unwrap();
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
