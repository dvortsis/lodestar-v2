//! The filters API (architecture §20.7, §20.10–§20.12): the rule set,
//! the review queue, and changes to filtering.
//!
//! Every change goes through one protocol:
//! - `POST /filters/preview` returns what the change would do: an impact
//!   preview and, for a change that weakens protection, a plain-language
//!   effect warning. It never applies anything and never asks for the
//!   password.
//! - `POST /filters/apply` applies it. A weakening change is refused with
//!   428 and the same warning until it's sent again with the admin
//!   password, every time (§20.11). Strengthening changes need neither.
//!
//! No change is recorded anywhere: no author, reason or history. The one
//! previous rule set is kept for Undo (§20.5.6). The guardrails (the
//! locked floor, protective core actions, valid overrides and exceptions)
//! hold on this server whatever a client sends.

use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use lodestar_classify::ContentType;
use lodestar_filter::lists::SharedIps;
use lodestar_filter::load::{KIND_HASHES, KIND_KEYWORD, KIND_PAYLOAD, KIND_QUERY, encode_match};
use lodestar_filter::{
    Action, CoreRule, Engine, Exception, Handle, HeldSet, Item, Match, Origin, Override, Pack,
    PackRule, RuleRef, Sealer, Strength,
};
use lodestar_search::{Node, parse};
use lodestar_store::{ExceptionRow, OverrideRow, RuleRow, RuleSet, Store};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{ApiError, ApiResult, ApiState, ClientIp, auth_error, internal, same_origin_json};

/// What the filters API works with. The store is the writable control
/// connection, not the ingest writer's.
#[derive(Debug)]
pub struct FiltersApi {
    pub store: Arc<Mutex<Store>>,
    pub engine: Arc<Handle>,
    pub held: Arc<HeldSet>,
    pub sealer: Option<Arc<Sealer>>,
    pub ips: Arc<SharedIps>,
}

/// The replay window (§20.10).
const REPLAY_SECS: i64 = 30 * 86_400;
/// Rows read for a preview.
const PREVIEW_ROWS: usize = 100_000;

fn bad(message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, message)
}

fn hex20(s: &str) -> ApiResult<[u8; 20]> {
    let s = s.trim();
    if s.len() != 40 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(bad(format!("{s:?} isn't a 40-digit info hash")));
    }
    let mut out = [0u8; 20];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex");
    }
    Ok(out)
}

fn hex(h: &[u8]) -> String {
    h.iter().map(|b| format!("{b:02x}")).collect()
}

fn action(name: &str) -> ApiResult<Action> {
    [
        Action::Flag,
        Action::Demote,
        Action::Hide,
        Action::Quarantine,
        Action::Drop,
    ]
    .into_iter()
    .find(|a| a.as_str() == name)
    .ok_or_else(|| bad("action: flag, demote, hide, quarantine or drop"))
}

fn query(text: &str) -> ApiResult<Node> {
    parse(text).map_err(|e| bad(format!("query: {}", e.message)))
}

fn days(d: Option<u32>, now: i64) -> Option<i64> {
    d.map(|d| now + i64::from(d.min(3650)) * 86_400)
}

/// What an override, exception or allow entry matches, as sent.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MatchInput {
    pub hashes: Option<Vec<String>>,
    pub keyword: Option<String>,
    pub query: Option<String>,
    /// A payload signature, 16 hex digits.
    pub payload: Option<String>,
}

impl MatchInput {
    fn to_match(&self) -> ApiResult<Match> {
        let given = [
            self.hashes.is_some(),
            self.keyword.is_some(),
            self.query.is_some(),
            self.payload.is_some(),
        ];
        if given.iter().filter(|g| **g).count() != 1 {
            return Err(bad("give exactly one of hashes, keyword, query or payload"));
        }
        if let Some(hs) = &self.hashes {
            if hs.is_empty() {
                return Err(bad("no hashes"));
            }
            return Ok(Match::Hashes(
                hs.iter().map(|h| hex20(h)).collect::<ApiResult<_>>()?,
            ));
        }
        if let Some(k) = &self.keyword {
            return Ok(Match::Keyword(k.clone()));
        }
        if let Some(q) = &self.query {
            return Ok(Match::Query(query(q)?));
        }
        let p = self.payload.as_deref().unwrap_or_default();
        u64::from_str_radix(p, 16)
            .ok()
            .filter(|_| p.len() == 16)
            .map(Match::Payload)
            .ok_or_else(|| bad("payload: 16 hex digits"))
    }
}

/// One change to filtering (§20.7).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Change {
    CoreSwitch {
        rule: String,
        on: bool,
    },
    /// A pack (`malware`) or one pack rule (`MW-2`).
    PackSwitch {
        id: String,
        on: bool,
    },
    AddRule {
        query: String,
        action: String,
        unless: Option<String>,
        shadow_days: Option<u32>,
    },
    UpdateRule {
        id: i64,
        query: Option<String>,
        action: Option<String>,
        enabled: Option<bool>,
        /// An empty string removes the clause.
        unless: Option<String>,
        shadow_days: Option<u32>,
    },
    RemoveRule {
        id: i64,
    },
    AddOverride {
        #[serde(flatten)]
        what: MatchInput,
        #[serde(default)]
        neutralize: bool,
        scope: Option<String>,
        expires_days: Option<u32>,
        shadow_days: Option<u32>,
    },
    RemoveOverride {
        id: i64,
    },
    AddException {
        rule: String,
        #[serde(flatten)]
        what: MatchInput,
    },
    RemoveException {
        id: i64,
    },
    AddAllow {
        query: String,
    },
    RemoveAllow {
        id: i64,
    },
    /// Report torrents as child-abuse material (§20.5.2).
    ReportAbuse {
        hashes: Vec<String>,
    },
    RemoveCs1 {
        hashes: Vec<String>,
    },
    /// Releases a quarantined or dropped torrent: fetched and processed
    /// normally next time it's seen.
    Release {
        hash: String,
    },
    /// Confirms a quarantined torrent as abuse material: into CS-1, sealed
    /// name deleted.
    Confirm {
        hash: String,
    },
    RevealName {
        hash: String,
    },
    Subscribe {
        url: String,
        name: Option<String>,
        /// `content` or `ip`.
        kind: String,
        format: Option<String>,
        action: Option<String>,
        #[serde(default)]
        floor: bool,
        refresh_hours: Option<u32>,
    },
    Unsubscribe {
        id: i64,
    },
    PauseSubscription {
        id: i64,
        enabled: bool,
    },
    /// Accepts or discards a version the sanity gates held back.
    ResolveHeld {
        id: i64,
        accept: bool,
    },
    Undo,
}

