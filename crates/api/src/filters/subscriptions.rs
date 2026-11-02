//! Blocklist subscriptions in the filters API (architecture §20.13,
//! owner's decision §2.9): the catalog and your lists with their
//! strictness, and what the subscription changes would do.
//!
//! Connecting, pausing, removing and settling a held version are changes
//! like any other: previewed with `POST /filters/preview` and applied with
//! `POST /filters/apply`, behind the same effect warning and password
//! (§20.11). Nothing here subscribes anything.

use std::collections::HashSet;

use sha2::Digest as _;

use axum::Json;
use axum::extract::State;
use lodestar_filter::lists::{Format, IpSet, KIND_IP};
use lodestar_filter::strictness::{self, Candidate, Category, Facts, ListFigures};
use lodestar_filter::{Action, Engine};
use lodestar_store::{FloorSource, Store, Subscription};

use super::{FiltersApi, action, bad, filters, not_found};
use crate::ApiState;
use crate::dto::filters::{
    CatalogList, Change, ConnectedList, Preview, Strictness, StrictnessList, SubscriptionEntry,
    Subscriptions,
};
use crate::error::{ApiResult, internal};
use lodestar_integrations::net::{NetError, NetPolicy, parse_url};

/// The longest list URL and name accepted.
const MAX_URL: usize = 2048;
const MAX_NAME: usize = 200;

/// The rule id a list's holds are stored under.
pub(super) fn rule_id(id: i64) -> String {
    format!("L-{id}")
}

fn hashes(data: &[u8]) -> HashSet<[u8; 20]> {
    data.chunks_exact(20)
        .map(|c| <[u8; 20]>::try_from(c).expect("20 bytes"))
        .collect()
}

fn kind_name(ip: bool) -> &'static str {
    if ip { "ip" } else { "content" }
}

/// A `subscribe` change, checked: the same in the preview and in apply.
pub(super) struct NewList<'a> {
    pub ip: bool,
    pub action: Action,
    pub url: &'a str,
    pub name: String,
    pub format: Option<&'a str>,
    pub floor: bool,
}

pub(super) fn check_subscribe(change: &Change) -> ApiResult<Option<NewList<'_>>> {
    let Change::Subscribe {
        url,
        name,
        kind,
        format,
        action: act,
        floor,
        ..
    } = change
    else {
        return Ok(None);
    };
    let ip = match kind.as_str() {
        "ip" => true,
        "content" => false,
        _ => {
            return Err(bad("unknown_list_kind", "A list's kind is content or ip.").field("kind"));
        }
    };
    let printable = |s: &str| !s.chars().any(char::is_control);
    let bad_url = || {
        bad(
            "bad_list_url",
            "The list's URL must be http or https, with a host and no user name or password.",
        )
        .field("url")
    };
    if url.len() > MAX_URL || !printable(url) || url.contains(' ') {
        return Err(bad_url());
    }
    // The SSRF guard's DNS-free checks (§16): scheme, host, no
    // credentials, and a literal address only if it's public. The fetch
    // checks every resolved address and redirect again.
    let parsed = match parse_url(url, NetPolicy::notification(false)) {
        Ok(u) => u,
        Err(NetError::Blocked(_)) => {
            return Err(bad(
                "list_url_refused",
                "Lists are fetched from public addresses only, never a private, loopback or metadata address.",
            )
            .field("url"));
        }
        Err(_) => return Err(bad_url()),
    };
    if let Some(n) = name
        && (n.trim().is_empty() || n.chars().count() > MAX_NAME || !printable(n))
    {
        return Err(bad("bad_list_name", "A list's name is 1–200 characters.").field("name"));
    }
    // Without a name, the host names it.
    let name = match name {
        Some(n) => n.clone(),
        None => parsed
            .host_str()
            .unwrap_or_default()
            .chars()
            .take(MAX_NAME)
            .collect(),
    };
    let act = act
        .as_deref()
        .map(action)
        .transpose()?
        .unwrap_or(Action::Drop);
    if !act.protective() {
        return Err(
            bad("bad_list_action", "A list's action is drop or quarantine.").field("action"),
        );
    }
    if let Some(f) = format {
        let fm = Format::from_name(f).ok_or_else(|| {
            bad(
                "unknown_list_format",
                "The format must be hashes, p2p, dat or cidr.",
            )
            .field("format")
        })?;
        if fm.is_ip() != ip {
            return Err(bad(
                "list_format_mismatch",
                "That format doesn't fit this kind of list.",
            )
            .field("format"));
        }
    }
    if *floor && ip {
        return Err(bad(
            "floor_needs_content_list",
            "Only content lists can join the floor.",
        )
        .field("floor"));
    }
    Ok(Some(NewList {
        ip,
        action: act,
        url,
        name,
        format: format.as_deref(),
        floor: *floor,
    }))
}

