//! The packs API (RP-3; architecture §20.7, §20.8, §20.10, §20.11): pack
//! and rule versions with their changelogs, pending versions and what
//! accepting them does, and the pack corpora's results.
//!
//! **Accepting a pending version** (`accept_pack_versions`) strengthens
//! filtering only when every accepted version's recorded direction is
//! `widens` or `action-heavier` and its shadow results agree
//! ([`pack_upgrade::acceptance_strengthens`]). Anything else is weakening:
//! the effect warning and the password, every time.
//!
//! **Shadow results come from `filter_hits`.** Ingest logs a pending
//! version's result as a shadow hit of its rule where the pending version
//! holds a torrent, and the applied version's as an ordinary hit where the
//! rule decides it. Per torrent in the replay window, the heaviest of each
//! is that torrent's [`PackShadow`]. Only hits after the shadow started
//! count ([`SHADOW_SINCE_KEY`], recorded when the version is first seen
//! pending, kept across restarts); before that's recorded, every hit in
//! the window counts, which can only read as more disagreement. A hit
//! whose action doesn't read, or more hits than are read, leaves the
//! shadow results in doubt, and accepting is then weakening.
//!
//! **Nothing is released.** Holds of an accepted rule (quarantine and
//! tombstones) that the shadow results don't show the new version making
//! stay as they are, and are listed for review ([`REVIEW_KEY`]).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use lodestar_filter::corpus::{Corpus, Expect};
use lodestar_filter::packs::manifest::{self, Manifest};
use lodestar_filter::packs::overrides::{self as pack_overrides, Effect, PackOverrides};
use lodestar_filter::packs::upgrade::{self as pack_upgrade, PackShadow, PackVersions};
use lodestar_filter::{Action, Pack, PackRule};
use lodestar_store::{SHADOW, Store};

use super::{PREVIEW_ROWS, REPLAY_SECS, bad, conflict, hex};
use crate::dto::filters::{
    ChangelogEntry, PackAcceptance, PackCorpus, PackFixtureCounts, PendingPackVersion,
};
use crate::error::{ApiResult, internal};

/// The holds an acceptance left for review: one `RULE-ID <40 hex>` per
/// line. Not part of Undo; no time, author or reason (§20.8).
pub(crate) const REVIEW_KEY: &str = "filter.packs.review";
/// Lines the review row keeps at most, newest first: as many holds as an
/// acceptance reads (quarantine and tombstones, [`PREVIEW_ROWS`] each), so
/// one acceptance never loses a line to the cap. Pruning keeps it to holds
/// that still exist.
const REVIEW_MAX: usize = 2 * PREVIEW_ROWS;
/// Hits of one rule read for its shadow results at most.
const HIT_ROWS: usize = 1_000_000;

/// Where each pending version's shadow results start: one
/// `RULE-ID@version <filter_hits id>` per line, the highest hit id when the
/// API first saw that version pending. Hits after it were judged with the
/// version in shadow; it survives restarts, and it's an id, not a time
/// (§20.8). Entries for versions no longer pending are dropped.
pub(crate) const SHADOW_SINCE_KEY: &str = "filter.packs.shadow_since";

/// The rules whose stored switches are on, as the next load reads them
/// (SC-1's switch standing in for SC-2's before seeding).
fn switches(set: &lodestar_store::RuleSet) -> lodestar_filter::PackSwitches {
    lodestar_filter::load::pack_switches(&pack_upgrade::copy_sc1_switch(&set.pack_switches))
}

/// The versions pending acceptance of rules that are on: a rule or pack
/// that's off runs no shadow, and switching it on records its current
/// version (RP-0), so nothing of it waits.
pub(super) fn pending_on(
    set: &lodestar_store::RuleSet,
    versions: &PackVersions,
) -> Vec<(PackRule, u32)> {
    let on = switches(set);
    versions
        .pending()
        .filter(|(r, _)| on.is_on(*r))
        .filter_map(|(r, st)| Some((r, st.shadow?)))
        .collect()
}

