//! Which pack-rule version applies on this install (RP-0, §20.6, §20.10,
//! owner §1.8.7): the applied-version record, seeding, fail-closed
//! recovery, pending versions in shadow, and acceptance.
//!
//! **State.** `filter.packs.applied` holds, per rule, the version this
//! install applied, one `RULE-ID@version` per line. It's written as two
//! identical rows ([`APPLIED_KEYS`]) in one transaction, so one row that
//! won't read never loses it, with a durable marker ([`SEEDED_KEY`]). None
//! of them is part of `rule_set_prev`: Undo restores rule-set edits, never
//! versions. An entry for a rule this build doesn't know is kept as it is.
//!
//! **Resolution** ([`PackVersions::resolve`]), per known rule:
//! - no row at all: the store was never seeded, and the rules apply as
//!   [`seed_record`] would record them ([`ensure_seeded`] writes it once);
//! - recorded at the current version: it applies;
//! - recorded at an older version whose code is kept: that version
//!   applies, and the current one is **pending**, in shadow;
//! - not recorded (a rule added after RP-0): nothing applies, and it's
//!   pending;
//! - recorded at a version whose code is gone (skipped): the **interim
//!   mode**, the stronger of the oldest retained and the current version;
//! - recorded newer than this build's (a downgrade): the build's current
//!   version applies and the row is left as it is;
//! - one applied row unreadable: the other is used; both read but differ:
//!   per rule, both recorded versions apply, the stronger winning; neither
//!   reads: every rule runs the interim mode.
//!
//! **Unaccepted versions never drop.** A version no readable record shows
//! accepted (in the interim mode, or recorded by only one of two differing
//! rows) holds at most by quarantine: a drop isn't reversible without a
//! preview (§1.8.7). With both rows lost, only version 1 of a rule that
//! existed before RP-0 counts as accepted, since seeding recorded it.
//!
//! Every condition but the ordinary pending one is reported
//! ([`Condition`]), in `filter.stand_ins` and as a Filters & Safety notice.

use std::collections::BTreeMap;

use lodestar_store::Store;

use super::manifest::{Code, Direction, Manifest, RuleEntry};
use super::{Ctx, Pack, PackRule, PackSwitches};
use crate::core::Action;

/// The two identical applied-version rows, and the durable marker that
/// seeding happened (the store reads them with the rule set).
pub use lodestar_store::{
    PACKS_APPLIED_KEYS as APPLIED_KEYS, PACKS_SEEDED_KEY as SEEDED_KEY, PackSeed as Seeded,
    StoredText,
};

/// The stored pack-version rows: the two applied rows, and whether the
/// marker row exists (readable or not).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rows {
    pub applied: [StoredText; 2],
    /// The marker row: `1`, or [`SEEDED_ON_RULES`]`@<time>`.
    pub marker: StoredText,
}

impl Rows {
    /// The rows a rule set was read with.
    pub fn of(set: &lodestar_store::RuleSet) -> Rows {
        Rows {
            applied: set.pack_versions.applied.clone(),
            marker: set.pack_versions.seeded.clone(),
        }
    }

    fn read(store: &Store) -> lodestar_store::Result<Rows> {
        let r = store.pack_version_rows()?;
        Ok(Rows {
            applied: r.applied,
            marker: r.seeded,
        })
    }

    /// Whether any row exists, readable or not: a row that won't read
    /// counts as state, so it never lets seeding run again.
    pub fn exist(&self) -> bool {
        self.marker.is_some() || self.applied.iter().any(Option::is_some)
    }
}

/// The applied-version record: stored rule id to version. Ids this build
/// doesn't know are kept as they are.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Applied(pub BTreeMap<String, u32>);

impl Applied {
    /// `None` when the text isn't a record: a token that isn't
    /// `ID@version`, or an id twice. Version 0 ([`UNKNOWN`]) means the
    /// applied version isn't known.
    pub fn decode(text: &str) -> Option<Applied> {
        let mut map = BTreeMap::new();
        for t in text.split_whitespace() {
            let (id, v) = t.split_once('@')?;
            let v: u32 = v.parse().ok()?;
            if id.is_empty() || map.insert(id.to_owned(), v).is_some() {
                return None;
            }
        }
        Some(Applied(map))
    }

    /// One `ID@version` per line, sorted by id.
    pub fn encode(&self) -> String {
        let lines: Vec<String> = self.0.iter().map(|(id, v)| format!("{id}@{v}")).collect();
        lines.join("\n")
    }

    pub fn get(&self, rule: PackRule) -> Option<u32> {
        self.0.get(rule.id()).copied()
    }

    /// Records `rule` at `version`, unless the record holds a version
    /// newer than this build's current one: a downgraded build never
    /// writes over what a newer one accepted, so the upgrade back finds it.
    fn record(&mut self, manifest: &Manifest, rule: PackRule, version: u32) {
        let cur = manifest.entry(rule).current().version;
        match self.0.get(rule.id()) {
            Some(&v) if v > cur => {}
            _ => {
                self.0.insert(rule.id().to_owned(), version);
            }
        }
    }
}

/// A recorded version of 0: the applied version isn't known, written by
/// an acceptance after both rows were lost for the rules it didn't
/// accept. Such a rule runs the interim mode, as with both rows lost.
pub const UNKNOWN: u32 = 0;