pub(super) fn subscription(store: &Store, id: i64) -> ApiResult<Subscription> {
    store
        .subscription(id)
        .map_err(internal)?
        .ok_or_else(|| not_found("subscription_not_found", "There's no such subscription."))
}

/// What accepting a list's held version would unblock.
pub(super) struct HeldEffect {
    /// The held version's fingerprint: SHA-256 of its data, in hex.
    pub version: String,
    /// It unblocks something the applied version blocks.
    pub loosens: bool,
    /// How many entries stop being blocked: hashes for a content list,
    /// applied ranges not wholly covered for an IP list.
    pub unblocked: usize,
    /// For a content list: the hashes the applied version lists and the
    /// held one doesn't.
    pub dropped: HashSet<[u8; 20]>,
}

/// `None` when no version is held. Read under the store lock in apply, so
/// a refresh since the preview can't widen what the password confirmed
/// without a 409: the fingerprint binds the applied version as well as
/// the held one, because what accepting unblocks is their difference.
pub(super) fn held_effect(store: &Store, sub: &Subscription) -> ApiResult<Option<HeldEffect>> {
    let Some(held) = store.subscription_data(sub.id, true).map_err(internal)? else {
        return Ok(None);
    };
    let applied = store.subscription_data(sub.id, false).map_err(internal)?;
    let base = sha2::Sha256::digest(applied.as_deref().unwrap_or_default());
    let digest = sha2::Sha256::new()
        .chain_update(base)
        .chain_update(&held)
        .finalize();
    let mut effect = HeldEffect {
        version: digest.iter().map(|b| format!("{b:02x}")).collect(),
        loosens: false,
        unblocked: 0,
        dropped: HashSet::new(),
    };
    let Some(applied) = applied else {
        return Ok(Some(effect));
    };
    if sub.kind == KIND_IP {
        // What doesn't decode blocks nothing, in the engine as here.
        let new = IpSet::decode(&held).unwrap_or_default();
        if let Some(old) = IpSet::decode(&applied) {
            effect.unblocked = new.uncovered(&old);
        }
        effect.loosens = effect.unblocked > 0;
    } else {
        let new = hashes(&held);
        effect.dropped = hashes(&applied)
            .into_iter()
            .filter(|h| !new.contains(h))
            .collect();
        effect.unblocked = effect.dropped.len();
        effect.loosens = effect.unblocked > 0;
    }
    Ok(Some(effect))
}

/// The holds a list keeps on `dropped`: how many are quarantined and
/// tombstoned, and the hashes, for releasing exactly what was counted.
pub(super) fn dropped_holds(
    store: &Store,
    id: i64,
    dropped: &HashSet<[u8; 20]>,
) -> ApiResult<(usize, usize, Vec<[u8; 20]>)> {
    let (q, t) = store.held_by_rule(&rule_id(id)).map_err(internal)?;
    let q: Vec<_> = q.into_iter().filter(|h| dropped.contains(h)).collect();
    let t: Vec<_> = t.into_iter().filter(|h| dropped.contains(h)).collect();
    let (nq, nt) = (q.len(), t.len());
    let mut all: Vec<_> = q.into_iter().chain(t).collect();
    all.sort_unstable();
    all.dedup();
    Ok((nq, nt, all))
}

/// How many entries leave the locked floor when a floor-marked list stops
/// applying: its version and its pending removals (§2.7), less what the
/// floor holds anyway (CS-1 entries, other floor-marked lists).
pub(super) fn floor_leaving(
    store: &Store,
    engine: &Engine,
    sub: &Subscription,
) -> ApiResult<usize> {
    if !sub.floor || !sub.enabled {
        return Ok(0);
    }
    let mut entries = store
        .subscription_data(sub.id, false)
        .map_err(internal)?
        .map(|d| hashes(&d))
        .unwrap_or_default();
    entries.extend(
        store
            .pending_floor_of(&FloorSource::List(sub.id))
            .map_err(internal)?,
    );
    let cs = &engine.core.cs;
    Ok(entries
        .iter()
        .filter(|h| !cs.held_without_list(h, sub.id as u32))
        .count())
}