fn read_shadow_since(store: &Store) -> ApiResult<HashMap<String, i64>> {
    let text = match store.setting_text(SHADOW_SINCE_KEY) {
        Ok(t) => t,
        // A row that doesn't read is replaced; meanwhile every hit counts.
        Err(lodestar_store::Error::Corrupt(_)) => None,
        Err(e) => return Err(internal(e)),
    };
    Ok(text
        .as_deref()
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (key, id) = l.split_once(' ')?;
            Some((key.to_owned(), id.parse().ok()?))
        })
        .collect())
}

/// Records where the shadow of each newly pending version starts, and
/// drops the entries of versions no longer pending. Run at start and after
/// every change, under the store lock.
pub(super) fn note_shadows(store: &mut Store, manifest: &Manifest) -> ApiResult<()> {
    let set = store.rule_set().map_err(internal)?;
    let versions = PackVersions::resolve(manifest, &pack_upgrade::Rows::of(&set));
    let pending = pending_on(&set, &versions);
    let before = read_shadow_since(store)?;
    let unreadable = matches!(
        store.setting_text(SHADOW_SINCE_KEY),
        Err(lodestar_store::Error::Corrupt(_))
    );
    let mut after = HashMap::new();
    let mut last = None;
    for (r, v) in pending {
        let key = format!("{}@{v}", r.id());
        let id = match before.get(&key) {
            Some(id) => *id,
            None => match last {
                Some(id) => id,
                None => {
                    let id: i64 = store
                        .connection()
                        .query_row("SELECT coalesce(max(id), 0) FROM filter_hits", [], |r| {
                            r.get(0)
                        })
                        .map_err(internal)?;
                    last = Some(id);
                    id
                }
            },
        };
        after.insert(key, id);
    }
    if after == before && !unreadable {
        return Ok(());
    }
    let mut lines: Vec<String> = after.iter().map(|(k, id)| format!("{k} {id}")).collect();
    lines.sort();
    if lines.is_empty() {
        store.update_settings_text(&[], &[SHADOW_SINCE_KEY])
    } else {
        store.set_settings_text(&[(SHADOW_SINCE_KEY, &lines.join("\n"))])
    }
    .map_err(internal)
}

/// The action that applies of a pack rule now: the base action, or the
/// override's where it applies (RP-2).
pub(super) fn applied_action(
    manifest: &Manifest,
    versions: &PackVersions,
    stored: &PackOverrides,
    rule: PackRule,
) -> Action {
    let base = pack_overrides::base_action(manifest, versions, rule);
    match stored.get(rule).map(|l| &l.effect) {
        Some(Effect::Replace(a, _)) => *a,
        Some(Effect::AtLeast(a)) => (*a).max(base),
        Some(Effect::None) | None => base,
    }
}

