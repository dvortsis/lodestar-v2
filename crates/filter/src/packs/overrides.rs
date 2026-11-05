//! Pack-rule overrides (RP-2; architecture §20.7, §20.8, §20.11; plan
//! 2026-11-04 Q8): you change what a pack rule **does**, never what it
//! matches.
//!
//! - **What an override is:** an action (hide, demote, flag or drop) in
//!   place of the rule's own, and optionally an `unless` query that keeps
//!   the rule from applying to what it matches. It's stored with the base
//!   version it was made against (the version this install applied then,
//!   [`super::upgrade`]), and nothing else: no author, time or reason.
//! - **Weakening or strengthening** ([`weakens`]): an `unless`, or an
//!   action lighter than the base rule's, weakens filtering (effect warning
//!   and password); a heavier or equal action without `unless` doesn't.
//! - **When the base changes** (an accepted version, or a version state in
//!   doubt) the override is **flagged** ([`Loaded::changed`]): one that
//!   doesn't weaken the rule as it now is keeps applying; one that does is
//!   **paused** ([`Loaded::paused`]) and the base rule applies, until it's
//!   confirmed again against the new version with the password, because
//!   the password confirmed it against a rule that no longer exists.
//! - **A row that doesn't read fails closed** (#357): of its action and the
//!   base rule's, the stronger applies, and its `unless` is dropped. It's
//!   listed in [`crate::Engine::unreadable`] and, by the filters API, in
//!   `filter.stand_ins` as [`label`].
//!
//! Precedence is the pack layer's (§20.1, §20.6 "as built"): the CSX-5
//! Override list, the core, your exceptions and *Always allow* still decide
//! first, an exception to the rule still releases from it, and the
//! strongest pack action still wins. The locked floor is never touched.
//!
//! **Encoding** of a row's value (`filter.pack_override:<RULE-ID>`): lines
//! `1` (the format), the base version, the action's name, and an optional
//! fourth line onward with the `unless` query text.

use lodestar_search::Node;

use super::PackRule;
use super::manifest::Manifest;
use super::upgrade::{PackVersions, UNKNOWN};
use crate::core::Action;
use crate::load::rule_query;

/// The encoding's format version.
pub const FORMAT: &str = "1";

/// The actions an override can set.
pub const ACTIONS: [Action; 4] = [Action::Flag, Action::Demote, Action::Hide, Action::Drop];

/// The `filter.stand_ins` and display label of an unreadable override:
/// `packs.override.MW-1`.
pub fn label(rule: PackRule) -> String {
    format!("{}override.{}", super::upgrade::LABEL_PREFIX, rule.id())
}

/// An override as you set it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackOverride {
    /// The applied version it was made against; [`UNKNOWN`] when the
    /// version state was in doubt then (it never counts as unchanged).
    pub base: u32,
    pub action: Action,
    /// The `unless` query text, as the engine reads it back.
    pub unless: Option<String>,
}

impl PackOverride {
    /// The stored value.
    pub fn encode(&self) -> String {
        let mut s = format!("{FORMAT}\n{}\n{}", self.base, self.action.as_str());
        if let Some(u) = &self.unless {
            s.push('\n');
            s.push_str(u);
        }
        s
    }
}

/// A stored value read part by part, so what reads of a damaged row still
/// counts ([`Loaded`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoded {
    /// The format is one this build reads.
    pub format: bool,
    pub base: Option<u32>,
    pub action: Option<Action>,
    /// `None`: there's none. `Some(None)`: there's one that doesn't read.
    pub unless: Option<Option<(String, Node)>>,
}

impl Decoded {
    /// Reads a stored value. A value that isn't text reads as nothing.
    pub fn read(value: &Result<String, Vec<u8>>) -> Decoded {
        let Ok(text) = value else {
            return Decoded {
                format: false,
                base: None,
                action: None,
                unless: None,
            };
        };
        let mut parts = text.splitn(4, '\n');
        let format = parts.next() == Some(FORMAT);
        let base = parts.next().and_then(|b| b.parse::<u32>().ok());
        let action = parts.next().and_then(action_named);
        // An `unless` that matches every torrent (an empty one, as a
        // trailing newline leaves) would switch the rule off: the API
        // refuses it, so a stored one doesn't read and fails closed.
        let unless = parts.next().map(|u| {
            rule_query(u)
                .filter(|n| *n != Node::All)
                .map(|n| (u.to_owned(), n))
        });
        // A format this build doesn't know: its fields may mean anything.
        if !format {
            return Decoded {
                format,
                base: None,
                action: None,
                unless: unless.map(|_| None),
            };
        }
        Decoded {
            format,
            base,
            action,
            unless,
        }
    }

