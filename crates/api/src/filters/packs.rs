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
//! is that torrent's [`PackShadow`]. Hits from before the shadow ran (the
//! earliest shadow hit of the rule, or this process's start, whichever is
//! earlier) are left out: the torrent wasn't judged by both sides then. A
//! hit whose action doesn't read, or more hits than are read, leaves the
//! shadow results in doubt, and accepting is then weakening.
//!
//! **Nothing is released.** Holds of an accepted rule (quarantine and
//! tombstones) that the shadow results don't show the new version making
//! stay as they are, and are listed for review ([`REVIEW_KEY`]).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};

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
/// Lines the review row keeps at most, newest first.
const REVIEW_MAX: usize = 10_000;
/// Hits of one rule read for its shadow results at most.
const HIT_ROWS: usize = 1_000_000;

/// When this process started judging with its build, as near as the API
/// can tell: forced at the filters API's start. A pending version's shadow
/// runs from then at the latest.
static STARTED: LazyLock<i64> = LazyLock::new(crate::unix_now);

/// Marks the start ([`STARTED`]).
pub(super) fn note_start() {
    LazyLock::force(&STARTED);
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

/// Every version waiting to be accepted, with its changelog entries.
pub(super) fn pending(manifest: &Manifest, versions: &PackVersions) -> Vec<PendingPackVersion> {
    versions
        .pending()
        .filter_map(|(r, st)| {
            let version = st.shadow?;
            // A version recorded at 0 isn't known: every entry up to the
            // pending one.
            let from = st.applied.filter(|v| *v != pack_upgrade::UNKNOWN);
            Some(PendingPackVersion {
                applied: from,
                changelog: changelog(manifest, r, from, version),
                direction: st.direction.map(|d| d.as_str().to_owned()),
                pack: r.pack().as_str().to_owned(),
                rule: r.id().to_owned(),
                version,
            })
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

fn sides(store: &Store, rule: PackRule, now: i64) -> ApiResult<Sides> {
    let since = now - REPLAY_SECS;
    let c = store.connection();
    let first_shadow: Option<i64> = c
        .query_row(
            "SELECT min(at) FROM filter_hits WHERE rule = ?1 AND at >= ?2 AND (action & ?3) != 0",
            lodestar_store::rusqlite::params![rule.id(), since, SHADOW],
            |r| r.get(0),
        )
        .map_err(internal)?;
    let from = first_shadow
        .map_or(*STARTED, |t| t.min(*STARTED))
        .max(since);
    let mut stmt = c
        .prepare_cached(
            "SELECT hash, action FROM filter_hits WHERE rule = ?1 AND at >= ?2 LIMIT ?3",
        )
        .map_err(internal)?;
    let rows = stmt
        .query_map(
            lodestar_store::rusqlite::params![rule.id(), from, HIT_ROWS as i64 + 1],
            |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, u8>(1)?)),
        )
        .map_err(internal)?;
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
        let side = if code & SHADOW != 0 {
            &mut e.1
        } else {
            &mut e.0
        };
        *side = (*side).max(Some(action));
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
}

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
    let set = store.rule_set().map_err(internal)?;
    let versions = PackVersions::resolve(manifest, &pack_upgrade::Rows::of(&set));
    let mut rules = Vec::new();
    for id in ids {
        if let Some(p) = Pack::from_name(id) {
            rules.extend(
                PackRule::ALL
                    .into_iter()
                    .filter(|r| r.pack() == p && versions.state(*r).pending()),
            );
        } else if let Some(r) = PackRule::from_id(id) {
            if !versions.state(r).pending() {
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
    let mut a = Acceptance::default();
    for r in &rules {
        let r = *r;
        let st = versions.state(r);
        let version = st
            .shadow
            .ok_or_else(|| internal("pending without a version"))?;
        let sides = sides(store, r, now)?;
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
                    "{} torrents in the last 30 days don't fit its record",
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
    format!(
        "Accepting {} may stop hiding or dropping malware, lures, fakes, spam or scams the \
         version you have now catches. Other rules still check them. Nothing held now is \
         released: {review} held items the new version may not hold stay held, marked for \
         review in the review queue. Undo can't undo an acceptance. Safer: keep the version \
         you have, and add an exception for anything it gets wrong.",
        weak.join(", ")
    )
}

/// The holds acceptances left for review, as stored.
pub(super) fn review_set(store: &Store) -> ApiResult<HashSet<(String, [u8; 20])>> {
    let text = store.setting_text(REVIEW_KEY).map_err(internal)?;
    Ok(text.as_deref().map(parse_review).unwrap_or_default())
}

fn parse_review(text: &str) -> HashSet<(String, [u8; 20])> {
    text.lines()
        .filter_map(|l| {
            let (rule, h) = l.split_once(' ')?;
            let h = super::hex20(h).ok()?;
            Some((rule.to_owned(), h))
        })
        .collect()
}

/// Adds holds to the review row, newest first, at most [`REVIEW_MAX`].
/// Lines that don't read are dropped.
pub(super) fn add_review(store: &mut Store, new: &[(PackRule, [u8; 20])]) -> ApiResult<()> {
    if new.is_empty() {
        return Ok(());
    }
    let old = store.setting_text(REVIEW_KEY).map_err(internal)?;
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
        .map_err(internal)
}

/// Drops review lines whose hold is gone (released, expired or purged), so
/// the row never keeps the hash of a torrent nothing holds. Skipped when
/// the holds can't all be read at once.
pub(super) fn prune_review(store: &mut Store) -> ApiResult<()> {
    let Some(text) = store.setting_text(REVIEW_KEY).map_err(internal)? else {
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

/// Forgets the shadow hits of accepted rules: they describe a version that
/// applies now, and would be read as a later version's otherwise.
pub(super) fn forget_shadow_hits(store: &Store, rules: &[PackRule]) -> ApiResult<()> {
    let c = store.connection();
    for r in rules {
        c.execute(
            "DELETE FROM filter_hits WHERE rule = ?1 AND (action & ?2) != 0",
            lodestar_store::rusqlite::params![r.id(), SHADOW],
        )
        .map_err(internal)?;
    }
    Ok(())
}

/// The pack corpora run against `manifest` (RP-1's report).
pub(super) fn corpus(manifest: &Arc<Manifest>) -> PackCorpus {
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