/// What a change would do (§20.10). Counts come from the review queue,
/// the tombstones, the filter hits of the replay window and the index.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Preview {
    /// Quarantined items it would release now.
    pub quarantine: usize,
    /// Dropped items it would let be fetched again.
    pub tombstones: usize,
    /// Stored items whose hide, demote or flag mark it would clear.
    pub stored: u64,
    /// Decisions in the replay window by the rules it affects.
    pub caught_30d: i64,
    /// Of those, by the child-safety core.
    pub core_caught_30d: i64,
    pub per_day: f64,
    /// Counts from names only (sealed names, no file lists), so files'
    /// evidence isn't re-checked: treat as an estimate.
    pub estimate: bool,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Assessment {
    pub weakening: bool,
    /// The password is needed (every weakening change, and marking a list
    /// as part of the locked floor).
    pub confirm: bool,
    pub warning: Option<String>,
    pub preview: Preview,
    /// For a keyword override: the same entry as *Neutralize term*.
    pub neutralize: Option<Preview>,
}

/// The current state, read once per preview.
struct Snapshot {
    quarantine: Vec<lodestar_store::Quarantined>,
    tombstones: Vec<([u8; 20], String, i64)>,
    hits: Vec<(String, u8, i64)>,
}

impl Snapshot {
    fn read(store: &Store, now: i64) -> lodestar_store::Result<Snapshot> {
        Ok(Snapshot {
            quarantine: store.quarantined(PREVIEW_ROWS)?,
            tombstones: store.tombstones(PREVIEW_ROWS)?.into_iter().collect(),
            hits: store.hit_counts(now - REPLAY_SECS)?,
        })
    }

    /// The preview for changes scoped to whole rules.
    fn for_rules(&self, rules: &[String]) -> Preview {
        let hit = |r: &str| rules.iter().any(|x| x == r);
        let core = |r: &str| CoreRule::from_id(r).is_some();
        let mark = |a: u8| {
            [Action::Hide, Action::Demote, Action::Flag]
                .iter()
                .any(|m| m.code() == a)
        };
        let live: Vec<&(String, u8, i64)> = self
            .hits
            .iter()
            .filter(|(r, a, _)| hit(r) && a & lodestar_store::SHADOW == 0)
            .collect();
        let caught: i64 = live.iter().map(|(_, _, n)| n).sum();
        Preview {
            quarantine: self.quarantine.iter().filter(|q| hit(&q.rule)).count(),
            tombstones: self.tombstones.iter().filter(|t| hit(&t.1)).count(),
            stored: live
                .iter()
                .filter(|(_, a, _)| mark(*a))
                .map(|(_, _, n)| *n as u64)
                .sum(),
            caught_30d: caught,
            core_caught_30d: live
                .iter()
                .filter(|(r, _, _)| core(r))
                .map(|(_, _, n)| n)
                .sum(),
            per_day: caught as f64 / 30.0,
            estimate: false,
        }
    }

    /// Items the `before` engine holds and `after` doesn't, judged from
    /// their sealed names; exact for hash matches.
    fn rejudged(
        &self,
        before: &Engine,
        after: &Engine,
        sealer: Option<&Sealer>,
        now: i64,
    ) -> Preview {
        let mut p = Preview {
            estimate: true,
            ..Preview::default()
        };
        for q in &self.quarantine {
            let name = sealer
                .zip(q.sealed.as_deref())
                .and_then(|(s, sealed)| s.open(sealed).ok())
                .unwrap_or_default();
            let hashes = [q.hash];
            let item = Item {
                hashes: &hashes,
                name: &name,
                files: &[],
                content_type: ContentType::Other,
            };
            let (b, a) = (
                before.judge(&item, None, now).decision,
                after.judge(&item, None, now).decision,
            );
            let released = match (b, a) {
                (Some(_), None) => true,
                // Held by evidence a name can't show (files): released if
                // the new rules let its exact hash through.
                (None, None) => {
                    let blank = Item { name: "", ..item };
                    after.judge(&blank, None, now).decision.is_none()
                        && released_by_hash(after, &q.hash, &q.rule)
                }
                _ => false,
            };
            if released {
                p.quarantine += 1;
            }
        }
        p.tombstones = self
            .tombstones
            .iter()
            .filter(|(h, rule, _)| released_by_hash(after, h, rule))
            .count();
        p
    }
}

/// Whether a hash-level entry (override or exception) in `engine` covers
/// this hash for this rule.
fn released_by_hash(engine: &Engine, hash: &[u8; 20], rule: &str) -> bool {
    let covers = |m: &Match| matches!(m, Match::Hashes(hs) if hs.contains(hash));
    let floor = CoreRule::from_id(rule).is_some_and(|r| r.floor());
    if floor {
        return false;
    }
    engine
        .overrides
        .iter()
        .any(|o| o.strength == Strength::Override && covers(&o.what))
        || engine
            .exceptions
            .iter()
            .any(|e| e.rule.id() == rule && covers(&e.what))
}