/// The effect warning for a subscription change that weakens filtering.
/// `unblocked`: for `resolve_held`, the entries that stop being blocked.
pub(super) fn warning(
    change: &Change,
    sub: &Subscription,
    p: &Preview,
    unblocked: usize,
    n: &str,
) -> String {
    let what = if sub.kind == KIND_IP {
        "The crawler will contact the peers and nodes it blocks again."
    } else {
        "Torrents it blocks can be fetched and indexed again."
    };
    let floor = if p.removed_from_floor > 0 {
        format!(
            " It's part of the locked floor: {} entries leave the floor and are blocked only \
             if another list or rule still catches them. Only do this if you know the list \
             is wrong.",
            p.removed_from_floor
        )
    } else {
        String::new()
    };
    match change {
        Change::ResolveHeld { .. } => format!(
            "This applies the version of {} the sanity gates held back. {unblocked} {} it \
             no longer has {} blocked. {what} {} Safer: keep the previous version until the \
             publisher's next update.",
            sub.name,
            match (sub.kind == KIND_IP, unblocked == 1) {
                (true, true) => "address range",
                (true, false) => "address ranges",
                (false, true) => "entry",
                (false, false) => "entries",
            },
            if unblocked == 1 {
                "stops being"
            } else {
                "stop being"
            },
            holds_line(p)
        ),
        Change::PauseSubscription { .. } => format!(
            "Pausing {} stops applying it until you resume it. {what}{floor} {n}",
            sub.name
        ),
        _ => format!(
            "Removing {} stops applying it and forgets its data. {what}{floor} {n} \
             Safer: pause it, or add an exception for what it got wrong.",
            sub.name
        ),
    }
}

/// What accepting a held version releases now, exactly: the list's holds
/// on the entries it drops. (The 30-day figures count the whole list, so
/// they'd overstate it.)
fn holds_line(p: &Preview) -> String {
    if p.quarantine + p.tombstones == 0 {
        "Nothing it holds now is released.".to_owned()
    } else {
        format!(
            "{} items in quarantine would be released and {} dropped items could be fetched again.",
            p.quarantine, p.tombstones
        )
    }
}

fn categories(sub: &Subscription) -> Vec<Category> {
    let ip = sub.kind == KIND_IP;
    match strictness::CATALOG.iter().find(|e| e.url == sub.url) {
        Some(e) => e.categories.to_vec(),
        None if ip => vec![Category::Peers],
        None if sub.floor => vec![Category::ChildAbuse],
        None => vec![Category::Content],
    }
}

/// One of your lists as a strictness candidate, measured from its applied
/// version (`pending`: its held one, when there is one).
pub(super) fn candidate(store: &Store, sub: &Subscription, pending: bool) -> ApiResult<Candidate> {
    let ip = sub.kind == KIND_IP;
    let listed = strictness::CATALOG.iter().find(|e| e.url == sub.url);
    let mut data = None;
    if pending {
        data = store.subscription_data(sub.id, true).map_err(internal)?;
    }
    if data.is_none() {
        data = store.subscription_data(sub.id, false).map_err(internal)?;
    }
    let facts = match (data, ip) {
        (Some(d), true) => IpSet::decode(&d).map_or(Facts::Unknown, Facts::Ips),
        (Some(d), false) => {
            let mut n = (d.len() / 20) as u64;
            if sub.floor {
                n += store
                    .pending_floor_of(&FloorSource::List(sub.id))
                    .map_err(internal)?
                    .len() as u64;
            }
            Facts::Hashes(n)
        }
        (None, _) => listed.map_or(Facts::Unknown, |e| Facts::Measured(e.measured)),
    };
    Ok(Candidate {
        name: sub.name.clone(),
        ip,
        floor: sub.floor && !ip,
        categories: categories(sub),
        facts,
    })
}

/// A list not subscribed yet: the catalog's measurement when it's a
/// catalog list, else unknown until it's fetched.
fn new_candidate(new: &NewList<'_>) -> Candidate {
    match strictness::CATALOG.iter().find(|e| e.url == new.url) {
        Some(e) => Candidate {
            floor: new.floor,
            ..Candidate::from_catalog(e)
        },
        None => Candidate {
            name: new.name.clone(),
            ip: new.ip,
            floor: new.floor,
            categories: vec![match (new.ip, new.floor) {
                (true, _) => Category::Peers,
                (false, true) => Category::ChildAbuse,
                (false, false) => Category::Content,
            }],
            facts: Facts::Unknown,
        },
    }
}

fn cats(c: &[Category]) -> Vec<String> {
    c.iter().map(|c| c.as_str().to_string()).collect()
}

fn figures(l: &ListFigures) -> StrictnessList {
    StrictnessList {
        categories: cats(&l.categories),
        floor: l.floor,
        hashes: l.hashes,
        ipv4_public_share: l.v4_public_share,
        ipv4_share: l.v4_share,
        ipv6_ranges: l.v6_ranges,
        kind: kind_name(l.ip).to_string(),
        measured: l.measured,
        name: l.name.clone(),
    }
}

fn one(c: Candidate) -> StrictnessList {
    let a = strictness::assess(&[c]);
    figures(&a.lists[0])
}

