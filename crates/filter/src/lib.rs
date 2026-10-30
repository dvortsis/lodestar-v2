//! Content filtering (architecture §20): the rule engine that replaces
//! the hash-only blocklist.
//!
//! This crate holds the parts that judge a single torrent:
//! - [`normalize`]: text to hashed tokens, resistant to look-alike
//!   letters, invisible characters, letter-for-number swaps and
//!   spelled-out words;
//! - [`TermSet`]: hashed term lists, so the core's lists and fixtures
//!   carry no indicative text;
//! - [`payload_sig`]: the payload signature CS-8a matches reposts by;
//! - [`Core`]: the child-safety core with its locked floor, switches and
//!   built-in exceptions.
//!
//! It's pure and CPU-only. Rules run in prepare on the worker and again,
//! authoritatively, at commit (§19.4).

mod core;
mod engine;
mod fasttok;
mod hash;
mod held;
pub mod lists;
pub mod load;
pub mod log_events;
mod matcher;
pub mod normalize;
mod packs;
mod payload;
mod prefilter;
pub mod reclassify;
mod rules;
mod seal;
pub mod strictness;
mod work;

pub use crate::core::{
    Action, Core, CoreRule, CsList, Evidence, Field, Fixture, Hit, Item, Lists, Locked, MAX_FILES,
    Origin, Switches,
};
pub use engine::{Decision, Engine, Handle, ListLayer, RuleRef, Verdict};
pub use hash::{TermSet, TermSetError, term_hash};
pub use held::HeldSet;
pub use matcher::{Doc, Facts};
pub use packs::{Pack, PackRule, PackSwitches};
pub use payload::{PayloadSig, SPECIFIC_BYTES, SPECIFIC_FILES, payload_sig};
pub use prefilter::RuleList;
pub use rules::{
    Allow, ClassRule, ClassRules, Exception, Invalid, MAX_CLASS_RULES, Match, Override,
    OverrideList, Strength, UserRule,
};
pub use seal::{SealError, Sealer};