    /// Every part reads.
    pub fn reads(&self) -> bool {
        self.format
            && self.base.is_some()
            && self.action.is_some()
            && !matches!(self.unless, Some(None))
    }
}

/// An action by its name, as the API and the encoding write it.
pub fn action_named(name: &str) -> Option<Action> {
    [
        Action::Flag,
        Action::Demote,
        Action::Hide,
        Action::Quarantine,
        Action::Drop,
    ]
    .into_iter()
    .find(|a| a.as_str() == name)
}

/// Whether an override with `action` and an `unless` (or not) weakens a
/// rule whose own action is `base`: any `unless`, or a lighter action.
pub fn weakens(action: Action, has_unless: bool, base: Action) -> bool {
    has_unless || action < base
}

/// What an override does to the rule's applied result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Nothing: the base rule applies (paused, or unreadable with no
    /// readable action).
    None,
    /// The action replaces the base rule's, and the `unless` keeps the rule
    /// from applying where it matches. Only for an override that applies
    /// against an unchanged base, whether or not it weakens.
    Replace(Action, Option<Node>),
    /// At least this action: the stronger of it and the base rule's. A
    /// strengthening override against a changed base, or what an
    /// unreadable row keeps.
    AtLeast(Action),
}

/// One rule's stored override, as this load reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    pub rule: PackRule,
    pub decoded: Decoded,
    /// The base rule's action now ([`base_action`]).
    pub base_action: Action,
    /// The base it was made against isn't the one applied now, or the
    /// version state is in doubt: "changed since you edited" (§20.8).
    pub changed: bool,
    /// Changed, and it weakens the rule as it now is: the base rule
    /// applies until it's confirmed again with the password.
    pub paused: bool,
    pub effect: Effect,
}

impl Loaded {
    /// The row doesn't read: it fails closed.
    pub fn unreadable(&self) -> bool {
        !self.decoded.reads()
    }

    /// The stored `unless` text, when it reads.
    pub fn unless_text(&self) -> Option<&str> {
        match &self.decoded.unless {
            Some(Some((t, _))) => Some(t),
            _ => None,
        }
    }
}

/// The base rule's action now: the heaviest of the versions that apply
/// (one, unless the version state is in doubt), or the current version's
/// when none applies yet.
pub fn base_action(manifest: &Manifest, versions: &PackVersions, rule: PackRule) -> Action {
    let entry = manifest.entry(rule);
    versions
        .state(rule)
        .apply
        .iter()
        .filter_map(|v| entry.get(*v))
        .map(|v| v.action)
        .max()
        .unwrap_or(entry.current().action)
}

/// The version an override made now is bound to: the applied one, or
/// [`UNKNOWN`] while the version state is in doubt.
pub fn current_base(versions: &PackVersions, rule: PackRule) -> u32 {
    match versions.state(rule).applied {
        Some(v) if !versions.in_doubt(rule) && v != UNKNOWN => v,
        _ => UNKNOWN,
    }
}

impl Loaded {
    /// Resolves one stored row against the versions that apply now.
    pub fn resolve(
        manifest: &Manifest,
        versions: &PackVersions,
        rule: PackRule,
        value: &Result<String, Vec<u8>>,
    ) -> Loaded {
        let decoded = Decoded::read(value);
        let base_action = base_action(manifest, versions, rule);
        let now = current_base(versions, rule);
        let changed = decoded.base.is_none_or(|b| b == UNKNOWN || b != now);
        let (paused, effect) = if !decoded.reads() {
            // Fails closed: a stronger action is kept, the `unless`
            // dropped.
            let effect = decoded.action.map_or(Effect::None, Effect::AtLeast);
            (false, effect)
        } else {
            let action = decoded.action.expect("reads");
            let unless = match &decoded.unless {
                Some(Some((_, n))) => Some(n.clone()),
                _ => None,
            };
            let weak = weakens(action, unless.is_some(), base_action);
            match (changed, weak) {
                (true, true) => (true, Effect::None),
                (true, false) => (false, Effect::AtLeast(action)),
                (false, _) => (false, Effect::Replace(action, unless)),
            }
        };
        Loaded {
            rule,
            decoded,
            base_action,
            changed,
            paused,
            effect,
        }
    }
}

/// Every pack rule's override, by [`PackRule::index`].
#[derive(Debug, Clone, Default)]
pub struct PackOverrides {
    rules: Vec<Option<Loaded>>,
}