/// The record seeding writes: every rule that existed before RP-0 at its
/// baseline version, 1, SC-2 included (it was part of SC-1).
pub fn seed_record(manifest: &Manifest) -> Applied {
    let mut a = Applied::default();
    for r in PackRule::ALL {
        if manifest.entry(r).baseline {
            a.0.insert(r.id().to_owned(), 1);
        }
    }
    a
}

/// A stored row, decoded: absent, a record, or a row that won't read.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Row {
    Absent,
    Read(Applied),
    Unreadable,
}

fn row(stored: &StoredText) -> Row {
    match stored {
        None => Row::Absent,
        Some(Ok(text)) => Applied::decode(text).map_or(Row::Unreadable, Row::Read),
        Some(Err(_)) => Row::Unreadable,
    }
}

/// A reported condition of the version state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Condition {
    /// One applied row doesn't read; the other is used.
    RowUnreadable,
    /// Neither applied row reads (or both are gone with the marker
    /// there): every rule runs the interim mode.
    Lost,
    /// Both rows read but differ: per rule the stronger applies.
    Differ,
    /// Applied rows exist without the marker: never seeded over.
    MarkerMissing,
    /// The marker row exists but doesn't read: still state, never seeded
    /// over.
    MarkerUnreadable,
    /// The applied version is newer than this build's.
    Downgrade(PackRule),
    /// The applied version's code is gone: the interim mode.
    Skipped(PackRule),
    /// A skipped step narrowed: accepting is weakening.
    Narrowed(PackRule),
    /// A readable record doesn't know what the rule had applied: a rule
    /// that existed before RP-0 is missing from it (seeding recorded every
    /// one, so the record was damaged), or it's recorded at [`UNKNOWN`].
    /// The interim mode, as with both rows lost.
    Unknown(PackRule),
}

impl Condition {
    /// The `filter.stand_ins` label: `packs.lost`, `packs.downgrade.MW-2`.
    pub fn label(self) -> String {
        match self {
            Condition::RowUnreadable => "packs.row".to_owned(),
            Condition::Lost => "packs.lost".to_owned(),
            Condition::Differ => "packs.differ".to_owned(),
            Condition::MarkerMissing => "packs.marker".to_owned(),
            Condition::MarkerUnreadable => "packs.marker-unreadable".to_owned(),
            Condition::Downgrade(r) => format!("packs.downgrade.{}", r.id()),
            Condition::Skipped(r) => format!("packs.skipped.{}", r.id()),
            Condition::Narrowed(r) => format!("packs.narrowed.{}", r.id()),
            Condition::Unknown(r) => format!("packs.unknown.{}", r.id()),
        }
    }
}

/// The `filter.stand_ins` label prefix of [`Condition`]s.
pub const LABEL_PREFIX: &str = "packs.";
/// The marker's value when seeding ran on a store whose rule set wasn't
/// empty: `rules@<unix seconds>` ([`PackVersions::seeded_over_rules`]).
pub const SEEDED_ON_RULES: &str = "rules";

/// What applies of one rule.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuleState {
    /// The version a readable record shows applied, if any.
    pub applied: Option<u32>,
    /// The versions that apply, combined: a match if any matches, the
    /// heaviest action winning. Empty: nothing applies (a pending rule
    /// this install never applied).
    pub apply: Vec<u32>,
    /// Of `apply`, the versions no readable record shows accepted: their
    /// action is held at most at quarantine.
    pub unaccepted: Vec<u32>,
    /// The version pending acceptance, run in shadow: a newer version, or
    /// in the interim mode the current one, which accepting would leave
    /// applying alone, accepted. So the preview always has the torrents an
    /// acceptance changes.
    pub shadow: Option<u32>,
    /// The direction accepting `shadow` would take, every skipped step
    /// combined; `None` when there's no record (weakening).
    pub direction: Option<Direction>,
    /// Exactly the current version applies, by the code in `Ctx::matches`,
    /// with nothing in shadow: the engine's fast path.
    pub plain: bool,
}

impl RuleState {
    fn of(entry: &RuleEntry, side: Side3, applied: Option<u32>, downgrade: bool) -> Self {
        let cur = entry.current().version;
        let (mut apply, mut unaccepted) = side;
        apply.sort_unstable();
        apply.dedup();
        unaccepted.sort_unstable();
        unaccepted.dedup();
        // A downgrade has nothing to accept: the newer record is kept.
        let shadow = (!downgrade && (apply != [cur] || !unaccepted.is_empty())).then_some(cur);
        let direction = shadow.and_then(|cur| entry.direction(applied, cur));
        let plain = apply == [cur]
            && unaccepted.is_empty()
            && matches!(entry.current().code, Some(Code::Current));
        RuleState {
            applied,
            apply,
            unaccepted,
            shadow,
            direction,
            plain,
        }
    }

    /// Whether a version is pending acceptance.
    pub fn pending(&self) -> bool {
        self.shadow.is_some()
    }
}

/// What one pending rule did with one torrent: the applied side and the
/// shadow side, each the action it would take or `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackShadow {
    pub rule: PackRule,
    /// The pending version.
    pub version: u32,
    pub applied: Option<Action>,
    pub shadow: Option<Action>,
}

/// Which side of a [`PackShadow`] holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    AppliedOnly,
    ShadowOnly,
    Both,
}

impl PackShadow {
    pub fn side(&self) -> Option<Side> {
        match (self.applied, self.shadow) {
            (Some(_), None) => Some(Side::AppliedOnly),
            (None, Some(_)) => Some(Side::ShadowOnly),
            (Some(_), Some(_)) => Some(Side::Both),
            (None, None) => None,
        }
    }