/// A pack's version: its rules' versions as this build ships them.
pub(super) fn pack_version(manifest: &Manifest, pack: Pack) -> String {
    manifest
        .pack_version(pack)
        .iter()
        .map(|(r, v)| format!("{}@{v}", r.id()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The changelog entries after `from` up to `to`, newest first.
fn changelog(
    manifest: &Manifest,
    rule: PackRule,
    from: Option<u32>,
    to: u32,
) -> Vec<ChangelogEntry> {
    let mut out: Vec<ChangelogEntry> = manifest
        .entry(rule)
        .versions()
        .iter()
        .filter(|v| v.version > from.unwrap_or(0) && v.version <= to)
        .map(|v| ChangelogEntry {
            direction: v.direction.as_str().to_owned(),
            text: manifest::changelog_entry(rule, v.version).map(|t| t.trim().to_owned()),
            version: v.version,
        })
        .collect();
    out.reverse();
    out
}

/// Every version waiting to be accepted, of rules that are on, with its
/// changelog entries.
pub(super) fn pending(
    manifest: &Manifest,
    set: &lodestar_store::RuleSet,
    versions: &PackVersions,
) -> Vec<PendingPackVersion> {
    pending_on(set, versions)
        .into_iter()
        .map(|(r, version)| {
            let st = versions.state(r);
            // A version recorded at 0 isn't known: every entry up to the
            // pending one.
            let from = st.applied.filter(|v| *v != pack_upgrade::UNKNOWN);
            PendingPackVersion {
                applied: from,
                changelog: changelog(manifest, r, from, version),
                direction: st.direction.map(|d| d.as_str().to_owned()),
                pack: r.pack().as_str().to_owned(),
                rule: r.id().to_owned(),
                version,
            }
        })
        .collect()
}

/// One rule's shadow results in the replay window: per torrent, the
/// heaviest action of the applied side and of the shadow side.
struct Sides {
    by_hash: HashMap<[u8; 20], (Option<Action>, Option<Action>)>,
    /// A hit didn't read, or there were more than [`HIT_ROWS`].
    doubt: bool,
}

/// `after`: the hit id the shadow started after ([`SHADOW_SINCE_KEY`]),
/// or `None` when none is recorded yet, and every hit in the window counts
/// (an applied-only hold from before the shadow then reads as a
/// disagreement: weakening). `applied`: the actions the applied versions
/// take. An applied hit means they matched, whatever action an override
/// gave it, so it counts as theirs (the heaviest, or the logged one when
/// it's one of them): the comparison is of the base rule's two versions.
fn sides(
    store: &Store,
    rule: PackRule,
    now: i64,
    after: Option<i64>,
    applied: &[Action],
) -> ApiResult<Sides> {
    let since = now - REPLAY_SECS;
    let mut stmt = store
        .connection()
        .prepare_cached(
            "SELECT hash, action FROM filter_hits WHERE rule = ?1 AND at >= ?2 AND id > ?3 LIMIT ?4",
        )
        .map_err(internal)?;
    let rows = stmt
        .query_map(
            lodestar_store::rusqlite::params![
                rule.id(),
                since,
                after.unwrap_or(0),
                HIT_ROWS as i64 + 1
            ],
            |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, u8>(1)?)),
        )
        .map_err(internal)?;
    let base = applied.iter().max().copied();
    let mut out = Sides {
        by_hash: HashMap::new(),
        doubt: false,
    };
    for (n, row) in rows.enumerate() {
        let (hash, code) = row.map_err(internal)?;
        let (Ok(hash), Some(action)) = (
            <[u8; 20]>::try_from(hash.as_slice()),
            Action::from_code(code & !SHADOW),
        ) else {
            out.doubt = true;
            continue;
        };
        if n >= HIT_ROWS {
            out.doubt = true;
            break;
        }
        let e = out.by_hash.entry(hash).or_default();
        if code & SHADOW != 0 {
            e.1 = e.1.max(Some(action));
        } else {
            let action = if applied.contains(&action) {
                Some(action)
            } else {
                base.or(Some(action))
            };
            e.0 = e.0.max(action);
        }
    }
    Ok(out)
}

/// What an `accept_pack_versions` change does.
#[derive(Debug, Clone, Default)]
pub(super) struct Acceptance {
    pub rules: Vec<PackRule>,
    /// `RULE@version`, the client sends back.
    pub versions: Vec<String>,
    pub weakening: bool,
    pub impact: Vec<PackAcceptance>,
    /// The holds it leaves for review.
    pub review: Vec<(PackRule, [u8; 20])>,
    /// Torrents the accepted versions hold in the replay window.
    pub caught: i64,
    /// More holds exist than are read at once ([`PREVIEW_ROWS`] of each):
    /// those past it aren't marked for review.
    pub holds_truncated: bool,
}

/// Rules or versions one acceptance may name: every pack and every rule.
pub(super) const MAX_NAMED: usize = Pack::ALL.len() + PackRule::COUNT;

/// Reads an acceptance against the store as the next load reads it: in
/// the preview and again under the store lock in apply, so one previewed
/// as strengthening that weakens by then is refused
/// (`changed_since_preview`).
pub(super) fn acceptance(
    store: &Store,
    manifest: &Manifest,
    ids: &[String],
    now: i64,
) -> ApiResult<Acceptance> {
    if ids.is_empty() {
        return Err(bad(
            "no_rules",
            "Name the pack rules or packs whose pending versions to accept.",
        )
        .field("rules"));
    }
    if ids.len() > MAX_NAMED {
        return Err(bad(
            "too_many_rules",
            format!("Name at most {MAX_NAMED} packs or pack rules."),
        )
        .field("rules"));
    }
    let set = store.rule_set().map_err(internal)?;
    let versions = PackVersions::resolve(manifest, &pack_upgrade::Rows::of(&set));
    let pending: Vec<PackRule> = pending_on(&set, &versions)
        .into_iter()
        .map(|(r, _)| r)
        .collect();
    let since = read_shadow_since(store)?;
    let mut rules = Vec::new();
    for id in ids {
        if let Some(p) = Pack::from_name(id) {
            rules.extend(pending.iter().copied().filter(|r| r.pack() == p));
        } else if let Some(r) = PackRule::from_id(id) {
            if !pending.contains(&r) {
                return Err(conflict(
                    "not_pending",
                    format!("{id} has no version waiting to be accepted."),
                ));
            }
            rules.push(r);
        } else {
            return Err(bad(
                "unknown_pack",
                format!("There's no pack or pack rule {id:?}."),
            )
            .field("rules"));
        }
    }
    rules.sort_by_key(|r| r.index());
    rules.dedup();
    if rules.is_empty() {
        return Err(conflict(
            "not_pending",
            "Nothing you named has a version waiting to be accepted.",
        ));
    }
    let quarantine = store.quarantined(PREVIEW_ROWS).map_err(internal)?;
    let tombstones = store.tombstones(PREVIEW_ROWS).map_err(internal)?;
    let mut a = Acceptance {
        holds_truncated: quarantine.len() >= PREVIEW_ROWS || tombstones.len() >= PREVIEW_ROWS,
        ..Acceptance::default()
    };
    for r in &rules {
        let r = *r;
        let st = versions.state(r);
        let version = st
            .shadow
            .ok_or_else(|| internal("pending without a version"))?;
        let entry = manifest.entry(r);
        let applied: Vec<Action> = st
            .apply
            .iter()
            .filter_map(|v| entry.get(*v))
            .map(|v| {
                if st.unaccepted.contains(&v.version) {
                    v.action.min(Action::Quarantine)
                } else {
                    v.action
                }
            })
            .collect();
        let after = since.get(&format!("{}@{version}", r.id())).copied();
        let sides = sides(store, r, now, after, &applied)?;
        let shadows: Vec<PackShadow> = sides
            .by_hash
            .values()
            .map(|(applied, shadow)| PackShadow {
                rule: r,
                version,
                applied: *applied,
                shadow: *shadow,
            })
            .collect();
        let in_doubt = versions.in_doubt(r);
        let strengthens =
            !sides.doubt && pack_upgrade::acceptance_strengthens(&versions, r, &shadows);
        let disagreements = st.direction.filter(|d| d.strengthens()).map_or(0, |d| {
            shadows
                .iter()
                .filter(|s| pack_upgrade::disagrees(d, s))
                .count()
        });
        let count = |f: &dyn Fn(&PackShadow) -> bool| shadows.iter().filter(|s| f(s)).count();
        let holds_new = |h: &[u8; 20]| sides.by_hash.get(h).is_some_and(|(_, s)| s.is_some());
        // A strengthening acceptance holds what the version before did, by
        // its record and its shadow results; otherwise every hold of the
        // rule the shadow results don't show the new version making.
        let (mut rq, mut rt) = (0, 0);
        if !strengthens {
            for q in quarantine
                .iter()
                .filter(|q| q.rule == r.id() && !holds_new(&q.hash))
            {
                rq += 1;
                a.review.push((r, q.hash));
            }
            for t in tombstones
                .iter()
                .filter(|t| t.rule == r.id() && !holds_new(&t.hash))
            {
                rt += 1;
                a.review.push((r, t.hash));
            }
        }
        a.caught += count(&|s| s.shadow.is_some()) as i64;
        a.weakening |= !strengthens;
        a.versions.push(format!("{}@{version}", r.id()));
        a.impact.push(PackAcceptance {
            applied: st.applied.filter(|v| *v != pack_upgrade::UNKNOWN),
            direction: st.direction.map(|d| d.as_str().to_owned()),
            disagreements,
            heavier: count(&|s| s.heavier()),
            in_doubt,
            lighter: count(&|s| s.lighter()),
            newly_held: count(&|s| s.applied.is_none() && s.shadow.is_some()),
            no_longer_held: count(&|s| s.applied.is_some() && s.shadow.is_none()),
            review_quarantine: rq,
            review_tombstones: rt,
            rule: r.id().to_owned(),
            strengthens,
            version,
        });
    }
    a.rules = rules;
    Ok(a)
}

/// The effect warning of a weakening acceptance (§20.11).
pub(super) fn warning(a: &Acceptance) -> String {
    let weak: Vec<String> = a
        .impact
        .iter()
        .filter(|i| !i.strengthens)
        .map(|i| {
            let why = match i.direction.as_deref() {
                _ if i.in_doubt => {
                    "the version this install applied isn't known for sure".to_owned()
                }
                Some("narrows") => "it matches less than the version before".to_owned(),
                Some("action-lighter") => "it takes a lighter action".to_owned(),
                Some("mixed") => "it changes what it matches and how it acts".to_owned(),
                None => "there's no record of what changed".to_owned(),
                Some(_) if i.disagreements > 0 => format!(
                    "{} torrents judged while it ran in shadow don't fit its record",
                    i.disagreements
                ),
                Some(_) => "its shadow results couldn't all be read".to_owned(),
            };
            format!("{}@{} ({why})", i.rule, i.version)
        })
        .collect();
    let review: usize = a
        .impact
        .iter()
        .map(|i| i.review_quarantine + i.review_tombstones)
        .sum();
    let more = if a.holds_truncated {
        format!(
            " There are more holds than can be read at once ({PREVIEW_ROWS} of each kind): \
             those past them stay held but aren't marked."
        )
    } else {
        String::new()
    };
    format!(
        "Accepting {} may stop hiding or dropping malware, lures, fakes, spam or scams the \
         version you have now catches. Other rules still check them. Nothing held now is \
         released: {review} held items the new version may not hold stay held, marked for \
         review in the review queue.{more} Undo can't undo an acceptance. Safer: keep the \
         version you have, and add an exception for anything it gets wrong.",
        weak.join(", ")
    )
}

/// The review row as stored.
enum ReviewRow {
    Absent,
    Text(String),
    /// Not text: it fails closed. Nothing is marked for review from it,
    /// it never blocks a change or a page, and every hold stays held
    /// (the row only ever marks holds, never releases them). The next
    /// acceptance that leaves holds for review replaces it.
    Unreadable,
}

fn review_row(store: &Store) -> ApiResult<ReviewRow> {
    match store.setting_text(REVIEW_KEY) {
        Ok(None) => Ok(ReviewRow::Absent),
        Ok(Some(t)) => Ok(ReviewRow::Text(t)),
        Err(lodestar_store::Error::Corrupt(_)) => Ok(ReviewRow::Unreadable),
        Err(e) => Err(internal(e)),
    }
}

/// The `pack_conditions` label of a review row that doesn't read.
pub(super) const REVIEW_UNREADABLE: &str = "packs.review-unreadable";

/// Whether the review row exists but doesn't read, or has lines that
/// don't (or more than [`REVIEW_MAX`]): what it can't mark is reported,
/// never dropped silently.
pub(super) fn review_unreadable(store: &Store) -> ApiResult<bool> {
    Ok(match review_row(store)? {
        ReviewRow::Absent => false,
        ReviewRow::Unreadable => true,
        ReviewRow::Text(t) => {
            let lines = t.lines().count();
            lines > REVIEW_MAX || parse_review(&t).len() < lines
        }
    })
}

/// The holds acceptances left for review, as stored: at most
/// [`REVIEW_MAX`] of them, whatever the row holds; none when it doesn't
/// read.
pub(super) fn review_set(store: &Store) -> ApiResult<HashSet<(String, [u8; 20])>> {
    Ok(match review_row(store)? {
        ReviewRow::Text(t) => parse_review(&t),
        ReviewRow::Absent | ReviewRow::Unreadable => HashSet::new(),
    })
}

fn parse_review(text: &str) -> HashSet<(String, [u8; 20])> {
    text.lines()
        .take(REVIEW_MAX)
        .filter_map(|l| {
            let (rule, h) = l.split_once(' ')?;
            let h = super::hex20(h).ok()?;
            Some((rule.to_owned(), h))
        })
        .collect()
}

/// The review row's value before an [`add_review`], to put back if the
/// acceptance it was written for fails.
pub(super) struct ReviewBefore(Option<Option<String>>);

/// Puts the review row back as it was before [`add_review`]. A row that
/// didn't read is left as rewritten: it marked nothing.
pub(super) fn restore_review(store: &mut Store, before: ReviewBefore) -> ApiResult<()> {
    match before.0 {
        None => Ok(()),
        Some(None) => store.update_settings_text(&[], &[REVIEW_KEY]),
        Some(Some(t)) => store.set_settings_text(&[(REVIEW_KEY, &t)]),
    }
    .map_err(internal)
}

/// Adds holds to the review row, newest first, at most [`REVIEW_MAX`].
/// Lines that don't read are dropped, and a row that doesn't read is
/// replaced. Written before the acceptance it's for, so the acceptance
/// never lands without its marks; returns what to restore if the
/// acceptance then fails.
pub(super) fn add_review(
    store: &mut Store,
    new: &[(PackRule, [u8; 20])],
) -> ApiResult<ReviewBefore> {
    if new.is_empty() {
        return Ok(ReviewBefore(None));
    }
    let (old, before) = match review_row(store)? {
        ReviewRow::Text(t) => (Some(t.clone()), Some(Some(t))),
        ReviewRow::Absent => (None, Some(None)),
        ReviewRow::Unreadable => (None, None),
    };
    let mut seen = HashSet::new();
    let mut lines = Vec::new();
    let new = new.iter().map(|(r, h)| (r.id().to_owned(), *h));
    let old = old.as_deref().unwrap_or_default().lines().filter_map(|l| {
        let (rule, h) = l.split_once(' ')?;
        Some((rule.to_owned(), super::hex20(h).ok()?))
    });
    for (rule, h) in new.chain(old) {
        if lines.len() == REVIEW_MAX {
            break;
        }
        if seen.insert((rule.clone(), h)) {
            lines.push(format!("{rule} {}", hex(&h)));
        }
    }
    store
        .set_settings_text(&[(REVIEW_KEY, &lines.join("\n"))])
        .map_err(internal)?;
    Ok(ReviewBefore(before))
}

/// Drops review lines whose hold is gone (released, expired or purged), so
/// the row never keeps the hash of a torrent nothing holds. Skipped when
/// the holds can't all be read at once, and left alone when the row
/// doesn't read (it marks nothing then).
pub(super) fn prune_review(store: &mut Store) -> ApiResult<()> {
    let ReviewRow::Text(text) = review_row(store)? else {
        return Ok(());
    };
    let quarantine = store.quarantined(PREVIEW_ROWS).map_err(internal)?;
    let tombstones = store.tombstones(PREVIEW_ROWS).map_err(internal)?;
    if quarantine.len() >= PREVIEW_ROWS || tombstones.len() >= PREVIEW_ROWS {
        return Ok(());
    }
    let held: HashSet<(&str, &[u8; 20])> = quarantine
        .iter()
        .map(|q| (q.rule.as_str(), &q.hash))
        .chain(tombstones.iter().map(|t| (t.rule.as_str(), &t.hash)))
        .collect();
    let kept: Vec<&str> = text
        .lines()
        .filter(|l| {
            l.split_once(' ')
                .is_some_and(|(rule, h)| super::hex20(h).is_ok_and(|h| held.contains(&(rule, &h))))
        })
        .collect();
    if kept.len() == text.lines().count() {
        return Ok(());
    }
    if kept.is_empty() {
        store.update_settings_text(&[], &[REVIEW_KEY])
    } else {
        store.set_settings_text(&[(REVIEW_KEY, &kept.join("\n"))])
    }
    .map_err(internal)
}

/// The pack corpora run against `manifest` (RP-1's report). The built-in
/// manifest's report is fixed for the build, so it's run once.
pub(super) fn corpus(manifest: &Arc<Manifest>) -> PackCorpus {
    static BUILTIN: OnceLock<PackCorpus> = OnceLock::new();
    if Arc::ptr_eq(manifest, &Manifest::builtin()) {
        return BUILTIN.get_or_init(|| run_corpus(manifest)).clone();
    }
    run_corpus(manifest)
}

fn run_corpus(manifest: &Arc<Manifest>) -> PackCorpus {
    let corpus = match Corpus::builtin() {
        Ok(c) => c,
        Err(e) => {
            return PackCorpus {
                error: Some(e.to_string()),
                failures: Vec::new(),
                passes: false,
                precision_failing: 0,
                precision_items: 0,
                rules: Vec::new(),
            };
        }
    };
    let report = corpus.run(manifest);
    let gaps = report.gaps();
    let failures = report.failures();
    PackCorpus {
        error: None,
        passes: failures.is_empty(),
        failures,
        precision_failing: report
            .precision
            .iter()
            .filter(|p| {
                p.held.iter().any(|(r, _)| !p.gaps.contains(r))
                    || p.gaps.iter().any(|g| !p.held.iter().any(|(r, _)| r == g))
            })
            .count(),
        precision_items: report.precision.len(),
        rules: report
            .packs
            .iter()
            .flat_map(|p| &p.rules)
            .map(|r| PackFixtureCounts {
                direction_disagreements: report
                    .directions
                    .iter()
                    .filter(|d| d.rule == r.rule && !d.agrees())
                    .count(),
                failing: r.disagreements(),
                gaps: gaps.iter().filter(|(g, _)| *g == r.rule).count(),
                must_match: r.count(Expect::Match),
                must_not_match: r.count(Expect::Miss),
                pack: r.rule.pack().as_str().to_owned(),
                rule: r.rule.id().to_owned(),
                version: r.version,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_review_row_is_capped_newest_first() {
        let mut store = Store::open_in_memory(Default::default()).unwrap();
        let hash = |n: usize| {
            let mut h = [0u8; 20];
            h[..8].copy_from_slice(&(n as u64).to_be_bytes());
            h
        };
        let old: Vec<(PackRule, [u8; 20])> = (0..10).map(|n| (PackRule::Mw2, hash(n))).collect();
        add_review(&mut store, &old).map(|_| ()).unwrap();
        let new: Vec<(PackRule, [u8; 20])> = (100..100 + REVIEW_MAX)
            .map(|n| (PackRule::Mw1, hash(n)))
            .collect();
        add_review(&mut store, &new).map(|_| ()).unwrap();
        let set = review_set(&store).unwrap();
        assert_eq!(set.len(), REVIEW_MAX);
        assert!(set.contains(&("MW-1".to_owned(), hash(100))));
        assert!(
            !set.contains(&("MW-2".to_owned(), hash(0))),
            "the oldest go first"
        );
        // A longer row, however it was written, reads at most the cap.
        let text = (0..REVIEW_MAX + 5)
            .map(|n| format!("MW-3 {}", hex(&hash(n))))
            .collect::<Vec<_>>()
            .join("\n");
        store.set_settings_text(&[(REVIEW_KEY, &text)]).unwrap();
        assert_eq!(review_set(&store).unwrap().len(), REVIEW_MAX);
    }
}