fn pack_rules(id: &str) -> ApiResult<Vec<String>> {
    if let Some(p) = Pack::from_name(id) {
        return Ok(PackRule::ALL
            .into_iter()
            .filter(|r| r.pack() == p)
            .map(|r| r.id().to_owned())
            .collect());
    }
    PackRule::from_id(id)
        .map(|r| vec![r.id().to_owned()])
        .ok_or_else(|| bad(format!("no pack or pack rule {id:?}")))
}

fn override_entry(
    what: &MatchInput,
    neutralize: bool,
    scope: Option<&str>,
    expires_days: Option<u32>,
    shadow_days: Option<u32>,
    now: i64,
) -> ApiResult<Override> {
    let o = Override {
        id: 0,
        what: what.to_match()?,
        strength: if neutralize {
            Strength::Neutralize
        } else {
            Strength::Override
        },
        scope: scope.map(query).transpose()?,
        expires_at: days(expires_days, now),
        shadow_until: days(shadow_days, now),
    };
    o.validate().map_err(|e| bad(e.to_string()))?;
    Ok(o)
}

fn exception_entry(rule: &str, what: &MatchInput) -> ApiResult<Exception> {
    let rule = RuleRef::from_id(rule).ok_or_else(|| bad(format!("no rule {rule:?}")))?;
    let e = Exception {
        id: 0,
        rule,
        what: what.to_match()?,
    };
    e.validate().map_err(|e| bad(e.to_string()))?;
    Ok(e)
}

fn count_line(p: &Preview) -> String {
    let core = if p.core_caught_30d > 0 {
        format!(" ({} caught by the child-safety core)", p.core_caught_30d)
    } else {
        String::new()
    };
    let mut s = format!(
        "In the last 30 days the rules it affects decided {} items{core}, about {:.1} per day.",
        p.caught_30d, p.per_day
    );
    if p.quarantine + p.tombstones > 0 {
        s.push_str(&format!(
            " {} items in quarantine would be released and {} dropped items could be fetched again.",
            p.quarantine, p.tombstones
        ));
    }
    if p.stored > 0 {
        s.push_str(&format!(
            " Up to {} stored items would lose their hide, demote or flag mark.",
            p.stored
        ));
    }
    if p.estimate {
        s.push_str(" Counts come from names only, so treat them as estimates.");
    }
    s
}

/// The effect warning (§20.11): what it does, what else it lets
/// through, the numbers, and a safer alternative.
fn warning(change: &Change, p: &Preview, neutral: Option<&Preview>) -> String {
    let n = count_line(p);
    match change {
        Change::CoreSwitch { rule, .. } => format!(
            "Turning off {rule} stops that child-safety check for every torrent. Other core \
             rules keep checking. {n} Safer: add an exception for the exact torrents it got \
             wrong, which keeps {rule} on for everything else."
        ),
        Change::PackSwitch { id, .. } => format!(
            "Switching off {id} stops it hiding or dropping malware, lures, fakes, spam or scams \
             it would have caught. {n} Safer: an exception for the torrents it got wrong."
        ),
        Change::UpdateRule { id, .. } | Change::RemoveRule { id } => format!(
            "This changes or removes your rule U-{id}, so it may stop catching what it caught. \
             {n} Safer: shadow the new version for a few days first."
        ),
        Change::AddOverride {
            what, neutralize, ..
        } => {
            let thing = what
                .keyword
                .as_ref()
                .map(|k| format!("\"{k}\""))
                .unwrap_or_else(|| "these torrents".into());
            if *neutralize {
                format!(
                    "Neutralizing {thing} stops the word from counting as evidence in every \
                     core rule that isn't locked and in the packs. Other evidence still counts. {n}"
                )
            } else {
                let safer = neutral.map_or_else(String::new, |q| {
                    format!(
                        " Safer: \"Neutralize term\" stops the word counting on its own while \
                         other signals still count; it would release {} items now.",
                        q.quarantine
                    )
                });
                format!(
                    "Overriding {thing} lets matching torrents skip every core rule that isn't \
                     locked, including the ones that detect explicit under-18 ages and code terms \
                     in file names, plus the packs and your rules. Abuse uploads can use the same \
                     words, so those would get through too. {n}{safer}"
                )
            }
        }
        Change::AddException { rule, .. } => format!(
            "This exception releases what it matches from {rule} only. Other rules still check \
             it. {n} Safer: the narrowest match, an exact torrent."
        ),
        Change::AddAllow { .. } => format!(
            "Everything this matches will skip your rules and the rule packs (malware, lures, \
             fakes, spam, scams). The child-safety core still applies. {n} Safer: narrow it, \
             for example to one media type."
        ),
        Change::RemoveCs1 { .. } => format!(
            "You confirmed these torrents as abuse material. Removing them lets them, and exact \
             reposts of their files, be fetched and indexed again. {n}"
        ),
        Change::Release { .. } => {
            "Releasing this torrent fetches and indexes it normally the next time it's seen. \
             Every rule checks it again then."
                .to_owned()
        }
        Change::RevealName { .. } => {
            "This shows the name of a torrent a filter held back. Nothing about it is stored."
                .to_owned()
        }
        Change::Unsubscribe { .. } | Change::PauseSubscription { .. } => format!(
            "This stops applying the list: what it blocked can be fetched or contacted again. {n}"
        ),
        Change::ResolveHeld { .. } => format!(
            "This applies a version the sanity gates held back because it shrank sharply: \
             entries it no longer has stop being blocked. {n}"
        ),
        Change::Subscribe { .. } => {
            "Marking this list as part of the locked floor blocks its torrents beyond any \
             override, and you can't release them while they're on it. Only do this for a \
             vetted child-abuse hash list."
                .to_owned()
        }
        Change::Undo => {
            "Undo puts back the previous rule set: changes made since are reverted, including \
             any that strengthened filtering."
                .to_owned()
        }
        _ => n,
    }
}