    /// The shadow action is heavier than the applied one (both hold).
    pub fn heavier(&self) -> bool {
        matches!((self.applied, self.shadow), (Some(a), Some(s)) if s > a)
    }

    /// The shadow action is lighter than the applied one (both hold).
    pub fn lighter(&self) -> bool {
        matches!((self.applied, self.shadow), (Some(a), Some(s)) if s < a)
    }
}

/// Whether one torrent's shadow result contradicts the pending version's
/// recorded direction, so accepting it is weakening whatever the record
/// says (§20.11: any doubt counts as weakening):
/// - **`widens`** (matches more, same action): a torrent only the applied
///   version holds, or one both hold with a different action;
/// - **`action-heavier`** (matches the same, heavier action): a torrent
///   only one side holds, or one both hold without a heavier action;
/// - any other direction is weakening already, so nothing more is checked.
pub fn disagrees(direction: Direction, s: &PackShadow) -> bool {
    match (direction, s.side()) {
        (_, None) => false,
        (Direction::Widens, Some(Side::AppliedOnly)) => true,
        (Direction::Widens, Some(Side::Both)) => s.applied != s.shadow,
        (Direction::Widens, Some(Side::ShadowOnly)) => false,
        (Direction::ActionHeavier, Some(Side::Both)) => !s.heavier(),
        (Direction::ActionHeavier, Some(_)) => true,
        _ => false,
    }
}

/// Whether accepting a pending version strengthens filtering, so it needs
/// no password: its recorded direction is `widens` or `action-heavier`,
/// the version state has nothing in doubt for it, and no shadow result
/// disagrees ([`disagrees`]). Anything else is weakening.
pub fn acceptance_strengthens<'a>(
    versions: &PackVersions,
    rule: PackRule,
    shadows: impl IntoIterator<Item = &'a PackShadow>,
) -> bool {
    let st = versions.state(rule);
    let Some(direction) = st.direction.filter(|d| d.strengthens()) else {
        return false;
    };
    !versions.in_doubt(rule)
        && shadows
            .into_iter()
            .filter(|s| s.rule == rule)
            .all(|s| !disagrees(direction, s))
}

/// Every rule's resolved state, and the reported conditions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackVersions {
    /// By [`PackRule::index`].
    rules: Vec<RuleState>,
    pub conditions: Vec<Condition>,
    /// No row existed: the store isn't seeded yet, and the rules apply as
    /// [`seed_record`] would record them. Seeding also copies SC-1's
    /// switch and exceptions to SC-2, which the engine does in memory
    /// meanwhile ([`crate::load`]).
    pub unseeded: bool,
    /// Seeding ran, at this time, on a store whose rule set already had
    /// rows: an upgrade from before RP-0, or an install whose version
    /// rows were all lost. Reported for the Filters & Safety notice.
    pub seeded_over_rules: Option<i64>,
}

impl PackVersions {
    pub fn state(&self, rule: PackRule) -> &RuleState {
        &self.rules[rule.index()]
    }

    /// Every rule with a version pending acceptance.
    pub fn pending(&self) -> impl Iterator<Item = (PackRule, &RuleState)> {
        PackRule::ALL
            .into_iter()
            .map(|r| (r, self.state(r)))
            .filter(|(_, s)| s.pending())
    }

    /// Whether the record can't say for sure what `rule` had applied:
    /// both rows lost or differing, its version skipped, or newer than
    /// this build's. Accepting is then weakening.
    pub fn in_doubt(&self, rule: PackRule) -> bool {
        self.conditions.iter().any(|c| match c {
            Condition::Lost | Condition::Differ => true,
            Condition::Downgrade(r)
            | Condition::Skipped(r)
            | Condition::Narrowed(r)
            | Condition::Unknown(r) => *r == rule,
            Condition::RowUnreadable | Condition::MarkerMissing | Condition::MarkerUnreadable => {
                false
            }
        })
    }

    /// The `filter.stand_ins` labels of the conditions.
    pub fn labels(&self) -> Vec<String> {
        self.conditions.iter().map(|c| c.label()).collect()
    }

