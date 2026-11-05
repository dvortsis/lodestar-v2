//! SP-3, flooding (§20.6, plan 2026-11-04 RP-4 and Q10): one publisher,
//! or one non-DHT source without a publisher, emitting near-duplicate
//! names above a rate. Flagged at [`crate::flood::FLAG_AT`] in an hour,
//! demoted at [`crate::flood::DEMOTE_AT`].
//!
//! - **Judged in the writer only.** The rate depends on the order
//!   torrents are committed in, and prepare runs in parallel (§19.4), so
//!   prepare's verdict leaves SP-3 out: [`Ctx::flood`] is `None` there
//!   and the rule never matches. The writer counts the name in its
//!   [`crate::flood::Flood`] and, once the count reaches a level, judges
//!   the torrent again through [`crate::Engine::judge_flooded`], which
//!   applies SP-3 inside the engine's precedence (CSX-5 Override, your
//!   exceptions, *Always allow*, the switches, your override and the
//!   strongest action in the pack layer).
//! - **One rule, two actions.** The version's action (demote) applies
//!   from the demote level; at the flag level it's capped at flag
//!   ([`cap`]). An override you set (RP-2) replaces both: it changes
//!   the rule's action wherever the rule matches.
//! - **Versions** follow RP-0 as every rule's do. SP-3 was added after
//!   RP-0, so on a store seeded before it shipped it's pending, in
//!   shadow, until accepted or until the spam pack or the rule is
//!   switched off and on again; a fresh store applies it. Its first
//!   version `widens`. Its matching is a sequence (a flood), so its
//!   must-match and must-not-match cases are the fixtures in
//!   `crates/filter/tests/flood.rs`, not RP-1's per-item corpus.

use super::{Ctx, PackRule};
use crate::core::Action;
use crate::flood::Level;

impl Ctx<'_> {
    /// SP-3 with the writer's count: the rule matches at either level.
    pub(super) fn sp3(&self) -> bool {
        self.flood.is_some()
    }

    /// The flooding level the writer counted for this torrent, if any.
    pub fn flood(&self) -> Option<Level> {
        self.flood
    }

    /// The same torrent, with the level the writer counted.
    pub fn with_flood(mut self, level: Option<Level>) -> Self {
        self.flood = level;
        self
    }
}

/// `action`, for `rule` at `level`: SP-3 flags below the demote level,
/// whatever its version's action; every other rule's action as it is.
pub(crate) fn cap(rule: PackRule, level: Option<Level>, action: Action) -> Action {
    match (rule, level) {
        (PackRule::Sp3, Some(Level::Flag)) => action.min(Action::Flag),
        _ => action,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flag_level_caps_only_sp3() {
        assert_eq!(
            cap(PackRule::Sp3, Some(Level::Flag), Action::Demote),
            Action::Flag
        );
        assert_eq!(
            cap(PackRule::Sp3, Some(Level::Demote), Action::Demote),
            Action::Demote
        );
        assert_eq!(
            cap(PackRule::Sp1, Some(Level::Flag), Action::Demote),
            Action::Demote
        );
    }
}