impl PackOverrides {
    /// Resolves the stored rows (`(pack rule id, value)`). A row for a rule
    /// this build doesn't know is left out: it can't apply, and it's kept
    /// in the store as it is.
    pub fn resolve(
        manifest: &Manifest,
        versions: &PackVersions,
        rows: &[(String, Result<String, Vec<u8>>)],
    ) -> PackOverrides {
        let mut rules = vec![None; PackRule::COUNT];
        for (id, value) in rows {
            if let Some(r) = PackRule::from_id(id) {
                rules[r.index()] = Some(Loaded::resolve(manifest, versions, r, value));
            }
        }
        PackOverrides { rules }
    }

    pub fn get(&self, rule: PackRule) -> Option<&Loaded> {
        self.rules.get(rule.index()).and_then(Option::as_ref)
    }

    /// Every override, in [`PackRule::ALL`] order.
    pub fn iter(&self) -> impl Iterator<Item = &Loaded> {
        PackRule::ALL.into_iter().filter_map(|r| self.get(r))
    }

    pub fn is_empty(&self) -> bool {
        self.rules.iter().all(Option::is_none)
    }

    /// The effect on `rule`, when there is one.
    pub(crate) fn effect(&self, rule: PackRule) -> Option<&Effect> {
        match self.get(rule)?.effect {
            Effect::None => None,
            ref e => Some(e),
        }
    }
}