    /// Resolves the stored rows against `manifest` (the module docs).
    pub fn resolve(manifest: &Manifest, rows: &Rows) -> PackVersions {
        let mut conditions = Vec::new();
        let unseeded = !rows.exist();
        let records: Vec<Applied> = if unseeded {
            vec![seed_record(manifest)]
        } else {
            if rows.marker.is_none() && rows.applied.iter().any(Option::is_some) {
                conditions.push(Condition::MarkerMissing);
            }
            let marker_reads = |t: &str| {
                t == "1"
                    || t.strip_prefix(SEEDED_ON_RULES)
                        .and_then(|t| t.strip_prefix('@'))
                        .is_some_and(|t| t.parse::<i64>().is_ok())
            };
            if matches!(&rows.marker, Some(Err(_)))
                || matches!(&rows.marker, Some(Ok(t)) if !marker_reads(t))
            {
                conditions.push(Condition::MarkerUnreadable);
            }
            let read: Vec<Row> = rows.applied.iter().map(row).collect();
            let ok: Vec<Applied> = read
                .iter()
                .filter_map(|r| match r {
                    Row::Read(a) => Some(a.clone()),
                    _ => None,
                })
                .collect();
            match ok.len() {
                2 if ok[0] != ok[1] => conditions.push(Condition::Differ),
                2 => {}
                // The other row won't read, or is gone with this one
                // still there: reported, and the next write repairs it.
                1 => conditions.push(Condition::RowUnreadable),
                _ => conditions.push(Condition::Lost),
            }
            let mut ok = ok;
            ok.dedup();
            ok
        };
        let rules = PackRule::ALL
            .into_iter()
            .map(|r| (r, manifest.entry(r)))
            .collect::<Vec<_>>();
        let mut states = vec![RuleState::default(); PackRule::COUNT];
        for (r, entry) in rules {
            let st = if records.is_empty() {
                RuleState::of(entry, interim(entry), None, false)
            } else {
                let sides: Vec<(Side3, Option<u32>, bool)> = records
                    .iter()
                    .map(|a| one_record(entry, a.get(r), &mut conditions))
                    .collect();
                if let [(side, applied, downgrade)] = &sides[..] {
                    RuleState::of(entry, side.clone(), *applied, *downgrade)
                } else {
                    // Two differing rows: both sides apply, the stronger
                    // winning; what only one side applies isn't accepted.
                    let ((a, ap_a, dg_a), (b, ap_b, dg_b)) = (&sides[0], &sides[1]);
                    let mut apply: Vec<u32> = a.0.iter().chain(&b.0).copied().collect();
                    apply.sort_unstable();
                    apply.dedup();
                    let unaccepted = apply
                        .iter()
                        .copied()
                        .filter(|v| {
                            a.1.contains(v)
                                || b.1.contains(v)
                                || !(a.0.contains(v) && b.0.contains(v))
                        })
                        .collect();
                    let applied = if ap_a == ap_b { *ap_a } else { None };
                    RuleState::of(entry, (apply, unaccepted), applied, *dg_a && *dg_b)
                }
            };
            states[r.index()] = st;
        }
        conditions.sort_by_key(|c| c.label());
        conditions.dedup();
        let seeded_over_rules = match &rows.marker {
            Some(Ok(t)) => t
                .strip_prefix(SEEDED_ON_RULES)
                .and_then(|t| t.strip_prefix('@'))
                .and_then(|t| t.parse().ok()),
            _ => None,
        };
        PackVersions {
            rules: states,
            conditions,
            unseeded,
            seeded_over_rules,
        }
    }

    /// The action the applied versions take on this torrent, if any.
    pub(crate) fn applied_action(
        &self,
        manifest: &Manifest,
        rule: PackRule,
        ctx: &Ctx<'_>,
    ) -> Option<Action> {
        let st = self.state(rule);
        if st.plain {
            return ctx
                .matches(rule)
                .then(|| manifest.entry(rule).current().action);
        }
        let entry = manifest.entry(rule);
        st.apply
            .iter()
            .filter_map(|v| entry.retained(*v))
            .filter(|v| v.matches(rule, ctx) == Some(true))
            .map(|v| {
                if st.unaccepted.contains(&v.version) {
                    v.action.min(Action::Quarantine)
                } else {
                    v.action
                }
            })
            .max()
    }

    /// The pending version's result beside the applied one, when either
    /// side holds the torrent.
    pub(crate) fn shadow(
        &self,
        manifest: &Manifest,
        rule: PackRule,
        ctx: &Ctx<'_>,
        applied: Option<Action>,
    ) -> Option<PackShadow> {
        let version = self.state(rule).shadow?;
        let v = manifest.entry(rule).retained(version)?;
        let shadow = (v.matches(rule, ctx) == Some(true)).then_some(v.action);
        (applied.is_some() || shadow.is_some()).then_some(PackShadow {
            rule,
            version,
            applied,
            shadow,
        })
    }
}

impl Default for PackVersions {
    /// The built-in manifest over a store never seeded: every rule as
    /// seeding records it.
    fn default() -> PackVersions {
        PackVersions::resolve(&Manifest::builtin(), &Rows::default())
    }
}

/// The versions that apply and, of them, those not accepted.
type Side3 = (Vec<u32>, Vec<u32>);

/// The interim mode without a record: the stronger of the oldest retained
/// and the current version, where only version 1 of a rule that existed
/// before RP-0 is known to be accepted (seeding recorded it).
fn interim(entry: &RuleEntry) -> Side3 {
    let both = vec![entry.oldest_retained().version, entry.current().version];
    let unaccepted = both
        .iter()
        .copied()
        .filter(|v| !(entry.baseline && *v == 1))
        .collect();
    (both, unaccepted)
}

/// One readable record's view of a rule: the versions that apply and
/// those of them not accepted, the accepted version, and whether it's a
/// downgrade.
fn one_record(
    entry: &RuleEntry,
    recorded: Option<u32>,
    conditions: &mut Vec<Condition>,
) -> (Side3, Option<u32>, bool) {
    let rule = entry.rule;
    let cur = entry.current().version;
    match recorded {
        // Missing from a readable record: a rule that existed before RP-0
        // was recorded by seeding, so the record is damaged and the
        // interim mode applies; a newer rule was never applied, so it's
        // pending and nothing applies.
        None if entry.baseline => {
            conditions.push(Condition::Unknown(rule));
            (interim(entry), None, false)
        }
        None => ((Vec::new(), Vec::new()), None, false),
        Some(UNKNOWN) => {
            conditions.push(Condition::Unknown(rule));
            (interim(entry), None, false)
        }
        Some(v) if v == cur => ((vec![cur], Vec::new()), Some(v), false),
        Some(v) if v > cur => {
            // The build's own version applies, uncapped (runner, #424):
            // the version this build ships, as the plan says.
            conditions.push(Condition::Downgrade(rule));
            ((vec![cur], Vec::new()), Some(v), true)
        }
        Some(v) if entry.retained(v).is_some() => ((vec![v], Vec::new()), Some(v), false),
        Some(v) => {
            // Skipped: its code is gone. Neither retained version was
            // accepted.
            conditions.push(Condition::Skipped(rule));
            let narrowed = entry
                .versions()
                .iter()
                .filter(|x| x.version > v && x.version <= cur)
                .any(|x| !x.direction.strengthens())
                || entry.get(v).is_none();
            if narrowed {
                conditions.push(Condition::Narrowed(rule));
            }
            let both = vec![entry.oldest_retained().version, cur];
            ((both.clone(), both), Some(v), false)
        }
    }
}

