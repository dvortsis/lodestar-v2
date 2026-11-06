//! SP-4, clones of hidden spam (§20.6, plan 2026-11-04 RP-5 and Q12): a
//! torrent with the payload signature of one SP-2 hid, or SP-3 demoted or
//! hid, inherits that original's action.
//!
//! - **Judged in the writer only,** as SP-3 is: the set grows in commit
//!   order, so prepare's verdict leaves SP-4 out ([`Ctx::clone_of`] is
//!   `None` there). The writer looks the commit's signature up in the
//!   [`crate::spam_sigs::SpamSigs`] set and, on a hit, judges the torrent
//!   again through [`crate::Engine::judge_at_commit`], which applies SP-4
//!   inside the engine's precedence: the floor, CSX-5 Override, the core,
//!   your exceptions, *Always allow*, your rules, then the pack layer,
//!   with SP-4's switches, your exception for it and your override of it,
//!   and the strongest action winning.
//! - **It follows its original.** A clone matches only while the rule
//!   that hid the original is on, and not where an exception to that
//!   rule covers the clone too (an exception for the payload covers every
//!   clone). An exception for the original itself, or its release,
//!   removes the signature (the filters API's release hook).
//! - **The action is the original's** (demote or hide, recorded with the
//!   signature): the version's action, hide, capped at it ([`cap`]). An
//!   override you set (RP-2) replaces it wherever SP-4 matches.
//! - **Versions** follow RP-0. SP-4 was added after RP-0, so on a store
//!   seeded before it shipped it's pending, in shadow, until accepted or
//!   until the spam pack or the rule is switched off and on again; a
//!   fresh store applies it. Its first version `widens`. Its matching
//!   depends on state, so its must-match and must-not-match cases are the
//!   sequence fixtures in `crates/filter/tests/sp4.rs`, not RP-1's
//!   per-item corpus.

use super::{Ctx, PackRule};
use crate::core::Action;
use crate::flood::Level;
use crate::spam_sigs::Original;

/// What the writer adds to a commit's judgment for the rules that depend
/// on state: SP-3's level and SP-4's original. `default()` everywhere but
/// the writer, where neither matches.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AtCommit {
    pub flood: Option<Level>,
    pub clone_of: Option<Original>,
}

impl AtCommit {
    pub fn any(&self) -> bool {
        self.flood.is_some() || self.clone_of.is_some()
    }
}

impl Ctx<'_> {
    /// SP-4: the torrent is a clone of an original still in the set.
    pub(super) fn sp4(&self) -> bool {
        self.clone_of.is_some()
    }

    /// The original this torrent clones, if the writer found one.
    pub fn clone_of(&self) -> Option<Original> {
        self.clone_of
    }

    /// The same torrent, as a clone of `original`.
    pub fn with_clone(mut self, original: Option<Original>) -> Self {
        self.clone_of = original;
        self
    }
}

/// `action`, for `rule` with what the writer found: SP-3 flags below its
/// demote level ([`super::sp3::cap`]); SP-4 takes at most its original's
/// action; every other rule's action as it is.
pub(crate) fn cap(rule: PackRule, at: AtCommit, action: Action) -> Action {
    match (rule, at.clone_of) {
        (PackRule::Sp4, Some(o)) => action.min(o.action),
        _ => super::sp3::cap(rule, at.flood, action),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clone_inherits_at_most_its_originals_action() {
        let at = |action| AtCommit {
            flood: None,
            clone_of: Some(Original {
                rule: PackRule::Sp3,
                action,
            }),
        };
        assert_eq!(
            cap(PackRule::Sp4, at(Action::Demote), Action::Hide),
            Action::Demote
        );
        assert_eq!(
            cap(PackRule::Sp4, at(Action::Hide), Action::Hide),
            Action::Hide
        );
        // Every other rule as it is.
        assert_eq!(
            cap(PackRule::Sp2, at(Action::Demote), Action::Hide),
            Action::Hide
        );
        let flood = AtCommit {
            flood: Some(Level::Flag),
            clone_of: None,
        };
        assert_eq!(cap(PackRule::Sp3, flood, Action::Demote), Action::Flag);
        assert!(!AtCommit::default().any() && flood.any());
    }
}