/// The action that applies once `effect` is laid over the base rule's
/// applied result `applied` (`None`: the rule doesn't hold the torrent),
/// with `unless_matches` saying whether a `Replace`'s `unless` matches it.
/// An override never makes the rule hold what it doesn't match. A version
/// no record shows accepted holds at most by quarantine (RP-0), so where
/// one applies (`capped`), an `AtLeast` action is held there too.
pub(crate) fn apply(
    effect: &Effect,
    applied: Option<Action>,
    capped: bool,
    unless_matches: impl FnOnce(&Node) -> bool,
) -> Option<Action> {
    let base = applied?;
    match effect {
        Effect::None => Some(base),
        Effect::Replace(action, unless) => {
            // Only against an unchanged base, which is never in doubt, and
            // every state with an unaccepted version is in doubt (RP-0).
            // Should that ever break, it fails closed: held at most at
            // quarantine, as the version's own action is.
            if unless.as_ref().is_some_and(unless_matches) {
                None
            } else if capped {
                Some((*action).min(Action::Quarantine))
            } else {
                Some(*action)
            }
        }
        Effect::AtLeast(action) => {
            let action = if capped {
                (*action).min(Action::Quarantine)
            } else {
                *action
            };
            Some(base.max(action))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packs::upgrade::Rows;

    fn text(s: &str) -> Result<String, Vec<u8>> {
        Ok(s.to_owned())
    }

    fn seeded() -> PackVersions {
        let m = Manifest::builtin();
        let applied = super::super::upgrade::current_record(&m).encode();
        PackVersions::resolve(
            &m,
            &Rows {
                applied: [Some(Ok(applied.clone())), Some(Ok(applied))],
                marker: Some(Ok("1".into())),
            },
        )
    }

    #[test]
    fn encodes_and_reads_back_with_no_author_time_or_reason() {
        let o = PackOverride {
            base: 3,
            action: Action::Demote,
            unless: Some("group:good\nyear:<1990".into()),
        };
        assert_eq!(o.encode(), "1\n3\ndemote\ngroup:good\nyear:<1990");
        let d = Decoded::read(&Ok(o.encode()));
        assert!(d.reads());
        assert_eq!(d.base, Some(3));
        assert_eq!(d.action, Some(Action::Demote));
        assert_eq!(
            d.unless.as_ref().unwrap().as_ref().unwrap().0,
            "group:good\nyear:<1990"
        );
        let plain = PackOverride { unless: None, ..o };
        assert_eq!(plain.encode(), "1\n3\ndemote");
        assert_eq!(Decoded::read(&Ok(plain.encode())).unless, None);
    }

    #[test]
    fn damaged_rows_read_part_by_part() {
        let d = Decoded::read(&text("1\nx\nhide"));
        assert!(!d.reads());
        assert_eq!(d.action, Some(Action::Hide));
        let d = Decoded::read(&text("1\n2\nsmite"));
        assert_eq!((d.base, d.action), (Some(2), None));
        let d = Decoded::read(&text("1\n2\nflag\nsize:>>"));
        assert!(!d.reads());
        assert_eq!(d.unless, Some(None));
        // A provenance field can't be judged: unreadable, as for your rules.
        assert!(!Decoded::read(&text("1\n2\nflag\n-network:bt")).reads());
        let d = Decoded::read(&text("2\n2\nflag"));
        assert!(!d.format && d.action.is_none() && !d.reads());
        let d = Decoded::read(&Err(vec![0xff]));
        assert!(!d.reads() && d.action.is_none());
        assert!(!Decoded::read(&text("")).reads());
        // An `unless` that matches everything: unreadable, never the rule
        // switched off.
        for v in ["1\n1\nflag\n", "1\n1\nflag\n   "] {
            let d = Decoded::read(&text(v));
            assert!(!d.reads(), "{v:?}");
            assert_eq!(d.unless, Some(None));
            assert_eq!(d.action, Some(Action::Flag));
        }
    }

    #[test]
    fn weakening_is_any_unless_or_a_lighter_action() {
        assert!(weakens(Action::Demote, false, Action::Hide));
        assert!(weakens(Action::Drop, true, Action::Hide));
        assert!(!weakens(Action::Hide, false, Action::Hide));
        assert!(!weakens(Action::Drop, false, Action::Hide));
    }

    #[test]
    fn unchanged_base_applies_changed_base_pauses_only_what_weakens() {
        let m = Manifest::builtin();
        let v = seeded();
        let r = PackRule::Mw1; // hide
        let base = current_base(&v, r);
        assert_ne!(base, UNKNOWN);
        let row = |base: u32, action: Action, unless: Option<&str>| {
            Ok(PackOverride {
                base,
                action,
                unless: unless.map(str::to_owned),
            }
            .encode())
        };
        let l = Loaded::resolve(&m, &v, r, &row(base, Action::Demote, Some("group:x")));
        assert!(!l.changed && !l.paused);
        assert!(matches!(l.effect, Effect::Replace(Action::Demote, Some(_))));
        // Bound to another version: flagged.
        let l = Loaded::resolve(&m, &v, r, &row(base + 1, Action::Demote, None));
        assert!(l.changed && l.paused);
        assert_eq!(l.effect, Effect::None);
        let l = Loaded::resolve(&m, &v, r, &row(base + 1, Action::Drop, Some("group:x")));
        assert!(l.paused, "any unless weakens");
        let l = Loaded::resolve(&m, &v, r, &row(base + 1, Action::Drop, None));
        assert!(l.changed && !l.paused);
        assert_eq!(l.effect, Effect::AtLeast(Action::Drop));
        // Made while in doubt: never unchanged.
        let l = Loaded::resolve(&m, &v, r, &row(UNKNOWN, Action::Hide, None));
        assert!(l.changed && !l.paused);
        // Unreadable: the stronger action kept, the `unless` dropped.
        let l = Loaded::resolve(&m, &v, r, &text(&format!("1\n{base}\ndemote\nsize:>>")));
        assert!(l.unreadable() && !l.paused);
        assert_eq!(l.effect, Effect::AtLeast(Action::Demote));
        let l = Loaded::resolve(&m, &v, r, &Err(vec![1]));
        assert_eq!(l.effect, Effect::None);
    }

    #[test]
    fn apply_never_holds_what_the_rule_doesnt_match() {
        let n = Node::All;
        for e in [
            Effect::None,
            Effect::Replace(Action::Drop, None),
            Effect::AtLeast(Action::Drop),
        ] {
            assert_eq!(apply(&e, None, false, |_| false), None);
        }
        let rep = Effect::Replace(Action::Flag, Some(n));
        assert_eq!(apply(&rep, Some(Action::Hide), false, |_| true), None);
        assert_eq!(
            apply(&rep, Some(Action::Hide), false, |_| false),
            Some(Action::Flag)
        );
        let at = Effect::AtLeast(Action::Demote);
        assert_eq!(
            apply(&at, Some(Action::Hide), false, |_| true),
            Some(Action::Hide),
            "never lighter than the base"
        );
        let at = Effect::AtLeast(Action::Drop);
        assert_eq!(
            apply(&at, Some(Action::Hide), true, |_| true),
            Some(Action::Quarantine),
            "an unaccepted version never drops"
        );
        assert_eq!(
            apply(&at, Some(Action::Hide), false, |_| true),
            Some(Action::Drop)
        );
        // A Replace over a version no record accepted fails closed too.
        let rep = Effect::Replace(Action::Drop, None);
        assert_eq!(
            apply(&rep, Some(Action::Hide), true, |_| false),
            Some(Action::Quarantine)
        );
        let rep = Effect::Replace(Action::Flag, None);
        assert_eq!(
            apply(&rep, Some(Action::Hide), true, |_| false),
            Some(Action::Flag),
            "a lighter action isn't raised"
        );
    }

    #[test]
    fn unknown_rules_are_left_out_and_labels_name_the_rule() {
        let o = PackOverrides::resolve(
            &Manifest::builtin(),
            &seeded(),
            &[("XX-9".into(), text("1\n1\nhide"))],
        );
        assert!(o.is_empty());
        assert_eq!(label(PackRule::Sp1), "packs.override.SP-1");
    }
}