/// The switches with SC-1's copied to SC-2, as seeding copies them
/// (when SC-1 has a stored row and SC-2 hasn't).
pub fn copy_sc1_switch(stored: &[(String, bool)]) -> Vec<(String, bool)> {
    let mut out = stored.to_vec();
    let has = |id: &str| stored.iter().any(|(i, _)| i == id);
    if let Some((_, on)) = stored.iter().find(|(i, _)| i == PackRule::Sc1.id())
        && !has(PackRule::Sc2.id())
    {
        out.push((PackRule::Sc2.id().to_owned(), *on));
    }
    out
}

/// The record a fresh store is seeded with (RP-DEF): every rule this
/// build knows at its current version, so none is pending.
pub fn current_record(manifest: &Manifest) -> Applied {
    let mut a = Applied::default();
    for r in PackRule::ALL {
        a.0.insert(r.id().to_owned(), manifest.entry(r).current().version);
    }
    a
}

/// Seeds the store once, before the filter is first read: run at every
/// start (the pipeline's start), and by the filters API before any change
/// as the fallback. Three steps, each its own transaction and each done at
/// most once (`docs/decisions/2026-11-05-RP-DEF.md`):
/// 1. **A fresh data directory** (the store's creating migration wrote
///    [`FRESH_KEY`]): every pack without a switch row is switched **on**,
///    the rules it turns on are recorded at their current versions (with
///    no version rows, [`current_record`]), [`DEFAULTS_KEY`] is set and
///    the fresh marker consumed, all together. Rule packs are on by
///    default (owner §2.39).
/// 2. **RP-0's version seeding**, only when no pack-version row exists at
///    all: [`seed_record`], SC-1's stored switch and exceptions copied to
///    SC-2, and the marker, which records `now` when the rule set already
///    had rows ([`SEEDED_ON_RULES`]), so such a seeding is never silent.
/// 3. **A store that existed before** (neither marker): every pack without
///    a switch row gets an explicit **off** row and [`DEFAULTS_KEY`] is
///    set, so nothing flips and filtering is unchanged.
/// 4. **A pack added since the defaults were applied** (RP-UC): `DEFAULTS_KEY`
///    is set once, so on a store an earlier build already set up, a pack
///    that build didn't know (`unauthorised-copy`) gets its explicit off
///    row here, and [`KNOWN_KEY`] records the packs whose defaults are
///    written ([`offer_new_packs`]).
///
/// Returns what changed filtering: `Some` after step 1 (a default
/// [`Seeded`]) or step 2, `None` when the store already had state.
pub fn ensure_seeded(
    store: &mut Store,
    manifest: &Manifest,
    now: i64,
) -> lodestar_store::Result<Option<Seeded>> {
    if seed_fresh(store, manifest)? {
        // Every pack has a row now: this only records that they're known.
        offer_new_packs(store)?;
        return Ok(Some(Seeded::default()));
    }
    let seeded = if Rows::read(store)?.exist() {
        None
    } else {
        store.seed_pack_versions(
            &seed_record(manifest).encode(),
            PackRule::Sc1.id(),
            PackRule::Sc2.id(),
            &format!("{SEEDED_ON_RULES}@{now}"),
        )?
    };
    // Off rows record what already applies: no reload is needed for them.
    // Once they're written, the common case is one read, without taking
    // the write lock; the transaction re-checks both markers itself.
    if matches!(store.setting_text(DEFAULTS_KEY), Ok(None)) {
        store.apply_pack_defaults(false, &pack_names(), None)?;
    }
    offer_new_packs(store)?;
    Ok(seeded)
}

/// The settings row listing the packs whose defaults this store has
/// written, space-separated (RP-UC). With no row, they're the packs the
/// first build that set [`DEFAULTS_KEY`] knew ([`DEFAULTS_PACKS`]).
pub const KNOWN_KEY: &str = "packs.known";

/// The packs RP-DEF's build knew: the defaults step of a store with
/// [`DEFAULTS_KEY`] and no [`KNOWN_KEY`] covered these and no others.
pub const DEFAULTS_PACKS: &[Pack] = &[
    Pack::Malware,
    Pack::Lures,
    Pack::Fakes,
    Pack::Spam,
    Pack::Scams,
];