impl FiltersApi {
    fn store(&self) -> std::sync::MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// What `change` would do, without doing it.
    pub fn assess(&self, change: &Change, now: i64) -> ApiResult<Assessment> {
        let store = self.store();
        let snap = Snapshot::read(&store, now).map_err(internal)?;
        let engine = self.engine.current();
        let sealer = self.sealer.as_deref();
        let mut a = Assessment::default();
        let rules = |ids: Vec<String>| snap.for_rules(&ids);
        match change {
            Change::CoreSwitch { rule, on } => {
                let r = CoreRule::from_id(rule).ok_or_else(|| bad(format!("no rule {rule:?}")))?;
                if r.floor() && !on {
                    return Err(ApiError::new(
                        StatusCode::CONFLICT,
                        format!("{rule} is part of the locked floor and can't be switched off"),
                    ));
                }
                if !on && engine.core.switches.is_on(r) {
                    a.weakening = true;
                    a.preview = rules(vec![rule.clone()]);
                }
            }
            Change::PackSwitch { id, on } => {
                let ids = pack_rules(id)?;
                if !on {
                    a.weakening = true;
                    a.preview = rules(ids);
                }
            }
            Change::AddRule {
                query: q,
                action: act,
                unless,
                ..
            } => {
                query(q)?;
                action(act)?;
                unless.as_deref().map(query).transpose()?;
            }
            Change::UpdateRule {
                id,
                enabled,
                query: q,
                action: act,
                unless,
                shadow_days,
            } => {
                q.as_deref().map(query).transpose()?;
                act.as_deref().map(action).transpose()?;
                unless
                    .as_deref()
                    .filter(|u| !u.is_empty())
                    .map(query)
                    .transpose()?;
                let only_enabling = *enabled == Some(true)
                    && q.is_none()
                    && act.is_none()
                    && unless.is_none()
                    && shadow_days.is_none();
                if !only_enabling {
                    a.weakening = true;
                    a.preview = rules(vec![format!("U-{id}")]);
                }
            }
            Change::RemoveRule { id } => {
                a.weakening = true;
                a.preview = rules(vec![format!("U-{id}")]);
            }
            Change::AddOverride {
                what,
                neutralize,
                scope,
                expires_days,
                shadow_days,
            } => {
                let o = override_entry(
                    what,
                    *neutralize,
                    scope.as_deref(),
                    *expires_days,
                    *shadow_days,
                    now,
                )?;
                a.weakening = true;
                let mut after = Engine::clone(&engine);
                after.overrides.push(Override {
                    id: u32::MAX,
                    ..o.clone()
                });
                a.preview = snap.rejudged(&engine, &after, sealer, now);
                // Every non-floor rule could be bypassed: the replay's
                // upper bound.
                let bypassable: Vec<String> = snap
                    .hits
                    .iter()
                    .map(|(r, _, _)| r.clone())
                    .filter(|r| !CoreRule::from_id(r).is_some_and(|c| c.floor()))
                    .collect();
                let bound = rules(bypassable);
                a.preview.caught_30d = bound.caught_30d;
                a.preview.core_caught_30d = bound.core_caught_30d;
                a.preview.per_day = bound.per_day;
                if what.keyword.is_some() && !neutralize {
                    let single = Override {
                        id: u32::MAX,
                        strength: Strength::Neutralize,
                        ..o
                    };
                    if single.validate().is_ok() {
                        let mut alt = Engine::clone(&engine);
                        alt.overrides.push(single);
                        a.neutralize = Some(snap.rejudged(&engine, &alt, sealer, now));
                    }
                }
            }
            Change::AddException { rule, what } => {
                let e = exception_entry(rule, what)?;
                a.weakening = true;
                let mut after = Engine::clone(&engine);
                after.exceptions.push(Exception { id: u32::MAX, ..e });
                a.preview = snap.rejudged(&engine, &after, sealer, now);
                let bound = rules(vec![rule.clone()]);
                a.preview.caught_30d = bound.caught_30d;
                a.preview.core_caught_30d = bound.core_caught_30d;
                a.preview.per_day = bound.per_day;
            }
            Change::AddAllow { query: q } => {
                let node = query(q)?;
                a.weakening = true;
                let mut after = Engine::clone(&engine);
                after.allow.push(lodestar_filter::Allow {
                    id: u32::MAX,
                    query: node,
                });
                a.preview = snap.rejudged(&engine, &after, sealer, now);
                let layers: Vec<String> = snap
                    .hits
                    .iter()
                    .map(|(r, _, _)| r.clone())
                    .filter(|r| {
                        matches!(
                            RuleRef::from_id(r),
                            Some(RuleRef::User(_) | RuleRef::Pack(_) | RuleRef::List(_))
                        )
                    })
                    .collect();
                let bound = rules(layers);
                a.preview.caught_30d = bound.caught_30d;
                a.preview.per_day = bound.per_day;
            }
            Change::RemoveCs1 { hashes } => {
                let hs: Vec<[u8; 20]> =
                    hashes.iter().map(|h| hex20(h)).collect::<ApiResult<_>>()?;
                a.weakening = true;
                a.preview.tombstones = snap.tombstones.iter().filter(|t| hs.contains(&t.0)).count();
            }
            Change::Release { hash } => {
                let h = hex20(hash)?;
                a.weakening = true;
                a.preview.quarantine = snap.quarantine.iter().filter(|q| q.hash == h).count();
                a.preview.tombstones = snap.tombstones.iter().filter(|t| t.0 == h).count();
            }
            Change::RevealName { hash } => {
                hex20(hash)?;
                a.weakening = true;
            }
            Change::Unsubscribe { id } => {
                a.weakening = true;
                a.preview = rules(vec![format!("L-{id}")]);
            }
            Change::PauseSubscription { id, enabled } => {
                if !enabled {
                    a.weakening = true;
                    a.preview = rules(vec![format!("L-{id}")]);
                }
            }
            Change::ResolveHeld { id, accept } => {
                let sub = store
                    .subscription(*id)
                    .map_err(internal)?
                    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no such subscription"))?;
                let shrank = sub.error.as_deref().is_some_and(|e| e.contains("shrank"));
                if *accept && shrank {
                    a.weakening = true;
                    a.preview = rules(vec![format!("L-{id}")]);
                }
            }
            Change::Subscribe { floor, kind, .. } => {
                if *floor && kind != "content" {
                    return Err(bad("only content lists can join the floor"));
                }
                a.confirm = *floor;
            }
            Change::Undo => {
                a.weakening = true;
                a.preview.estimate = true;
            }
            Change::RemoveOverride { .. }
            | Change::RemoveException { .. }
            | Change::RemoveAllow { .. }
            | Change::ReportAbuse { .. }
            | Change::Confirm { .. } => {}
        }
        a.confirm |= a.weakening;
        if a.confirm {
            a.warning = Some(warning(change, &a.preview, a.neutralize.as_ref()));
        }
        Ok(a)
    }