pub(super) fn to_dto(a: &strictness::Strictness) -> Strictness {
    Strictness {
        categories: cats(&a.categories),
        estimate: a.estimate,
        floor_hashes: a.floor_hashes,
        hashes: a.hashes,
        ipv4_public_share: a.v4_public_share,
        ipv4_share: a.v4_share,
        ipv6_ranges: a.v6_ranges,
        ipv6_share: a.v6_share,
        level: a.level.as_str().to_string(),
        lists: a.lists.iter().map(figures).collect(),
        text: a.text.clone(),
        unknown: a.unknown.clone(),
    }
}

pub(super) fn entry(s: &Subscription) -> SubscriptionEntry {
    SubscriptionEntry {
        action: Action::from_code(s.action).map(|a| a.as_str().to_string()),
        checked_at: s.checked_at,
        enabled: s.enabled,
        entries: s.entries,
        error: s.error.clone(),
        floor: s.floor,
        format: s.format.clone(),
        id: s.id,
        kind: kind_name(s.kind == KIND_IP).to_string(),
        name: s.name.clone(),
        status: s.status.clone(),
        updated_at: s.updated_at,
        url: s.url.clone(),
    }
}

/// How strict the enabled lists would be after a subscription change;
/// `None` for any other change.
pub(super) fn strictness_after(store: &Store, change: &Change) -> ApiResult<Option<Strictness>> {
    if !matches!(
        change,
        Change::Subscribe { .. }
            | Change::Unsubscribe { .. }
            | Change::PauseSubscription { .. }
            | Change::ResolveHeld { .. }
    ) {
        return Ok(None);
    }
    let subs = store.subscriptions().map_err(internal)?;
    let mut extra = None;
    let (mut skip, mut resume, mut pending) = (None, None, None);
    match change {
        Change::Subscribe { url, .. } => {
            if !subs.iter().any(|s| &s.url == url)
                && let Some(new) = check_subscribe(change)?
            {
                extra = Some(new_candidate(&new));
            }
        }
        Change::Unsubscribe { id }
        | Change::PauseSubscription {
            id, enabled: false, ..
        } => skip = Some(*id),
        Change::PauseSubscription { id, .. } => resume = Some(*id),
        Change::ResolveHeld { id, accept, .. } => pending = accept.then_some(*id),
        _ => return Ok(None),
    }
    let mut candidates = Vec::new();
    for s in &subs {
        if (s.enabled || resume == Some(s.id)) && skip != Some(s.id) {
            candidates.push(candidate(store, s, pending == Some(s.id))?);
        }
    }
    candidates.extend(extra);
    Ok(Some(to_dto(&strictness::assess(&candidates))))
}

impl FiltersApi {
    /// The catalog, your lists and the enabled lists' strictness.
    pub fn subscriptions(&self) -> ApiResult<Subscriptions> {
        let store = self.store();
        let subs = store.subscriptions().map_err(internal)?;
        let mut enabled = Vec::new();
        let mut connected = Vec::new();
        for s in &subs {
            let c = candidate(&store, s, false)?;
            if s.enabled {
                enabled.push(c.clone());
            }
            connected.push(ConnectedList {
                figures: one(c),
                // Not the status: a failed fetch after a held one keeps
                // the held version waiting.
                held: store
                    .subscription_data(s.id, true)
                    .map_err(internal)?
                    .is_some(),
                subscription: entry(s),
            });
        }
        let mut catalog = Vec::new();
        for e in strictness::CATALOG {
            let sub = subs.iter().find(|s| s.url == e.url);
            let c = match sub {
                Some(s) => candidate(&store, s, false)?,
                None => Candidate::from_catalog(e),
            };
            catalog.push(CatalogList {
                about: e.about.to_owned(),
                figures: one(c),
                format: e.format.as_str().to_owned(),
                key: e.key.to_owned(),
                kind: kind_name(e.format.is_ip()).to_owned(),
                name: e.name.to_owned(),
                subscription: sub.map(|s| s.id),
                url: e.url.to_owned(),
            });
        }
        Ok(Subscriptions {
            catalog,
            connected,
            strictness: to_dto(&strictness::assess(&enabled)),
        })
    }
}

/// The one-click catalog and your lists, each with what it blocks, and
/// how strict the enabled lists are together (§2.9, §20.13). Connect,
/// pause, remove or settle a held version with `/filters/preview` and
/// `/filters/apply`.
#[utoipa::path(
    operation_id = "getFilterSubscriptions",
    get,
    path = "/filters/subscriptions",
    tag = "filters",
    responses((status = 200, body = Subscriptions))
)]
pub(crate) async fn list(State(s): State<ApiState>) -> ApiResult<Json<Subscriptions>> {
    let f = filters(&s)?;
    tokio::task::spawn_blocking(move || f.subscriptions().map(Json))
        .await
        .map_err(internal)?
}