/// Step 4 of [`ensure_seeded`]: once the defaults are applied (and the
/// fresh marker is gone), every pack this build knows that the store's
/// defaults didn't cover gets an explicit **off** row, unless it has a
/// row (a choice, or a fresh store's on row), and [`KNOWN_KEY`] is
/// written. It reads as off with no row anyway (`load::pack_switches`),
/// so filtering doesn't change: the row makes the state explicit, and the
/// wizard and the filters screen offer the pack. Returns the packs given
/// an off row.
///
/// Done once per new pack: an Undo that later deletes the row leaves it
/// off, as it reads, and isn't undone again. The row goes before the
/// list, and each write is idempotent, so a crash between the two only
/// repeats a write that changes nothing. Like [`seed_fresh`], it reads
/// before it writes: every caller holds the store's one writing
/// connection.
pub fn offer_new_packs(store: &mut Store) -> lodestar_store::Result<Vec<Pack>> {
    let known = match store.setting_text(KNOWN_KEY) {
        // Every pack this build knows is covered already (a newer build's
        // longer list included).
        Ok(Some(k))
            if pack_names()
                .iter()
                .all(|p| k.split_whitespace().any(|w| w == *p)) =>
        {
            return Ok(Vec::new());
        }
        Ok(Some(k)) => k,
        // A row that won't read: the covered packs aren't known, so the
        // first set is assumed. An off row only states what already reads.
        Ok(None) | Err(_) => String::new(),
    };
    // Not set up yet (or the marker won't read): the steps above own it.
    if !matches!(store.setting_text(DEFAULTS_KEY), Ok(Some(_)))
        || !matches!(store.setting_text(FRESH_KEY), Ok(None))
    {
        return Ok(Vec::new());
    }
    let covered: Vec<&str> = if known.is_empty() {
        DEFAULTS_PACKS.iter().map(|p| p.as_str()).collect()
    } else {
        known.split_whitespace().collect()
    };
    let set = store.rule_set()?;
    let has_row = |p: Pack| set.pack_switches.iter().any(|(id, _)| id == p.as_str());
    let new: Vec<Pack> = Pack::ALL
        .into_iter()
        .filter(|p| !covered.contains(&p.as_str()) && !has_row(*p))
        .collect();
    for p in &new {
        store.set_pack_switch(p.as_str(), false)?;
    }
    // The union: a pack a newer build recorded stays recorded after a
    // downgrade, so its upgrade back doesn't take it for new.
    let mut known: Vec<&str> = covered;
    for p in pack_names() {
        if !known.contains(&p) {
            known.push(p);
        }
    }
    store.update_settings_text(&[(KNOWN_KEY, &known.join(" "))], &[])?;
    Ok(new)
}

/// The settings rows of RP-DEF's markers.
pub use lodestar_store::{PACKS_DEFAULTS_KEY as DEFAULTS_KEY, PACKS_FRESH_KEY as FRESH_KEY};

fn pack_names() -> Vec<&'static str> {
    Pack::ALL.into_iter().map(Pack::as_str).collect()
}

/// Step 1 of [`ensure_seeded`]. A pack that already has a switch row keeps
/// it (a choice); every other pack is switched on, recording its rules as
/// [`set_pack_switch`] does. Returns whether the fresh marker was there
/// and is now consumed.
///
/// The rows are read before the write transaction, as in
/// [`set_pack_switch`]: every caller holds the store's one writing
/// connection (the pipeline's start runs before the filters API exists,
/// and the API calls this under its store lock), so nothing writes
/// between the read and the write. The transaction still re-checks the
/// fresh marker, so a second caller never seeds twice.
fn seed_fresh(store: &mut Store, manifest: &Manifest) -> lodestar_store::Result<bool> {
    // The common case, cheaply: no marker, so the rule set isn't read. A
    // marker that won't read as text still counts as there.
    if matches!(store.setting_text(FRESH_KEY), Ok(None)) {
        return Ok(false);
    }
    let set = store.rule_set()?;
    let stored = crate::load::pack_switches(&set.pack_switches);
    let has_row = |p: Pack| set.pack_switches.iter().any(|(id, _)| id == p.as_str());
    let packs: Vec<Pack> = Pack::ALL.into_iter().filter(|p| !has_row(*p)).collect();
    let rows = Rows::of(&set);
    let texts = if rows.exist() {
        let rules: Vec<PackRule> = packs
            .iter()
            .flat_map(|p| turned_on(&stored, p.as_str(), true))
            .collect();
        merged(manifest, &rows, &rules)
    } else {
        let a = current_record(manifest).encode();
        Some([a.clone(), a])
    };
    let names: Vec<&str> = packs.iter().map(|p| p.as_str()).collect();
    store.apply_pack_defaults(
        true,
        &names,
        texts.as_ref().map(|[a, b]| [a.as_str(), b.as_str()]),
    )
}

/// Both applied rows with `rules` recorded at their current versions:
/// each readable row merged on its own, keeping versions newer than this
/// build's and ids it doesn't know; an unreadable one replaced by the
/// readable one's merge. `None` when there's nothing to record or neither
/// row reads (a partial record would leave the other rules pending, while
/// the interim mode applies them all).
fn merged(manifest: &Manifest, rows: &Rows, rules: &[PackRule]) -> Option<[String; 2]> {
    if rules.is_empty() {
        return None;
    }
    let read: Vec<Option<Applied>> = rows
        .applied
        .iter()
        .map(|r| match row(r) {
            Row::Read(mut a) => {
                for r in rules {
                    a.record(manifest, *r, manifest.entry(*r).current().version);
                }
                Some(a)
            }
            _ => None,
        })
        .collect();
    match (&read[0], &read[1]) {
        (Some(a), Some(b)) => Some([a.encode(), b.encode()]),
        (Some(a), None) | (None, Some(a)) => Some([a.encode(), a.encode()]),
        (None, None) => None,
    }
}