    /// Reloads the rule layers and switches from the store.
    fn reload(&self, store: &Store) -> ApiResult<()> {
        let set = store.rule_set().map_err(internal)?;
        let state = store.filter_state().map_err(internal)?;
        for note in self.engine.update_engine(|e| {
            for (rule, on) in &state.switches {
                if let Some(r) = CoreRule::from_id(rule) {
                    let _ = e.core.switches.set(r, *on);
                }
            }
            e.load_rules(&set)
        }) {
            tracing::warn!("filters: {note}");
        }
        Ok(())
    }

    /// The current rule set, kept for one-step Undo once a change to it
    /// has succeeded.
    fn previous_data(&self, store: &Store) -> ApiResult<Vec<u8>> {
        let set = store.rule_set().map_err(internal)?;
        let switches = store.filter_state().map_err(internal)?.switches;
        let data = json!({
            "rules": set.rules.iter().map(|r| json!({
                "id": r.id, "query": r.query, "action": r.action, "enabled": r.enabled,
                "shadow_until": r.shadow_until, "unless": r.unless_query,
            })).collect::<Vec<_>>(),
            "packs": set.pack_switches,
            "overrides": set.overrides.iter().map(|o| json!({
                "id": o.id, "kind": o.kind, "value": hex(&o.value), "strength": o.strength,
                "scope": o.scope, "created_at": o.created_at, "expires_at": o.expires_at,
                "shadow_until": o.shadow_until, "hits": o.hits,
            })).collect::<Vec<_>>(),
            "exceptions": set.exceptions.iter().map(|e| json!({
                "id": e.id, "rule": e.rule, "kind": e.kind, "value": hex(&e.value),
            })).collect::<Vec<_>>(),
            "allow": set.allow,
            "core": switches,
        });
        Ok(data.to_string().into_bytes())
    }