/// The rules a pack switch turns on, off → on only, read against the
/// stored switches: a pack switched on records its rules not switched
/// off; a rule switched back on records itself while its pack is on.
/// Anything else (on → on, a rule in a pack that's off, switching off)
/// records nothing.
pub fn turned_on(stored: &PackSwitches, id: &str, on: bool) -> Vec<PackRule> {
    if !on {
        return Vec::new();
    }
    if let Some(p) = Pack::from_name(id) {
        if stored.on.contains(&p) {
            return Vec::new();
        }
        return PackRule::ALL
            .into_iter()
            .filter(|r| r.pack() == p && !stored.rules_off.contains(r))
            .collect();
    }
    match PackRule::from_id(id) {
        Some(r) if stored.rules_off.contains(&r) && stored.on.contains(&r.pack()) => vec![r],
        _ => Vec::new(),
    }
}

/// Sets a pack switch, recording the current versions of the rules it
/// turns on ([`turned_on`]) in the same transaction: previewed against
/// them under §20.10, so they need no second acceptance. Each readable
/// applied row is merged on its own, per rule, keeping versions newer
/// than this build's and ids it doesn't know; an unreadable one is
/// replaced by the readable one's merge. With neither readable, nothing
/// is recorded: a partial record would leave the other rules pending,
/// while the interim mode applies them all. Returns the rules recorded.
pub fn set_pack_switch(
    store: &mut Store,
    manifest: &Manifest,
    id: &str,
    on: bool,
    now: i64,
) -> lodestar_store::Result<Vec<PackRule>> {
    ensure_seeded(store, manifest, now)?;
    let set = store.rule_set()?;
    let stored = crate::load::pack_switches(&set.pack_switches);
    let rules = turned_on(&stored, id, on);
    let Some([a, b]) = merged(manifest, &Rows::of(&set), &rules) else {
        // Nothing to record: the switch alone, as before RP-0.
        store.set_pack_switch(id, on)?;
        return Ok(Vec::new());
    };
    store.write_pack_versions([Some(&a), Some(&b)], Some((id, on)))?;
    Ok(rules)
}

/// Accepts the pending versions of `rules` (RP-3 wires it to the API):
/// each accepted rule at its current version, the marker, and nothing
/// else changed. Versions newer than this build's, and ids it doesn't
/// know, are kept.
/// - **One record** (both rows the same, or one readable): written to both
///   rows, which repairs an unreadable one.
/// - **Two differing rows:** each row is merged on its own, so a rule
///   nobody accepted keeps both rows' versions (both still apply, the
///   stronger winning) rather than one being picked unpreviewed.
/// - **Neither readable:** a fresh record with the accepted rules at their
///   current versions and every other known rule at [`UNKNOWN`], which
///   keeps them in the interim mode as before. Ids this build doesn't
///   know can't be recovered.
///
/// Returns the first row written.
///
/// It only switches versions. The caller reloads the engine, so the
/// writer's sweeps re-judge through the ordinary rule-change path; nothing
/// is released (Q7): tombstones and stored holds the new version wouldn't
/// make stay as they are, for review, and only `release` with the warning
/// and password lets them go. Assessing whether it's weakening
/// ([`acceptance_strengthens`]) is the caller's, before this.
pub fn accept(
    store: &mut Store,
    manifest: &Manifest,
    rules: &[PackRule],
    now: i64,
) -> lodestar_store::Result<Applied> {
    ensure_seeded(store, manifest, now)?;
    let rows = Rows::read(store)?;
    let accept_into = |a: &Applied| {
        let mut a = a.clone();
        for r in rules {
            a.record(manifest, *r, manifest.entry(*r).current().version);
        }
        a
    };
    let read: Vec<Option<Applied>> = rows
        .applied
        .iter()
        .map(|r| match row(r) {
            Row::Read(a) => Some(a),
            _ => None,
        })
        .collect();
    let (first, second) = match (&read[0], &read[1]) {
        (Some(a), Some(b)) if a != b => (accept_into(a), accept_into(b)),
        (Some(a), _) | (None, Some(a)) => {
            let a = accept_into(a);
            (a.clone(), a)
        }
        (None, None) => {
            let mut fresh = Applied::default();
            for r in PackRule::ALL {
                fresh.0.insert(r.id().to_owned(), UNKNOWN);
            }
            let fresh = accept_into(&fresh);
            (fresh.clone(), fresh)
        }
    };
    let (a, b) = (first.encode(), second.encode());
    store.write_pack_versions([Some(&a), Some(&b)], None)?;
    Ok(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_record_round_trips_and_refuses_bad_text() {
        let a = Applied::decode("MW-1@1\nSC-2@3 XX-9@7").unwrap();
        assert_eq!(a.get(PackRule::Sc2), Some(3));
        assert_eq!(a.0.get("XX-9"), Some(&7), "unknown ids are kept");
        assert_eq!(Applied::decode(&a.encode()), Some(a));
        assert_eq!(Applied::decode(""), Some(Applied::default()));
        assert_eq!(
            Applied::decode("MW-1@0").unwrap().get(PackRule::Mw1),
            Some(UNKNOWN)
        );
        for bad in ["MW-1", "MW-1@", "MW-1@x", "@1", "MW-1@1 MW-1@2", "MW-1@-1"] {
            assert_eq!(Applied::decode(bad), None, "{bad}");
        }
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            /// The record's decoder never panics, and what it reads
            /// encodes back to a text it reads the same.
            #[test]
            fn decode_never_panics_and_round_trips(text in "[A-Z0-9@ \n\t.-]{0,80}") {
                if let Some(a) = Applied::decode(&text) {
                    prop_assert_eq!(Applied::decode(&a.encode()), Some(a));
                }
            }

            #[test]
            fn records_round_trip(
                map in prop::collection::btree_map("[A-Z]{1,3}-[0-9]{1,2}", 0..1000u32, 0..20),
            ) {
                let a = Applied(map);
                prop_assert_eq!(Applied::decode(&a.encode()), Some(a));
            }
        }
    }

    #[test]
    fn the_seed_has_every_baseline_rule_at_one() {
        let s = seed_record(&Manifest::builtin());
        let added = super::super::manifest::ADDED_AFTER_RP0;
        assert_eq!(s.0.len(), PackRule::COUNT - added.len());
        assert!(
            added.iter().all(|r| s.get(*r).is_none()),
            "never recorded by seeding"
        );
        assert!(s.0.values().all(|v| *v == 1));
        assert_eq!(s.get(PackRule::Sc2), Some(1));
    }

    #[test]
    fn unseeded_and_seeded_resolve_to_today() {
        let m = Manifest::builtin();
        let none = PackVersions::resolve(&m, &Default::default());
        assert!(none.unseeded && none.conditions.is_empty());
        let rows = Rows {
            applied: [
                Some(Ok(seed_record(&m).encode())),
                Some(Ok(seed_record(&m).encode())),
            ],
            marker: Some(Ok("1".into())),
        };
        let seeded = PackVersions::resolve(&m, &rows);
        assert!(!seeded.unseeded && seeded.conditions.is_empty());
        // Rules added after RP-0 never applied on a store seeded before
        // them: nothing of theirs applies, and they're pending, in shadow.
        // Rules with a version after their first (MW-2@2 and FK-2@2,
        // RP-1b) keep version 1 applied, with the new one pending.
        let added = super::super::manifest::ADDED_AFTER_RP0;
        let bumped = [PackRule::Mw2, PackRule::Fk2];
        for r in PackRule::ALL {
            if added.contains(&r) {
                for st in [none.state(r), seeded.state(r)] {
                    assert!(st.apply.is_empty() && st.pending(), "{}", r.id());
                    assert_eq!(st.direction, Some(Direction::Widens), "{}", r.id());
                }
            } else if bumped.contains(&r) {
                for st in [none.state(r), seeded.state(r)] {
                    assert!(st.pending(), "{}", r.id());
                    assert_eq!(st.direction, Some(Direction::Narrows), "{}", r.id());
                }
            } else {
                assert!(none.state(r).plain && seeded.state(r).plain, "{}", r.id());
            }
        }
        assert_eq!(seeded.pending().count(), added.len() + bumped.len());
        // A fresh store's record has every rule at its current version.
        let fresh = Rows {
            applied: [
                Some(Ok(current_record(&m).encode())),
                Some(Ok(current_record(&m).encode())),
            ],
            marker: Some(Ok("1".into())),
        };
        let fresh = PackVersions::resolve(&m, &fresh);
        assert!(PackRule::ALL.iter().all(|r| fresh.state(*r).plain));
    }

    #[test]
    fn shadow_disagreement() {
        use Action::*;
        let s = |applied, shadow| PackShadow {
            rule: PackRule::Mw2,
            version: 2,
            applied,
            shadow,
        };
        let w = Direction::Widens;
        assert!(!disagrees(w, &s(None, Some(Hide))));
        assert!(!disagrees(w, &s(Some(Hide), Some(Hide))));
        assert!(disagrees(w, &s(Some(Hide), None)), "an applied-only hold");
        assert!(
            disagrees(w, &s(Some(Hide), Some(Demote))),
            "a lighter action"
        );
        assert!(
            disagrees(w, &s(Some(Hide), Some(Drop))),
            "not the same action"
        );
        let h = Direction::ActionHeavier;
        assert!(!disagrees(h, &s(Some(Hide), Some(Drop))));
        assert!(disagrees(h, &s(Some(Hide), Some(Hide))), "not heavier");
        assert!(disagrees(h, &s(Some(Hide), Some(Flag))), "lighter");
        assert!(disagrees(h, &s(Some(Hide), None)), "narrowed");
        assert!(disagrees(h, &s(None, Some(Drop))), "widened");
        assert!(!disagrees(w, &s(None, None)) && !disagrees(h, &s(None, None)));
        assert!(!disagrees(Direction::Narrows, &s(Some(Hide), None)));
    }

    #[test]
    fn turning_on_records_off_to_on_only() {
        let s = |on: &[Pack], off: &[PackRule]| PackSwitches {
            on: on.iter().copied().collect(),
            rules_off: off.iter().copied().collect(),
        };
        let off = s(&[], &[PackRule::Mw2]);
        assert_eq!(
            turned_on(&off, "malware", true),
            [
                PackRule::Mw4,
                PackRule::Mw1,
                PackRule::Mw3,
                PackRule::Mw5,
                PackRule::Mw6
            ]
        );
        let on = s(&[Pack::Malware], &[PackRule::Mw2]);
        assert!(turned_on(&on, "malware", true).is_empty(), "on → on");
        assert!(turned_on(&on, "malware", false).is_empty());
        assert_eq!(turned_on(&on, "MW-2", true), [PackRule::Mw2]);
        assert!(turned_on(&on, "MW-1", true).is_empty(), "MW-1 was on");
        assert!(turned_on(&off, "MW-2", true).is_empty(), "its pack is off");
        assert!(turned_on(&on, "nope", true).is_empty());
    }
}