    fn undo(&self, store: &mut Store) -> ApiResult<()> {
        let data = store
            .previous()
            .map_err(internal)?
            .ok_or_else(|| ApiError::new(StatusCode::CONFLICT, "nothing to undo"))?;
        let v: Value = serde_json::from_slice(&data).map_err(internal)?;
        let unhex = |s: &Value| -> Vec<u8> {
            let s = s.as_str().unwrap_or_default();
            (0..s.len() / 2)
                .filter_map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok())
                .collect()
        };
        let arr = |k: &str| v[k].as_array().cloned().unwrap_or_default();
        let set = RuleSet {
            rules: arr("rules")
                .iter()
                .map(|r| RuleRow {
                    id: r["id"].as_i64().unwrap_or_default(),
                    query: r["query"].as_str().unwrap_or_default().to_owned(),
                    action: r["action"].as_u64().unwrap_or_default() as u8,
                    enabled: r["enabled"].as_bool().unwrap_or(true),
                    shadow_until: r["shadow_until"].as_i64(),
                    unless_query: r["unless"].as_str().map(str::to_owned),
                })
                .collect(),
            pack_switches: arr("packs")
                .iter()
                .filter_map(|p| Some((p[0].as_str()?.to_owned(), p[1].as_bool()?)))
                .collect(),
            overrides: arr("overrides")
                .iter()
                .map(|o| OverrideRow {
                    id: o["id"].as_i64().unwrap_or_default(),
                    kind: o["kind"].as_u64().unwrap_or_default() as u8,
                    value: unhex(&o["value"]),
                    strength: o["strength"].as_u64().unwrap_or_default() as u8,
                    scope: o["scope"].as_str().map(str::to_owned),
                    created_at: o["created_at"].as_i64().unwrap_or_default(),
                    expires_at: o["expires_at"].as_i64(),
                    shadow_until: o["shadow_until"].as_i64(),
                    hits: o["hits"].as_i64().unwrap_or_default(),
                })
                .collect(),
            exceptions: arr("exceptions")
                .iter()
                .map(|e| ExceptionRow {
                    id: e["id"].as_i64().unwrap_or_default(),
                    rule: e["rule"].as_str().unwrap_or_default().to_owned(),
                    kind: e["kind"].as_u64().unwrap_or_default() as u8,
                    value: unhex(&e["value"]),
                })
                .collect(),
            allow: arr("allow")
                .iter()
                .filter_map(|a| Some((a[0].as_i64()?, a[1].as_str()?.to_owned())))
                .collect(),
        };
        // The set being replaced becomes the one Undo goes back to.
        let replaced = self.previous_data(store)?;
        store.replace_rule_set(&set).map_err(internal)?;
        for r in CoreRule::ALL.iter().filter(|r| !r.floor()) {
            let on = arr("core")
                .iter()
                .find(|c| c[0].as_str() == Some(r.id()))
                .and_then(|c| c[1].as_bool())
                .unwrap_or(true);
            store.set_core_switch(r.id(), on).map_err(internal)?;
        }
        store.save_previous(&replaced).map_err(internal)
    }

    fn release(&self, store: &mut Store, hash: [u8; 20]) -> ApiResult<bool> {
        let had = store.release(&hash).map_err(internal)?;
        self.held.remove(&hash);
        Ok(had)
    }

    /// Applies `change`; the caller has already checked the password.
    pub fn apply(&self, change: &Change, now: i64) -> ApiResult<Value> {
        let mut guard = self.store();
        let store = &mut *guard;
        let rule_layers = matches!(
            change,
            Change::CoreSwitch { .. }
                | Change::PackSwitch { .. }
                | Change::AddRule { .. }
                | Change::UpdateRule { .. }
                | Change::RemoveRule { .. }
                | Change::AddOverride { .. }
                | Change::RemoveOverride { .. }
                | Change::AddException { .. }
                | Change::RemoveException { .. }
                | Change::AddAllow { .. }
                | Change::RemoveAllow { .. }
        );
        // Read before the change, saved only once it succeeds, so a
        // refused change never replaces what Undo goes back to.
        let previous = if rule_layers {
            Some(self.previous_data(store)?)
        } else {
            None
        };
        let gone = || ApiError::new(StatusCode::NOT_FOUND, "no such entry");
        let mut out = json!({});
        match change {
            Change::CoreSwitch { rule, on } => {
                let r = CoreRule::from_id(rule).ok_or_else(|| bad("no such rule"))?;
                if r.floor() {
                    if !on {
                        return Err(ApiError::new(StatusCode::CONFLICT, "the floor is locked"));
                    }
                } else {
                    store.set_core_switch(r.id(), *on).map_err(internal)?;
                    if !on {
                        // Its quarantine is released and processed (§20.5.3).
                        let held: Vec<[u8; 20]> = store
                            .quarantined(PREVIEW_ROWS)
                            .map_err(internal)?
                            .into_iter()
                            .filter(|q| q.rule == r.id())
                            .map(|q| q.hash)
                            .collect();
                        for h in &held {
                            self.release(store, *h)?;
                        }
                        out["released"] = json!(held.len());
                    }
                }
            }
            Change::PackSwitch { id, on } => {
                pack_rules(id)?;
                store.set_pack_switch(id, *on).map_err(internal)?;
            }
            Change::AddRule {
                query: q,
                action: act,
                unless,
                shadow_days,
            } => {
                let q = query(q)?.to_query();
                let unless = unless
                    .as_deref()
                    .map(query)
                    .transpose()?
                    .map(|u| u.to_query());
                let id = store
                    .add_rule(
                        &q,
                        action(act)?.code(),
                        days(*shadow_days, now),
                        unless.as_deref(),
                    )
                    .map_err(internal)?;
                out["id"] = json!(id);
            }
            Change::UpdateRule {
                id,
                query: q,
                action: act,
                enabled,
                unless,
                shadow_days,
            } => {
                let mut row = store
                    .rule_set()
                    .map_err(internal)?
                    .rules
                    .into_iter()
                    .find(|r| r.id == *id)
                    .ok_or_else(gone)?;
                if let Some(q) = q {
                    row.query = query(q)?.to_query();
                }
                if let Some(a) = act {
                    row.action = action(a)?.code();
                }
                if let Some(e) = enabled {
                    row.enabled = *e;
                }
                if let Some(u) = unless {
                    row.unless_query = if u.is_empty() {
                        None
                    } else {
                        Some(query(u)?.to_query())
                    };
                }
                if shadow_days.is_some() {
                    row.shadow_until = days(*shadow_days, now);
                }
                store.update_rule(&row).map_err(internal)?;
            }
            Change::RemoveRule { id } => {
                if !store.remove_rule(*id).map_err(internal)? {
                    return Err(gone());
                }
            }
            Change::AddOverride {
                what,
                neutralize,
                scope,
                expires_days,
                shadow_days,
            } => {
                let o = override_entry(
                    what,
                    *neutralize,
                    scope.as_deref(),
                    *expires_days,
                    *shadow_days,
                    now,
                )?;
                let (kind, value) = encode_match(&o.what);
                let strength = u8::from(o.strength == Strength::Neutralize);
                let id = store
                    .add_override(
                        kind,
                        &value,
                        strength,
                        o.scope.as_ref().map(Node::to_query).as_deref(),
                        now,
                        o.expires_at,
                        o.shadow_until,
                    )
                    .map_err(internal)?;
                out["id"] = json!(id);
            }
            Change::RemoveOverride { id } => {
                if !store.remove_override(*id).map_err(internal)? {
                    return Err(gone());
                }
            }
            Change::AddException { rule, what } => {
                let e = exception_entry(rule, what)?;
                let (kind, value) = encode_match(&e.what);
                let id = store
                    .add_exception(&e.rule.id(), kind, &value)
                    .map_err(internal)?;
                out["id"] = json!(id);
            }
            Change::RemoveException { id } => {
                if !store.remove_exception(*id).map_err(internal)? {
                    return Err(gone());
                }
            }
            Change::AddAllow { query: q } => {
                let id = store.add_allow(&query(q)?.to_query()).map_err(internal)?;
                out["id"] = json!(id);
            }
            Change::RemoveAllow { id } => {
                if !store.remove_allow(*id).map_err(internal)? {
                    return Err(gone());
                }
            }
            Change::ReportAbuse { hashes } => {
                let hs: Vec<[u8; 20]> =
                    hashes.iter().map(|h| hex20(h)).collect::<ApiResult<_>>()?;
                store
                    .add_cs_hashes(&hs, Origin::Reported.code(), now)
                    .map_err(internal)?;
                // Removed from the index and store by the ingest writer's
                // purge in a later slice; refused from now on.
                self.engine.update(|c| {
                    for h in &hs {
                        c.cs.add_hash(*h, Origin::Reported);
                    }
                });
            }
            Change::RemoveCs1 { hashes } => {
                let hs: Vec<[u8; 20]> =
                    hashes.iter().map(|h| hex20(h)).collect::<ApiResult<_>>()?;
                let n = store.remove_cs_hashes(&hs).map_err(internal)?;
                self.engine.update(|c| {
                    for h in &hs {
                        c.cs.remove_hash(h);
                    }
                });
                for h in &hs {
                    self.release(store, *h)?;
                }
                out["removed"] = json!(n);
            }
            Change::Release { hash } => {
                if !self.release(store, hex20(hash)?)? {
                    return Err(gone());
                }
            }
            Change::Confirm { hash } => {
                let h = hex20(hash)?;
                let held = store
                    .quarantined(PREVIEW_ROWS)
                    .map_err(internal)?
                    .into_iter()
                    .any(|q| q.hash == h);
                if !held {
                    return Err(gone());
                }
                store
                    .add_cs_hashes(&[h], Origin::Confirmed.code(), now)
                    .map_err(internal)?;
                // Deletes the sealed name with the quarantine row.
                store.release(&h).map_err(internal)?;
                self.engine.update(|c| c.cs.add_hash(h, Origin::Confirmed));
            }
            Change::RevealName { hash } => {
                let h = hex20(hash)?;
                let q = store
                    .quarantined(PREVIEW_ROWS)
                    .map_err(internal)?
                    .into_iter()
                    .find(|q| q.hash == h)
                    .ok_or_else(gone)?;
                let name = self
                    .sealer
                    .as_deref()
                    .zip(q.sealed.as_deref())
                    .and_then(|(s, sealed)| s.open(sealed).ok())
                    .ok_or_else(|| ApiError::new(StatusCode::GONE, "no sealed name is kept"))?;
                out["name"] = json!(name);
            }
            Change::Subscribe {
                url,
                name,
                kind,
                format,
                action: act,
                floor,
                refresh_hours,
            } => {
                let ip = match kind.as_str() {
                    "ip" => true,
                    "content" => false,
                    _ => return Err(bad("kind: content or ip")),
                };
                if !(url.starts_with("https://") || url.starts_with("http://")) {
                    return Err(bad("the list's URL must be http or https"));
                }
                let act = act
                    .as_deref()
                    .map(action)
                    .transpose()?
                    .unwrap_or(Action::Drop);
                if !act.protective() {
                    return Err(bad("lists drop or quarantine"));
                }
                if let Some(f) = format {
                    let fm = lodestar_filter::lists::Format::from_name(f)
                        .ok_or_else(|| bad("format: hashes, p2p, dat or cidr"))?;
                    if fm.is_ip() != ip {
                        return Err(bad("that format doesn't fit this kind of list"));
                    }
                }
                let id = store
                    .add_subscription(&lodestar_store::NewSubscription {
                        url,
                        name: name.as_deref().unwrap_or(url),
                        kind: u8::from(ip),
                        format: format.as_deref(),
                        action: act.code(),
                        floor: *floor && !ip,
                        interval_secs: i64::from(refresh_hours.unwrap_or(24).max(1)) * 3600,
                    })
                    .map_err(internal)?;
                out["id"] = json!(id);
            }
            Change::Unsubscribe { id } => {
                if !store.remove_subscription(*id).map_err(internal)? {
                    return Err(gone());
                }
                self.release_list(store, *id)?;
            }
            Change::PauseSubscription { id, enabled } => {
                if !store
                    .set_subscription_enabled(*id, *enabled)
                    .map_err(internal)?
                {
                    return Err(gone());
                }
                if !enabled {
                    self.release_list(store, *id)?;
                }
            }
            Change::ResolveHeld { id, accept } => {
                if !store.resolve_held(*id, *accept, now).map_err(internal)? {
                    return Err(ApiError::new(StatusCode::CONFLICT, "no version is held"));
                }
            }
            Change::Undo => self.undo(store)?,
        }
        if let Some(p) = previous {
            store.save_previous(&p).map_err(internal)?;
        }
        if matches!(
            change,
            Change::Subscribe { .. }
                | Change::Unsubscribe { .. }
                | Change::PauseSubscription { .. }
                | Change::ResolveHeld { .. }
        ) {
            lodestar_filter::lists::apply(store, &self.engine, &self.ips).map_err(internal)?;
        }
        self.reload(store)?;
        out["version"] = json!(self.engine.current().version);
        Ok(out)
    }

    /// A list that stopped applying lets its tombstones go.
    fn release_list(&self, store: &mut Store, id: i64) -> ApiResult<()> {
        let rule = format!("L-{id}");
        let dropped: Vec<[u8; 20]> = store
            .tombstones(PREVIEW_ROWS)
            .map_err(internal)?
            .into_iter()
            .filter(|t| t.1 == rule)
            .map(|t| t.0)
            .collect();
        for h in dropped {
            self.release(store, h)?;
        }
        Ok(())
    }

    /// The rule set, switches and lists, as the Filters & Safety screens
    /// show them.
    pub fn overview(&self) -> ApiResult<Value> {
        let engine = self.engine.current();
        let store = self.store();
        let set = store.rule_set().map_err(internal)?;
        let subs = store.subscriptions().map_err(internal)?;
        let quarantine = store.quarantined(PREVIEW_ROWS).map_err(internal)?.len();
        let match_json = |kind: u8, value: &[u8]| match kind {
            KIND_HASHES => json!({ "hashes": value.chunks(20).map(hex).collect::<Vec<_>>() }),
            KIND_KEYWORD => json!({ "keyword": String::from_utf8_lossy(value) }),
            KIND_QUERY => json!({ "query": String::from_utf8_lossy(value) }),
            KIND_PAYLOAD => json!({ "payload": hex(value) }),
            _ => json!({}),
        };
        Ok(json!({
            "version": engine.version,
            "core": CoreRule::ALL.iter().map(|r| json!({
                "id": r.id(),
                "floor": r.floor(),
                "on": engine.core.switches.is_on(*r),
                "action": r.action().as_str(),
            })).collect::<Vec<_>>(),
            "packs": Pack::ALL.iter().map(|p| json!({
                "id": p.as_str(),
                "on": engine.packs.on.contains(p),
                "rules": PackRule::ALL.iter().filter(|r| r.pack() == *p).map(|r| json!({
                    "id": r.id(),
                    "on": engine.packs.is_on(*r),
                    "action": r.action().as_str(),
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "rules": set.rules.iter().map(|r| json!({
                "id": r.id,
                "query": r.query,
                "action": Action::from_code(r.action).map(Action::as_str),
                "enabled": r.enabled,
                "shadow_until": r.shadow_until,
                "unless": r.unless_query,
            })).collect::<Vec<_>>(),
            "overrides": set.overrides.iter().map(|o| {
                let mut v = match_json(o.kind, &o.value);
                v["id"] = json!(o.id);
                v["strength"] = json!(if o.strength == 1 { "neutralize" } else { "override" });
                v["scope"] = json!(o.scope);
                v["expires_at"] = json!(o.expires_at);
                v["shadow_until"] = json!(o.shadow_until);
                v
            }).collect::<Vec<_>>(),
            "exceptions": set.exceptions.iter().map(|e| {
                let mut v = match_json(e.kind, &e.value);
                v["id"] = json!(e.id);
                v["rule"] = json!(e.rule);
                v
            }).collect::<Vec<_>>(),
            "allow": set.allow.iter().map(|(id, q)| json!({ "id": id, "query": q })).collect::<Vec<_>>(),
            "cs1_entries": engine.core.cs.len(),
            "quarantine": quarantine,
            "ip_ranges": self.ips.current().len(),
            "subscriptions": subs.iter().map(|s| json!({
                "id": s.id,
                "name": s.name,
                "url": s.url,
                "kind": if s.kind == lodestar_filter::lists::KIND_IP { "ip" } else { "content" },
                "format": s.format,
                "action": Action::from_code(s.action).map(Action::as_str),
                "floor": s.floor,
                "enabled": s.enabled,
                "entries": s.entries,
                "status": s.status,
                "error": s.error,
                "checked_at": s.checked_at,
                "updated_at": s.updated_at,
            })).collect::<Vec<_>>(),
        }))
    }

    /// The review queue (§20.7): quarantine, never with names, and the
    /// replay window's decisions per rule.
    pub fn queue(&self, now: i64) -> ApiResult<Value> {
        let store = self.store();
        let q = store.quarantined(1_000).map_err(internal)?;
        let hits = store.hit_counts(now - REPLAY_SECS).map_err(internal)?;
        Ok(json!({
            "quarantine": q.iter().map(|q| json!({
                "hash": hex(&q.hash),
                "rule": q.rule,
                "at": q.at,
                "expires_at": q.expires_at,
                "sealed": q.sealed.is_some(),
            })).collect::<Vec<_>>(),
            "decisions_30d": hits.iter().map(|(rule, act, n)| json!({
                "rule": rule,
                "action": Action::from_code(act & !lodestar_store::SHADOW).map(Action::as_str),
                "shadow": act & lodestar_store::SHADOW != 0,
                "count": n,
            })).collect::<Vec<_>>(),
        }))
    }
}

fn filters(s: &ApiState) -> ApiResult<Arc<FiltersApi>> {
    s.filters
        .clone()
        .ok_or_else(|| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "filtering isn't available"))
}

#[derive(Debug, Deserialize)]
pub struct ChangeBody {
    pub change: Change,
    pub password: Option<String>,
}

pub(crate) async fn overview(State(s): State<ApiState>) -> ApiResult<Json<Value>> {
    let f = filters(&s)?;
    tokio::task::spawn_blocking(move || f.overview().map(Json))
        .await
        .map_err(internal)?
}

pub(crate) async fn queue(State(s): State<ApiState>) -> ApiResult<Json<Value>> {
    let f = filters(&s)?;
    tokio::task::spawn_blocking(move || f.queue(crate::unix_now()).map(Json))
        .await
        .map_err(internal)?
}

pub(crate) async fn preview(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<ChangeBody>,
) -> ApiResult<Json<Assessment>> {
    same_origin_json(&headers)?;
    let f = filters(&s)?;
    tokio::task::spawn_blocking(move || f.assess(&body.change, crate::unix_now()).map(Json))
        .await
        .map_err(internal)?
}

pub(crate) async fn apply(
    State(s): State<ApiState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    Json(body): Json<ChangeBody>,
) -> ApiResult<Json<Value>> {
    same_origin_json(&headers)?;
    let f = filters(&s)?;
    let auth = s.auth.clone();
    tokio::task::spawn_blocking(move || apply_change(&f, &auth, ip, body))
        .await
        .map_err(internal)?
}

fn apply_change(
    f: &FiltersApi,
    auth: &crate::Auth,
    ip: IpAddr,
    body: ChangeBody,
) -> ApiResult<Json<Value>> {
    let now = crate::unix_now();
    let assessment = f.assess(&body.change, now)?;
    if assessment.confirm {
        match body.password.as_deref() {
            None | Some("") => {
                return Err(ApiError {
                    status: StatusCode::PRECONDITION_REQUIRED,
                    body: json!({
                        "error": "this change weakens filtering: read the warning and confirm with your password",
                        "assessment": assessment,
                    }),
                });
            }
            Some(p) => auth.confirm(ip, p).map_err(auth_error)?,
        }
    }
    let mut out = f.apply(&body.change, now)?;
    out["applied"] = json!(true);
    out["assessment"] = serde_json::to_value(&assessment).map_err(internal)?;
    Ok(Json(out))
}
